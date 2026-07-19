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
use crate::ids::{DefId, ParamIdx};
use crate::iface::head_of;
use crate::ty::{Ty, TyKind};

/// The maximum obligation-solving depth, guarding against cyclic impls.
const MAX_DEPTH: u32 = 32;

/// Where a resolved method's implementation actually lives. Operator dispatch
/// (Slice 4B) needs this distinction: codegen can compile a direct call
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
	/// `this` inside an interface default body (Slice 4C-b binds it to a rigid
	/// synthetic `Param` so the body checks generically once for every impl) —
	/// tagged honestly rather than reused as one of the other variants.
	GenericBound,
}

/// A resolved method call: its instantiated return type plus where the matched
/// method body actually lives.
pub(crate) struct MethodResolution {
	pub(crate) ty: Ty,
	pub(crate) source: MethodSource,
}

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
				self.emit(
					span,
					TypeError::NamedWrongArgCount {
						name: name.into(),
						expected: params.len(),
						found: arg_tys.len(),
					},
				);
				return ret;
			}
			for (p, a) in params.iter().zip(arg_tys) {
				self.unify(*p, *a, span);
			}
			return ret;
		}
		self.emit(span, TypeError::NoNamespacedFnOnParam { name: name.into() });
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
				self.emit(
					span,
					TypeError::NamedWrongArgCount {
						name: name.into(),
						expected: params.len(),
						found: arg_tys.len(),
					},
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
					self.emit(b.span, TypeError::ConflictingImpls { iface });
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
	) -> Option<MethodResolution> {
		let recv = self.shallow_resolve(recv);

		// While checking an interface's own default-method body (Slice 4C-b), `this`
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
			let empty = FxHashMap::default();
			let params: Vec<Ty> = method
				.params
				.iter()
				.map(|t| self.subst(*t, &empty, Some(recv)))
				.collect();
			let ret = self.subst(method.ret, &empty, Some(recv));
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
					source: MethodSource::GenericBound,
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
				source: MethodSource::GenericBound,
			});
		}

		// Inherent methods take priority over interface methods.
		if let Some(ret) = self.resolve_inherent(recv, name, arg_tys, arg_lits, span) {
			return Some(MethodResolution {
				ty: ret,
				source: MethodSource::Inherent,
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
				return self
					.resolve_param_method(idx, name, arg_tys, arg_lits, span)
					.map(|ty| MethodResolution {
						ty,
						source: MethodSource::GenericBound,
					});
			}
			return None;
		}

		let phase1_chosen = self.most_specific(&receiver_matches);
		if phase1_chosen.len() == 1 {
			// `most_specific` only ever filters on `Self`-type concreteness — a
			// concrete impl differing from a blanket sibling ONLY in some other
			// generic parameter (e.g. `Equals<Other = uint> for int` alongside the
			// blanket `impl<T> Equals<Other = self> for T`) still "wins" here purely
			// because it's concrete, even when it doesn't actually apply to the given
			// arguments (`method_matches_receiver`, phase 1's filter, only unifies
			// `Self` + constraints, never `arg_tys`). Confirm the sole survivor really
			// applies before committing to it outright; when a blanket impl was
			// filtered out of `phase1_chosen` alongside it (`phase1_chosen.len() <
			// receiver_matches.len()`), fall through to phase 2 over the *full*
			// `receiver_matches` instead — the blanket sibling stays reachable rather
			// than being shadowed by a concrete impl for the wrong argument shape.
			// Skip this double-check entirely when there was nothing else to fall
			// back to (`phase1_chosen.len() == receiver_matches.len()`): the
			// overwhelmingly common single-impl case commits exactly as before, no
			// extra trial unification.
			let applies = phase1_chosen.len() == receiver_matches.len() || {
				let snapshot = self.table.snapshot();
				let applies = self
					.try_method(phase1_chosen[0], recv, recv_is_mut, name, arg_tys, arg_lits)
					.is_some();
				self.table.rollback_to(snapshot);
				applies
			};
			if applies {
				self.gate_mutating(
					self.impls.impls[phase1_chosen[0]].interface,
					name,
					recv_is_mut,
					span,
				);
				return Some(self.commit_method(phase1_chosen[0], recv, name, arg_tys, arg_lits, span));
			}
		}

		// Phase 2: several impls share the receiver — disambiguate by argument types.
		// Re-widened to `receiver_matches` (rather than `phase1_chosen`) whenever the
		// sole "most specific" candidate above didn't actually apply, so a blanket
		// impl `most_specific` discarded purely for not being concrete is still a
		// candidate here; otherwise (the ordinary multiple-concrete-impls case)
		// `phase1_chosen` is exactly the concrete-only bucket it always was.
		let phase2_candidates: &[usize] = if phase1_chosen.len() > 1 {
			&phase1_chosen
		} else {
			&receiver_matches
		};
		let mut arg_matches: Vec<usize> = Vec::new();
		for &idx in phase2_candidates {
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
				// No candidate's argument types actually unify with what was passed —
				// reported directly, this would be the internal `NoMatchingOverload`,
				// which names the *interface's* method (`equals`, `less_than`, …)
				// rather than the operator or the operand types, and always did once
				// a receiver had two-or-more receiver-matching impls of the same
				// interface (e.g. a primitive with both a same-type concrete impl and
				// a newly-added cross-type one, alongside the interface's blanket).
				// `phase1_chosen[0]` is well-defined here — `receiver_matches` was
				// non-empty on entry, so `most_specific` never returns empty — so
				// commit to it anyway: its own unification of `arg_tys` against the
				// impl's real signature produces the ordinary, actionable
				// `MismatchedTypes` diagnostic instead of this leaky one.
				self.gate_mutating(
					self.impls.impls[phase1_chosen[0]].interface,
					name,
					recv_is_mut,
					span,
				);
				Some(self.commit_method(phase1_chosen[0], recv, name, arg_tys, arg_lits, span))
			}
			1 => Some(self.commit_method(chosen[0], recv, name, arg_tys, arg_lits, span)),
			_ => {
				self.emit(span, TypeError::AmbiguousCall { name: name.into() });
				Some(MethodResolution {
					ty: self.interner.error(),
					source: MethodSource::ImplDirect,
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
		let (params, ret, _source) = self.method_signature(&def, &subst, recv, name)?;
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
	) -> MethodResolution {
		let def = self.impls.impls[idx].clone();
		let subst = self.fresh_subst(def.generics.len());
		let impl_self = self.subst(def.self_ty, &subst, None);
		self.unify(recv, impl_self, span);

		let Some((params, ret, source)) = self.method_signature(&def, &subst, recv, name) else {
			// Unreachable in practice: `candidates` was assembled from interfaces whose
			// `methods` map already contains `name`, so `method_signature` always finds
			// either the impl's own method or the interface's default. Kept total rather
			// than `unreachable!()` so a future change to that invariant fails loudly via
			// a wrong-but-safe error type instead of a panic mid-typecheck.
			return MethodResolution {
				ty: self.interner.error(),
				source: MethodSource::ImplDirect,
			};
		};
		if params.len() != arg_tys.len() {
			self.emit(
				span,
				TypeError::NamedWrongArgCount {
					name: name.into(),
					expected: params.len(),
					found: arg_tys.len(),
				},
			);
			return MethodResolution { ty: ret, source };
		}
		for (i, (param, arg)) in params.iter().zip(arg_tys).enumerate() {
			self.unify_arg(
				*param,
				*arg,
				arg_lits.get(i).copied().unwrap_or(false),
				span,
			);
		}
		MethodResolution { ty: ret, source }
	}

	/// The instantiated `(params, ret, source)` of `name` for impl `def` under
	/// substitution `subst` and receiver `recv`. Prefers the impl's own signature
	/// (`source: ImplDirect`), else the interface's (default) method with interface
	/// parameters mapped to impl args (`source: InterfaceDefault`) — this is the
	/// seam Slice 4B's operator dispatch reads to decide whether codegen can compile
	/// a direct method call or must defer.
	fn method_signature(
		&mut self,
		def: &crate::iface::ImplDef,
		subst: &FxHashMap<ParamIdx, Ty>,
		recv: Ty,
		name: &str,
	) -> Option<(Vec<Ty>, Ty, MethodSource)> {
		if let Some(method) = def.methods.get(name) {
			let params = method
				.params
				.iter()
				.map(|t| self.subst(*t, subst, Some(recv)))
				.collect();
			let ret = self.subst(method.ret, subst, Some(recv));
			return Some((params, ret, MethodSource::ImplDirect));
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
		Some((params, ret, MethodSource::InterfaceDefault))
	}
}
