//! Hindley-Milner-style structural unification and subtyping.
//!
//! `unify` implements the symmetric structural unifier via occurs-check and
//! inference-variable binding through a union-find table: walks two types,
//! binds inference variables (rejecting occurrence), and reports precise mismatches.
//! The occurs-check prevents infinite types; `try_unify` implements trial unification
//! for overload resolution (bindings kept iff the trial succeeds).

use crate::errors::TypeError;
use nymph_ast::{
	Span,
	expr::{Expr, ExprKind},
};

use crate::check::Checker;
use crate::ty::{Ty, TyKind};

impl Checker<'_> {
	fn uint_fits_int(&mut self, expected: Ty, found: Ty) -> bool {
		let expected = self.shallow_resolve(expected);
		let found = self.strip_mut(found);
		matches!(self.interner.kind(expected), TyKind::Int)
			&& matches!(self.interner.kind(found), TyKind::UInt)
	}

	pub(crate) fn record_implicit_uint_to_int(&mut self, expr: &Expr, found: Ty, expected: Ty) {
		if self.uint_fits_int(expected, found) {
			self.annotations.record_implicit_uint_to_int(expr.id);
		}
	}

	/// Whether an `int` literal argument may implicitly widen to the parameter type
	/// (`float`/`uint`) — the argument-position counterpart of the check-mode literal
	/// widening, since method/operator arguments are synthesised, not checked.
	fn int_literal_fits_param(&mut self, param: Ty, arg: Ty) -> bool {
		let arg = self.shallow_resolve(arg);
		let param = self.shallow_resolve(param);
		matches!(self.interner.kind(arg), TyKind::Int)
			&& matches!(self.interner.kind(param), TyKind::Float | TyKind::UInt)
	}

	/// Unify a call/operator argument against its parameter, letting an `int` *literal*
	/// widen to `float`/`uint`, any `uint` value widen to `int`, and a `mut`-typed
	/// argument satisfy a non-`mut` parameter (the one-way `mut T <: T` rule,
	/// mirroring [`Checker::subtype`] — see the comment there for why the peel is
	/// asymmetric). Every method-style call (inherent, interface-impl, interface-default,
	/// generic-bound) routes arguments through here, so without this, `mut T <: T` held
	/// only for free-function calls and operators, not method calls.
	pub(crate) fn unify_arg(&mut self, param: Ty, arg: Ty, is_int_literal: bool, span: Span) {
		if is_int_literal && self.int_literal_fits_param(param, arg) {
			return;
		}
		if self.uint_fits_int(param, arg) {
			return;
		}
		let param_r = self.shallow_resolve(param);
		let arg_r = self.shallow_resolve(arg);
		match (self.interner.kind(param_r), self.interner.kind(arg_r)) {
			// `mut T` param, `mut U` arg: peel both, same as `subtype`'s `(Mut, Mut)` arm.
			(&TyKind::Mut(p), &TyKind::Mut(a)) => self.unify_arg(p, a, is_int_literal, span),
			// Non-`mut` param, `mut U` arg: peel just the argument — dropping mutability
			// is always allowed. Never the reverse (a `mut` param can't be satisfied by a
			// plain arg): that falls to `unify` below, which mismatches correctly.
			(_, &TyKind::Mut(a)) => self.unify_arg(param, a, is_int_literal, span),
			_ => self.unify(param, arg, span),
		}
	}

	/// Trial version of [`Checker::unify_arg`] for overload selection (non-emitting).
	/// On success also reports whether the match required numeric widening (`int`
	/// literal → `float`/`uint`, or `uint` → `int`) rather than exact-type unification.
	/// Overload phase 2 uses this to prefer an exact-type
	/// argument match over a widened one, so a same-type operator impl
	/// (`Plus<Other = int> for int`) beats a cross-type sibling
	/// (`Plus<Other = uint> for int`) for an `int`-literal argument instead of the two
	/// tying into an `AmbiguousCall`. Returns `None` on unification failure, `Some(true)`
	/// when at least one widening step was taken, `Some(false)` for an exact match.
	pub(crate) fn try_unify_arg_widened(
		&mut self,
		param: Ty,
		arg: Ty,
		is_int_literal: bool,
	) -> Option<bool> {
		if is_int_literal && self.int_literal_fits_param(param, arg) {
			return Some(true);
		}
		if self.uint_fits_int(param, arg) {
			return Some(true);
		}
		let param_r = self.shallow_resolve(param);
		let arg_r = self.shallow_resolve(arg);
		match (self.interner.kind(param_r), self.interner.kind(arg_r)) {
			(&TyKind::Mut(p), &TyKind::Mut(a)) => self.try_unify_arg_widened(p, a, is_int_literal),
			(_, &TyKind::Mut(a)) => self.try_unify_arg_widened(param, a, is_int_literal),
			_ => self.try_unify(param, arg).then_some(false),
		}
	}

	/// Structural unification (Hindley-Milner): make two types equal by walking them
	/// together, binding inference variables through union-find, and applying the
	/// occurs-check to reject infinite types. On conflict, emits a type mismatch and
	/// returns, allowing checking to continue.
	pub(crate) fn unify(&mut self, a: Ty, b: Ty, span: Span) {
		let a = self.shallow_resolve(a);
		let b = self.shallow_resolve(b);
		if a == b {
			return;
		}
		let ka = self.interner.kind(a).clone();
		let kb = self.interner.kind(b).clone();
		match (ka, kb) {
			// A prior error already produced a diagnostic; don't cascade.
			(TyKind::Error, _) | (_, TyKind::Error) => {}

			(TyKind::Infer(va), TyKind::Infer(vb)) => self.table.union_var(va, vb),
			(TyKind::Infer(v), _) => self.bind_var(v, b, span),
			(_, TyKind::Infer(v)) => self.bind_var(v, a, span),

			(TyKind::List(x), TyKind::List(y)) => self.unify(x, y, span),
			(TyKind::Map(k1, v1), TyKind::Map(k2, v2)) => {
				self.unify(k1, k2, span);
				self.unify(v1, v2, span);
			}
			(TyKind::Tuple(xs), TyKind::Tuple(ys)) => {
				if xs.len() != ys.len() {
					self.mismatch(a, b, span);
					return;
				}
				for (x, y) in xs.iter().zip(&ys) {
					self.unify(*x, *y, span);
				}
			}
			(
				TyKind::Fn {
					params: p1,
					ret: r1,
				},
				TyKind::Fn {
					params: p2,
					ret: r2,
				},
			) => {
				if p1.len() != p2.len() {
					self.mismatch(a, b, span);
					return;
				}
				for (x, y) in p1.iter().zip(&p2) {
					self.unify(*x, *y, span);
				}
				self.unify(r1, r2, span);
			}
			(TyKind::Adt(d1, a1), TyKind::Adt(d2, a2)) if d1 == d2 => {
				if a1.positional.len() == a2.positional.len() {
					for (x, y) in a1.positional.iter().zip(&a2.positional) {
						self.unify(*x, *y, span);
					}
				}
				for (name, x) in &a1.named {
					if let Some((_, y)) = a2.named.iter().find(|(m, _)| m == name) {
						self.unify(*x, *y, span);
					}
				}
			}
			(TyKind::Mut(x), TyKind::Mut(y)) => self.unify(x, y, span),

			_ => self.mismatch(a, b, span),
		}
	}

	/// Trial unification (Hindley-Milner): like [`Self::unify`] but silent (no diagnostic)
	/// and returns success/failure. Rejects on occurs-check failure. Used for overload
	/// and impl resolution; caller wraps in union-find snapshots to roll back bindings.
	pub(crate) fn try_unify(&mut self, a: Ty, b: Ty) -> bool {
		let a = self.shallow_resolve(a);
		let b = self.shallow_resolve(b);
		if a == b {
			return true;
		}
		let ka = self.interner.kind(a).clone();
		let kb = self.interner.kind(b).clone();
		match (ka, kb) {
			(TyKind::Error, _) | (_, TyKind::Error) => true,
			(TyKind::Infer(va), TyKind::Infer(vb)) => {
				self.table.union_var(va, vb);
				true
			}
			(TyKind::Infer(v), _) => self.try_bind(v, b),
			(_, TyKind::Infer(v)) => self.try_bind(v, a),
			(TyKind::List(x), TyKind::List(y)) => self.try_unify(x, y),
			(TyKind::Map(k1, v1), TyKind::Map(k2, v2)) => {
				self.try_unify(k1, k2) && self.try_unify(v1, v2)
			}
			(TyKind::Tuple(xs), TyKind::Tuple(ys)) => {
				xs.len() == ys.len() && xs.iter().zip(&ys).all(|(&x, &y)| self.try_unify(x, y))
			}
			(
				TyKind::Fn {
					params: p1,
					ret: r1,
				},
				TyKind::Fn {
					params: p2,
					ret: r2,
				},
			) => {
				p1.len() == p2.len()
					&& p1.iter().zip(&p2).all(|(&x, &y)| self.try_unify(x, y))
					&& self.try_unify(r1, r2)
			}
			(TyKind::Adt(d1, a1), TyKind::Adt(d2, a2)) if d1 == d2 => {
				if a1.positional.len() != a2.positional.len() {
					return false;
				}
				for (&x, &y) in a1.positional.iter().zip(&a2.positional) {
					if !self.try_unify(x, y) {
						return false;
					}
				}
				for (name, x) in &a1.named {
					let mut matched = None;
					for (m, y) in &a2.named {
						if m == name {
							matched = Some(*y);
							break;
						}
					}
					match matched {
						Some(y) if self.try_unify(*x, y) => {}
						_ => return false,
					}
				}
				true
			}
			(TyKind::Mut(x), TyKind::Mut(y)) => self.try_unify(x, y),
			_ => false,
		}
	}

	fn try_bind(&mut self, var: crate::ids::InferVar, ty: Ty) -> bool {
		if crate::ty::fold::occurs(&self.interner, var, ty) {
			return false;
		}
		self.table.assign(var, ty);
		true
	}

	/// Subtyping treats `never` as a subtype of everything, lets `error` absorb,
	/// `mut T` is (one-way) assignable to `T`, `uint` is safely assignable to `int`,
	/// and everything else is invariant unification. (Covariance for containers and
	/// functions is a later refinement.)
	pub(crate) fn subtype(&mut self, sub: Ty, sup: Ty, span: Span) {
		let sub = self.shallow_resolve(sub);
		let sup = self.shallow_resolve(sup);
		if let TyKind::Param(idx) = self.interner.kind(sup)
			&& let Some(bounds) = self.synthetic_bound_details.get(idx).cloned()
		{
			for bound in bounds {
				if !self.holds(sub, bound.interface, &bound.args, 0) {
					let ty = self.display(sub);
					let interface = self.defs.data(bound.interface).name.clone();
					self.emit(span, TypeError::BoundNotSatisfied { ty, interface });
				}
			}
			return;
		}
		match (self.interner.kind(sub), self.interner.kind(sup)) {
			(TyKind::Never, _) | (TyKind::Error, _) | (_, TyKind::Error) => {}
			(TyKind::UInt, TyKind::Int) => {}
			// `mut T <: mut U` iff `T <: U` (inner variance carries through).
			(&TyKind::Mut(a), &TyKind::Mut(b)) => self.subtype(a, b, span),
			// `mut T <: T`, one-way: dropping mutability is always allowed. Never
			// the reverse — that falls through to `unify`, which mismatches since
			// only `unify`'s `(Mut, Mut)` arm matches on `Mut`.
			(&TyKind::Mut(a), _) => self.subtype(a, sup, span),
			_ => self.unify(sub, sup, span),
		}
	}

	/// If `expected` is (shallow-resolved to) `mut T` and `expr` is a freshly-
	/// constructed `#{…}`/`#[…]` collection literal, check `expr` directly against
	/// the peeled `T` and report success — a collection literal is a uniquely-owned
	/// temporary with no other aliases, so it may stand in for `mut T` the same way
	/// an explicit `mut`-annotated `let`'s initializer already can
	/// (`check_let_statement`'s own `strip_mut(declared)` peel). This is narrower
	/// than that: it keys off the EXPRESSION being a literal, not off `expected`
	/// alone, so a NAMED binding (whose expression is `ExprKind::Identifier`, never
	/// matched here) still falls through to the ordinary `infer` + `subtype` path
	/// and correctly rejects a plain-typed named binding against a `mut` target —
	/// `mut T <: T` stays one-way; this only ever widens on the LITERAL side, never
	/// the named-binding side. Returns `false` (leaving the caller's ordinary path
	/// untouched) whenever `expected` isn't `mut` or `expr` isn't a literal.
	pub(crate) fn try_coerce_owned_literal_to_mut(&mut self, expr: &Expr, expected: Ty) -> bool {
		if !matches!(expr.kind, ExprKind::Map(_) | ExprKind::List(_)) {
			return false;
		}
		let expected_r = self.shallow_resolve(expected);
		let TyKind::Mut(inner) = self.interner.kind(expected_r) else {
			return false;
		};
		let inner = *inner;
		self.check(expr, inner);
		true
	}

	/// Report a type mismatch. The dominant caller is `subtype(got, expected)` →
	/// `unify(got, expected)`, so `a` reads as "found" and `b` as "expected".
	fn mismatch(&mut self, a: Ty, b: Ty, span: Span) {
		let found = self.display(a);
		let expected = self.display(b);
		self.emit(span, TypeError::MismatchedTypes { expected, found });
	}
}
