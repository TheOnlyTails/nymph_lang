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
	decl::{Declaration, FuncDeclaration, ImplMember, StructInnerMember},
	ty::{GenericArg, GenericParam, Type},
};
use rustc_hash::FxHashMap;

use crate::check::Checker;
use crate::def::DefKind;
use crate::ids::{DefId, ParamIdx};
use crate::lower::build_param_scope;
use crate::ty::{GenericArgs, Interner, Ty, TyKind};

/// A method signature as seen through an interface or impl. `Param` indices refer to
/// the owning interface's (or impl's) generic parameters; `SelfTy` to the receiver.
#[derive(Debug, Clone)]
pub struct IfaceMethod {
	pub params: Vec<Ty>,
	pub ret: Ty,
}

/// A collected `interface`.
#[derive(Debug, Clone, Default)]
pub struct InterfaceDef {
	pub generics: Vec<EcoString>,
	pub methods: FxHashMap<EcoString, IfaceMethod>,
}

/// One required bound, e.g. `T: Comparable<T>` on an impl's generic parameter.
#[derive(Debug, Clone)]
pub struct Bound {
	pub ty: Ty,
	pub interface: DefId,
	pub args: Vec<(EcoString, Ty)>,
}

/// A collected `impl Interface<…> for Type { … }`.
#[derive(Debug, Clone)]
pub struct ImplDef {
	pub generics: Vec<EcoString>,
	pub self_ty: Ty,
	pub interface: DefId,
	/// The span of the interface reference in the `impl … for …` header, used to
	/// anchor a coherence (conflicting-impl) diagnostic.
	pub span: Span,
	/// Interface argument bindings (`Other = …`, `Output = …`), by parameter name,
	/// with `self` already substituted to `self_ty`.
	pub args: Vec<(EcoString, Ty)>,
	pub methods: FxHashMap<EcoString, IfaceMethod>,
	pub constraints: Vec<Bound>,
	/// True when the implementing type is a bare generic parameter (a blanket impl).
	pub blanket: bool,
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
#[derive(Debug, Default)]
pub struct ImplRegistry {
	pub impls: Vec<ImplDef>,
	keyed: FxHashMap<(DefId, Head), Vec<usize>>,
	blanket: FxHashMap<DefId, Vec<usize>>,
}

impl ImplRegistry {
	fn add(&mut self, interner: &Interner, def: ImplDef) {
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
		matches!(self.defs.data(def).kind, DefKind::Interface { .. })
	}

	// ── Interface collection ─────────────────────────────────────────────────
	pub(crate) fn collect_interfaces(&mut self) {
		let module = self.module;
		let ifaces: Vec<(DefId, usize)> = self
			.defs
			.defs
			.iter()
			.enumerate()
			.filter_map(|(i, d)| match d.kind {
				DefKind::Interface { member } => Some((DefId(i as u32), member)),
				_ => None,
			})
			.collect();

		for (id, member) in ifaces {
			let Declaration::Interface {
				generics, members, ..
			} = &module.members[member]
			else {
				continue;
			};
			let names = generic_names(generics);
			self.push_params(build_param_scope(generics));
			let mut methods = FxHashMap::default();
			for m in members {
				use nymph_ast::decl::{InterfaceElement, InterfaceMember};
				if let InterfaceMember::Element(element) = &m.0
					&& let InterfaceElement::Func { meta, .. } = &element.0
				{
					let sig = self.lower_method_sig(meta);
					methods.insert(meta.name.0.clone(), sig);
				}
			}
			self.pop_params();
			self.interfaces.insert(
				id,
				InterfaceDef {
					generics: names,
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
		let constraints = self.lower_constraints(generics);
		self.finish_interface_impl(names, self_ty, iface_name, iface_args, members, constraints);
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
			.filter_map(|(i, d)| match d.kind {
				DefKind::Struct { member } | DefKind::Enum { member } => Some((DefId(i as u32), member)),
				_ => None,
			})
			.collect();
		for (def, member) in adts {
			self.collect_adt_inner_impls(def, member);
		}
	}

	fn collect_adt_inner_impls(&mut self, def: DefId, member: usize) {
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
		for m in members {
			if let StructInnerMember::Impl {
				interface,
				generics: impl_generics,
				members: impl_members,
			} = &m.0
			{
				self.collect_inner_impl(def, generics, interface, impl_generics, impl_members);
			}
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
		// The impl's generic scope is the owner's generics (indices `0..k`) followed by
		// any the impl block declares itself.
		let combined: Vec<Spanned<GenericParam>> = owner_generics
			.iter()
			.chain(impl_generics)
			.cloned()
			.collect();
		let names = generic_names(&combined);
		self.push_params(build_param_scope(&combined));
		let owner_len = owner_generics.len();
		let positional: Vec<Ty> = (0..owner_len)
			.map(|i| self.interner.mk_param(ParamIdx(i as u32)))
			.collect();
		let self_ty = self
			.interner
			.mk_adt(def, GenericArgs::new(positional, Vec::new()));
		let (iface_name, iface_args) = interface;
		let constraints = self.lower_constraints(&combined);
		self.finish_interface_impl(names, self_ty, iface_name, iface_args, members, constraints);
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
	fn finish_interface_impl(
		&mut self,
		names: Vec<EcoString>,
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

		// Interface argument bindings, with `self` substituted to the implementing type.
		let raw_args = self.align_args(interface, iface_args);
		let empty = FxHashMap::default();
		let args: Vec<(EcoString, Ty)> = raw_args
			.into_iter()
			.map(|(name, ty)| (name, self.subst(ty, &empty, Some(self_ty))))
			.collect();

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
		for m in members {
			let meta = match &m.0 {
				ImplMember::Func { meta, .. } => meta,
				ImplMember::ExternalFunc(_, _, meta) => meta,
				ImplMember::Let { .. } | ImplMember::ExternalLet(..) => continue,
			};
			let params = meta
				.params
				.iter()
				.map(|p| self.lower_type(&p.0.type_))
				.collect();
			let ret = match &meta.return_type {
				Some(ty) => self.lower_type(ty),
				None => self.interface_method_ret(interface, &meta.name.0, &isubst, self_ty),
			};
			methods.insert(meta.name.0.clone(), IfaceMethod { params, ret });
		}

		let blanket = matches!(self.interner.kind(self_ty), TyKind::Param(_));
		let def = ImplDef {
			generics: names,
			self_ty,
			interface,
			span: iface_name.1,
			args,
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
	fn lower_method_sig(&mut self, meta: &FuncDeclaration) -> IfaceMethod {
		let params = meta
			.params
			.iter()
			.map(|p| self.lower_type(&p.0.type_))
			.collect();
		let ret = match &meta.return_type {
			Some(ty) => self.lower_type(ty),
			None => self.interner.void(),
		};
		IfaceMethod { params, ret }
	}

	/// Align an interface reference's generic arguments to the interface's parameter
	/// names, lowering each argument type.
	fn align_args(&mut self, interface: DefId, args: &[Spanned<GenericArg>]) -> Vec<(EcoString, Ty)> {
		let names = self
			.interfaces
			.get(&interface)
			.map(|i| i.generics.clone())
			.unwrap_or_default();
		let mut out = Vec::new();
		let mut positional = 0;
		for arg in args {
			let ty = self.lower_type(&arg.0.value);
			match &arg.0.name {
				Some(label) => out.push((label.0.clone(), ty)),
				None => {
					if let Some(name) = names.get(positional) {
						out.push((name.clone(), ty));
					}
					positional += 1;
				}
			}
		}
		out
	}

	/// Collect the interface bounds declared on a generic parameter list into solver
	/// obligations (`Param(i): Interface<…>`).
	pub(crate) fn lower_constraints(
		&mut self,
		generics: &[Spanned<nymph_ast::ty::GenericParam>],
	) -> Vec<Bound> {
		let mut out = Vec::new();
		for (i, g) in generics.iter().enumerate() {
			if let Some(constraint) = &g.0.constraint {
				let target = self.interner.mk_param(crate::ids::ParamIdx(i as u32));
				self.lower_bound_into(constraint, target, &mut out);
			}
		}
		out
	}

	/// Record, into `self.param_bounds`, the interfaces bounding each parameter in a
	/// generics list (offset by `base` for method generics that follow owner generics).
	/// Enables resolving `P.method()` through `P`'s bound during body checking.
	pub(crate) fn record_param_bounds(
		&mut self,
		generics: &[Spanned<nymph_ast::ty::GenericParam>],
		base: usize,
	) {
		for (i, g) in generics.iter().enumerate() {
			if let Some(constraint) = &g.0.constraint {
				let idx = crate::ids::ParamIdx((base + i) as u32);
				let mut interfaces = Vec::new();
				self.collect_bound_interfaces(constraint, &mut interfaces);
				if !interfaces.is_empty() {
					self.param_bounds.entry(idx).or_default().extend(interfaces);
				}
			}
		}
	}

	fn collect_bound_interfaces(&self, ty: &Spanned<Type>, out: &mut Vec<DefId>) {
		match &ty.0 {
			Type::Intersection(a, b) => {
				self.collect_bound_interfaces(a, out);
				self.collect_bound_interfaces(b, out);
			}
			Type::Reference { name, .. } => {
				if let Some(interface) = self.defs.get(&name.0).filter(|&d| self.is_interface(d)) {
					out.push(interface);
				}
			}
			_ => {}
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
					let args = self.align_args(interface, generics);
					out.push(Bound {
						ty: target,
						interface,
						args,
					});
				}
			}
			_ => {}
		}
	}
}

fn generic_names(generics: &[Spanned<nymph_ast::ty::GenericParam>]) -> Vec<EcoString> {
	generics.iter().map(|g| g.0.name.0.clone()).collect()
}
