//! The obligation solver and interface-method resolution.
//!
//! `holds` answers "does `Self` implement `Interface<args>`?" — used for constraint
//! winnowing. `resolve_method` answers "what does `recv.name(args)` resolve to?" —
//! used for method calls and, since operators desugar to method calls, for operators
//! too. Both work by candidate assembly against the impl index, trial unification
//! under a snapshot (rolled back so failed candidates leave no trace), constraint
//! checking, and — for `resolve_method` — argument-type-directed overload selection
//! with concrete impls winning over blanket ones.

use crate::errors::TypeError;
use ecow::EcoString;
use nymph_ast::Span;
use rustc_hash::FxHashMap;

use crate::check::Checker;
use crate::identity::DefinitionId;
use crate::ids::{DefId, ParamIdx};
use crate::iface::head_of;
use crate::ty::{Ty, TyKind};

/// The maximum obligation-solving depth, guarding against cyclic impls.
const MAX_DEPTH: u32 = 32;

/// Where a resolved method's implementation actually lives. Operator dispatch
/// needs this distinction: codegen can compile a direct call
/// (`lhs.method(rhs)`) for an inherent or impl-defined method, but not yet for a
/// method that only exists as an interface's default body (that body isn't
/// materialized on any class).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MethodSource {
	/// An inherent method (`impl Type { .. }`, no interface involved).
	Inherent,
	/// A method the matched impl defines directly.
	ImplDirect,
	/// The interface's own default-method body; the impl relies on it rather
	/// than overriding it (e.g. `Comparable`'s `less_than` over `compare_to`).
	InterfaceDefault,
	/// Resolved through a generic parameter's interface bound (`resolve_param_method`):
	/// the concrete impl is only known once the parameter is instantiated, which
	/// this type-erased-at-lowering compiler does not track. Reached by a
	/// `Param`-typed receiver — either a bounded generic function parameter, or
	/// `this` inside an interface default body (bound to a rigid
	/// synthetic `Param` so the body checks generically once for every impl) —
	/// tagged honestly rather than reused as one of the other variants.
	GenericBound,
}

/// A resolved method call: its instantiated return type plus where the matched
/// method body actually lives.
pub(crate) struct MethodResolution {
	pub(crate) ty: Ty,
	pub(crate) params: Vec<Ty>,
	pub(crate) type_arguments: Vec<Ty>,
	pub(crate) source: MethodSource,
	pub(crate) target: Option<DefinitionId>,
	pub(crate) implementation: Option<DefinitionId>,
	pub(crate) resolved_target: Option<crate::annotate::ResolvedMethodTarget>,
}

impl Checker<'_> {
	/// Resolve a method as a first-class callable from receiver facts alone.
	/// Unlike a call, a stored method has no argument expressions with which to
	/// disambiguate overloads, so multiple receiver-applicable candidates are an
	/// explicit ambiguity rather than an invitation to invent argument types.
	pub(crate) fn resolve_method_value(
		&mut self,
		recv: Ty,
		name: &str,
		span: Span,
	) -> Option<MethodResolution> {
		let receiver = self.shallow_resolve(recv);

		if let Some((params, ty, target, implementation, type_arguments)) =
			self.resolve_inherent_value(receiver, name, span)
		{
			let resolved_target =
				target
					.clone()
					.zip(implementation.clone())
					.map(
						|(member, implementation)| crate::annotate::ResolvedMethodTarget::Inherent {
							member,
							implementation,
						},
					);
			return Some(MethodResolution {
				ty,
				params,
				type_arguments,
				source: MethodSource::Inherent,
				target,
				implementation,
				resolved_target,
			});
		}

		if let TyKind::Param(parameter) = *self.interner.kind(receiver)
			&& let Some(resolution) = self.resolve_param_method_value(parameter, name, span)
		{
			return Some(resolution);
		}

		let head = head_of(&self.interner, receiver);
		let interfaces = self
			.interfaces
			.iter()
			.filter(|(_, definition)| definition.methods.contains_key(name))
			.map(|(interface, _)| *interface)
			.collect::<Vec<_>>();
		let mut candidates = Vec::new();
		for interface in interfaces {
			candidates.extend(self.impls.candidates(interface, head));
		}
		candidates.sort_unstable();
		candidates.dedup();
		candidates.retain(|index| self.implementation_supplies_method(*index, name));
		let mut receiver_matches = Vec::new();
		for index in candidates {
			let snapshot = self.table.snapshot();
			let matched = self.method_matches_receiver(index, receiver);
			self.table.rollback_to(snapshot);
			if matched {
				receiver_matches.push(index);
			}
		}
		let receiver_matches = self.most_specific(&receiver_matches);
		let index = match receiver_matches.as_slice() {
			[] => return None,
			[index] => *index,
			_ => {
				self.emit(span, TypeError::AmbiguousCall { name: name.into() });
				return Some(self.error_method_resolution());
			}
		};
		Some(self.commit_method(index, receiver, name, None, span))
	}

	fn resolve_param_method_value(
		&mut self,
		parameter: ParamIdx,
		name: &str,
		span: Span,
	) -> Option<MethodResolution> {
		let receiver = self.interner.mk_param(parameter);
		let mut candidates = Vec::new();
		for (interface, bound_args) in self.param_interface_bounds(parameter) {
			let Some(definition) = self.interfaces.get(&interface).cloned() else {
				continue;
			};
			let Some(method) = definition.methods.get(name).cloned() else {
				continue;
			};
			candidates.push((interface, bound_args, definition, method));
		}
		if candidates.len() > 1 {
			self.emit(span, TypeError::AmbiguousCall { name: name.into() });
			return Some(self.error_method_resolution());
		}
		let (interface, bound_args, definition, method) = candidates.pop()?;
		let mut substitution = FxHashMap::default();
		for (index, generic) in definition.generics.iter().enumerate() {
			let ty = bound_args
				.iter()
				.find(|(name, _)| name == generic)
				.map(|(_, ty)| *ty)
				.unwrap_or_else(|| self.fresh());
			substitution.insert(ParamIdx(index as u32), ty);
		}
		let (params, ty, type_arguments) = self.instantiate_iface_method_signature(
			&method,
			substitution,
			definition.generics.len(),
			receiver,
			span,
		);
		let target = method.definition;
		let resolved_target = self
			.defs
			.stable(interface)
			.cloned()
			.zip(target.clone())
			.map(
				|(interface, interface_member)| crate::annotate::ResolvedMethodTarget::GenericBound {
					interface,
					interface_member,
				},
			);
		Some(MethodResolution {
			ty,
			params,
			type_arguments,
			source: MethodSource::GenericBound,
			target,
			implementation: None,
			resolved_target,
		})
	}

	fn error_method_resolution(&self) -> MethodResolution {
		MethodResolution {
			ty: self.interner.error(),
			params: Vec::new(),
			type_arguments: Vec::new(),
			source: MethodSource::ImplDirect,
			target: None,
			implementation: None,
			resolved_target: None,
		}
	}

	pub(crate) fn param_interface_bounds(
		&self,
		param: ParamIdx,
	) -> Vec<(DefId, Vec<(EcoString, Ty)>)> {
		let mut bounds = Vec::new();
		if let Some(interfaces) = self.param_bounds.get(&param) {
			for &interface in interfaces {
				let args = self
					.param_bound_details
					.get(&param)
					.and_then(|details| details.iter().find(|bound| bound.interface == interface))
					.map(|bound| bound.args.clone())
					.unwrap_or_default();
				bounds.push((interface, args));
			}
		}
		if let Some(interfaces) = self.synthetic_bounds.get(&param) {
			for &interface in interfaces {
				let args = self
					.synthetic_bound_details
					.get(&param)
					.and_then(|details| details.iter().find(|bound| bound.interface == interface))
					.map(|bound| bound.args.clone())
					.unwrap_or_default();
				bounds.push((interface, args));
			}
		}
		bounds
	}

	pub(crate) fn resolve_param_iface_arg(
		&self,
		param: ParamIdx,
		interface: DefId,
		arg_name: &str,
	) -> Option<Ty> {
		self
			.param_interface_bounds(param)
			.into_iter()
			.find(|(bound, _)| *bound == interface)
			.and_then(|(_, args)| args.into_iter().find(|(name, _)| name == arg_name))
			.map(|(_, ty)| ty)
	}

	/// Does `self_ty` implement `interface`, with the given known argument bindings?
	/// Existence only — used for winnowing an impl's constraints.
	pub(crate) fn holds(
		&mut self,
		self_ty: Ty,
		interface: DefId,
		known: &[(EcoString, Ty)],
		depth: u32,
	) -> bool {
		if depth > MAX_DEPTH {
			return false;
		}
		let resolved = self.shallow_resolve(self_ty);
		if let TyKind::Param(parameter) = self.interner.kind(resolved) {
			let parameter = *parameter;
			let declared_bounds = self
				.param_bounds
				.get(&parameter)
				.cloned()
				.unwrap_or_default();
			for bound in declared_bounds {
				if bound != interface {
					continue;
				}
				let arguments = self
					.param_bound_details
					.get(&parameter)
					.and_then(|details| details.iter().find(|detail| detail.interface == bound))
					.map(|detail| detail.args.clone())
					.unwrap_or_default();
				let snapshot = self.table.snapshot();
				let matched = known.iter().all(|(name, expected)| {
					arguments
						.iter()
						.find(|(argument, _)| argument == name)
						.is_some_and(|(_, actual)| self.try_unify(*expected, *actual))
				});
				self.table.rollback_to(snapshot);
				if matched {
					return true;
				}
			}
		}
		let head = head_of(&self.interner, resolved);
		for idx in self.impls.candidates(interface, head) {
			let snapshot = self.table.snapshot();
			let matched = self.try_impl(idx, self_ty, known, depth);
			self.table.rollback_to(snapshot);
			if matched {
				return true;
			}
		}
		false
	}

	/// Trial: can impl `idx` satisfy the obligation `self_ty: interface<known>`?
	/// Binds inference variables (the caller controls rollback).
	fn try_impl(&mut self, idx: usize, self_ty: Ty, known: &[(EcoString, Ty)], depth: u32) -> bool {
		if !self.structural_blanket_allows(idx, self_ty, depth) {
			return false;
		}
		let def = self.impls.impls[idx].clone();
		let inst = self.instantiate_impl_scheme(&def);
		let subst = inst.substitution;
		let impl_self = inst.ty;
		if !self.try_unify(self_ty, impl_self) {
			return false;
		}
		let impl_args: Vec<(EcoString, Ty)> = def
			.args
			.iter()
			.map(|(name, ty)| (name.clone(), self.subst(*ty, &subst, None)))
			.collect();
		for (known_name, known_ty) in known {
			if let Some((_, impl_ty)) = impl_args.iter().find(|(n, _)| n == known_name)
				&& !self.try_unify(*known_ty, *impl_ty)
			{
				return false;
			}
		}
		self.instantiated_constraints_hold(&inst.obligations, depth)
	}

	/// Does `self_ty` implement `interface`, and if so what is it bound to for
	/// the interface argument named `arg_name` (e.g. `"Item"` for
	/// `Iterator<Item>`, `"T"` for `Iterable<T>`)? Unlike `holds` (existence only —
	/// every trial rolls back so no candidate leaves a binding behind), this keeps
	/// the type argument bound by the first matching generic impl. Failed candidates
	/// roll back, so they cannot leak bindings. Assumes single-impl coherence, as do
	/// the other call sites in this module.
	pub(crate) fn resolve_iface_arg_with_implementation(
		&mut self,
		self_ty: Ty,
		interface: DefId,
		arg_name: &str,
		depth: u32,
	) -> Option<(Ty, usize)> {
		if depth > MAX_DEPTH {
			return None;
		}
		let resolved = self.shallow_resolve(self_ty);
		let head = head_of(&self.interner, resolved);
		for idx in self.impls.candidates(interface, head) {
			let snapshot = self.table.snapshot();
			match self.try_impl_arg(idx, self_ty, arg_name, depth) {
				Some(ty) => return Some((ty, idx)),
				None => self.table.rollback_to(snapshot),
			}
		}
		None
	}

	/// Trial for [`Self::resolve_iface_arg_with_implementation`]: like `try_impl`, but on success
	/// returns the impl's substituted binding for `arg_name` instead of a bare
	/// `bool`. Leaves the unification bindings live on success (caller commits);
	/// the caller rolls back on `None`.
	fn try_impl_arg(&mut self, idx: usize, self_ty: Ty, arg_name: &str, depth: u32) -> Option<Ty> {
		if !self.structural_blanket_allows(idx, self_ty, depth) {
			return None;
		}
		let def = self.impls.impls[idx].clone();
		let inst = self.instantiate_impl_scheme(&def);
		let subst = inst.substitution;
		let impl_self = inst.ty;
		if !self.try_unify(self_ty, impl_self) {
			return None;
		}
		if !self.instantiated_constraints_hold(&inst.obligations, depth) {
			return None;
		}
		def
			.args
			.iter()
			.find(|(n, _)| n == arg_name)
			.map(|(_, ty)| self.subst(*ty, &subst, None))
	}

	pub(crate) fn instantiated_constraints_hold(
		&mut self,
		constraints: &[crate::check::InstantiatedObligation],
		depth: u32,
	) -> bool {
		constraints
			.iter()
			.all(|bound| self.holds(bound.ty, bound.interface, &bound.args, depth + 1))
	}

	fn structural_blanket_capability(&self, index: usize) -> Option<bool> {
		let implementation = &self.impls.impls[index];
		if !implementation.blanket {
			return None;
		}
		let interface = self.defs.data(implementation.interface);
		if !implementation
			.runtime_members
			.iter()
			.any(|member| member.external)
		{
			return None;
		}
		match interface.name.as_str() {
			"Equals" => Some(false),
			"Hash" => Some(true),
			_ => None,
		}
	}

	fn structural_blanket_allows(&mut self, index: usize, ty: Ty, depth: u32) -> bool {
		let Some(hash) = self.structural_blanket_capability(index) else {
			return true;
		};
		self.structural_capability_holds(ty, hash, depth, &mut rustc_hash::FxHashSet::default())
	}

	fn structural_capability_holds(
		&mut self,
		ty: Ty,
		hash: bool,
		depth: u32,
		seen: &mut rustc_hash::FxHashSet<(Ty, bool)>,
	) -> bool {
		if depth > MAX_DEPTH {
			return false;
		}
		let ty = self.shallow_resolve(ty);
		if !seen.insert((ty, hash)) {
			return true;
		}

		if self.explicit_capability_holds(ty, hash, depth) {
			return true;
		}
		if hash
			&& matches!(self.interner.kind(ty), TyKind::Adt(..))
			&& self.explicit_capability_holds(ty, false, depth)
		{
			return false;
		}

		let child =
			|checker: &mut Self, child_ty, child_hash, seen: &mut rustc_hash::FxHashSet<(Ty, bool)>| {
				checker.structural_capability_holds(child_ty, child_hash, depth + 1, seen)
			};
		match self.interner.kind(ty).clone() {
			TyKind::Int
			| TyKind::UInt
			| TyKind::Char
			| TyKind::String
			| TyKind::Boolean
			| TyKind::Void
			| TyKind::Never => true,
			TyKind::Float
			| TyKind::Fn { .. }
			| TyKind::Task { .. }
			| TyKind::Handle(_)
			| TyKind::HandleOutcome(_) => false,
			TyKind::List(item) => child(self, item, hash, seen),
			TyKind::Tuple(items) => items.into_iter().all(|item| child(self, item, hash, seen)),
			TyKind::Map(key, value) => child(self, key, true, seen) && child(self, value, hash, seen),
			TyKind::Adt(definition, arguments) => {
				let mut substitution = FxHashMap::default();
				for (index, argument) in arguments.positional.into_iter().enumerate() {
					substitution.insert(ParamIdx(index as u32), argument);
				}
				let fields = match self.defs.data(definition).kind {
					crate::DefKind::Struct => self.sigs.structs[&definition]
						.fields
						.iter()
						.map(|(_, field)| *field)
						.collect::<Vec<_>>(),
					crate::DefKind::Enum => self.sigs.enums[&definition]
						.variants
						.iter()
						.flat_map(|variant| variant.fields.iter().map(|(_, field)| *field))
						.collect(),
					_ => return false,
				};
				fields.into_iter().all(|field| {
					let field = self.subst(field, &substitution, None);
					child(self, field, hash, seen)
				})
			}
			TyKind::Param(parameter) => {
				self
					.param_interface_bounds(parameter)
					.into_iter()
					.any(|(interface, _)| {
						let name = self.defs.data(interface).name.as_str();
						name == if hash { "Hash" } else { "Equals" } || (!hash && name == "Hash")
					})
			}
			TyKind::Error => true,
			TyKind::Infer(_) | TyKind::SelfTy | TyKind::Intersection(_) => false,
		}
	}

	fn explicit_capability_holds(&mut self, ty: Ty, hash: bool, depth: u32) -> bool {
		let interface_name = if hash { "Hash" } else { "Equals" };
		let equals_args = [(EcoString::from("Other"), ty)];
		let known = if hash { &[][..] } else { &equals_args };
		let interfaces = self
			.interfaces
			.keys()
			.copied()
			.filter(|interface| self.defs.data(*interface).name == interface_name)
			.collect::<Vec<_>>();
		let head = head_of(&self.interner, ty);
		for interface in interfaces {
			for index in self.impls.candidates(interface, head) {
				if self.structural_blanket_capability(index).is_some() {
					continue;
				}
				let snapshot = self.table.snapshot();
				let matched = self.try_impl(index, ty, known, depth + 1);
				self.table.rollback_to(snapshot);
				if matched {
					return true;
				}
			}
		}
		false
	}

	fn instantiate_impl_scheme(
		&mut self,
		def: &crate::iface::ImplDef,
	) -> crate::check::Instantiation {
		self.instantiate(
			def.self_ty,
			&def.constraints,
			(0..def.generics.len()).map(|index| ParamIdx(index as u32)),
			FxHashMap::default(),
			None,
		)
	}

	/// Resolve a namespaced interface function reached through a generic parameter's
	/// bound: `P.name(args)` where `P: Interface` and `Interface` declares `name` (e.g.
	/// `R.default()` with `R: Default`). The result is the method's return type with
	/// `Self` bound to the parameter — the concrete impl is chosen later, where `P` is
	/// instantiated.
	pub(crate) fn resolve_param_namespaced(
		&mut self,
		param: ParamIdx,
		name: &str,
		arg_tys: &[Ty],
		span: Span,
	) -> (Ty, Option<(DefinitionId, DefinitionId)>, Vec<Ty>) {
		let param_ty = self.interner.mk_param(param);
		let interfaces = self.param_interface_bounds(param);
		for (iface_def, bound_args) in interfaces {
			let Some(iface) = self.interfaces.get(&iface_def).cloned() else {
				continue;
			};
			let Some(method) = iface.methods.get(name).cloned() else {
				continue;
			};
			// Interface generics → fresh vars; `Self` → the parameter type.
			let mut isubst: FxHashMap<ParamIdx, Ty> = FxHashMap::default();
			for (k, generic) in iface.generics.iter().enumerate() {
				let ty = bound_args
					.iter()
					.find(|(name, _)| name == generic)
					.map(|(_, ty)| *ty)
					.unwrap_or_else(|| self.fresh());
				isubst.insert(ParamIdx(k as u32), ty);
			}
			let (params, ret, type_arguments) = self.instantiate_iface_method_signature(
				&method,
				isubst,
				iface.generics.len(),
				param_ty,
				span,
			);
			if params.len() != arg_tys.len() {
				self.emit(
					span,
					TypeError::NamedWrongArgCount {
						name: name.into(),
						expected: params.len(),
						found: arg_tys.len(),
					},
				);
				let target = self
					.defs
					.stable(iface_def)
					.cloned()
					.zip(method.definition.clone());
				return (ret, target, type_arguments);
			}
			for (p, a) in params.iter().zip(arg_tys) {
				self.unify(*p, *a, span);
			}
			let target = self
				.defs
				.stable(iface_def)
				.cloned()
				.zip(method.definition.clone());
			return (ret, target, type_arguments);
		}
		self.emit(span, TypeError::NoNamespacedFnOnParam { name: name.into() });
		(self.interner.error(), None, Vec::new())
	}

	pub(crate) fn resolve_param_namespaced_value(
		&mut self,
		param: ParamIdx,
		name: &str,
		span: Span,
	) -> Option<(Vec<Ty>, Ty)> {
		let param_ty = self.interner.mk_param(param);
		for (iface_def, bound_args) in self.param_interface_bounds(param) {
			let iface = self.interfaces.get(&iface_def)?.clone();
			let Some(method) = iface.methods.get(name).cloned() else {
				continue;
			};
			let mut substitution = FxHashMap::default();
			for (index, generic) in iface.generics.iter().enumerate() {
				let ty = bound_args
					.iter()
					.find(|(name, _)| name == generic)
					.map(|(_, ty)| *ty)
					.unwrap_or_else(|| self.fresh());
				substitution.insert(ParamIdx(index as u32), ty);
			}
			let (params, ret, _) = self.instantiate_iface_method_signature(
				&method,
				substitution,
				iface.generics.len(),
				param_ty,
				span,
			);
			return Some((params, ret));
		}
		None
	}

	/// Resolve an instance method `recv.name(args)` where `recv` is a generic parameter,
	/// through one of the parameter's interface bounds (declared `<T: Iface>` bounds in
	/// `param_bounds`, or bounds minted for an `impl Iface` type in `synthetic_bounds`).
	/// The concrete impl is chosen later, where the parameter is instantiated; here the
	/// result is the interface method's return type with `Self` bound to the parameter,
	/// paired with the stable identity of the interface that satisfied the bound.
	/// Runtime ownership and projection are derived from that identity.
	pub(crate) fn resolve_param_method(
		&mut self,
		param: ParamIdx,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: Span,
	) -> Option<(Ty, DefId, Vec<Ty>, Vec<Ty>)> {
		let param_ty = self.interner.mk_param(param);
		for (iface_def, bound_args) in self.param_interface_bounds(param) {
			let Some(iface) = self.interfaces.get(&iface_def).cloned() else {
				continue;
			};
			let Some(method) = iface.methods.get(name).cloned() else {
				continue;
			};
			// Interface generics → fresh vars; `Self` → the parameter type.
			let mut isubst: FxHashMap<ParamIdx, Ty> = FxHashMap::default();
			for (k, generic) in iface.generics.iter().enumerate() {
				let ty = bound_args
					.iter()
					.find(|(name, _)| name == generic)
					.map(|(_, ty)| *ty)
					.unwrap_or_else(|| self.fresh());
				isubst.insert(ParamIdx(k as u32), ty);
			}
			let (params, ret, type_arguments) = self.instantiate_iface_method_signature(
				&method,
				isubst,
				iface.generics.len(),
				param_ty,
				span,
			);
			if params.len() != arg_tys.len() {
				self.emit(
					span,
					TypeError::NamedWrongArgCount {
						name: name.into(),
						expected: params.len(),
						found: arg_tys.len(),
					},
				);
				return Some((ret, iface_def, params, type_arguments));
			}
			for (i, (p, a)) in params.iter().zip(arg_tys).enumerate() {
				self.unify_arg(*p, *a, arg_lits.get(i).copied().unwrap_or(false), span);
			}
			return Some((ret, iface_def, params, type_arguments));
		}
		None
	}

	/// Resolve one compiler protocol slot through precisely the requested bound.
	/// Unlike ordinary source method lookup this never lets an earlier, same-named
	/// bound capture protocol lowering.
	pub(crate) fn resolve_param_exact_method(
		&mut self,
		param: ParamIdx,
		interface: DefId,
		member: &DefinitionId,
		span: Span,
	) -> Option<(Ty, Vec<Ty>)> {
		let param_ty = self.interner.mk_param(param);
		let (_, bound_args) = self
			.param_interface_bounds(param)
			.into_iter()
			.find(|(bound, _)| *bound == interface)?;
		let iface = self.interfaces.get(&interface).cloned()?;
		let method = iface
			.methods
			.values()
			.find(|method| method.definition.as_ref() == Some(member))?
			.clone();
		let mut substitution: FxHashMap<ParamIdx, Ty> = FxHashMap::default();
		for (index, generic) in iface.generics.iter().enumerate() {
			let ty = bound_args
				.iter()
				.find(|(name, _)| name == generic)
				.map(|(_, ty)| *ty)
				.unwrap_or_else(|| self.fresh());
			substitution.insert(ParamIdx(index as u32), ty);
		}
		let (parameters, ret, type_arguments) = self.instantiate_iface_method_signature(
			&method,
			substitution,
			iface.generics.len(),
			param_ty,
			span,
		);
		parameters.is_empty().then_some((ret, type_arguments))
	}

	/// Reject overlapping impls of the same interface.
	///
	/// Two concrete (non-blanket) impls of one interface conflict when their headers can
	/// unify: their self types unify *and* every shared named argument unifies. This
	/// catches genuine duplicates (`impl Equals for int` twice) while permitting
	/// argument-directed overloads (`Plus<Other = int>` vs `Plus<Other = float>` for
	/// `int`, whose `Other` bindings can't unify). Blanket impls are exempt — a blanket
	/// and a concrete impl overlap by construction, and concrete-beats-blanket
	/// specificity already disambiguates them at resolution time.
	pub(crate) fn check_coherence(&mut self) {
		let count = self.impls.impls.len();
		for i in 0..count {
			for j in (i + 1)..count {
				let a = &self.impls.impls[i];
				let b = &self.impls.impls[j];
				if a.interface != b.interface || a.blanket || b.blanket {
					continue;
				}
				// Concrete types with distinct head constructors cannot unify. This is
				// the same semantic partition used by `ImplRegistry` candidate lookup;
				// retaining the full overlap check when either head is unavailable keeps
				// inference-variable/intersection recovery conservative.
				if let (Some(a_head), Some(b_head)) = (
					head_of(&self.interner, a.self_ty),
					head_of(&self.interner, b.self_ty),
				) && a_head != b_head
				{
					continue;
				}
				let a = a.clone();
				let b = b.clone();
				if self.impls_overlap(&a, &b) {
					let iface = self.defs.diagnostic_name(a.interface).clone();
					// Imported implementations deliberately carry no foreign source span.
					// When two dependencies introduce the conflict, anchor the diagnostic
					// at the consuming module rather than silently accepting incoherence or
					// reconstructing provenance from dependency syntax.
					let span = b
						.source_span
						.or(a.source_span)
						.unwrap_or_else(|| Span::new(0, 0));
					self.emit(span, TypeError::ConflictingImpls { iface });
				}
			}
		}
	}

	/// Do two impls' headers overlap under fresh instantiation? Trial-only: bindings are
	/// rolled back before returning.
	fn impls_overlap(&mut self, a: &crate::iface::ImplDef, b: &crate::iface::ImplDef) -> bool {
		let snapshot = self.table.snapshot();
		let a_inst = self.instantiate_impl_scheme(a);
		let b_inst = self.instantiate_impl_scheme(b);
		let a_subst = a_inst.substitution;
		let b_subst = b_inst.substitution;
		// Peel `mut` off both self types: `impl A for B` (self `B`)
		// and `impl A for mut B` (self `Mut(B)`) both apply to a `mut B` receiver,
		// so they OVERLAP and coherence must reject them at declaration — otherwise
		// a `mut`-receiver call finds both applicable and falls through to a
		// confusing `AmbiguousCall`. Without the peel, `try_unify(B, Mut(B))` fails
		// (it has only a `(Mut, Mut)` arm) and the conflict slips through.
		let a_self = self.shallow_resolve(a_inst.ty);
		let b_self = self.shallow_resolve(b_inst.ty);
		let mut overlap = self.try_unify(a_self, b_self);
		if overlap {
			for (name, a_ty) in &a.args {
				if let Some((_, b_ty)) = b.args.iter().find(|(n, _)| n == name) {
					let a_ty = self.subst(*a_ty, &a_subst, None);
					let b_ty = self.subst(*b_ty, &b_subst, None);
					if !self.try_unify(a_ty, b_ty) {
						overlap = false;
						break;
					}
				}
			}
		}
		self.table.rollback_to(snapshot);
		overlap
	}

	/// Resolve `recv.name(args…)` through the interface solver, returning the method's
	/// (instantiated) return type.
	///
	/// Two phases, so error quality survives overloading: first pick the impl by
	/// **receiver** (and constraints); if that is unique, commit it and check the
	/// arguments against its method — a wrong argument then reports a real "mismatched
	/// types" instead of "method not found". Only when several impls share the receiver
	/// (genuine overloads like `int: Plus<Other = int>` vs `Plus<Other = float>`) do the
	/// argument types disambiguate. Concrete impls beat blanket ones throughout.
	pub(crate) fn resolve_method(
		&mut self,
		recv: Ty,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: Span,
	) -> Option<MethodResolution> {
		let recv = self.shallow_resolve(recv);

		// While checking an interface's own default-method body, `this`
		// is bound to a rigid synthetic `Param` so the body checks once, generically,
		// for every future implementor (`check_interface_default_body`). A call to
		// *another method of that same interface* on `this` must resolve directly
		// against the interface's own signature — the ordinary impl search below
		// would instead match that interface's own blanket impl, if one happens to
		// exist anywhere in the program (a real, common pattern — e.g. stdlib's
		// `impl<T> Comparable<Other = T> for T`), which pins the interface's *other*
		// generics to `Self` for this one lookup. That is correct when the receiver
		// really is some concrete `T`, but wrong here: the default body being checked
		// must stay valid for every possible implementor, most of which do *not* set
		// `Other = Self`. Bypassing impl search (no `isubst`: the interface's own
		// generics stay the literal, still-abstract `Param(k)` they already are)
		// keeps e.g. `this.compare_to(other)` inside `Comparable`'s own `less_than`
		// default checked against `compare_to`'s *abstract* signature, matching what
		// `other`'s own (equally abstract) parameter type already is.
		if let Some((iface_id, self_idx)) = self.checking_interface_default
			&& matches!(self.interner.kind(recv), TyKind::Param(idx) if *idx == self_idx)
			&& let Some(method) = self
				.interfaces
				.get(&iface_id)
				.and_then(|i| i.methods.get(name))
				.cloned()
		{
			let owner_generics = self.interfaces[&iface_id].generics.len();
			let (params, ret, type_arguments) = self.instantiate_iface_method_signature(
				&method,
				FxHashMap::default(),
				owner_generics,
				recv,
				span,
			);
			let resolved_target = self
				.defs
				.stable(iface_id)
				.cloned()
				.zip(method.definition.clone())
				.map(
					|(interface, interface_member)| crate::annotate::ResolvedMethodTarget::GenericBound {
						interface,
						interface_member,
					},
				);
			if params.len() != arg_tys.len() {
				self.emit(
					span,
					TypeError::NamedWrongArgCount {
						name: name.into(),
						expected: params.len(),
						found: arg_tys.len(),
					},
				);
				return Some(MethodResolution {
					ty: ret,
					params,
					type_arguments,
					source: MethodSource::GenericBound,
					target: method.definition.clone(),
					implementation: None,
					resolved_target: resolved_target.clone(),
				});
			}
			for (i, (param, arg)) in params.iter().zip(arg_tys).enumerate() {
				self.unify_arg(
					*param,
					*arg,
					arg_lits.get(i).copied().unwrap_or(false),
					span,
				);
			}
			return Some(MethodResolution {
				ty: ret,
				params,
				type_arguments,
				source: MethodSource::GenericBound,
				target: method.definition,
				implementation: None,
				resolved_target,
			});
		}

		// Inherent methods take priority over interface methods.
		if let Some((params, ret, target, implementation, type_arguments)) =
			self.resolve_matching_inherent(recv, name, arg_tys, arg_lits, span)
		{
			let resolved_target =
				target
					.clone()
					.zip(implementation.clone())
					.map(
						|(member, implementation)| crate::annotate::ResolvedMethodTarget::Inherent {
							member,
							implementation,
						},
					);
			return Some(MethodResolution {
				ty: ret,
				params,
				type_arguments,
				source: MethodSource::Inherent,
				target,
				implementation,
				resolved_target,
			});
		}

		// For a still-generic `Param` receiver, consult the parameter's OWN bounds
		// (declared `<T: Iface>` bounds, plus any synthetic bounds minted for an
		// `impl Iface` type) *before* the impl-index search below. `head_of` returns
		// `None` for a `Param` receiver, which collapses candidate gathering to each
		// interface's blanket bucket only (`ImplRegistry::candidates`) — so without
		// this branch, an unrelated blanket impl of the same method name silently
		// wins over the method the param's own bound actually declares (the
		// resolver-precedence bug: a user bound's `less_than` losing to the
		// prelude's blanket `Comparable.less_than`). Only fall through to phase 1
		// when NONE of the param's bounds provide the method — that preserves
		// unconstrained blanket dispatch (e.g. `func same<T>(a: T, b: T): boolean =
		// a.equals(b)` with no bound on `T`, resolved through a blanket `Equals`
		// impl below). A bounds miss falls through to preserve that case; this
		// branch must never `return None` itself, only fall through.
		if let crate::ty::TyKind::Param(idx) = *self.interner.kind(recv)
			&& let Some((ty, iface_def, params, type_arguments)) =
				self.resolve_param_method(idx, name, arg_tys, arg_lits, span)
		{
			let target = self.interfaces[&iface_def].methods[name].definition.clone();
			let resolved_target = self
				.defs
				.stable(iface_def)
				.cloned()
				.zip(target.clone())
				.map(
					|(interface, interface_member)| crate::annotate::ResolvedMethodTarget::GenericBound {
						interface,
						interface_member,
					},
				);
			return Some(MethodResolution {
				ty,
				params,
				type_arguments,
				source: MethodSource::GenericBound,
				target,
				implementation: None,
				resolved_target,
			});
		}

		let head = head_of(&self.interner, recv);

		let interfaces: Vec<DefId> = self
			.interfaces
			.iter()
			.filter(|(_, def)| def.methods.contains_key(name))
			.map(|(id, _)| *id)
			.collect();
		let mut candidates = Vec::new();
		for interface in interfaces {
			candidates.extend(self.impls.candidates(interface, head));
		}
		candidates.sort_unstable();
		candidates.dedup();
		candidates.retain(|&idx| self.implementation_supplies_method(idx, name));

		// Phase 1: impls whose receiver (and constraints) match.
		let mut receiver_matches: Vec<usize> = Vec::new();
		for idx in candidates {
			let snapshot = self.table.snapshot();
			let matched = self.method_matches_receiver(idx, recv);
			self.table.rollback_to(snapshot);
			if matched {
				receiver_matches.push(idx);
			}
		}
		if receiver_matches.is_empty() {
			// No impl matches the receiver. For a `Param` receiver, the bound-first
			// branch above already tried `resolve_param_method` and would have
			// returned on a hit — reaching here means it also found nothing, so
			// there is nothing left to try. Preserve the diagnostics from a same-name
			// inherent method whose arguments did not fit; it was skipped above only
			// to give a viable interface overload the opportunity to resolve.
			if let Some((params, ret, target, implementation, type_arguments)) =
				self.resolve_inherent(recv, name, arg_tys, arg_lits, span)
			{
				let resolved_target =
					target
						.clone()
						.zip(implementation.clone())
						.map(
							|(member, implementation)| crate::annotate::ResolvedMethodTarget::Inherent {
								member,
								implementation,
							},
						);
				return Some(MethodResolution {
					ty: ret,
					params,
					type_arguments,
					source: MethodSource::Inherent,
					target,
					implementation,
					resolved_target,
				});
			}
			return None;
		}

		// The overwhelmingly common case: exactly one impl matches the receiver, so
		// commit it directly without argument disambiguation.
		if receiver_matches.len() == 1 {
			return Some(self.commit_method(
				receiver_matches[0],
				recv,
				name,
				Some((arg_tys, arg_lits)),
				span,
			));
		}

		// Phase 2: several impls share the receiver — disambiguate by argument types over
		// the FULL receiver-match set (not `most_specific`'d first), so a blanket impl
		// stays reachable when a concrete sibling matches the receiver but does not
		// actually fit the arguments — e.g. `intVal.equals(intVal)` must fall to the
		// blanket `Equals<Other = self>` rather than committing the cross-type
		// `Equals<Other = uint> for int` and then mismatching the `int` argument.
		// Each surviving candidate is tagged with whether it matched only via numeric
		// literal widening (`int` literal → `uint`/`float`); an exact-type match is
		// strictly more specific, so if any candidate matches exactly, the widened ones
		// are dropped, and `most_specific` (concrete over blanket) breaks any remaining
		// tie. This keeps `a.plus(2)` (a: int) resolving to `Plus<Other = int> for int`
		// rather than tying with the cross-type `Plus<Other = uint> for int`, and
		// also covers the `int`/`float` operator overload pair.
		let mut arg_matches: Vec<(usize, bool)> = Vec::new();
		for &idx in &receiver_matches {
			let snapshot = self.table.snapshot();
			let matched = self.try_method(idx, recv, name, arg_tys, arg_lits);
			self.table.rollback_to(snapshot);
			if let Some((_, widened)) = matched {
				arg_matches.push((idx, widened));
			}
		}
		let exact: Vec<usize> = arg_matches
			.iter()
			.filter(|(_, widened)| !widened)
			.map(|(idx, _)| *idx)
			.collect();
		let arg_matches: Vec<usize> = if exact.is_empty() {
			arg_matches.iter().map(|(idx, _)| *idx).collect()
		} else {
			exact
		};
		let chosen = self.most_specific(&arg_matches);
		match chosen.len() {
			0 => {
				self.emit(span, TypeError::NoMatchingOverload { name: name.into() });
				// An error path: diagnostics are already emitted, so `Checked::diags` is
				// non-empty and lowering never runs on this result — the `source` tag is
				// inert here, but `ImplDirect` is picked to keep this branch total without
				// implying a (nonexistent) default-method body.
				Some(MethodResolution {
					ty: self.interner.error(),
					params: Vec::new(),
					type_arguments: Vec::new(),
					source: MethodSource::ImplDirect,
					target: None,
					implementation: None,
					resolved_target: None,
				})
			}
			1 => Some(self.commit_method(chosen[0], recv, name, Some((arg_tys, arg_lits)), span)),
			_ => {
				self.emit(span, TypeError::AmbiguousCall { name: name.into() });
				Some(MethodResolution {
					ty: self.interner.error(),
					params: Vec::new(),
					type_arguments: Vec::new(),
					source: MethodSource::ImplDirect,
					target: None,
					implementation: None,
					resolved_target: None,
				})
			}
		}
	}

	/// Keep only concrete impls if any are present (they beat blanket impls).
	fn most_specific(&self, indices: &[usize]) -> Vec<usize> {
		let concrete: Vec<usize> = indices
			.iter()
			.copied()
			.filter(|&i| !self.impls.impls[i].blanket)
			.collect();
		if concrete.is_empty() {
			indices.to_vec()
		} else {
			concrete
		}
	}

	fn implementation_supplies_method(&self, idx: usize, name: &str) -> bool {
		let implementation = &self.impls.impls[idx];
		let method = self
			.interfaces
			.get(&implementation.interface)
			.and_then(|interface| interface.methods.get(name));
		match method.and_then(|method| method.definition.as_ref()) {
			Some(member) => implementation.member_catalog.target(member).is_some(),
			None => {
				// Direct `check_module` has no compiler-owned member identities.
				// Preserve type checking there, but never fabricate a stable target.
				implementation.methods.contains_key(name) || method.is_some_and(|method| method.has_default)
			}
		}
	}

	/// Non-trial receiver unification for `commit_method`.
	fn unify_self(&mut self, recv: Ty, impl_self: Ty, span: Span) {
		self.unify(recv, impl_self, span);
	}

	/// Does impl `idx`'s receiver type (and its constraints) match `recv`?
	fn method_matches_receiver(&mut self, idx: usize, recv: Ty) -> bool {
		if !self.structural_blanket_allows(idx, recv, 0) {
			return false;
		}
		let def = self.impls.impls[idx].clone();
		let inst = self.instantiate_impl_scheme(&def);
		let impl_self = inst.ty;
		self.try_unify(recv, impl_self) && self.instantiated_constraints_hold(&inst.obligations, 0)
	}

	/// Trial (arg-aware): does impl `idx` provide `name` applicable to `recv(args)`?
	/// On success returns `(return type, widened)`, where `widened` is true iff any
	/// argument matched only via numeric widening (see
	/// [`Checker::try_unify_arg_widened`]) rather than an exact-type unification —
	/// phase 2 uses it to prefer exact matches over widened ones.
	fn try_method(
		&mut self,
		idx: usize,
		recv: Ty,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
	) -> Option<(Ty, bool)> {
		if !self.structural_blanket_allows(idx, recv, 0) {
			return None;
		}
		let def = self.impls.impls[idx].clone();
		let inst = self.instantiate_impl_scheme(&def);
		let subst = inst.substitution;
		let impl_self = inst.ty;
		if !self.try_unify(recv, impl_self) || !self.instantiated_constraints_hold(&inst.obligations, 0)
		{
			return None;
		}
		let (params, ret, _source, _) = self.method_signature(&def, &subst, recv, name, None)?;
		if params.len() != arg_tys.len() {
			return None;
		}
		let mut widened = false;
		for (i, (param, arg)) in params.iter().zip(arg_tys).enumerate() {
			widened |=
				self.try_unify_arg_widened(*param, *arg, arg_lits.get(i).copied().unwrap_or(false))?
		}
		Some((ret, widened))
	}

	/// Commit a chosen impl for real: unify the receiver, then check each argument
	/// against the method's parameter (emitting mismatches). Returns the method's
	/// return type.
	pub(crate) fn commit_method(
		&mut self,
		idx: usize,
		recv: Ty,
		name: &str,
		arguments: Option<(&[Ty], &[bool])>,
		span: Span,
	) -> MethodResolution {
		let def = self.impls.impls[idx].clone();
		let inst = self.instantiate_impl_scheme(&def);
		let subst = inst.substitution;
		let impl_self = inst.ty;
		self.unify_self(recv, impl_self, span);
		let implementation = def.definition.clone();
		let interface = self.defs.stable(def.interface).cloned();
		let interface_member = self
			.interfaces
			.get(&def.interface)
			.and_then(|interface| interface.methods.get(name))
			.and_then(|method| method.definition.clone());
		let slot = interface_member
			.as_ref()
			.and_then(|member| def.member_catalog.target(member))
			.cloned();
		let target = slot.as_ref().map(|slot| slot.member_id.clone());
		let implementation_arguments = (0..def.generics.len())
			.map(|index| self.shallow_resolve(subst[&ParamIdx(index as u32)]))
			.collect::<Vec<_>>();
		let resolved_target = interface.zip(slot).map(|(interface, slot)| {
			crate::annotate::ResolvedMethodTarget::InterfaceImplementation {
				interface,
				slot,
				implementation_arguments: implementation_arguments.clone(),
				method_arguments: Vec::new(),
			}
		});

		let Some((params, ret, source, type_arguments)) =
			self.method_signature(&def, &subst, recv, name, Some(span))
		else {
			// Unreachable in practice: `candidates` is assembled from interfaces whose
			// `methods` map already contains `name`, so `method_signature` always finds
			// either the impl's own method or the interface's default. Kept total rather
			// than `unreachable!()` so an invariant violation produces a safe error type.
			return MethodResolution {
				ty: self.interner.error(),
				params: Vec::new(),
				type_arguments: Vec::new(),
				source: MethodSource::ImplDirect,
				target,
				implementation,
				resolved_target,
			};
		};
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
				return MethodResolution {
					ty: ret,
					params,
					type_arguments: Vec::new(),
					source,
					target,
					implementation,
					resolved_target,
				};
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
		let mut resolved_target = resolved_target;
		if let Some(crate::annotate::ResolvedMethodTarget::InterfaceImplementation {
			method_arguments,
			..
		}) = &mut resolved_target
		{
			*method_arguments = type_arguments.clone();
		}
		MethodResolution {
			ty: ret,
			params,
			type_arguments,
			source,
			target,
			implementation,
			resolved_target,
		}
	}

	/// The instantiated `(params, ret, source)` selected by `def`'s exact member
	/// catalog. The catalog, rather than another name/default check, owns whether
	/// the implementation override or interface default supplies the body.
	fn method_signature(
		&mut self,
		def: &crate::iface::ImplDef,
		subst: &FxHashMap<ParamIdx, Ty>,
		recv: Ty,
		name: &str,
		obligation_span: Option<Span>,
	) -> Option<(Vec<Ty>, Ty, MethodSource, Vec<Ty>)> {
		let interface = self.interfaces.get(&def.interface).cloned()?;
		let interface_method = interface.methods.get(name).cloned()?;
		let source = if let Some(interface_member) = interface_method.definition.as_ref() {
			def.member_catalog.target(interface_member)?.source
		} else if def.methods.contains_key(name) {
			crate::ImplementationMemberSource::Override
		} else if interface_method.has_default {
			crate::ImplementationMemberSource::InheritedDefault
		} else {
			return None;
		};
		if source == crate::ImplementationMemberSource::Override {
			let method = def.methods.get(name)?;
			let (params, ret, type_arguments) = self.instantiate_iface_method_signature(
				method,
				subst.clone(),
				def.generics.len(),
				recv,
				obligation_span,
			);
			return Some((params, ret, MethodSource::ImplDirect, type_arguments));
		}

		// Interface default method: map interface Param(k) → this impl's arg bindings.
		if source != crate::ImplementationMemberSource::InheritedDefault {
			return None;
		}
		let method = interface_method;
		let mut isubst: FxHashMap<ParamIdx, Ty> = FxHashMap::default();
		for (k, param_name) in interface.generics.iter().enumerate() {
			let value = def
				.args
				.iter()
				.find(|(n, _)| n == param_name)
				.map(|(_, t)| self.subst(*t, subst, None))
				.unwrap_or_else(|| self.fresh());
			isubst.insert(ParamIdx(k as u32), value);
		}
		let iface_len = interface.generics.len();
		let (params, ret, type_arguments) =
			self.instantiate_iface_method_signature(&method, isubst, iface_len, recv, obligation_span);
		Some((params, ret, MethodSource::InterfaceDefault, type_arguments))
	}

	fn instantiate_iface_method_signature(
		&mut self,
		method: &crate::iface::IfaceMethod,
		mut subst: FxHashMap<ParamIdx, Ty>,
		owner_generics: usize,
		recv: Ty,
		obligation_span: impl Into<Option<Span>>,
	) -> (Vec<Ty>, Ty, Vec<Ty>) {
		let site = obligation_span.into();
		let inst = self.instantiate(
			method.ret,
			&method.bounds,
			(0..method.generics.len()).map(|index| ParamIdx((owner_generics + index) as u32)),
			subst,
			Some(recv),
		);
		if let Some(site) = site {
			self.defer_obligations(site, inst.obligations.iter().cloned());
		}
		subst = inst.substitution;
		let params = method
			.params
			.iter()
			.map(|ty| self.subst(*ty, &subst, Some(recv)))
			.collect();
		let ret = self.instantiate_opaque_return(method.ret, &method.bounds, &subst, recv);
		let type_arguments = (owner_generics..owner_generics + method.generics.len())
			.map(|index| {
				subst
					.get(&ParamIdx(index as u32))
					.copied()
					.unwrap_or_else(|| self.fresh())
			})
			.collect();
		(params, ret, type_arguments)
	}

	/// Instantiate an opaque interface return at the call site while retaining its
	/// interface arguments. A rigid synthetic parameter belongs to the declaration's
	/// generic scope; reusing it would leave arguments such as `#(K, V)` unsubstituted
	/// when a caller invokes methods on the returned iterator.
	fn instantiate_opaque_return(
		&mut self,
		ret: Ty,
		method_bounds: &[crate::iface::Bound],
		subst: &FxHashMap<ParamIdx, Ty>,
		recv: Ty,
	) -> Ty {
		let TyKind::Param(source) = self.interner.kind(ret) else {
			return self.subst(ret, subst, Some(recv));
		};
		let source = *source;
		let details = method_bounds
			.iter()
			.filter(|bound| matches!(self.interner.kind(bound.ty), TyKind::Param(idx) if *idx == source))
			.cloned()
			.collect::<Vec<_>>();
		let details = if details.is_empty() {
			self
				.synthetic_bound_details
				.get(&source)
				.cloned()
				.unwrap_or_default()
		} else {
			details
		};
		if details.is_empty() {
			return self.subst(ret, subst, Some(recv));
		}
		let idx = ParamIdx(Self::SYNTHETIC_PARAM_BASE + self.synthetic_params);
		self.synthetic_params += 1;
		let ty = self.interner.mk_param(idx);
		for bound in details {
			let args = bound
				.args
				.into_iter()
				.map(|(name, arg)| (name, self.subst(arg, subst, Some(recv))))
				.collect();
			self
				.synthetic_bounds
				.entry(idx)
				.or_default()
				.push(bound.interface);
			self
				.synthetic_bound_details
				.entry(idx)
				.or_default()
				.push(crate::iface::Bound {
					ty,
					interface: bound.interface,
					args,
					effect_args: bound.effect_args,
				});
		}
		ty
	}
}
