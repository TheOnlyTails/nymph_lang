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

use ecow::EcoString;
use nymph_ast::{
	Spanned,
	decl::{Declaration, FuncDeclaration, ImplMember, StructInnerMember},
	expr::Expr,
	ty::GenericParam,
};
use rustc_hash::FxHashMap;

use crate::check::Checker;
use crate::def::DefKind;
use crate::ids::{DefId, ParamIdx};
use crate::iface::{Bound, Head, head_of};
use crate::lower::build_param_scope;
use crate::ty::{GenericArgs, Ty};

/// One inherent method's signature plus the AST needed to check its body.
pub struct InherentMethod<'m> {
	pub own_generics: usize,
	pub params: Vec<Ty>,
	pub ret: Ty,
	pub namespaced: bool,
	pub meta: &'m FuncDeclaration,
	pub body: Option<&'m Expr>,
}

/// A set of inherent methods sharing a self type (a `struct`/`enum` body, or a
/// top-level inherent `impl`).
pub struct InherentImpl<'m> {
	/// The owning type's generic parameters (for building the body's param scope).
	pub owner_generics: &'m [Spanned<GenericParam>],
	/// Number of owner generics; the self type's `Param`s are `0..generics_len`.
	pub generics_len: usize,
	pub self_ty: Ty,
	pub methods: FxHashMap<EcoString, InherentMethod<'m>>,
	pub constraints: Vec<Bound>,
}

/// Inherent impls indexed by the self type's head constructor.
#[derive(Default)]
pub struct InherentRegistry<'m> {
	pub impls: Vec<InherentImpl<'m>>,
	by_head: FxHashMap<Head, Vec<usize>>,
}

impl<'m> InherentRegistry<'m> {
	fn add(&mut self, head: Option<Head>, def: InherentImpl<'m>) {
		let idx = self.impls.len();
		if let Some(head) = head {
			self.by_head.entry(head).or_default().push(idx);
		}
		self.impls.push(def);
	}

	fn candidates(&self, head: Head) -> Vec<usize> {
		self.by_head.get(&head).cloned().unwrap_or_default()
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
			.filter_map(|(i, d)| match d.kind {
				DefKind::Struct { member } | DefKind::Enum { member } => Some((DefId(i as u32), member)),
				_ => None,
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

		let mut methods = FxHashMap::default();
		for m in members {
			match &m.0 {
				StructInnerMember::Member(inner) => {
					self.collect_impl_member(&inner.0, generics_len, false, &mut methods);
				}
				StructInnerMember::ImplMut(members) => {
					for inner in members {
						self.collect_impl_member(&inner.0, generics_len, false, &mut methods);
					}
				}
				StructInnerMember::Namespace(members) => {
					for inner in members {
						self.collect_impl_member(&inner.0, generics_len, true, &mut methods);
					}
				}
				// Inner interface impls are Milestone-B-later.
				StructInnerMember::Impl { .. } => {}
			}
		}
		self.pop_params();

		let head = head_of(&self.interner, self_ty);
		self.inherent.add(
			head,
			InherentImpl {
				owner_generics: generics,
				generics_len,
				self_ty,
				methods,
				constraints: Vec::new(),
			},
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
		for m in members {
			self.collect_impl_member(&m.0, generics_len, false, &mut methods);
		}
		let constraints = self.lower_constraints(generics);
		self.pop_params();

		let head = head_of(&self.interner, self_ty);
		self.inherent.add(
			head,
			InherentImpl {
				owner_generics: generics,
				generics_len,
				self_ty,
				methods,
				constraints,
			},
		);
	}

	fn collect_impl_member(
		&mut self,
		member: &'m ImplMember,
		base: usize,
		namespaced: bool,
		out: &mut FxHashMap<EcoString, InherentMethod<'m>>,
	) {
		let (meta, body): (&'m FuncDeclaration, Option<&'m Expr>) = match member {
			ImplMember::Func { meta, body, .. } => (meta, Some(body)),
			ImplMember::ExternalFunc(_, _, meta) => (meta, None),
			ImplMember::Let { .. } | ImplMember::ExternalLet(..) => return,
		};
		let own_generics = meta.generics.len();
		let mut scope = FxHashMap::default();
		for (j, g) in meta.generics.iter().enumerate() {
			scope.insert(g.0.name.0.clone(), ParamIdx((base + j) as u32));
		}
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
		self.pop_params();
		out.insert(
			meta.name.0.clone(),
			InherentMethod {
				own_generics,
				params,
				ret,
				namespaced,
				meta,
				body,
			},
		);
	}

	// ── Resolution ───────────────────────────────────────────────────────────
	/// Resolve an inherent instance method `recv.name(args)`, if one exists.
	pub(crate) fn resolve_inherent(
		&mut self,
		recv: Ty,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: nymph_ast::Span,
	) -> Option<Ty> {
		let recv = self.shallow_resolve(recv);
		let head = head_of(&self.interner, recv)?;
		let candidates = self.inherent.candidates(head);
		for idx in candidates {
			let has = self
				.inherent
				.impls
				.get(idx)
				.and_then(|i| i.methods.get(name))
				.is_some_and(|m| !m.namespaced);
			if !has {
				continue;
			}
			let snapshot = self.table.snapshot();
			let matched = self.inherent_receiver_matches(idx, recv);
			self.table.rollback_to(snapshot);
			if matched {
				return Some(self.commit_inherent(idx, recv, name, arg_tys, arg_lits, span, false));
			}
		}
		None
	}

	/// Resolve a namespaced function `Type.name(args)`.
	pub(crate) fn resolve_namespaced(
		&mut self,
		type_def: DefId,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: nymph_ast::Span,
	) -> Option<Ty> {
		let candidates = self.inherent.candidates(Head::Adt(type_def));
		for idx in candidates {
			let has = self
				.inherent
				.impls
				.get(idx)
				.and_then(|i| i.methods.get(name))
				.is_some_and(|m| m.namespaced);
			if has {
				let placeholder = self.interner.error();
				return Some(self.commit_inherent(idx, placeholder, name, arg_tys, arg_lits, span, true));
			}
		}
		None
	}

	fn inherent_receiver_matches(&mut self, idx: usize, recv: Ty) -> bool {
		let def = &self.inherent.impls[idx];
		let generics_len = def.generics_len;
		let self_ty = def.self_ty;
		let constraints = def.constraints.clone();
		let subst = self.fresh_subst(generics_len);
		let impl_self = self.subst(self_ty, &subst, None);
		self.try_unify(recv, impl_self) && self.constraints_hold(&constraints, &subst, 0)
	}

	#[allow(clippy::too_many_arguments)]
	fn commit_inherent(
		&mut self,
		idx: usize,
		recv: Ty,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: nymph_ast::Span,
		namespaced: bool,
	) -> Ty {
		let def = &self.inherent.impls[idx];
		let generics_len = def.generics_len;
		let self_pattern = def.self_ty;
		let method = def.methods.get(name).expect("checked by caller");
		let own = method.own_generics;
		let params = method.params.clone();
		let ret = method.ret;

		let mut subst = self.fresh_subst(generics_len);
		let impl_self = self.subst(self_pattern, &subst, None);
		if !namespaced {
			self.unify(recv, impl_self, span);
		}
		for j in 0..own {
			subst.insert(ParamIdx((generics_len + j) as u32), self.fresh());
		}
		let self_concrete = impl_self;
		let params: Vec<Ty> = params
			.iter()
			.map(|t| self.subst(*t, &subst, Some(self_concrete)))
			.collect();
		let ret = self.subst(ret, &subst, Some(self_concrete));

		if params.len() != arg_tys.len() {
			self.error(
				format!(
					"`{name}` expects {} argument(s), found {}",
					params.len(),
					arg_tys.len()
				),
				span,
			);
			return ret;
		}
		for (i, (param, arg)) in params.iter().zip(arg_tys).enumerate() {
			self.unify_arg(
				*param,
				*arg,
				arg_lits.get(i).copied().unwrap_or(false),
				span,
			);
		}
		ret
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
				.inherent
				.impls
				.iter()
				.enumerate()
				.flat_map(|(i, imp)| {
					imp
						.methods
						.iter()
						.filter(|(_, m)| m.body.is_some() && m.meta.return_type.is_none())
						.map(move |(n, _)| (i, n.clone()))
				})
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
			let imp = &self.inherent.impls[i];
			let method = imp.methods.get(name)?;
			let body = method.body?;
			(
				imp.owner_generics,
				imp.self_ty,
				method.meta,
				body,
				method.params.clone(),
				method.namespaced,
			)
		};

		let base = owner_generics.len();
		let mut scope = build_param_scope(owner_generics);
		for (j, g) in meta.generics.iter().enumerate() {
			scope.insert(g.0.name.0.clone(), ParamIdx((base + j) as u32));
		}

		let snapshot = self.table.snapshot();
		let diag_mark = self.diags.len();
		self.param_bounds.clear();
		self.record_param_bounds(owner_generics, 0);
		self.record_param_bounds(&meta.generics, base);
		self.push_params(scope);
		self.push_scope();
		let prev_self = std::mem::replace(
			&mut self.self_ty,
			if namespaced { None } else { Some(self_ty) },
		);

		let empty = FxHashMap::default();
		for (param, &ty) in meta.params.iter().zip(&params) {
			let ty = self.subst(ty, &empty, Some(self_ty));
			self.bind_pattern(&param.0.name, ty, param.0.mutable);
		}
		let body_ty = self.infer(body);
		let ret = self.resolve_deep(body_ty);

		self.self_ty = prev_self;
		self.pop_scope();
		self.pop_params();
		self.diags.truncate(diag_mark);
		self.table.rollback_to(snapshot);

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
		let mut jobs: Vec<Job<'m>> = Vec::new();
		for imp in &self.inherent.impls {
			for method in imp.methods.values() {
				if let Some(body) = method.body {
					jobs.push(Job {
						owner_generics: imp.owner_generics,
						self_ty: imp.self_ty,
						meta: method.meta,
						body,
						params: method.params.clone(),
						ret: method.ret,
						namespaced: method.namespaced,
					});
				}
			}
		}

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
		let mut scope = build_param_scope(owner_generics);
		for (j, g) in meta.generics.iter().enumerate() {
			scope.insert(g.0.name.0.clone(), ParamIdx((base + j) as u32));
		}
		self.param_bounds.clear();
		self.record_param_bounds(owner_generics, 0);
		self.record_param_bounds(&meta.generics, base);
		self.push_params(scope);
		self.push_scope();
		let prev_self = std::mem::replace(
			&mut self.self_ty,
			if namespaced { None } else { Some(self_ty) },
		);

		let empty = FxHashMap::default();
		for (param, &ty) in meta.params.iter().zip(params) {
			let ty = self.subst(ty, &empty, Some(self_ty));
			self.bind_pattern(&param.0.name, ty, param.0.mutable);
		}
		let ret = self.subst(ret, &empty, Some(self_ty));
		let prev_ret = self.ret_ty.replace(ret);
		self.check(body, ret);

		self.ret_ty = prev_ret;
		self.self_ty = prev_self;
		self.pop_scope();
		self.pop_params();
	}

	fn check_impl_for_bodies(&mut self) {
		let module = self.module;
		for i in 0..module.members.len() {
			let Declaration::ImplFor {
				generics,
				type_,
				members,
				..
			} = &module.members[i]
			else {
				continue;
			};
			self.push_params(build_param_scope(generics));
			self.param_bounds.clear();
			self.record_param_bounds(generics, 0);
			let self_ty = self.lower_type(type_);
			self.check_interface_impl_members(self_ty, members);
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
			.filter_map(|(i, d)| match d.kind {
				DefKind::Struct { member } | DefKind::Enum { member } => Some((DefId(i as u32), member)),
				_ => None,
			})
			.collect();
		for (def, member) in adts {
			let module = self.module;
			let (generics, members) = match &module.members[member] {
				Declaration::Struct {
					generics, members, ..
				} => (generics.as_slice(), members.as_slice()),
				Declaration::Enum {
					generics, members, ..
				} => (generics.as_slice(), members.as_slice()),
				_ => continue,
			};
			for m in members {
				let StructInnerMember::Impl {
					generics: impl_generics,
					members: impl_members,
					..
				} = &m.0
				else {
					continue;
				};
				let combined: Vec<Spanned<GenericParam>> =
					generics.iter().chain(impl_generics).cloned().collect();
				self.push_params(build_param_scope(&combined));
				self.param_bounds.clear();
				self.record_param_bounds(&combined, 0);
				let owner_len = generics.len();
				let positional: Vec<Ty> = (0..owner_len)
					.map(|i| self.interner.mk_param(ParamIdx(i as u32)))
					.collect();
				let self_ty = self
					.interner
					.mk_adt(def, GenericArgs::new(positional, Vec::new()));
				self.check_interface_impl_members(self_ty, impl_members);
				self.pop_params();
			}
		}
	}

	/// Check each `func` body in an interface-impl block against its declared (or fresh,
	/// if omitted) return type, with `this: self_ty` bound. Assumes the impl's generic
	/// scope and `param_bounds` are already set up by the caller.
	fn check_interface_impl_members(&mut self, self_ty: Ty, members: &'m [Spanned<ImplMember>]) {
		let empty = FxHashMap::default();
		for m in members {
			let (meta, body) = match &m.0 {
				ImplMember::Func { meta, body, .. } => (meta, body),
				_ => continue,
			};
			self.push_scope();
			let prev_self = self.self_ty.replace(self_ty);
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
			self.check(body, ret);
			self.ret_ty = prev_ret;
			self.self_ty = prev_self;
			self.pop_scope();
		}
	}
}
