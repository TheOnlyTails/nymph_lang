//! The obligation solver and interface-method resolution.
//!
//! `holds` answers "does `Self` implement `Interface<args>`?" — used for constraint
//! winnowing. `resolve_method` answers "what does `recv.name(args)` resolve to?" —
//! used for method calls and, since operators desugar to method calls, for operators
//! too. Both work by candidate assembly against the impl index, trial unification
//! under a snapshot (rolled back so failed candidates leave no trace), constraint
//! checking, and — for `resolve_method` — argument-type-directed overload selection
//! with concrete impls winning over blanket ones.

use ecow::EcoString;
use nymph_ast::Span;
use rustc_hash::FxHashMap;

use crate::check::Checker;
use crate::ids::{DefId, ParamIdx};
use crate::iface::head_of;
use crate::ty::Ty;

/// The maximum obligation-solving depth, guarding against cyclic impls.
const MAX_DEPTH: u32 = 32;

impl Checker<'_> {
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
		let def = self.impls.impls[idx].clone();
		let subst = self.fresh_subst(def.generics.len());
		let impl_self = self.subst(def.self_ty, &subst, None);
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
		self.constraints_hold(&def.constraints, &subst, depth)
	}

	pub(crate) fn constraints_hold(
		&mut self,
		constraints: &[crate::iface::Bound],
		subst: &FxHashMap<ParamIdx, Ty>,
		depth: u32,
	) -> bool {
		for bound in constraints {
			let ty = self.subst(bound.ty, subst, None);
			let args: Vec<(EcoString, Ty)> = bound
				.args
				.iter()
				.map(|(name, t)| (name.clone(), self.subst(*t, subst, None)))
				.collect();
			if !self.holds(ty, bound.interface, &args, depth + 1) {
				return false;
			}
		}
		true
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
	) -> Ty {
		let param_ty = self.interner.mk_param(param);
		let interfaces = self.param_bounds.get(&param).cloned().unwrap_or_default();
		for iface_def in interfaces {
			let Some(iface) = self.interfaces.get(&iface_def).cloned() else {
				continue;
			};
			let Some(method) = iface.methods.get(name).cloned() else {
				continue;
			};
			// Interface generics → fresh vars; `Self` → the parameter type.
			let mut isubst: FxHashMap<ParamIdx, Ty> = FxHashMap::default();
			for k in 0..iface.generics.len() {
				isubst.insert(ParamIdx(k as u32), self.fresh());
			}
			let params: Vec<Ty> = method
				.params
				.iter()
				.map(|t| self.subst(*t, &isubst, Some(param_ty)))
				.collect();
			let ret = self.subst(method.ret, &isubst, Some(param_ty));
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
			for (p, a) in params.iter().zip(arg_tys) {
				self.unify(*p, *a, span);
			}
			return ret;
		}
		self.error(
			format!("no namespaced function `{name}` found on this type parameter"),
			span,
		);
		self.interner.error()
	}

	/// Resolve an instance method `recv.name(args)` where `recv` is a generic parameter,
	/// through one of the parameter's interface bounds (declared `<T: Iface>` bounds in
	/// `param_bounds`, or bounds minted for an `impl Iface` type in `synthetic_bounds`).
	/// The concrete impl is chosen later, where the parameter is instantiated; here the
	/// result is the interface method's return type with `Self` bound to the parameter.
	fn resolve_param_method(
		&mut self,
		param: ParamIdx,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: Span,
	) -> Option<Ty> {
		let param_ty = self.interner.mk_param(param);
		let mut ifaces: Vec<DefId> = Vec::new();
		if let Some(bounds) = self.param_bounds.get(&param) {
			ifaces.extend(bounds.iter().copied());
		}
		if let Some(bounds) = self.synthetic_bounds.get(&param) {
			ifaces.extend(bounds.iter().copied());
		}
		for iface_def in ifaces {
			let Some(iface) = self.interfaces.get(&iface_def).cloned() else {
				continue;
			};
			let Some(method) = iface.methods.get(name).cloned() else {
				continue;
			};
			// Interface generics → fresh vars; `Self` → the parameter type.
			let mut isubst: FxHashMap<ParamIdx, Ty> = FxHashMap::default();
			for k in 0..iface.generics.len() {
				isubst.insert(ParamIdx(k as u32), self.fresh());
			}
			let params: Vec<Ty> = method
				.params
				.iter()
				.map(|t| self.subst(*t, &isubst, Some(param_ty)))
				.collect();
			let ret = self.subst(method.ret, &isubst, Some(param_ty));
			if params.len() != arg_tys.len() {
				self.error(
					format!(
						"`{name}` expects {} argument(s), found {}",
						params.len(),
						arg_tys.len()
					),
					span,
				);
				return Some(ret);
			}
			for (i, (p, a)) in params.iter().zip(arg_tys).enumerate() {
				self.unify_arg(*p, *a, arg_lits.get(i).copied().unwrap_or(false), span);
			}
			return Some(ret);
		}
		None
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
				let a = self.impls.impls[i].clone();
				let b = self.impls.impls[j].clone();
				if a.interface != b.interface || a.blanket || b.blanket {
					continue;
				}
				if self.impls_overlap(&a, &b) {
					let iface = self.defs.data(a.interface).name.clone();
					self.error(
						format!("conflicting implementations of interface `{iface}`"),
						b.span,
					);
				}
			}
		}
	}

	/// Do two impls' headers overlap under fresh instantiation? Trial-only: bindings are
	/// rolled back before returning.
	fn impls_overlap(&mut self, a: &crate::iface::ImplDef, b: &crate::iface::ImplDef) -> bool {
		let snapshot = self.table.snapshot();
		let a_subst = self.fresh_subst(a.generics.len());
		let b_subst = self.fresh_subst(b.generics.len());
		let a_self = self.subst(a.self_ty, &a_subst, None);
		let b_self = self.subst(b.self_ty, &b_subst, None);
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
	) -> Option<Ty> {
		let recv = self.shallow_resolve(recv);

		// Inherent methods take priority over interface methods.
		if let Some(ret) = self.resolve_inherent(recv, name, arg_tys, arg_lits, span) {
			return Some(ret);
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
			// No impl matches. If the receiver is a generic parameter with an interface
			// bound (a declared `<T: Iface>` or a synthetic one minted for `impl Iface`),
			// resolve the method through that bound.
			if let crate::ty::TyKind::Param(idx) = *self.interner.kind(recv) {
				return self.resolve_param_method(idx, name, arg_tys, arg_lits, span);
			}
			return None;
		}

		let chosen = self.most_specific(&receiver_matches);
		if chosen.len() == 1 {
			return Some(self.commit_method(chosen[0], recv, name, arg_tys, arg_lits, span));
		}

		// Phase 2: several impls share the receiver — disambiguate by argument types.
		let mut arg_matches: Vec<usize> = Vec::new();
		for &idx in &chosen {
			let snapshot = self.table.snapshot();
			let matched = self
				.try_method(idx, recv, name, arg_tys, arg_lits)
				.is_some();
			self.table.rollback_to(snapshot);
			if matched {
				arg_matches.push(idx);
			}
		}
		let chosen = self.most_specific(&arg_matches);
		match chosen.len() {
			0 => {
				self.error(
					format!("no overload of `{name}` matches these arguments"),
					span,
				);
				Some(self.interner.error())
			}
			1 => Some(self.commit_method(chosen[0], recv, name, arg_tys, arg_lits, span)),
			_ => {
				self.error(
					format!("ambiguous call to `{name}`: multiple impls apply"),
					span,
				);
				Some(self.interner.error())
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

	/// Does impl `idx`'s receiver type (and its constraints) match `recv`?
	fn method_matches_receiver(&mut self, idx: usize, recv: Ty) -> bool {
		let def = self.impls.impls[idx].clone();
		let subst = self.fresh_subst(def.generics.len());
		let impl_self = self.subst(def.self_ty, &subst, None);
		self.try_unify(recv, impl_self) && self.constraints_hold(&def.constraints, &subst, 0)
	}

	/// Trial (arg-aware): does impl `idx` provide `name` applicable to `recv(args)`?
	fn try_method(
		&mut self,
		idx: usize,
		recv: Ty,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
	) -> Option<Ty> {
		let def = self.impls.impls[idx].clone();
		let subst = self.fresh_subst(def.generics.len());
		let impl_self = self.subst(def.self_ty, &subst, None);
		if !self.try_unify(recv, impl_self) || !self.constraints_hold(&def.constraints, &subst, 0) {
			return None;
		}
		let (params, ret) = self.method_signature(&def, &subst, recv, name)?;
		if params.len() != arg_tys.len() {
			return None;
		}
		for (i, (param, arg)) in params.iter().zip(arg_tys).enumerate() {
			if !self.try_unify_arg(*param, *arg, arg_lits.get(i).copied().unwrap_or(false)) {
				return None;
			}
		}
		Some(ret)
	}

	/// Commit a chosen impl for real: unify the receiver, then check each argument
	/// against the method's parameter (emitting mismatches). Returns the method's
	/// return type.
	fn commit_method(
		&mut self,
		idx: usize,
		recv: Ty,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: Span,
	) -> Ty {
		let def = self.impls.impls[idx].clone();
		let subst = self.fresh_subst(def.generics.len());
		let impl_self = self.subst(def.self_ty, &subst, None);
		self.unify(recv, impl_self, span);

		let Some((params, ret)) = self.method_signature(&def, &subst, recv, name) else {
			return self.interner.error();
		};
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

	/// The instantiated `(params, ret)` of `name` for impl `def` under substitution
	/// `subst` and receiver `recv`. Prefers the impl's own signature, else the
	/// interface's (default) method with interface parameters mapped to impl args.
	fn method_signature(
		&mut self,
		def: &crate::iface::ImplDef,
		subst: &FxHashMap<ParamIdx, Ty>,
		recv: Ty,
		name: &str,
	) -> Option<(Vec<Ty>, Ty)> {
		if let Some(method) = def.methods.get(name) {
			let params = method
				.params
				.iter()
				.map(|t| self.subst(*t, subst, Some(recv)))
				.collect();
			let ret = self.subst(method.ret, subst, Some(recv));
			return Some((params, ret));
		}

		// Interface default method: map interface Param(k) → this impl's arg bindings.
		let interface = self.interfaces.get(&def.interface).cloned()?;
		let method = interface.methods.get(name).cloned()?;
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
		let params = method
			.params
			.iter()
			.map(|t| self.subst(*t, &isubst, Some(recv)))
			.collect();
		let ret = self.subst(method.ret, &isubst, Some(recv));
		Some((params, ret))
	}
}
