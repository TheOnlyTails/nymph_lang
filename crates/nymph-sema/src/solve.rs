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
	/// The span of the matched impl's `interface … for …` header (`ImplDef::span`),
	/// when resolution went through the impl index (`Inherent`/`GenericBound`
	/// don't: neither commits a specific `impl` block, see their construction
	/// sites). Lets a caller tell whether the impl that provided this method (or
	/// whose interface default it fell back to) lives in the program lowering
	/// will actually walk, or was cloned from an offset prelude module (a span
	/// `>= check_module_with_prelude`'s `SPAN_BASE`) and so is never lowered —
	/// see `dispatch_kind_for` in `infer_expr.rs`.
	pub(crate) impl_span: Option<Span>,
}

impl Checker<'_> {
	/// MT2 OO1/OO3: emit [`TypeError::MutMethodNeedsMutReceiver`] if `interface`'s
	/// OWN declared kind for `name` (the source of truth — an impl's restatement
	/// is checked to MATCH it at collection time, `iface.rs`'s OO2 check) is
	/// `mut func`, but the receiver the caller actually wrote wasn't `mut`. The
	/// single gate every method-resolution path in `resolve_method` calls,
	/// keyed by whichever interface actually provided the resolved method.
	pub(crate) fn gate_mutating(
		&mut self,
		interface: DefId,
		name: &str,
		recv_is_mut: bool,
		span: Span,
	) {
		let mutating = self
			.interfaces
			.get(&interface)
			.and_then(|i| i.methods.get(name))
			.is_some_and(|m| m.mutating);
		if mutating && !recv_is_mut {
			self.emit(
				span,
				TypeError::MutMethodNeedsMutReceiver {
					method: name.into(),
				},
			);
		}
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
		self.holds_self(self_ty, false, interface, known, depth)
	}

	/// [`Self::holds`], additionally honoring a `Mut` impl self type (MT2 OO4/OO5:
	/// `impl A for mut B` / `impl mut A for B`) the same way method resolution's
	/// `try_unify_self` does: `self_is_mut` records whether the caller's argument
	/// was actually written `mut`, and `try_impl` peels a `Mut` impl self type one
	/// way against `self_ty`, requiring `self_is_mut` for that match — WITHOUT
	/// requiring `self_ty` itself to be wrapped in `Mut` (unlike a plain
	/// `try_unify(self_ty, impl_self)`, which only has a `(Mut, Mut)` arm and so
	/// can't match `self_ty` against a plain impl once `self_ty` is `Mut`-wrapped;
	/// see `finalize_pending_bounds`'s doc comment). `self_ty` is always passed
	/// as the plain (un-wrapped) resolved type: a `mut` argument still satisfies
	/// an ordinary, non-mut-specific bound one-way, same as the `mut T <: T`
	/// subtype rule everywhere else — a mut-only impl is an ADDITIONAL match,
	/// never a replacement for that.
	pub(crate) fn holds_self(
		&mut self,
		self_ty: Ty,
		self_is_mut: bool,
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
			let matched = self.try_impl(idx, self_ty, self_is_mut, known, depth);
			self.table.rollback_to(snapshot);
			if matched {
				return true;
			}
		}
		false
	}

	/// Trial: can impl `idx` satisfy the obligation `self_ty: interface<known>`?
	/// Binds inference variables (the caller controls rollback).
	fn try_impl(
		&mut self,
		idx: usize,
		self_ty: Ty,
		self_is_mut: bool,
		known: &[(EcoString, Ty)],
		depth: u32,
	) -> bool {
		let def = self.impls.impls[idx].clone();
		let subst = self.fresh_subst(def.generics.len());
		let impl_self = self.subst(def.self_ty, &subst, None);
		if !self.try_unify_self(self_ty, impl_self, self_is_mut) {
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

	/// Does `self_ty` implement `interface`, and if so what is it bound to for
	/// the interface argument named `arg_name` (e.g. `"Item"` for
	/// `Iterator<Item>`, `"T"` for `Iterable<T>`)? Unlike `holds`/`holds_self`
	/// (existence only — every trial rolls back so no candidate ever leaves a
	/// binding behind), a caller here needs the ACTUAL type argument a still-
	/// generic impl bound during unification, so the first matching candidate's
	/// bindings are committed rather than rolled back (only failed candidates
	/// roll back, so a failed trial never leaks bindings the caller didn't ask
	/// for). Assumes single-impl coherence, same as every other call site in
	/// this module: the first matching candidate wins.
	///
	/// `self_ty` is the PEELED (non-`mut`) type, same convention `holds`/
	/// `resolve_method` use throughout; `self_is_mut` carries whether the
	/// caller's value was actually written `mut` (mirrors `resolve_method`'s own
	/// `recv_is_mut`), so an impl reachable only through the mutable view (`impl
	/// A for mut B` / `impl mut A for B`, MT2 OO4/OO5 — the only way an
	/// `Iterator`/`Iterable` impl can mutate `this` inside its own body, since
	/// `check_method_body` binds `this: mut Self` only for a `mut func`) still
	/// matches correctly rather than being permanently unreachable.
	pub(crate) fn resolve_iface_arg(
		&mut self,
		self_ty: Ty,
		self_is_mut: bool,
		interface: DefId,
		arg_name: &str,
		depth: u32,
	) -> Option<Ty> {
		if depth > MAX_DEPTH {
			return None;
		}
		let resolved = self.shallow_resolve(self_ty);
		let head = head_of(&self.interner, resolved);
		for idx in self.impls.candidates(interface, head) {
			let snapshot = self.table.snapshot();
			match self.try_impl_arg(idx, self_ty, self_is_mut, arg_name, depth) {
				Some(ty) => return Some(ty),
				None => self.table.rollback_to(snapshot),
			}
		}
		None
	}

	/// Trial for [`Self::resolve_iface_arg`]: like `try_impl`, but on success
	/// returns the impl's substituted binding for `arg_name` instead of a bare
	/// `bool`. Leaves the unification bindings live on success (caller commits);
	/// the caller rolls back on `None`.
	fn try_impl_arg(
		&mut self,
		idx: usize,
		self_ty: Ty,
		self_is_mut: bool,
		arg_name: &str,
		depth: u32,
	) -> Option<Ty> {
		let def = self.impls.impls[idx].clone();
		let subst = self.fresh_subst(def.generics.len());
		let impl_self = self.subst(def.self_ty, &subst, None);
		if !self.try_unify_self(self_ty, impl_self, self_is_mut) {
			return None;
		}
		if !self.constraints_hold(&def.constraints, &subst, depth) {
			return None;
		}
		def
			.args
			.iter()
			.find(|(n, _)| n == arg_name)
			.map(|(_, ty)| self.subst(*ty, &subst, None))
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
			// The method's own generics (`Param(iface_len + j)`) → fresh vars too.
			for j in 0..method.generics.len() {
				isubst.insert(ParamIdx((iface.generics.len() + j) as u32), self.fresh());
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
	/// result is the interface method's return type with `Self` bound to the parameter,
	/// paired with the `DefId` of the interface the bound was satisfied through — its
	/// caller (`resolve_method`) reads that back into `MethodResolution::impl_span`
	/// (Finding 1/3, stdlib linkage groundwork review round 2): a bound interface
	/// declared inside an offset prelude clone has a `DefData::span` `>= SPAN_BASE`,
	/// letting `impl_is_unmaterialized` (`infer_expr.rs`) flag a `GenericBound`
	/// resolution as prelude-origin exactly the way it already does for
	/// `ImplDirect`/`InterfaceDefault`, without changing behavior for an ordinary
	/// user-declared interface bound (whose span is always well below `SPAN_BASE`).
	fn resolve_param_method(
		&mut self,
		param: ParamIdx,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
		span: Span,
	) -> Option<(Ty, DefId)> {
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
			// The method's own generics (`Param(iface_len + j)`) → fresh vars too.
			for j in 0..method.generics.len() {
				isubst.insert(ParamIdx((iface.generics.len() + j) as u32), self.fresh());
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
				return Some((ret, iface_def));
			}
			for (i, (p, a)) in params.iter().zip(arg_tys).enumerate() {
				self.unify_arg(*p, *a, arg_lits.get(i).copied().unwrap_or(false), span);
			}
			return Some((ret, iface_def));
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
		// Peel `mut` off both self types (MT2 OO4/OO5): `impl A for B` (self `B`)
		// and `impl A for mut B` (self `Mut(B)`) both apply to a `mut B` receiver,
		// so they OVERLAP and coherence must reject them at declaration — otherwise
		// a `mut`-receiver call finds both applicable and falls through to a
		// confusing `AmbiguousCall`. Without the peel, `try_unify(B, Mut(B))` fails
		// (it has only a `(Mut, Mut)` arm) and the conflict slips through.
		let a_self = self.subst(a.self_ty, &a_subst, None);
		let a_self = self.strip_mut(a_self);
		let b_self = self.subst(b.self_ty, &b_subst, None);
		let b_self = self.strip_mut(b_self);
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
		// Captured BEFORE the peel below erases it: whether the receiver, as the
		// caller actually wrote it, is `mut` (MT2, OO1/OO3). A concrete `mut B`
		// receiver, or a `mut T` bound parameter (which lowers to
		// `TyKind::Mut(Param(idx))`), both read `true` here; a plain `B` or bare
		// `T` param reads `false`. This is the single flag every `mut func`
		// call-site gate below consults — see `mutating_gate`.
		let recv_resolved = self.shallow_resolve(recv);
		let recv_is_mut = matches!(self.interner.kind(recv_resolved), TyKind::Mut(_));

		// A `mut` receiver dispatches exactly like its inner type — `mut` is
		// transparent to method/impl resolution (mirrors `head_of`'s own peel);
		// every impl's `Self` type is never itself `mut`, so unifying an unpeeled
		// `mut Adt` receiver against it would spuriously mismatch. Peeled once
		// here, at entry, covers every downstream use in this function
		// (`resolve_inherent`, `method_matches_receiver`, `commit_method`, …).
		// Method resolution/matching against a `Mut`-self-type impl (OO4/OO5)
		// still needs the real receiver mutability, which is why `recv_is_mut`
		// was captured above rather than derived from this peeled `recv`.
		let recv = self.strip_mut(recv);

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
			// OO1 gate: the default body of one interface method calling another
			// (`mut func`) method of the same interface on `this` needs `this` to
			// be `mut` too — mirrors every other call-site gate below.
			self.gate_mutating(iface_id, name, recv_is_mut, span);
			let empty = FxHashMap::default();
			let params: Vec<Ty> = method
				.params
				.iter()
				.map(|t| self.subst(*t, &empty, Some(recv)))
				.collect();
			let ret = self.subst(method.ret, &empty, Some(recv));
			// `iface_id`'s own `DefData::span` doubles as the prelude-origin marker
			// `impl_is_unmaterialized` (`infer_expr.rs`) reads (Finding 1/3, stdlib
			// linkage groundwork review round 2): an interface declared inside an
			// offset prelude clone has a span `>= SPAN_BASE`, so a default-method
			// body checked generically against a prelude-only interface is
			// correctly flagged as unmaterialized too, exactly like every other
			// `GenericBound` construction site below.
			let iface_span = self.defs.data(iface_id).span;
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
					impl_span: Some(iface_span),
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
				impl_span: Some(iface_span),
			});
		}

		// Inherent methods take priority over interface methods.
		if let Some((ret, method_span)) = self.resolve_inherent(recv, name, arg_tys, arg_lits, span) {
			return Some(MethodResolution {
				ty: ret,
				source: MethodSource::Inherent,
				impl_span: Some(method_span),
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
		// impl below). A prior fix attempt returned eagerly on a bounds miss here
		// and broke exactly that case with a spurious "no method" error — this
		// branch must never `return None` itself, only fall through.
		if let crate::ty::TyKind::Param(idx) = *self.interner.kind(recv)
			&& let Some((ty, iface_def)) = self.resolve_param_method(idx, name, arg_tys, arg_lits, span)
		{
			// OO3 gate: `x.method()` where `x: T` (or `x: mut T`) and `T: A`
			// resolved `method` through `A`'s bound — same gate as everywhere
			// else, keyed off the interface the bound was satisfied through.
			self.gate_mutating(iface_def, name, recv_is_mut, span);
			return Some(MethodResolution {
				ty,
				source: MethodSource::GenericBound,
				impl_span: Some(self.defs.data(iface_def).span),
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
			let matched = self.method_matches_receiver(idx, recv, recv_is_mut);
			self.table.rollback_to(snapshot);
			if matched {
				receiver_matches.push(idx);
			}
		}
		if receiver_matches.is_empty() {
			// No impl matches the receiver. For a `Param` receiver, the bound-first
			// branch above already tried `resolve_param_method` and would have
			// returned on a hit — reaching here means it also found nothing, so
			// there is nothing left to try.
			return None;
		}

		// The overwhelmingly common case: exactly one impl matches the receiver, so
		// commit it directly without argument disambiguation.
		if receiver_matches.len() == 1 {
			self.gate_mutating(
				self.impls.impls[receiver_matches[0]].interface,
				name,
				recv_is_mut,
				span,
			);
			return Some(self.commit_method(receiver_matches[0], recv, name, arg_tys, arg_lits, span));
		}

		// Phase 2: several impls share the receiver — disambiguate by argument types over
		// the FULL receiver-match set (not `most_specific`'d first), so a blanket impl
		// stays reachable when a concrete sibling matches the receiver but does not
		// actually fit the arguments — e.g. `intVal.equals(intVal)` must fall to the
		// blanket `Equals<Other = self>` rather than committing the cross-type
		// `Equals<Other = uint> for int` and then mismatching the `int` argument.
		// Each surviving candidate is tagged with whether it matched only via
		// `int`-literal widening (`int` literal → `uint`/`float`); an exact-type match is
		// strictly more specific, so if any candidate matches exactly, the widened ones
		// are dropped, and `most_specific` (concrete over blanket) breaks any remaining
		// tie. This keeps `a.plus(2)` (a: int) resolving to `Plus<Other = int> for int`
		// rather than tying with the cross-type `Plus<Other = uint> for int`, and
		// generalises the pre-existing `int`/`float` operator overload pair the same way.
		let mut arg_matches: Vec<(usize, bool)> = Vec::new();
		for &idx in &receiver_matches {
			let snapshot = self.table.snapshot();
			let matched = self.try_method(idx, recv, recv_is_mut, name, arg_tys, arg_lits);
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
					source: MethodSource::ImplDirect,
					impl_span: None,
				})
			}
			1 => {
				self.gate_mutating(
					self.impls.impls[chosen[0]].interface,
					name,
					recv_is_mut,
					span,
				);
				Some(self.commit_method(chosen[0], recv, name, arg_tys, arg_lits, span))
			}
			_ => {
				self.emit(span, TypeError::AmbiguousCall { name: name.into() });
				Some(MethodResolution {
					ty: self.interner.error(),
					source: MethodSource::ImplDirect,
					impl_span: None,
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

	/// Trial-unify a receiver against an impl's (subst'ed) self type, honoring a
	/// `Mut` self type (MT2 OO4/OO5: `impl A for mut B` / `impl mut A for B`,
	/// normalized to a `Mut` self type at collection — `iface.rs`). Such an impl
	/// matches ONLY a receiver the caller actually wrote as `mut`: without this,
	/// `recv` (already `strip_mut`-peeled, like every other receiver use in this
	/// file) would unify against a `Mut(B)` self type and always fail — the
	/// "dead impl" bug (`try_unify`'s only `Mut` arm is `(Mut, Mut)`) — for
	/// *every* receiver, mut or not, since the peel already stripped the tag
	/// `try_unify` would need to see. `recv_is_mut` supplies that original
	/// mutability, captured once at `resolve_method`'s entry.
	fn try_unify_self(&mut self, recv: Ty, impl_self: Ty, recv_is_mut: bool) -> bool {
		match self.interner.kind(impl_self) {
			&TyKind::Mut(inner) => recv_is_mut && self.try_unify(recv, inner),
			_ => self.try_unify(recv, impl_self),
		}
	}

	/// Non-trial counterpart of [`Self::try_unify_self`], for `commit_method`:
	/// only ever reached after `method_matches_receiver` already confirmed the
	/// receiver's mutability matches, so no `recv_is_mut` check is needed here —
	/// just the same `Mut` self-type peel so the real unification (which emits
	/// diagnostics on failure) compares the right pair of types.
	fn unify_self(&mut self, recv: Ty, impl_self: Ty, span: Span) {
		match self.interner.kind(impl_self) {
			&TyKind::Mut(inner) => self.unify(recv, inner, span),
			_ => self.unify(recv, impl_self, span),
		}
	}

	/// Does impl `idx`'s receiver type (and its constraints) match `recv`?
	fn method_matches_receiver(&mut self, idx: usize, recv: Ty, recv_is_mut: bool) -> bool {
		let def = self.impls.impls[idx].clone();
		let subst = self.fresh_subst(def.generics.len());
		let impl_self = self.subst(def.self_ty, &subst, None);
		self.try_unify_self(recv, impl_self, recv_is_mut)
			&& self.constraints_hold(&def.constraints, &subst, 0)
	}

	/// Trial (arg-aware): does impl `idx` provide `name` applicable to `recv(args)`?
	/// On success returns `(return type, widened)`, where `widened` is true iff any
	/// argument matched only via `int`-literal widening (see
	/// [`Checker::try_unify_arg_widened`]) rather than an exact-type unification —
	/// phase 2 uses it to prefer exact matches over widened ones.
	fn try_method(
		&mut self,
		idx: usize,
		recv: Ty,
		recv_is_mut: bool,
		name: &str,
		arg_tys: &[Ty],
		arg_lits: &[bool],
	) -> Option<(Ty, bool)> {
		let def = self.impls.impls[idx].clone();
		let subst = self.fresh_subst(def.generics.len());
		let impl_self = self.subst(def.self_ty, &subst, None);
		if !self.try_unify_self(recv, impl_self, recv_is_mut)
			|| !self.constraints_hold(&def.constraints, &subst, 0)
		{
			return None;
		}
		let (params, ret, _source) = self.method_signature(&def, &subst, recv, name)?;
		if params.len() != arg_tys.len() {
			return None;
		}
		let mut widened = false;
		for (i, (param, arg)) in params.iter().zip(arg_tys).enumerate() {
			match self.try_unify_arg_widened(*param, *arg, arg_lits.get(i).copied().unwrap_or(false)) {
				Some(w) => widened |= w,
				None => return None,
			}
		}
		Some((ret, widened))
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
		self.unify_self(recv, impl_self, span);

		let Some((params, ret, source)) = self.method_signature(&def, &subst, recv, name) else {
			// Unreachable in practice: `candidates` was assembled from interfaces whose
			// `methods` map already contains `name`, so `method_signature` always finds
			// either the impl's own method or the interface's default. Kept total rather
			// than `unreachable!()` so a future change to that invariant fails loudly via
			// a wrong-but-safe error type instead of a panic mid-typecheck.
			return MethodResolution {
				ty: self.interner.error(),
				source: MethodSource::ImplDirect,
				impl_span: Some(def.span),
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
			return MethodResolution {
				ty: ret,
				source,
				impl_span: Some(def.span),
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
		MethodResolution {
			ty: ret,
			source,
			impl_span: Some(def.span),
		}
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
		// The method's OWN generics sit at `Param(iface_len + j)` (see `lower_method_sig`);
		// instantiate each to a fresh inference variable so a call like `it.map(f)` infers
		// them from the arguments instead of leaking the rigid parameter.
		let iface_len = interface.generics.len();
		for j in 0..method.generics.len() {
			isubst.insert(ParamIdx((iface_len + j) as u32), self.fresh());
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
