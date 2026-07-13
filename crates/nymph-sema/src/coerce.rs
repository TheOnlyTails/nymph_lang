//! Hindley-Milner-style structural unification and (Milestone-A) subtyping.
//!
//! `unify` implements the symmetric structural unifier via occurs-check and
//! inference-variable binding through a union-find table: walks two types,
//! binds inference variables (rejecting occurrence), and reports precise mismatches.
//! The occurs-check prevents infinite types; `try_unify` implements trial unification
//! for overload resolution (bindings kept iff the trial succeeds).

use crate::errors::TypeError;
use nymph_ast::Span;

use crate::check::Checker;
use crate::ty::{Ty, TyKind};

impl Checker<'_> {
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
	/// argument widen to a `float`/`uint` parameter instead of clashing.
	pub(crate) fn unify_arg(&mut self, param: Ty, arg: Ty, is_int_literal: bool, span: Span) {
		if is_int_literal && self.int_literal_fits_param(param, arg) {
			return;
		}
		self.unify(param, arg, span);
	}

	/// Trial version of [`Checker::unify_arg`] for overload selection (non-emitting).
	pub(crate) fn try_unify_arg(&mut self, param: Ty, arg: Ty, is_int_literal: bool) -> bool {
		if is_int_literal && self.int_literal_fits_param(param, arg) {
			return true;
		}
		self.try_unify(param, arg)
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

	/// Milestone-A subtyping: `never` is a subtype of everything, `error` absorbs,
	/// and everything else is invariant unification. (Covariance for containers and
	/// functions is a later refinement.)
	pub(crate) fn subtype(&mut self, sub: Ty, sup: Ty, span: Span) {
		let sub = self.shallow_resolve(sub);
		let sup = self.shallow_resolve(sup);
		match (self.interner.kind(sub), self.interner.kind(sup)) {
			(TyKind::Never, _) | (TyKind::Error, _) | (_, TyKind::Error) => {}
			_ => self.unify(sub, sup, span),
		}
	}

	/// Report a type mismatch. The dominant caller is `subtype(got, expected)` →
	/// `unify(got, expected)`, so `a` reads as "found" and `b` as "expected".
	fn mismatch(&mut self, a: Ty, b: Ty, span: Span) {
		let found = self.display(a);
		let expected = self.display(b);
		self.emit(span, TypeError::MismatchedTypes { expected, found });
	}
}
