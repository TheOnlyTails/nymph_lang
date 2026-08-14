//! Inherent methods and namespaced functions: instance methods declared directly in a
//! `struct`/`enum` body (or a top-level inherent `impl Type { … }`), plus `namespace` functions.
//!
//! These are modelled as "impls without an interface": each has a self-type pattern
//! (with rigid `Param`s for the type's generics) and per-method signatures. The same
//! machinery as interface impls — fresh instantiation, receiver unification, `self`
//! substitution — drives both **resolution** (`recv.method(args)`) and **body
//! checking** (each method's body is checked with `this: Self` bound). Storing the
//! signatures once means an omitted return type's inference variable is shared
//! between the two, so a method like `is_some()` resolves to `boolean` for callers.

use crate::errors::TypeError;
use ecow::EcoString;
use nymph_ast::{
	Spanned,
	decl::{Declaration, FuncDeclaration, FuncKind, ImplMember, InterfaceElement, InterfaceMember},
	expr::Expr,
	ty::GenericParam,
};
use rustc_hash::FxHashMap;

use crate::check::Checker;
use crate::def::DefKind;
use crate::identity::DefinitionId;
use crate::ids::{DefId, ParamIdx};
use crate::iface::{Bound, Head, head_of};
use crate::lower::{build_param_scope, build_param_scope_at};
use crate::ty::{GenericArgs, Ty};

/// Owned semantic facts for one inherent method. Local syntax is kept separately in
/// [`InherentBodyJob`], so imported methods cannot accidentally become body jobs.
#[derive(Debug, Clone)]
pub struct InherentMethod {
	pub definition: Option<DefinitionId>,
	pub local_span: Option<nymph_ast::Span>,
	pub generic_names: Vec<EcoString>,
	pub params: Vec<Ty>,
	pub ret: Ty,
	/// The interface bounds declared on this method's own generics (Slice 4G-b),
	/// e.g. `func apply<U: Area>(u: U)` — one [`Bound`] per bound, with `ty =
	/// Param(base + j)` where `base` is the owner's generic count (the same offset
	/// `collect_impl_member` uses for the method's own scope, and `commit_inherent`'s
	/// subst covers), so a call site can substitute them exactly like `params`/`ret`.
	pub bounds: Vec<Bound>,
	pub namespaced: bool,
	pub mutating: bool,
	pub external: bool,
}

/// A set of inherent methods sharing a self type (a `struct`/`enum` body, or a
/// top-level inherent `impl`).
#[derive(Debug, Clone)]
pub struct InherentImpl {
	pub definition: Option<DefinitionId>,
	pub owner_generic_names: Vec<EcoString>,
	pub self_ty: Ty,
	pub methods: FxHashMap<EcoString, InherentMethod>,
	pub constraints: Vec<Bound>,
	pub imported: bool,
}

/// Current-module syntax required to infer/generalise and check a local method body.
pub(crate) struct InherentBodyJob<'m> {
	pub implementation: usize,
	pub method: EcoString,
	pub owner_generics: &'m [Spanned<GenericParam>],
	pub meta: &'m FuncDeclaration,
	pub body: &'m Expr,
}

/// Inherent impls indexed by the self type's head constructor.
#[derive(Debug, Default, Clone)]
pub struct InherentRegistry {
	pub impls: Vec<InherentImpl>,
	by_head: FxHashMap<Head, Vec<usize>>,
}

impl InherentRegistry {
	pub(crate) fn add(&mut self, head: Option<Head>, def: InherentImpl) -> usize {
		let idx = self.impls.len();
		if let Some(head) = head {
			self.by_head.entry(head).or_default().push(idx);
		}
		self.impls.push(def);
		idx
	}

	pub(crate) fn candidates(&self, head: Head) -> Vec<usize> {
		self.by_head.get(&head).cloned().unwrap_or_default()
	}

	/// The span of the non-namespaced (instance) inherent method named `name`
	/// reachable from `head`'s self type, if any. Used by `finish_interface_impl`
	/// (iface.rs, Slice 4K/HH3) to catch an interface-impl method (top-level `impl
	/// … for` or a nested `impl Iface { .. }`) colliding with a same-named inherent
	/// INSTANCE method on the same type — a collision formerly caught only by a
	/// runtime-lowering assertion and panic.
	///
	/// Deliberately excludes `namespace func` statics (`m.namespaced`): a static
	/// and an interface-impl instance method of the same name are DIFFERENT JS
	/// slots (a class static vs. a prototype method), so they're ordinary
	/// overloading, not a collision. Without this filter, a `namespace func foo`
	/// static false-positived a `DuplicateMember` against any interface-impl
	/// instance method also named `foo` — legal, resolvable JS the checker
	/// wrongly rejected (Slice 4K, Defect 2).
	pub(crate) fn method_span(&self, head: Head, name: &str) -> Option<nymph_ast::Span> {
		self.by_head.get(&head)?.iter().find_map(|&idx| {
			self.impls[idx]
				.methods
				.get(name)
				.filter(|m| !m.namespaced)
				.and_then(|m| m.local_span)
		})
	}
}

impl<'m> Checker<'m> {
	// ── Collection ───────────────────────────────────────────────────────────
	pub(crate) fn collect_inherent(&mut self) {
		let module = self.module;

		// Instance/namespaced functions declared inside struct/enum bodies.
		let adts: Vec<(DefId, usize)> = self
			.defs
			.defs
			.iter()
			.enumerate()
			.filter_map(|(i, d)| {
				let id = DefId(i as u32);
				matches!(d.kind, DefKind::Struct | DefKind::Enum)
					.then(|| self.defs.local_member(id).map(|member| (id, member)))
					.flatten()
			})
			.collect();
		for (def, member) in adts {
			self.collect_adt_inherent(def, member);
		}

		// Top-level inherent impls: `impl<G> Type { … }` (no `for`).
		for i in 0..module.members.len() {
			if let Declaration::Impl { .. } = &module.members[i] {
				self.collect_impl_inherent(i);
			}
		}
	}

	fn collect_adt_inherent(&mut self, def: DefId, member: usize) {
		let module = self.module;
		let (generics, members) = match &module.members[member] {
			Declaration::Struct {
				generics, members, ..
			} => (generics.as_slice(), members.as_slice()),
			Declaration::Enum {
				generics, members, ..
			} => (generics.as_slice(), members.as_slice()),
			_ => return,
		};
		let generics_len = generics.len();
		self.push_params(build_param_scope(generics));
		let positional = (0..generics_len)
			.map(|i| self.interner.mk_param(ParamIdx(i as u32)))
			.collect();
		let self_ty = self
			.interner
			.mk_adt(def, GenericArgs::new(positional, Vec::new()));

		// A `namespace func` is a static (`namespaced = true`); an instance `func`
		// and a `mut func` both attach to `this` (`namespaced = false`; `mut`
		// carries no extra checker restriction yet — see mutable-types, Task #1).
		// Nested interface impls live in the separate `impls` field and are
		// Milestone-B-later, so this inherent pass never sees them.
		let mut methods = FxHashMap::default();
		let mut body_jobs = Vec::new();
		for m in members {
			let namespaced = match &m.0 {
				ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => {
					meta.kind == FuncKind::Namespace
				}
				_ => false,
			};
			self.collect_impl_member(
				&m.0,
				generics_len,
				namespaced,
				self_ty,
				&mut methods,
				&mut body_jobs,
			);
		}
		self.pop_params();

		let head = head_of(&self.interner, self_ty);
		let implementation = self.inherent.add(
			head,
			InherentImpl {
				definition: self.defs.stable(def).cloned(),
				owner_generic_names: generics.iter().map(|g| g.0.name.0.clone()).collect(),
				self_ty,
				methods,
				constraints: Vec::new(),
				imported: false,
			},
		);
		self
			.inherent_body_jobs
			.extend(
				body_jobs
					.into_iter()
					.map(|(method, meta, body)| InherentBodyJob {
						implementation,
						method,
						owner_generics: generics,
						meta,
						body,
					}),
			);
	}

	fn collect_impl_inherent(&mut self, member: usize) {
		let module = self.module;
		let Declaration::Impl {
			generics,
			type_,
			members,
			..
		} = &module.members[member]
		else {
			return;
		};
		let generics_len = generics.len();
		self.push_params(build_param_scope(generics));
		let self_ty = self.lower_type(type_);
		let mut methods = FxHashMap::default();
		let mut body_jobs = Vec::new();
		for m in members {
			// Same kind-driven namespacing as a struct/enum body: a `namespace func`
			// in an `impl Type { … }` block is a static.
			let namespaced = match &m.0 {
				ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => {
					meta.kind == FuncKind::Namespace
				}
				_ => false,
			};
			self.collect_impl_member(
				&m.0,
				generics_len,
				namespaced,
				self_ty,
				&mut methods,
				&mut body_jobs,
			);
		}
		let constraints = self.lower_constraints(generics, 0);
		self.pop_params();

		let head = head_of(&self.interner, self_ty);
		if let Some(head) = head {
			let previous = self
				.inherent
				.candidates(head)
				.into_iter()
				.filter(|&idx| self.inherent.impls[idx].self_ty == self_ty)
				.flat_map(|idx| {
					self.inherent.impls[idx]
						.methods
						.iter()
						.filter_map(|(name, method)| {
							methods
								.get(name)
								.filter(|new| new.namespaced || method.namespaced)
								.and_then(|new| {
									let span = new.local_span?;
									Some((span, name.clone(), method.local_span.unwrap_or(span)))
								})
						})
				})
				.collect::<Vec<_>>();
			let ty = self.display(self_ty);
			for (span, name, prev) in previous {
				self.emit(
					span,
					TypeError::DuplicateMember {
						name,
						ty: ty.clone(),
						redefined_span: span,
						prev,
					},
				);
			}
		}
		let implementation = self.inherent.add(
			head,
			InherentImpl {
				definition: None,
				owner_generic_names: generics.iter().map(|g| g.0.name.0.clone()).collect(),
				self_ty,
				methods,
				constraints,
				imported: false,
			},
		);
		self
			.inherent_body_jobs
			.extend(
				body_jobs
					.into_iter()
					.map(|(method, meta, body)| InherentBodyJob {
						implementation,
						method,
						owner_generics: generics,
						meta,
						body,
					}),
			);
	}

	fn collect_impl_member(
		&mut self,
		member: &'m ImplMember,
		base: usize,
		namespaced: bool,
		self_ty: Ty,
		out: &mut FxHashMap<EcoString, InherentMethod>,
		body_jobs: &mut Vec<(EcoString, &'m FuncDeclaration, &'m Expr)>,
	) {
		let (meta, body): (&'m FuncDeclaration, Option<&'m Expr>) = match member {
			ImplMember::Func { meta, body, .. } => (meta, Some(body)),
			ImplMember::ExternalFunc(_, _, meta) => (meta, None),
			ImplMember::ExternalLet(_, marker, meta) => {
				let ty = meta.type_.as_ref().map(|ty| self.lower_type(ty));
				self.check_external_value(marker, meta.name.1, meta.is_mutable(), ty);
				return;
			}
			ImplMember::Let { .. } => return,
		};
		// A struct/enum inner member of ANY kind (instance `func`, `namespace func`
		// static, `mut func` method) shares this one per-type map keyed
		// only by name — so a same-named member of a DIFFERENT kind collides here
		// exactly as readily as two of the same kind (e.g. an instance `func at`
		// and a `namespace func at`). Report it now, before it can silently
		// shadow an unchecked body (see `TypeError::DuplicateMember`'s doc
		// comment). Mirrors `build_def_map`'s top-level "duplicate reported, later
		// definition wins" convention (def.rs) — the natural `out.insert` below
		// already overwrites, so keeping that (rather than skipping the insert)
		// keeps this check's fallback behavior consistent with the rest of the
		// checker rather than introducing a second, different collision policy.
		if let Some(prev) = out.get(&meta.name.0) {
			let ty = self.display(self_ty);
			self.emit(
				meta.name.1,
				TypeError::DuplicateMember {
					name: meta.name.0.clone(),
					ty,
					redefined_span: meta.name.1,
					prev: prev
						.local_span
						.expect("only local methods share this collection map"),
				},
			);
		}
		let scope = build_param_scope_at(&meta.generics, base);
		self.push_params(scope);
		let params = meta
			.params
			.iter()
			.map(|p| self.lower_type(&p.0.type_))
			.collect();
		let ret = match &meta.return_type {
			Some(ty) => self.lower_type(ty),
			None => self.fresh(),
		};
		// Lower the method's own generics' bounds while their scope is still active
		// (Slice 4G-b), offset past the owner's generics so `Bound::ty` lands at
		// `Param(base + j)` — the exact index `commit_inherent`'s subst mints into.
		let bounds = self.lower_constraints(&meta.generics, base);
		self.pop_params();
		out.insert(
			meta.name.0.clone(),
			InherentMethod {
				definition: None,
				local_span: Some(meta.name.1),
				generic_names: meta.generics.iter().map(|g| g.0.name.0.clone()).collect(),
				params,
				ret,
				bounds,
				namespaced,
				mutating: meta.kind == FuncKind::Mut,
				external: body.is_none(),
			},
		);
		if let Some(body) = body {
			body_jobs.push((meta.name.0.clone(), meta, body));
		}
	}

	// ── Resolution ───────────────────────────────────────────────────────────
	/// Resolve an inherent instance method `recv.name(args)`, returning its
	/// instantiated signature and stable declaration identities.
	pub(crate) fn resolve_inherent(
		&mut self,
		recv: Ty,
		receiver_is_mut: bool,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: nymph_ast::Span,
	) -> Option<(
		Vec<Ty>,
		Ty,
		Option<DefinitionId>,
		Option<DefinitionId>,
		Vec<Ty>,
	)> {
		self.resolve_inherent_impl(recv, receiver_is_mut, name, arg_tys, arg_lits, span, false)
	}

	/// Resolve only an inherent overload whose arguments fit. This lets ordinary
	/// method resolution fall through to an interface overload with the same name.
	pub(crate) fn resolve_matching_inherent(
		&mut self,
		recv: Ty,
		receiver_is_mut: bool,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: nymph_ast::Span,
	) -> Option<(
		Vec<Ty>,
		Ty,
		Option<DefinitionId>,
		Option<DefinitionId>,
		Vec<Ty>,
	)> {
		self.resolve_inherent_impl(recv, receiver_is_mut, name, arg_tys, arg_lits, span, true)
	}

	#[allow(clippy::too_many_arguments)]
	fn resolve_inherent_impl(
		&mut self,
		recv: Ty,
		receiver_is_mut: bool,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: nymph_ast::Span,
		require_argument_match: bool,
	) -> Option<(
		Vec<Ty>,
		Ty,
		Option<DefinitionId>,
		Option<DefinitionId>,
		Vec<Ty>,
	)> {
		// See `resolve_method`'s matching comment: peel `mut` before matching
		// against any impl's (never-`mut`) `Self` type.
		let recv = self.strip_mut(recv);
		let head = head_of(&self.interner, recv)?;
		let candidates = self.inherent.candidates(head);
		for idx in candidates {
			let has = self
				.inherent
				.impls
				.get(idx)
				.and_then(|i| i.methods.get(name))
				.is_some_and(|m| !m.namespaced && (!m.mutating || receiver_is_mut));
			if !has {
				continue;
			}
			let snapshot = self.table.snapshot();
			let matched = self.inherent_receiver_matches(idx, recv);
			self.table.rollback_to(snapshot);
			if matched {
				let snapshot = self.table.snapshot();
				let diagnostic_mark = self.diags.len();
				let pending_bound_mark = self.pending_bounds.len();
				let pending_bound_arg_mut = self.pending_bound_arg_mut.clone();
				self.commit_inherent(idx, recv, name, Some((arg_tys, arg_lits)), span, false);
				let arguments_match = self.diags.len() == diagnostic_mark;
				self.diags.truncate(diagnostic_mark);
				self.pending_bounds.truncate(pending_bound_mark);
				self.pending_bound_arg_mut = pending_bound_arg_mut;
				self.table.rollback_to(snapshot);
				if require_argument_match && !arguments_match {
					continue;
				}
				let implementation = self.inherent.impls[idx].definition.clone();
				let method = &self.inherent.impls[idx].methods[name];
				let target = method.definition.clone();
				let (params, ret, type_arguments) =
					self.commit_inherent(idx, recv, name, Some((arg_tys, arg_lits)), span, false);
				return Some((params, ret, target, implementation, type_arguments));
			}
		}
		None
	}

	pub(crate) fn resolve_inherent_value(
		&mut self,
		recv: Ty,
		receiver_is_mut: bool,
		name: &str,
		span: nymph_ast::Span,
	) -> Option<(
		Vec<Ty>,
		Ty,
		Option<DefinitionId>,
		Option<DefinitionId>,
		Vec<Ty>,
	)> {
		let recv = self.strip_mut(recv);
		let head = head_of(&self.interner, recv)?;
		let mut matches = Vec::new();
		for idx in self.inherent.candidates(head) {
			let Some(_) = self.inherent.impls[idx]
				.methods
				.get(name)
				.filter(|method| !method.namespaced && (!method.mutating || receiver_is_mut))
			else {
				continue;
			};
			let snapshot = self.table.snapshot();
			let matched = self.inherent_receiver_matches(idx, recv);
			self.table.rollback_to(snapshot);
			if matched {
				matches.push(idx);
			}
		}
		if matches.len() > 1 {
			self.emit(span, TypeError::AmbiguousCall { name: name.into() });
			return Some((Vec::new(), self.interner.error(), None, None, Vec::new()));
		}
		let idx = matches.pop()?;
		let method = &self.inherent.impls[idx].methods[name];
		let target = method.definition.clone();
		let implementation = self.inherent.impls[idx].definition.clone();
		let (params, ret, type_arguments) = self.commit_inherent(idx, recv, name, None, span, false);
		Some((params, ret, target, implementation, type_arguments))
	}

	/// Resolve a namespaced function `Type.name(args)`.
	pub(crate) fn resolve_namespaced(
		&mut self,
		type_def: DefId,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: nymph_ast::Span,
	) -> Option<(Ty, Option<crate::DefinitionId>, Vec<Ty>)> {
		self.resolve_namespaced_on(type_def, None, name, arg_tys, arg_lits, span)
	}

	pub(crate) fn resolve_namespaced_on(
		&mut self,
		type_def: DefId,
		receiver: Option<Ty>,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: nymph_ast::Span,
	) -> Option<(Ty, Option<crate::DefinitionId>, Vec<Ty>)> {
		let candidates = self.inherent.candidates(Head::Adt(type_def));
		for idx in candidates {
			if let Some(receiver) = receiver {
				let snapshot = self.table.snapshot();
				let applicable = self.inherent_receiver_matches(idx, receiver);
				self.table.rollback_to(snapshot);
				if !applicable {
					continue;
				}
			}
			let target = self
				.inherent
				.impls
				.get(idx)
				.and_then(|i| i.methods.get(name))
				.filter(|method| method.namespaced)
				.map(|method| method.definition.clone());
			if let Some(target) = target {
				let placeholder = receiver.unwrap_or_else(|| self.interner.error());
				let (_, ret, type_arguments) = self.commit_inherent(
					idx,
					placeholder,
					name,
					Some((arg_tys, arg_lits)),
					span,
					true,
				);
				return Some((ret, target, type_arguments));
			}
		}
		None
	}

	/// Resolve a namespaced/static function as a first-class callable without
	/// argument expressions. As with an instance method value, multiple
	/// receiver-applicable declarations are ambiguous rather than selected from
	/// an invented argument list.
	pub(crate) fn resolve_namespaced_value(
		&mut self,
		type_def: DefId,
		name: &str,
		span: nymph_ast::Span,
	) -> Option<(Vec<Ty>, Ty, Option<DefinitionId>, Vec<Ty>)> {
		self.resolve_namespaced_value_on(type_def, None, name, span)
	}

	pub(crate) fn resolve_namespaced_value_on(
		&mut self,
		type_def: DefId,
		receiver: Option<Ty>,
		name: &str,
		span: nymph_ast::Span,
	) -> Option<(Vec<Ty>, Ty, Option<DefinitionId>, Vec<Ty>)> {
		let mut matches = Vec::new();
		for index in self.inherent.candidates(Head::Adt(type_def)) {
			let implementation = &self.inherent.impls[index];
			if !implementation
				.methods
				.get(name)
				.is_some_and(|method| method.namespaced)
			{
				continue;
			}
			let receiver = receiver.unwrap_or(implementation.self_ty);
			let snapshot = self.table.snapshot();
			let applicable = self.inherent_receiver_matches(index, receiver);
			self.table.rollback_to(snapshot);
			if applicable {
				matches.push(index);
			}
		}
		if matches.len() > 1 {
			self.emit(span, TypeError::AmbiguousCall { name: name.into() });
			return Some((Vec::new(), self.interner.error(), None, Vec::new()));
		}
		let index = matches.pop()?;
		let target = self.inherent.impls[index].methods[name].definition.clone();
		let placeholder = receiver.unwrap_or_else(|| self.interner.error());
		let (parameters, return_type, type_arguments) =
			self.commit_inherent(index, placeholder, name, None, span, true);
		Some((parameters, return_type, target, type_arguments))
	}

	fn inherent_receiver_matches(&mut self, idx: usize, recv: Ty) -> bool {
		let def = &self.inherent.impls[idx];
		let generics_len = def.owner_generic_names.len();
		let self_ty = def.self_ty;
		let constraints = def.constraints.clone();
		let inst = self.instantiate(
			self_ty,
			&constraints,
			(0..generics_len).map(|index| ParamIdx(index as u32)),
			FxHashMap::default(),
			None,
		);
		self.try_unify(recv, inst.ty) && self.instantiated_constraints_hold(&inst.obligations, 0)
	}

	#[allow(clippy::too_many_arguments)]
	fn commit_inherent(
		&mut self,
		idx: usize,
		recv: Ty,
		name: &str,
		arguments: Option<(&[Ty], &[bool])>,
		span: nymph_ast::Span,
		namespaced: bool,
	) -> (Vec<Ty>, Ty, Vec<Ty>) {
		let def = &self.inherent.impls[idx];
		let generics_len = def.owner_generic_names.len();
		let self_pattern = def.self_ty;
		let method = def.methods.get(name).expect("checked by caller");
		let own = method.generic_names.len();
		let params = method.params.clone();
		let ret = method.ret;
		let bounds = method.bounds.clone();

		let owner = self.instantiate(
			self_pattern,
			&[],
			(0..generics_len).map(|index| ParamIdx(index as u32)),
			FxHashMap::default(),
			None,
		);
		let mut subst = owner.substitution;
		let impl_self = owner.ty;
		if !namespaced || !matches!(self.interner.kind(recv), crate::TyKind::Error) {
			self.unify(recv, impl_self, span);
		}
		let self_concrete = impl_self;
		// Defer one `pending_bounds` obligation per bound on the method's own
		// generics (Slice 4G-b), substituted through the same `subst` as
		// `params`/`ret` so it lands on the freshly-minted variable — mirrors
		// `fn_type_of`'s treatment of `FuncSig::bounds` exactly. Owner/impl-level
		// constraints are NOT pushed here: they are already enforced eagerly by
		// `inherent_receiver_matches`'s `constraints_hold` call for instance
		// receivers, and namespaced methods only exist in ADT bodies (whose
		// `constraints` is always empty).
		let method_inst = self.instantiate(
			ret,
			&bounds,
			(0..own).map(|j| ParamIdx((generics_len + j) as u32)),
			subst,
			Some(self_concrete),
		);
		self.defer_obligations(span, method_inst.obligations.iter().cloned());
		subst = method_inst.substitution;
		let params: Vec<Ty> = params
			.iter()
			.map(|t| self.subst(*t, &subst, Some(self_concrete)))
			.collect();
		let ret = method_inst.ty;

		if let Some((arg_tys, arg_lits)) = arguments {
			if params.len() != arg_tys.len() {
				self.emit(
					span,
					TypeError::NamedWrongArgCount {
						name: name.into(),
						expected: params.len(),
						found: arg_tys.len(),
					},
				);
				let start = if namespaced { 0 } else { generics_len };
				let type_arguments = (start..generics_len + own)
					.map(|index| subst[&ParamIdx(index as u32)])
					.collect();
				return (params, ret, type_arguments);
			}
			for (i, (param, arg)) in params.iter().zip(arg_tys).enumerate() {
				self.unify_arg(
					*param,
					*arg,
					arg_lits.get(i).copied().unwrap_or(false),
					span,
				);
			}
		}
		let start = if namespaced { 0 } else { generics_len };
		let type_arguments = (start..generics_len + own)
			.map(|index| subst[&ParamIdx(index as u32)])
			.collect();
		(params, ret, type_arguments)
	}

	// ── Return-type generalisation ───────────────────────────────────────────
	/// Infer the return type of every inherent method that omitted one, so callers see
	/// a *generalised* type (in terms of the method's `Param`s) rather than a shared
	/// inference variable that would leak a rigid parameter across call sites (e.g.
	/// `option.map<R>` returning `Option<R>`). Runs before any body is checked.
	///
	/// A method whose body calls another still-ungeneralised method can't be resolved in
	/// one go, so we iterate to a fixpoint (a small bound suffices for real code).
	pub(crate) fn generalize_returns(&mut self) {
		for _ in 0..4 {
			let targets: Vec<(usize, EcoString)> = self
				.inherent_body_jobs
				.iter()
				.filter(|job| job.meta.return_type.is_none())
				.map(|job| (job.implementation, job.method.clone()))
				.collect();

			let mut changed = false;
			for (i, name) in targets {
				let r = self.infer_inherent_return(i, &name);
				if let Some(ret) = r {
					let slot = &mut self.inherent.impls[i].methods.get_mut(&name).unwrap().ret;
					if *slot != ret {
						*slot = ret;
						changed = true;
					}
				}
			}
			if !changed {
				break;
			}
		}
	}

	/// Infer method `name`'s return type from its body in isolation, returning it only
	/// when it is fully generalised (no leftover inference variables). Trial-only:
	/// unification bindings and any diagnostics are discarded.
	fn infer_inherent_return(&mut self, i: usize, name: &str) -> Option<Ty> {
		let (owner_generics, self_ty, meta, body, params, namespaced) = {
			let job = self
				.inherent_body_jobs
				.iter()
				.find(|job| job.implementation == i && job.method == name)?;
			let imp = &self.inherent.impls[i];
			let method = imp.methods.get(name)?;
			(
				job.owner_generics,
				imp.self_ty,
				job.meta,
				job.body,
				method.params.clone(),
				method.namespaced,
			)
		};

		let base = owner_generics.len();
		let scope = build_param_scope_at(&meta.generics, base);

		let snapshot = self.table.snapshot();
		let diag_mark = self.diags.len();
		let pending_mark = self.pending_operators.len();
		let pending_bounds_mark = self.pending_bounds.len();
		let pending_mut_snapshot = self.pending_bound_arg_mut.clone();
		self.param_bounds.clear();
		self.param_bound_details.clear();
		self.push_params(build_param_scope(owner_generics));
		self.record_param_bounds(owner_generics, 0);
		self.pop_params();
		self.push_params(build_param_scope(owner_generics));
		self.push_params(scope);
		self.record_param_bounds(&meta.generics, base);
		self.push_scope();
		// A `mut func`'s `this` is bound as `mut Self` — the smaller correct step
		// short of full MT2 mut-func semantics (per-method mut availability, bound-
		// method typing), needed so field-slot reassignment through `this` inside
		// an existing `mut func` body keeps type-checking. Param/return
		// substitution below still uses the plain `self_ty` — `self` referenced
		// there is unaffected by the receiver's own mutability.
		let receiver_ty = if meta.kind == FuncKind::Mut {
			self.interner.mk_mut(self_ty)
		} else {
			self_ty
		};
		let prev_self = std::mem::replace(
			&mut self.self_ty,
			if namespaced { None } else { Some(receiver_ty) },
		);

		let empty = FxHashMap::default();
		for (param, &ty) in meta.params.iter().zip(&params) {
			let ty = self.subst(ty, &empty, Some(self_ty));
			self.bind_pattern(&param.0.name, ty, param.0.mutable);
		}
		let outer_labels = std::mem::take(&mut self.control_labels);
		let trial_ret = self.fresh();
		self.push_control_label(
			Some(&meta.name),
			body.id,
			crate::check::ControlLabelKind::Callable,
			None,
			Some(trial_ret),
		);
		let previous_ret = self.ret_ty.replace(trial_ret);
		self.resolve_anon(body, Some(trial_ret));
		let body_ty = self.infer(body);
		self.subtype(body_ty, trial_ret, body.span);
		self.ret_ty = previous_ret;
		self.control_labels = outer_labels;
		let ret = self.resolve_deep(trial_ret);

		self.self_ty = prev_self;
		self.pop_scope();
		self.pop_params();
		self.pop_params();
		self.diags.truncate(diag_mark);
		self.table.rollback_to(snapshot);
		// This trial run is entirely discarded (diags truncated, unify bindings
		// rolled back) and the real body is re-checked later by `check_method_body`
		// — so any operator this trial deferred must be discarded too, not left to
		// be finalized against a rolled-back table or leak into the next body's
		// drain.
		self.pending_operators.truncate(pending_mark);
		// Same discard for any bound obligation this trial deferred (Slice 4G) —
		// see the comment just above.
		self.pending_bounds.truncate(pending_bounds_mark);
		self.pending_bound_arg_mut = pending_mut_snapshot;

		// Accept the inferred type only if it is fully generalised.
		if self.has_infer(ret) { None } else { Some(ret) }
	}

	// ── Body checking ────────────────────────────────────────────────────────
	/// Check the bodies of every inherent method (and top-level impl method) with
	/// `this: Self` bound. Reuses the collected signatures so an omitted return
	/// type's variable is the same one callers see.
	pub(crate) fn check_member_bodies(&mut self) {
		// Gather jobs first (immutable borrow), then check (mutable borrow).
		struct Job<'m> {
			owner_generics: &'m [Spanned<GenericParam>],
			self_ty: Ty,
			meta: &'m FuncDeclaration,
			body: &'m Expr,
			params: Vec<Ty>,
			ret: Ty,
			namespaced: bool,
		}
		let jobs: Vec<Job<'m>> = self
			.inherent_body_jobs
			.iter()
			.map(|body_job| {
				let imp = &self.inherent.impls[body_job.implementation];
				let method = &imp.methods[&body_job.method];
				Job {
					owner_generics: body_job.owner_generics,
					self_ty: imp.self_ty,
					meta: body_job.meta,
					body: body_job.body,
					params: method.params.clone(),
					ret: method.ret,
					namespaced: method.namespaced,
				}
			})
			.collect();

		for job in jobs {
			self.check_method_body(
				job.owner_generics,
				job.self_ty,
				job.meta,
				job.body,
				&job.params,
				job.ret,
				job.namespaced,
			);
		}

		// Interface-impl method bodies are re-traversed from the AST: top-level
		// `impl … for …` and the `impl Iface { … }` blocks nested in ADT bodies.
		self.check_impl_for_bodies();
		self.check_inner_impl_bodies();
		// An interface's own default-bodied methods (Slice 4C-b): checked once,
		// generically, with `this` bound to a rigid synthetic `Param` constrained to
		// the interface — see `check_interface_default_bodies` for why that (and not
		// `SelfTy`) is the receiver type that actually resolves method calls.
		self.check_interface_default_bodies();
	}

	#[allow(clippy::too_many_arguments)]
	fn check_method_body(
		&mut self,
		owner_generics: &'m [Spanned<GenericParam>],
		self_ty: Ty,
		meta: &'m FuncDeclaration,
		body: &'m Expr,
		params: &[Ty],
		ret: Ty,
		namespaced: bool,
	) {
		let base = owner_generics.len();
		let scope = build_param_scope_at(&meta.generics, base);
		self.param_bounds.clear();
		self.param_bound_details.clear();
		self.push_params(build_param_scope(owner_generics));
		self.record_param_bounds(owner_generics, 0);
		self.pop_params();
		self.push_params(build_param_scope(owner_generics));
		self.push_params(scope);
		self.record_param_bounds(&meta.generics, base);
		self.push_scope();
		// See the matching comment in `infer_inherent_return`: a `mut func`'s
		// `this` is bound as `mut Self`.
		let receiver_ty = if meta.kind == FuncKind::Mut {
			self.interner.mk_mut(self_ty)
		} else {
			self_ty
		};
		let prev_self = std::mem::replace(
			&mut self.self_ty,
			if namespaced { None } else { Some(receiver_ty) },
		);

		let empty = FxHashMap::default();
		for (param, &ty) in meta.params.iter().zip(params) {
			let ty = self.subst(ty, &empty, Some(self_ty));
			self.bind_pattern(&param.0.name, ty, param.0.mutable);
		}
		let ret = self.subst(ret, &empty, Some(self_ty));
		let prev_ret = self.ret_ty.replace(ret);
		self.check_named_callable_body(&meta.name, body, ret);
		// Drain this method body's deferred operators now, while its `param_bounds`
		// (owner generics + this method's own) are still live — see
		// `pending_operators`'s doc comment.
		self.finalize_pending_operators();
		self.finalize_pending_bounds();

		self.ret_ty = prev_ret;
		self.self_ty = prev_self;
		self.pop_scope();
		self.pop_params();
		self.pop_params();
	}

	fn check_impl_for_bodies(&mut self) {
		let module = self.module;
		for i in 0..module.members.len() {
			let Declaration::ImplFor {
				generics,
				mutable,
				type_,
				for_interface,
				members,
				..
			} = &module.members[i]
			else {
				continue;
			};
			self.push_params(build_param_scope(generics));
			self.param_bounds.clear();
			self.param_bound_details.clear();
			self.record_param_bounds(generics, 0);
			let self_ty = self.lower_type(type_);
			let self_ty = if *mutable {
				self.interner.mk_mut(self_ty)
			} else {
				self_ty
			};
			let bound_ty = self.strip_mut(self_ty);
			if let crate::ty::TyKind::Param(param) = *self.interner.kind(bound_ty) {
				let (interface_name, interface_args) = for_interface;
				if let Some(interface) = self
					.defs
					.get(&interface_name.0)
					.filter(|&definition| self.is_interface(definition))
				{
					let args = self
						.align_args(interface, interface_args)
						.into_iter()
						.map(|(name, ty)| (name, self.subst(ty, &FxHashMap::default(), Some(self_ty))))
						.collect();
					self
						.param_bounds
						.entry(param)
						.or_default()
						.insert(0, interface);
					self.param_bound_details.entry(param).or_default().insert(
						0,
						crate::iface::Bound {
							ty: bound_ty,
							interface,
							args,
						},
					);
				}
			}
			self.check_interface_impl_members(self_ty, members, generics.len());
			self.pop_params();
		}
	}

	/// Check the bodies of the `impl Iface { … }` blocks nested in `struct`/`enum`
	/// bodies, with `this: Self` (the enclosing ADT) bound.
	fn check_inner_impl_bodies(&mut self) {
		let adts: Vec<(DefId, usize)> = self
			.defs
			.defs
			.iter()
			.enumerate()
			.filter_map(|(i, d)| {
				let id = DefId(i as u32);
				matches!(d.kind, DefKind::Struct | DefKind::Enum)
					.then(|| self.defs.local_member(id).map(|member| (id, member)))
					.flatten()
			})
			.collect();
		for (def, member) in adts {
			let module = self.module;
			let (generics, impls) = match &module.members[member] {
				Declaration::Struct {
					generics, impls, ..
				} => (generics.as_slice(), impls.as_slice()),
				Declaration::Enum {
					generics, impls, ..
				} => (generics.as_slice(), impls.as_slice()),
				_ => continue,
			};
			for m in impls {
				let impl_generics = &m.0.generics;
				let impl_members = &m.0.members;
				self.param_bounds.clear();
				self.param_bound_details.clear();
				self.push_params(build_param_scope(generics));
				self.record_param_bounds(generics, 0);
				self.push_params(build_param_scope_at(impl_generics, generics.len()));
				self.record_param_bounds(impl_generics, generics.len());
				let owner_len = generics.len();
				let positional: Vec<Ty> = (0..owner_len)
					.map(|i| self.interner.mk_param(ParamIdx(i as u32)))
					.collect();
				let self_ty = self
					.interner
					.mk_adt(def, GenericArgs::new(positional, Vec::new()));
				self.check_interface_impl_members(
					self_ty,
					impl_members,
					generics.len() + impl_generics.len(),
				);
				self.pop_params();
				self.pop_params();
			}
		}
	}

	/// Check each `func` body in an interface-impl block against its declared (or fresh,
	/// if omitted) return type, with `this: self_ty` bound. Assumes the impl's generic
	/// scope and `param_bounds` are already set up by the caller.
	fn check_interface_impl_members(
		&mut self,
		self_ty: Ty,
		members: &'m [Spanned<ImplMember>],
		implementation_generic_base: usize,
	) {
		let empty = FxHashMap::default();
		for m in members {
			let (meta, body) = match &m.0 {
				ImplMember::Func { meta, body, .. } => (meta, body),
				_ => continue,
			};
			let saved_param_bounds = self.param_bounds.clone();
			let saved_param_bound_details = self.param_bound_details.clone();
			self.push_params(build_param_scope_at(
				&meta.generics,
				implementation_generic_base,
			));
			self.record_param_bounds(&meta.generics, implementation_generic_base);
			self.push_scope();
			// See the matching comment in `check_method_body`: a `mut func`'s
			// `this` is bound as `mut Self`.
			let receiver_ty = if meta.kind == FuncKind::Mut {
				self.interner.mk_mut(self_ty)
			} else {
				self_ty
			};
			let prev_self = self.self_ty.replace(receiver_ty);
			for param in &meta.params {
				let ty = self.lower_type(&param.0.type_);
				let ty = self.subst(ty, &empty, Some(self_ty));
				self.bind_pattern(&param.0.name, ty, param.0.mutable);
			}
			let ret = match &meta.return_type {
				Some(ty) => {
					let t = self.lower_type(ty);
					self.subst(t, &empty, Some(self_ty))
				}
				None => self.fresh(),
			};
			let prev_ret = self.ret_ty.replace(ret);
			self.check_named_callable_body(&meta.name, body, ret);
			// Drain this member body's deferred operators now, while the impl
			// block's `param_bounds` are still live — see `pending_operators`'s doc
			// comment. All members of one impl block share the same bounds, but the
			// next impl block (or nested impl block) clears and rebuilds them.
			self.finalize_pending_operators();
			self.finalize_pending_bounds();
			self.ret_ty = prev_ret;
			self.self_ty = prev_self;
			self.pop_scope();
			self.pop_params();
			self.param_bounds = saved_param_bounds;
			self.param_bound_details = saved_param_bound_details;
		}
	}

	/// Check the default (non-abstract) `func` bodies declared directly in every
	/// `interface { … }` block. Nothing else visited these before Slice 4C-b:
	/// `collect_interfaces` (iface.rs) only lowers signatures, discarding the body,
	/// and every other body-checking path here re-traverses `impl`/`impl … for`
	/// blocks, never `Declaration::Interface` itself.
	fn check_interface_default_bodies(&mut self) {
		let module = self.module;
		let ifaces: Vec<(DefId, usize)> = self
			.defs
			.defs
			.iter()
			.enumerate()
			.filter_map(|(i, d)| {
				let id = DefId(i as u32);
				matches!(d.kind, DefKind::Interface)
					.then(|| self.defs.local_member(id).map(|member| (id, member)))
					.flatten()
			})
			.collect();
		for (iface_id, member) in ifaces {
			let Declaration::Interface {
				generics, members, ..
			} = &module.members[member]
			else {
				continue;
			};
			for m in members {
				let InterfaceMember::Element(element) = &m.0 else {
					continue;
				};
				let InterfaceElement::Func {
					meta,
					body: Some(body),
				} = &element.0
				else {
					continue;
				};
				self.check_interface_default_body(iface_id, generics, meta, body);
			}
		}
	}

	/// Check one interface default method body. `this` cannot be bound to `SelfTy`
	/// directly — `head_of(SelfTy)` is `None`, so `resolve_method` on a `SelfTy`
	/// receiver would find no candidates and no fallback (`SelfTy` isn't
	/// `TyKind::Param`, so the generic-parameter fallback in `resolve_method`
	/// doesn't trigger either). Instead `this` is bound to a *rigid synthetic
	/// `Param`* placed right after the interface's own generics (and this method's
	/// own, if it declares any), with `param_bounds` recording that it is
	/// constrained by this very interface — exactly mirroring how a bounded generic
	/// function parameter's body (`f<T: Comparable<Other = T>>`) already checks
	/// today via `resolve_param_method` (solve.rs). A body whose operator depends on
	/// `Self` (e.g. `this + other` under an arithmetic-operator interface) still
	/// resolves through that same generic-bound path, recording
	/// `MethodSource::GenericBound` → `DispatchKind::UserImplDefaultMethod` — an
	/// honest deferral (never a silent miscompile), not a defect of this check.
	///
	/// Reuses the method's signature already collected into `self.interfaces` (its
	/// params/ret in terms of `SelfTy`/interface `Param(k)`) rather than re-lowering
	/// it, so an omitted return type's inference variable is the same one every
	/// caller sees.
	fn check_interface_default_body(
		&mut self,
		iface_id: DefId,
		iface_generics: &'m [Spanned<GenericParam>],
		meta: &'m FuncDeclaration,
		body: &'m Expr,
	) {
		let iface_len = iface_generics.len();
		let self_idx = ParamIdx((iface_len + meta.generics.len()) as u32);

		let scope = build_param_scope_at(&meta.generics, iface_len);
		self.param_bounds.clear();
		self.param_bound_details.clear();
		self.push_params(build_param_scope(iface_generics));
		self.record_param_bounds(iface_generics, 0);
		self.pop_params();
		self.push_params(build_param_scope(iface_generics));
		self.push_params(scope);
		self.record_param_bounds(&meta.generics, iface_len);
		self
			.param_bounds
			.entry(self_idx)
			.or_default()
			.push(iface_id);

		self.push_scope();

		// A `mut func` default binds `this` as `mut Self`, exactly as a `mut func` on a
		// concrete type does — so its body may mutate the receiver (call a `mut` method,
		// e.g. iterate `this` via `this.next()` in `Iterator`'s `for_each`/`fold`/…).
		let self_ty = self.interner.mk_param(self_idx);
		let self_ty = if meta.kind == FuncKind::Mut {
			self.interner.mk_mut(self_ty)
		} else {
			self_ty
		};
		let prev_self = self.self_ty.replace(self_ty);
		// See `resolve_method`'s doc comment: a call to another method of *this same
		// interface* on `this` must bypass ordinary impl search (which could
		// otherwise match this interface's own blanket impl elsewhere in the
		// program and wrongly pin its generics to `Self`).
		let prev_checking = self
			.checking_interface_default
			.replace((iface_id, self_idx));

		let Some(method) = self
			.interfaces
			.get(&iface_id)
			.and_then(|i| i.methods.get(&meta.name.0))
			.cloned()
		else {
			// `collect_interfaces` inserts a signature for every `Func` element, so
			// this is unreachable in practice; kept total rather than `unreachable!()`
			// so a future desync fails as a no-op instead of a panic mid-typecheck.
			self.checking_interface_default = prev_checking;
			self.self_ty = prev_self;
			self.pop_scope();
			self.pop_params();
			return;
		};

		let empty = FxHashMap::default();
		for (param, &ty) in meta.params.iter().zip(&method.params) {
			let ty = self.subst(ty, &empty, Some(self_ty));
			self.bind_pattern(&param.0.name, ty, param.0.mutable);
		}
		let ret = self.subst(method.ret, &empty, Some(self_ty));
		let prev_ret = self.ret_ty.replace(ret);
		self.check_named_callable_body(&meta.name, body, ret);
		// Drain this body's deferred operators now, while its `param_bounds` (the
		// interface's generics + the synthetic self bound) are still live — see
		// `pending_operators`'s doc comment.
		self.finalize_pending_operators();
		self.finalize_pending_bounds();

		self.ret_ty = prev_ret;
		self.checking_interface_default = prev_checking;
		self.self_ty = prev_self;
		self.pop_scope();
		self.pop_params();
		self.pop_params();
	}
}
