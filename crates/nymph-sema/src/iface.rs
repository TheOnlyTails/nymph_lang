//! Collecting interfaces and `impl` blocks into the registries the solver queries.
//!
//! This runs after signature lowering and before body inference. It produces:
//! - [`InterfaceDef`] per `interface`: its generic parameter names and the signature
//!   of each method (including default methods), expressed in terms of `SelfTy` and
//!   the interface's own `Param(k)`.
//! - [`ImplDef`] per `impl … for`: the implementing type, the interface plus its
//!   argument bindings (with `self` already substituted to the implementing type),
//!   the method signatures, and any constraints from the impl's generic bounds.
//!
//! Impls are indexed by `(interface, head-constructor)` so the solver assembles
//! candidates in O(candidates), with blanket impls (`impl<T> … for T`) in a separate
//! per-interface bucket.

use crate::errors::TypeError;
use ecow::EcoString;
use nymph_ast::{
	Ident, Span, Spanned,
	decl::{
		Declaration, FuncDeclaration, FuncKind, ImplMember, InterfaceElement, InterfaceMember, LetKind,
		StructImpl,
	},
	expr::Pattern,
	ty::{GenericArg, GenericParam, Type},
};
use rustc_hash::FxHashMap;

use crate::check::Checker;
use crate::def::DefKind;
use crate::identity::DefinitionId;
use crate::ids::{DefId, ParamIdx};
use crate::lower::{build_param_scope, build_param_scope_at};
use crate::ty::{GenericArgs, Interner, Ty, TyKind};

/// A method signature as seen through an interface or impl. `Param` indices refer to
/// the owning interface's (or impl's) generic parameters; `SelfTy` to the receiver.
#[derive(Debug, Clone)]
pub struct IfaceMethod {
	pub definition: Option<DefinitionId>,
	pub has_default: bool,
	pub params: Vec<Ty>,
	pub ret: Ty,
	pub effects: crate::ty::EffectRow,
	/// The method's OWN generic parameter names (e.g. `map<R>` → `["R"]`), in
	/// declaration order. Their `Param` indices follow the owning interface's/impl's
	/// generics: a method generic `j` is `Param(owner_generics + j)`, and any
	/// synthetic `Self` param sits after them (see `check_interface_default_body`).
	/// Empty for the common no-generics method. Call sites read this to allocate a
	/// fresh inference variable per method generic when instantiating the signature.
	pub generics: Vec<EcoString>,
	pub bounds: Vec<Bound>,
}

#[derive(Debug, Clone)]
pub struct RuntimeMemberDef {
	pub definition: Option<DefinitionId>,
	pub name: EcoString,
	pub kind: crate::MemberKind,
	pub has_default: bool,
	pub external: bool,
	pub marshal: Option<nymph_hir::hir::MarshalKind>,
}

/// A collected `interface`.
#[derive(Debug, Clone, Default)]
pub struct InterfaceDef {
	pub generics: Vec<EcoString>,
	pub generic_kinds: Vec<crate::GenericParameterKind>,
	/// All runtime-bearing interface members in declaration order.
	pub runtime_members: Vec<RuntimeMemberDef>,
	pub methods: FxHashMap<EcoString, IfaceMethod>,
}

/// One required bound, e.g. `T: Comparable<T>` on an impl's generic parameter.
#[derive(Debug, Clone)]
pub struct Bound {
	pub ty: Ty,
	pub interface: DefId,
	pub args: Vec<(EcoString, Ty)>,
	pub effect_args: Vec<(EcoString, crate::ty::EffectRow)>,
}

/// A collected `impl Interface<…> for Type { … }`.
#[derive(Debug, Clone)]
pub struct ImplDef {
	pub definition: Option<DefinitionId>,
	/// Final exact interface-member dispatch relation. Local catalogs are filled
	/// after stable identities are assigned and before any body is checked;
	/// imported catalogs are copied from their module interface.
	pub member_catalog: crate::ImplementationMemberCatalog,
	pub runtime_members: Vec<RuntimeMemberDef>,
	pub generics: Vec<EcoString>,
	pub generic_kinds: Vec<crate::GenericParameterKind>,
	pub self_ty: Ty,
	pub interface: DefId,
	/// The span of the interface reference in the `impl … for …` header, used to
	/// anchor a coherence (conflicting-impl) diagnostic.
	pub source_span: Option<Span>,
	/// Interface argument bindings (`Other = …`, `Output = …`), by parameter name,
	/// with `self` already substituted to `self_ty`.
	pub args: Vec<(EcoString, Ty)>,
	/// Erased effect argument bindings, retained for interface effect contracts.
	pub effect_args: Vec<(EcoString, crate::ty::EffectRow)>,
	pub methods: FxHashMap<EcoString, IfaceMethod>,
	pub constraints: Vec<Bound>,
	/// True when the implementing type is a bare generic parameter (a blanket impl).
	pub blanket: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AlignedInterfaceArgs {
	pub types: Vec<(EcoString, Ty)>,
	pub effects: Vec<(EcoString, crate::ty::EffectRow)>,
}

/// The head constructor of a type, used as an impl-index key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Head {
	Int,
	UInt,
	Float,
	Char,
	String,
	Boolean,
	Void,
	Never,
	List,
	Tuple,
	Map,
	Fn,
	Task,
	Adt(DefId),
}

/// The head of a type, or `None` for a variable/parameter/intersection (which route
/// to the blanket bucket).
pub fn head_of(interner: &Interner, ty: Ty) -> Option<Head> {
	match interner.kind(ty) {
		TyKind::Int => Some(Head::Int),
		TyKind::UInt => Some(Head::UInt),
		TyKind::Float => Some(Head::Float),
		TyKind::Char => Some(Head::Char),
		TyKind::String => Some(Head::String),
		TyKind::Boolean => Some(Head::Boolean),
		TyKind::Void => Some(Head::Void),
		TyKind::Never => Some(Head::Never),
		TyKind::List(_) => Some(Head::List),
		TyKind::Tuple(_) => Some(Head::Tuple),
		TyKind::Map(..) => Some(Head::Map),
		TyKind::Fn { .. } => Some(Head::Fn),
		TyKind::Task { .. } => Some(Head::Task),
		TyKind::Handle(_) | TyKind::HandleOutcome(_) => None,
		TyKind::Adt(def, _) => Some(Head::Adt(*def)),
		TyKind::Param(_)
		| TyKind::Infer(_)
		| TyKind::SelfTy
		| TyKind::Intersection(_)
		| TyKind::Error => None,
	}
}

/// The impl index: impls keyed by `(interface, head)`, plus a blanket bucket per
/// interface.
#[derive(Debug, Default, Clone)]
pub struct ImplRegistry {
	pub impls: Vec<ImplDef>,
	keyed: FxHashMap<(DefId, Head), Vec<usize>>,
	blanket: FxHashMap<DefId, Vec<usize>>,
}

impl ImplRegistry {
	pub(crate) fn add(&mut self, interner: &Interner, def: ImplDef) {
		let idx = self.impls.len();
		match (def.blanket, head_of(interner, def.self_ty)) {
			(false, Some(head)) => self
				.keyed
				.entry((def.interface, head))
				.or_default()
				.push(idx),
			_ => self.blanket.entry(def.interface).or_default().push(idx),
		}
		self.impls.push(def);
	}

	/// Candidate impl indices for an obligation: those keyed on the self type's head,
	/// followed by the interface's blanket impls.
	pub fn candidates(&self, interface: DefId, head: Option<Head>) -> Vec<usize> {
		let mut out = Vec::new();
		if let Some(head) = head
			&& let Some(keyed) = self.keyed.get(&(interface, head))
		{
			out.extend(keyed.iter().copied());
		}
		if let Some(blanket) = self.blanket.get(&interface) {
			out.extend(blanket.iter().copied());
		}
		out
	}
}

impl Checker<'_> {
	pub(crate) fn is_interface(&self, def: DefId) -> bool {
		matches!(self.defs.data(def).kind, DefKind::Interface)
	}

	// ── Interface collection ─────────────────────────────────────────────────
	pub(crate) fn collect_interfaces(&mut self) {
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

		for (id, member) in ifaces {
			let Declaration::Interface {
				generics,
				super_interfaces,
				members,
				..
			} = &module.members[member]
			else {
				continue;
			};
			for super_interface in super_interfaces {
				let name = &super_interface.0.0;
				if let Some(interface) = self.defs.get(&name.0).filter(|&d| self.is_interface(d)) {
					self
						.annotations
						.record_type_definition_target(name.1, self.defs.stable(interface));
				}
			}
			let names = generic_names(generics);
			self.push_params(build_param_scope(generics));
			let mut runtime_members = Vec::new();
			let mut methods = FxHashMap::default();
			for m in members {
				if let InterfaceMember::Element(element) = &m.0 {
					match &element.0 {
						InterfaceElement::Func { meta, body } => {
							let mut sig = self.lower_method_sig(meta, generics.len());
							self.record_effect_spec(meta, sig.effects.clone(), body.is_some());
							sig.has_default = body.is_some();
							runtime_members.push(RuntimeMemberDef {
								definition: None,
								name: meta.name.0.clone(),
								kind: func_member_kind(meta.kind.clone()),
								has_default: body.is_some(),
								external: false,
								marshal: None,
							});
							methods.insert(meta.name.0.clone(), sig);
						}
						InterfaceElement::Let { meta, value } => {
							if let Pattern::Binding { name, .. } = &meta.name.0 {
								runtime_members.push(RuntimeMemberDef {
									definition: None,
									name: name.0.clone(),
									kind: let_member_kind(meta.kind),
									has_default: value.is_some(),
									external: false,
									marshal: None,
								});
							}
						}
					}
				}
			}
			self.pop_params();
			self.interfaces.insert(
				id,
				InterfaceDef {
					generics: names,
					generic_kinds: generics
						.iter()
						.map(|generic| match generic.0.kind {
							nymph_ast::ty::GenericParamKind::Type => crate::GenericParameterKind::Type,
							nymph_ast::ty::GenericParamKind::Effect => crate::GenericParameterKind::Effect,
						})
						.collect(),
					runtime_members,
					methods,
				},
			);
		}
	}

	// ── Impl collection ──────────────────────────────────────────────────────
	pub(crate) fn collect_impls(&mut self) {
		let module = self.module;
		let indices: Vec<usize> = (0..module.members.len()).collect();
		for i in indices {
			if let Declaration::ImplFor { .. } = &module.members[i] {
				self.collect_impl_for(i);
			}
		}
	}

	fn collect_impl_for(&mut self, member: usize) {
		let module = self.module;
		let Declaration::ImplFor {
			generics,
			type_,
			for_interface,
			members,
			..
		} = &module.members[member]
		else {
			return;
		};
		let (iface_name, iface_args) = for_interface;
		let names = generic_names(generics);

		self.push_params(build_param_scope(generics));
		let self_ty = self.lower_type(type_);
		let constraints = self.lower_constraints(generics, 0);
		self.finish_interface_impl(
			names,
			generic_kinds(generics),
			self_ty,
			iface_name,
			iface_args,
			members,
			constraints,
		);
		self.pop_params();
	}

	/// Collect the `impl Iface { … }` blocks nested inside `struct`/`enum` bodies. The
	/// self type is the enclosing ADT applied to its own generic parameters; the block
	/// may add its own generics on top. Registered into the same [`ImplRegistry`] as
	/// top-level `impl … for` so the solver treats inner and outer impls uniformly.
	pub(crate) fn collect_inner_impls(&mut self) {
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
			self.collect_adt_inner_impls(def, member);
		}
	}

	fn collect_adt_inner_impls(&mut self, def: DefId, member: usize) {
		let module = self.module;
		let (generics, impls) = match &module.members[member] {
			Declaration::Struct {
				generics, impls, ..
			} => (generics.as_slice(), impls.as_slice()),
			Declaration::Enum {
				generics, impls, ..
			} => (generics.as_slice(), impls.as_slice()),
			_ => return,
		};
		for m in impls {
			let StructImpl {
				interface,
				generics: impl_generics,
				members: impl_members,
			} = &m.0;
			self.collect_inner_impl(def, generics, interface, impl_generics, impl_members);
		}
	}

	fn collect_inner_impl(
		&mut self,
		def: DefId,
		owner_generics: &[Spanned<GenericParam>],
		interface: &(Ident, Vec<Spanned<GenericArg>>),
		impl_generics: &[Spanned<GenericParam>],
		members: &[Spanned<ImplMember>],
	) {
		// Keep the owner and nested impl as separate lexical scopes so an impl generic
		// may legally shadow an owner generic while retaining a distinct parameter index.
		let combined: Vec<Spanned<GenericParam>> = owner_generics
			.iter()
			.chain(impl_generics)
			.cloned()
			.collect();
		let names = generic_names(&combined);
		let kinds = generic_kinds(&combined);
		let owner_len = owner_generics.len();
		self.push_params(build_param_scope(owner_generics));
		let mut constraints = self.lower_constraints(owner_generics, 0);
		self.push_params(build_param_scope_at(impl_generics, owner_len));
		constraints.extend(self.lower_constraints(impl_generics, owner_len));
		let owner_has_effects = owner_generics
			.iter()
			.any(|generic| generic.0.kind == nymph_ast::ty::GenericParamKind::Effect);
		let positional = if !owner_has_effects {
			owner_generics
				.iter()
				.enumerate()
				.filter(|(_, generic)| generic.0.kind == nymph_ast::ty::GenericParamKind::Type)
				.map(|(index, _)| self.interner.mk_param(ParamIdx(index as u32)))
				.collect()
		} else {
			Default::default()
		};
		let named = if owner_has_effects {
			owner_generics
				.iter()
				.enumerate()
				.filter(|(_, generic)| generic.0.kind == nymph_ast::ty::GenericParamKind::Type)
				.map(|(index, generic)| {
					(
						generic.0.name.0.clone(),
						self.interner.mk_param(ParamIdx(index as u32)),
					)
				})
				.collect()
		} else {
			Default::default()
		};
		let self_ty = self
			.interner
			.mk_adt(def, GenericArgs::new(positional, named));
		let (iface_name, iface_args) = interface;
		self.finish_interface_impl(
			names,
			kinds,
			self_ty,
			iface_name,
			iface_args,
			members,
			constraints,
		);
		self.pop_params();
		self.pop_params();
	}

	/// Shared tail of interface-impl collection (top-level `impl … for` and nested
	/// `impl Iface { … }`): resolve the interface, align its arguments (with `self`
	/// substituted to `self_ty`), lower the method signatures, and register the impl.
	/// Assumes the impl's generic scope is already active.
	///
	/// A method that omits its return type inherits the interface's declared return
	/// (mapped through the impl's interface arguments), so callers see the real result
	/// type — e.g. `impl Plus<Output = Complex> for Complex { func plus(o) = … }` returns
	/// `Complex`, not `void`.
	#[allow(clippy::too_many_arguments)]
	fn finish_interface_impl(
		&mut self,
		names: Vec<EcoString>,
		generic_kinds: Vec<crate::GenericParameterKind>,
		self_ty: Ty,
		iface_name: &Ident,
		iface_args: &[Spanned<GenericArg>],
		members: &[Spanned<ImplMember>],
		constraints: Vec<Bound>,
	) {
		let Some(interface) = self
			.defs
			.get(&iface_name.0)
			.filter(|&d| self.is_interface(d))
		else {
			self.emit(
				iface_name.1,
				TypeError::NotAnInterface {
					name: iface_name.0.clone(),
				},
			);
			return;
		};
		self
			.annotations
			.record_type_definition_target(iface_name.1, self.defs.stable(interface));

		// Interface argument bindings, with `self` substituted to the implementing type.
		let raw_args = self.align_args(interface, iface_args);
		let empty = FxHashMap::default();
		let args: Vec<(EcoString, Ty)> = raw_args
			.types
			.into_iter()
			.map(|(name, ty)| (name, self.subst(ty, &empty, Some(self_ty))))
			.collect();
		let effect_args = raw_args.effects;

		// Map each interface `Param(k)` to this impl's binding for the k-th generic, so an
		// omitted method return can be resolved to the interface's declared return type.
		let iface_generics = self
			.interfaces
			.get(&interface)
			.map(|i| i.generics.clone())
			.unwrap_or_default();
		let mut isubst: FxHashMap<ParamIdx, Ty> = FxHashMap::default();
		for (k, gname) in iface_generics.iter().enumerate() {
			if let Some((_, ty)) = args.iter().find(|(n, _)| n == gname) {
				isubst.insert(ParamIdx(k as u32), *ty);
			}
		}

		let mut methods = FxHashMap::default();
		// `IfaceMethod` carries no span (unlike `InherentMethod`'s `meta`), so track
		// each name's first-seen span in a side map local to this one impl block —
		// just enough for `DuplicateMember`'s `prev` label.
		let mut first_seen: FxHashMap<EcoString, Span> = FxHashMap::default();
		// The self type's inherent methods, if it's a struct/enum (`Head::Adt`) —
		// collected before impls (see `check_module_impl`, check.rs) precisely so
		// this cross-check has data to query. An interface-impl
		// method (top-level `impl … for` or a nested `impl Iface {.. }`) of the
		// same name as the ADT's own inherent method can escape the separate
		// collection passes and panic later in
		// runtime lowering's duplicate-method assertion — which walks exactly this
		// combined method list for a struct/enum's *generated JS class*. Deliberately
		// scoped to `Head::Adt`: builtin scalar/collection types (e.g. `string`) never
		// materialize a JS class this way, so an inherent method and an interface-impl
		// method of the same name on them (see `stdlib/src/string.nym`'s `contains`,
		// both an inherent `external` method AND a `Contains` impl) is ordinary
		// overloading, not a collision — flagging it here would be a false positive.
		let inherent_head = head_of(&self.interner, self_ty).filter(|h| matches!(h, Head::Adt(_)));
		for m in members {
			let meta = match &m.0 {
				ImplMember::Func { meta, .. } => meta,
				ImplMember::ExternalFunc(_, _, meta) => meta,
				ImplMember::Let { .. } | ImplMember::ExternalLet(..) => continue,
			};
			// A same-named method declared twice inside ONE `impl Iface {.. }`/`impl
			// Iface for T {.. }` block would otherwise silently use the last entry
			// (mirrored by the `methods.insert` below), leaving the first body unchecked
			// while runtime lowering emits every declaration — the same soundness hole
			// `collect_impl_member` (members.rs) closes for inherent members. This is
			// the shared insert point for BOTH top-level `impl … for` and nested
			// `impl Iface {.. }` (both funnel through `finish_interface_impl`), so
			// guarding here covers a within-block duplicate in either shape at once.
			if let Some(&prev) = first_seen.get(&meta.name.0) {
				let ty = self.display(self_ty);
				self.emit(
					meta.name.1,
					TypeError::DuplicateMember {
						name: meta.name.0.clone(),
						ty,
						redefined_span: meta.name.1,
						prev,
					},
				);
			} else {
				first_seen.insert(meta.name.0.clone(), meta.name.1);
				// Only check the type's inherent methods on this name's first
				// appearance in the block — an already-reported within-block
				// duplicate needn't also be reported against the inherent method.
				if let Some(prev) = inherent_head.and_then(|h| self.inherent.method_span(h, &meta.name.0)) {
					let ty = self.display(self_ty);
					self.emit(
						meta.name.1,
						TypeError::DuplicateMember {
							name: meta.name.0.clone(),
							ty,
							redefined_span: meta.name.1,
							prev,
						},
					);
				}
			}
			let base = names.len();
			let method_scope = build_param_scope_at(&meta.generics, base);
			self.push_params(method_scope);
			let params = meta
				.params
				.iter()
				.map(|p| self.lower_type(&p.0.type_))
				.collect();
			let ret = match &meta.return_type {
				Some(ty) => self.lower_type(ty),
				None => self.interface_method_ret(interface, &meta.name.0, &isubst, self_ty),
			};
			let effects = meta
				.effects
				.as_ref()
				.map(|effects| self.lower_effect_row(effects).0)
				.unwrap_or_else(crate::ty::EffectRow::pure);
			self.record_effect_spec(meta, effects.clone(), true);
			let bounds = self.lower_constraints(&meta.generics, base);
			self.pop_params();
			// OO2 (MT2): a plain-target impl (`impl A for B`, not `for mut B`)
			// restates each method's `mut func`/`func` kind and it must MATCH the
			// interface's own declaration — the interface is the source of truth
			// every call-site gate (`solve.rs`) reads, so a mismatched restatement
			// here would silently desync what the impl body actually requires
			// (`this: mut Self` or not, `members.rs`) from what callers are gated
			// against. Skipped for a `Mut` self type (`impl A for mut B` / `impl
			// mut A for B`, ): there, EVERY method of `A` requires a `mut`
			// receiver by construction (the interface is only reachable through
			// the mutable view at all), so per-method kind restatement inside the
			// impl body doesn't carry the same meaning and isn't checked against
			// the interface's declared kind.
			methods.insert(
				meta.name.0.clone(),
				IfaceMethod {
					definition: None,
					has_default: false,
					params,
					ret,
					effects,
					generics: generic_names(&meta.generics),
					bounds,
				},
			);
		}

		for member in members {
			if let ImplMember::ExternalLet(_, marker, meta) = &member.0 {
				let ty = meta.type_.as_ref().map(|ty| self.lower_type(ty));
				self.check_external_value(marker, meta.name.1, ty);
			}
		}

		let blanket = matches!(self.interner.kind(self_ty), TyKind::Param(_));
		let runtime_members = members
			.iter()
			.filter_map(|member| {
				runtime_member_def(
					&member.0,
					self.interfaces.get(&interface),
					&self.external_value_marshals,
				)
			})
			.collect();
		let def = ImplDef {
			definition: None,
			member_catalog: Default::default(),
			runtime_members,
			generics: names,
			generic_kinds,
			self_ty,
			interface,
			source_span: Some(iface_name.1),
			args,
			effect_args,
			methods,
			constraints,
			blanket,
		};
		self.impls.add(&self.interner, def);
	}

	/// The declared return type of interface method `name`, mapped through `isubst`
	/// (interface `Param(k)` → this impl's argument) and `self_ty`. Falls back to `void`
	/// when the interface does not declare the method.
	fn interface_method_ret(
		&mut self,
		interface: DefId,
		name: &str,
		isubst: &FxHashMap<ParamIdx, Ty>,
		self_ty: Ty,
	) -> Ty {
		let Some(iface) = self.interfaces.get(&interface).cloned() else {
			return self.interner.void();
		};
		match iface.methods.get(name) {
			Some(m) => {
				let ret = m.ret;
				self.subst(ret, isubst, Some(self_ty))
			}
			None => self.interner.void(),
		}
	}

	// ── Helpers ──────────────────────────────────────────────────────────────
	/// Lower a function declaration to a method signature (params + return), under the
	/// currently-active parameter scope. `self`/`Self` stays as `SelfTy`.
	/// Lower one interface method's signature. `base` is the number of generic
	/// parameters already occupying `0..base` in the active scope (the owning
	/// interface's generics); the method's OWN generics are registered on top at
	/// `Param(base + j)` so a signature that mentions them (`map<R>(f: (Item) -> R): …R…`)
	/// resolves instead of failing `cannot find type R`.
	fn lower_method_sig(&mut self, meta: &FuncDeclaration, base: usize) -> IfaceMethod {
		let method_scope = build_param_scope_at(&meta.generics, base);
		self.push_params(method_scope);
		let params = meta
			.params
			.iter()
			.map(|p| self.lower_type(&p.0.type_))
			.collect();
		let output = match &meta.return_type {
			Some(ty) => self.lower_type(ty),
			None => self.interner.void(),
		};
		let latent_effects = meta
			.effects
			.as_ref()
			.map(|effects| self.lower_effect_row(effects).0)
			.unwrap_or_else(crate::ty::EffectRow::pure);
		let (ret, effects) = if meta.is_async {
			(
				self.interner.mk_task(output, latent_effects),
				crate::ty::EffectRow::pure(),
			)
		} else {
			(output, latent_effects)
		};
		let bounds = self.lower_constraints(&meta.generics, base);
		self.pop_params();
		IfaceMethod {
			definition: None,
			has_default: false,
			params,
			ret,
			effects,
			generics: generic_names(&meta.generics),
			bounds,
		}
	}

	/// Align an interface reference's generic arguments to the interface's parameter
	/// names, lowering each argument type.
	pub(crate) fn align_args(
		&mut self,
		interface: DefId,
		args: &[Spanned<GenericArg>],
	) -> AlignedInterfaceArgs {
		if let Some(owner) = self.defs.stable(interface).cloned() {
			self.queue_named_generic_labels(owner, args);
		}
		let (names, kinds) = self
			.interfaces
			.get(&interface)
			.map(|i| (i.generics.clone(), i.generic_kinds.clone()))
			.unwrap_or_default();
		let mut out = AlignedInterfaceArgs::default();
		let mut positional = 0;
		for arg in args {
			let (name, index) = if let Some(label) = &arg.0.name {
				(
					label.0.clone(),
					names.iter().position(|name| name == &label.0),
				)
			} else {
				let index = positional;
				positional += 1;
				let Some(name) = names.get(index).cloned() else {
					continue;
				};
				(name, Some(index))
			};
			let kind = index
				.and_then(|index| kinds.get(index))
				.copied()
				.unwrap_or(match arg.0.value {
					nymph_ast::ty::GenericArgValue::Type(_) => crate::GenericParameterKind::Type,
					nymph_ast::ty::GenericArgValue::Effect(_) => crate::GenericParameterKind::Effect,
				});
			match (&arg.0.value, kind) {
				(nymph_ast::ty::GenericArgValue::Type(value), crate::GenericParameterKind::Type) => {
					out.types.push((name, self.lower_type(value)));
				}
				(nymph_ast::ty::GenericArgValue::Effect(value), crate::GenericParameterKind::Effect) => {
					let (row, infer) = self.lower_effect_row(value);
					if infer {
						self.emit(value.1, TypeError::CannotInferEffectRow);
					}
					out.effects.push((name, row));
				}
				(_, crate::GenericParameterKind::Type) => self.emit(
					arg.1,
					TypeError::GenericKindMismatch {
						name,
						expected: "type",
					},
				),
				(_, crate::GenericParameterKind::Effect) => self.emit(
					arg.1,
					TypeError::GenericKindMismatch {
						name,
						expected: "effect",
					},
				),
			}
		}
		out
	}

	/// Collect the interface bounds declared on a generic parameter list into solver
	/// obligations (`Param(base + i): Interface<…>`). `base` offsets the target past
	/// any generics already occupying `0..base` in the active scope (e.g. a method's
	/// own generics, lowered after its owner's) — every existing caller that has no
	/// such prefix passes `0`.
	pub(crate) fn lower_constraints(
		&mut self,
		generics: &[Spanned<nymph_ast::ty::GenericParam>],
		base: usize,
	) -> Vec<Bound> {
		let mut out = Vec::new();
		for (i, g) in generics.iter().enumerate() {
			if let Some(constraint) = &g.0.constraint {
				let target = self
					.interner
					.mk_param(crate::ids::ParamIdx((base + i) as u32));
				self.lower_bound_into(constraint, target, &mut out);
			}
		}
		out
	}

	/// Record, into `self.param_bounds`, the interfaces bounding each parameter in a
	/// generics list (offset by `base` for method generics that follow owner generics).
	/// Enables resolving `P.method` through `P`'s bound during body checking.
	pub(crate) fn record_param_bounds(
		&mut self,
		generics: &[Spanned<nymph_ast::ty::GenericParam>],
		base: usize,
	) {
		for (i, g) in generics.iter().enumerate() {
			if let Some(constraint) = &g.0.constraint {
				let idx = crate::ids::ParamIdx((base + i) as u32);
				let target = self.interner.mk_param(idx);
				let mut bounds = Vec::new();
				self.lower_bound_into(constraint, target, &mut bounds);
				self
					.param_bounds
					.entry(idx)
					.or_default()
					.extend(bounds.iter().map(|bound| bound.interface));
				self
					.param_bound_details
					.entry(idx)
					.or_default()
					.extend(bounds);
			}
		}
	}

	fn lower_bound_into(&mut self, ty: &Spanned<Type>, target: Ty, out: &mut Vec<Bound>) {
		match &ty.0 {
			Type::Intersection(a, b) => {
				self.lower_bound_into(a, target, out);
				self.lower_bound_into(b, target, out);
			}
			Type::Reference { name, generics } => {
				if let Some(interface) = self.defs.get(&name.0).filter(|&d| self.is_interface(d)) {
					self
						.annotations
						.record_type_definition_target(name.1, self.defs.stable(interface));
					let args = self.align_args(interface, generics);
					out.push(Bound {
						ty: target,
						interface,
						args: args.types,
						effect_args: args.effects,
					});
				}
			}
			_ => {}
		}
	}
}

fn func_member_kind(kind: FuncKind) -> crate::MemberKind {
	match kind {
		FuncKind::Instance => crate::MemberKind::Function,
		FuncKind::Namespace => crate::MemberKind::StaticFunction,
	}
}

fn let_member_kind(kind: LetKind) -> crate::MemberKind {
	match kind {
		LetKind::Instance | LetKind::Use => crate::MemberKind::Value,
		LetKind::Namespace => crate::MemberKind::StaticValue,
	}
}

fn runtime_member_def(
	member: &ImplMember,
	interface: Option<&InterfaceDef>,
	external_value_marshals: &FxHashMap<Span, nymph_hir::hir::MarshalKind>,
) -> Option<RuntimeMemberDef> {
	let (name, source_kind) = match member {
		ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => {
			(meta.name.0.clone(), func_member_kind(meta.kind.clone()))
		}
		ImplMember::Let { meta, .. } | ImplMember::ExternalLet(_, _, meta) => {
			let Pattern::Binding { name, .. } = &meta.name.0 else {
				return None;
			};
			(name.0.clone(), let_member_kind(meta.kind))
		}
	};
	let kind = interface
		.and_then(|interface| {
			interface
				.runtime_members
				.iter()
				.find(|candidate| candidate.name == name)
		})
		.map_or(source_kind, |member| member.kind);
	Some(RuntimeMemberDef {
		definition: None,
		name,
		kind,
		has_default: false,
		external: matches!(
			member,
			ImplMember::ExternalFunc(..) | ImplMember::ExternalLet(..)
		),
		marshal: match member {
			ImplMember::ExternalLet(_, _, meta) => external_value_marshals.get(&meta.name.1).copied(),
			_ => None,
		},
	})
}

fn generic_names(generics: &[Spanned<nymph_ast::ty::GenericParam>]) -> Vec<EcoString> {
	generics.iter().map(|g| g.0.name.0.clone()).collect()
}

fn generic_kinds(
	generics: &[Spanned<nymph_ast::ty::GenericParam>],
) -> Vec<crate::GenericParameterKind> {
	generics
		.iter()
		.map(|generic| match generic.0.kind {
			nymph_ast::ty::GenericParamKind::Type => crate::GenericParameterKind::Type,
			nymph_ast::ty::GenericParamKind::Effect => crate::GenericParameterKind::Effect,
		})
		.collect()
}
