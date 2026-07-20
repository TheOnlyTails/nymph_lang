//! Desugaring for anonymous closure parameters (`$`, `$0`, `$1`, …).
//!
//! An expression containing `$N` implicitly becomes a closure. The hard part is
//! that WHICH enclosing expression becomes that closure's body — the
//! "boundary" — is *type-directed*: it is the SMALLEST enclosing expression such
//! that the resulting closure type-checks in its position (see the module-level
//! examples below). A purely syntactic pre-pass cannot pick the boundary on its
//! own (it has no types), so the search below runs interleaved with the checker
//! itself, via a trial-and-rollback loop identical in spirit to
//! `members::infer_inherent_return`'s trial pattern.
//!
//! ```text
//! f($1, $0)           => (p0, p1) => f(p1, p0)      -- both $ are direct call args
//! xs.map($0 + 1)       => xs.map((p0) => p0 + 1)     -- boundary is `$0 + 1` itself
//! .filter($ % 2 == 0)  => .filter((p0) => p0 % 2 == 0)
//!   -- KEY CASE: `$ % 2` alone would give `((p0) => p0 % 2) == 0`, a closure
//!   -- compared to an int — ill-typed — so the boundary EXPANDS outward to
//!   -- `$ % 2 == 0`, which checks as `(T) -> boolean`.
//! ```
//!
//! Once a boundary is picked, it desugars into an ordinary closure and reuses
//! every existing closure code path start to finish: [`Checker::form_anon_closure`]
//! mirrors `infer_closure`/`check_closure`'s param-binding shape (minus the
//! `generics_stack` frame, exactly like a real closure never declares generics
//! either), and `lower_hir::Lowerer::lower_anon_closure` mirrors `lower_closure`
//! byte for byte. The one new piece of machinery either side needs is a channel
//! from the checker's boundary DECISION back to lowering (which re-walks the
//! original AST and has no type/solver access of its own): committed boundaries
//! are recorded on [`crate::annotate::Annotations`] (`record_anon_boundary`),
//! keyed by [`NodeId`] — see that type's doc comment.
//!
//! ## Boundary search
//!
//! [`Checker::resolve_anon`] is called at every "closure slot" — a free-function
//! call argument (`check_call_arg`), a `let` initializer (`check_let_body`), a
//! `return` operand, a constructor field (`check_ctor_args`), and an explicit
//! closure's own body (`infer_closure`/`check_closure`, itself a hard boundary
//! `$N` cannot escape past) — immediately before that site's ordinary
//! `check`/`infer` call. It scans the slot for `$N` occurrences (bailing out
//! immediately, the overwhelming common case, if there are none) and, for each,
//! computes its candidate boundary chain: every ancestor expression from its own
//! immediate parent up to (and always ending at) the slot itself.
//!
//! Occurrences whose smallest candidate is the same node start out grouped into
//! one hypothesized closure (`f($1, $0)`: both `$`'s immediate parent is the
//! `Call`, one group from round zero). The search installs the current
//! round's hypothesis, trial-checks the WHOLE slot, and rolls every side effect
//! back (diagnostics, unification bindings, the `pending_operators`/
//! `pending_bounds`/`pending_bound_arg_mut` queues) whether it passed or not —
//! win or lose, this is only ever a trial; the caller's own subsequent, REAL
//! `check`/`infer` call is what actually produces lasting diagnostics and
//! annotations. On failure, the smallest (lowest-level) hypothesis still able to
//! grow expands by one ancestor step and the round repeats; on total
//! exhaustion (every occurrence has reached the slot itself), that widest
//! hypothesis is committed anyway so the real evaluation surfaces its natural
//! `subtype` mismatch loudly instead of silently.
//!
//! Multi-boundary expansion order (which hypothesis to widen when SEVERAL are
//! simultaneously ill-typed) is a heuristic — smallest-level first — rather
//! than a precise "which one is actually in a function-rejecting position"
//! analysis; every case in this slice's test suite has at most one boundary
//! that ever needs to expand, so this doesn't bite here, but a contrived
//! multi-boundary program could in principle expand the "wrong" one first.

use ecow::EcoString;
use nymph_ast::{
	NodeId,
	expr::{Expr, ExprKind, ListItem, MapEntry, RangeKind, Statement, StringPart},
};
use rustc_hash::FxHashMap;

use crate::check::Checker;
use crate::ty::Ty;

/// The synthesized JS-visible parameter name for anonymous-closure param index
/// `i`. A leading-`$` identifier is guaranteed collision-free with any
/// user-written Nymph identifier — the lexer's identifier rule (`ident()`,
/// `lexer.rs`) never admits a leading `$` (that's the `AnonymousParam` token
/// itself) — and `Lowerer::declare`'s Y2 shadow-rename already relies on that
/// same invariant for its own `name$1`/`name$2` suffixes.
pub(crate) fn anon_param_name(i: u8) -> EcoString {
	format!("anon${i}").into()
}

/// One `$N` occurrence found while scanning a closure slot for anonymous
/// params: its param index, its own `NodeId` (for [`Checker::anon_consumed`]),
/// and its candidate boundary chain — every ancestor expression from the
/// SMALLEST (its own immediate parent) to the WIDEST (the slot itself, always
/// the last entry).
struct Occurrence<'e> {
	idx: u8,
	node: NodeId,
	candidates: Vec<&'e Expr>,
}

impl<'m> Checker<'m> {
	/// Scan `slot` for `$N` occurrences and, if any exist, run the
	/// type-directed boundary search and permanently commit the winning
	/// boundary set via [`crate::annotate::Annotations::record_anon_boundary`]
	/// — see the module doc comment for the full algorithm. A no-op when
	/// `slot` contains no (unconsumed) anonymous param at all, which is the
	/// overwhelming common case: every call site pays only a cheap tree walk.
	///
	/// Must be called BEFORE the caller's own `self.check(slot, expected)` /
	/// `self.infer(slot)` — that unchanged call is what actually performs the
	/// real (non-trial) evaluation, now seeing whatever boundaries this
	/// committed.
	pub(crate) fn resolve_anon(&mut self, slot: &Expr, expected: Option<Ty>) {
		let mut occurrences = Vec::new();
		let mut path = Vec::new();
		self.collect_anon(slot, &mut path, &mut occurrences);
		if occurrences.is_empty() {
			return;
		}
		// Every occurrence found here is now OWNED by this scan — mark it
		// consumed immediately so a NESTED slot reached while trial- or
		// really-evaluating a committed boundary (e.g. `check_call_arg` on an
		// argument that turns out to just be a bare, already-claimed `$0`)
		// never rediscovers it as an independent, spurious boundary of its own.
		for occ in &occurrences {
			self.anon_consumed.insert(occ.node);
		}

		let mut levels = vec![0usize; occurrences.len()];
		loop {
			let groups = group_by_current_boundary(&occurrences, &levels);
			if self.trial_boundary_checks(slot, expected, &groups) {
				return; // `groups` is the winning, now-permanent hypothesis.
			}
			for &id in groups.keys() {
				self.annotations.remove_anon_boundary(id);
			}

			// Expand the smallest (lowest-level) occurrence still able to grow,
			// by one ancestor step — along with every OTHER occurrence
			// currently sharing that same boundary node (they move together;
			// see the module doc comment on grouping).
			let Some(i) = (0..occurrences.len())
				.filter(|&i| levels[i] + 1 < occurrences[i].candidates.len())
				.min_by_key(|&i| levels[i])
			else {
				// Every occurrence has reached the slot itself and the
				// widest hypothesis STILL doesn't check — commit it for real
				// anyway, so the caller's real check/infer surfaces the
				// natural, loud `subtype` mismatch at the closure rather than
				// silently falling through to `AnonymousParamUnsupported`.
				for (&id, &arity) in &groups {
					self.annotations.record_anon_boundary(id, arity);
				}
				return;
			};
			let target = occurrences[i].candidates[levels[i]].id;
			for (j, occ) in occurrences.iter().enumerate() {
				if levels[j] < occ.candidates.len() && occ.candidates[levels[j]].id == target {
					levels[j] += 1;
				}
			}
		}
	}

	/// Install one round's hypothesized `boundary -> arity` map, trial-check
	/// the whole slot against `expected` (mirroring exactly what the caller's
	/// own subsequent real `check`/`infer` call does), and roll back every
	/// side effect — diagnostics, unification bindings, and the three
	/// per-body deferred-obligation queues — regardless of outcome. Only the
	/// boundary map itself survives a passing trial (the caller removes it on
	/// failure). Mirrors `members::infer_inherent_return`'s trial pattern.
	fn trial_boundary_checks(
		&mut self,
		slot: &Expr,
		expected: Option<Ty>,
		boundaries: &FxHashMap<NodeId, u8>,
	) -> bool {
		let diag_mark = self.diags.len();
		let pending_op_mark = self.pending_operators.len();
		let pending_bound_mark = self.pending_bounds.len();
		let pending_mut_snapshot = self.pending_bound_arg_mut.clone();
		let table_snapshot = self.table.snapshot();

		for (&id, &arity) in boundaries {
			self.annotations.record_anon_boundary(id, arity);
		}
		match expected {
			Some(ty) => self.check(slot, ty),
			None => {
				self.infer(slot);
			}
		}
		let ok = self.diags.len() == diag_mark;

		self.diags.truncate(diag_mark);
		self.table.rollback_to(table_snapshot);
		self.pending_operators.truncate(pending_op_mark);
		self.pending_bounds.truncate(pending_bound_mark);
		self.pending_bound_arg_mut = pending_mut_snapshot;

		ok
	}

	/// Form the closure hypothesized/committed at `expr` (a node id
	/// [`crate::annotate::Annotations::anon_boundary_arity`] maps to `arity`):
	/// push a fresh param-type frame onto [`Checker::anon_ctx`], dispatch
	/// `expr`'s OWN kind through it (exactly the shape `$N` reads back out of,
	/// via `ExprKind::AnonymousParam`'s arm in `infer_dispatch`), and return the
	/// resulting `(fresh params) -> body_ty` function type — mirroring
	/// `infer_closure`'s shape (minus a `generics_stack` frame: an anonymous
	/// closure never declares generics, exactly like an explicit one never
	/// does either).
	///
	/// Always runs the body in INFER mode regardless of whether the boundary
	/// is being checked or inferred: confirmed by probe that an unannotated
	/// param (a fresh `Infer` var) resolves cleanly through ordinary operators
	/// in EITHER mode, so `check`'s caller subtyping the resulting `Fn` type
	/// against its own expected type afterward is enough to pin the params —
	/// no separate check-mode variant of this is needed.
	pub(crate) fn form_anon_closure(&mut self, expr: &Expr, arity: u8) -> Ty {
		let param_tys: Vec<Ty> = (0..arity).map(|_| self.fresh()).collect();
		self.anon_ctx.push(param_tys.clone());
		let body_ty = self.infer_dispatch(expr);
		self.anon_ctx.pop();
		self.interner.mk_fn(param_tys, body_ty)
	}

	/// Walk `expr`'s subtree collecting every (unconsumed) `$N` occurrence,
	/// alongside its candidate boundary chain. `path` accumulates the current
	/// ancestor chain (smallest-last) as the walk descends; an `AnonymousParam`
	/// leaf records `path` reversed (smallest-first) as its candidates, ending
	/// with `slot` — the walk's own root — always last. A nested EXPLICIT
	/// closure is never descended into: its body is its own hard boundary,
	/// scanned independently when `infer_closure`/`check_closure` later call
	/// `resolve_anon` on it directly.
	fn collect_anon<'e>(
		&self,
		expr: &'e Expr,
		path: &mut Vec<&'e Expr>,
		out: &mut Vec<Occurrence<'e>>,
	) {
		if let ExprKind::AnonymousParam(idx) = &expr.kind {
			if self.anon_consumed.contains(&expr.id) {
				return;
			}
			let candidates = if path.is_empty() {
				vec![expr]
			} else {
				path.iter().rev().copied().collect()
			};
			out.push(Occurrence {
				idx: idx.unwrap_or(0),
				node: expr.id,
				candidates,
			});
			return;
		}
		if matches!(expr.kind, ExprKind::Closure { .. }) {
			return;
		}
		path.push(expr);
		self.collect_anon_children(expr, path, out);
		path.pop();
	}

	/// The child-`Expr` half of [`Self::collect_anon`], split out only to keep
	/// that function's own short-circuits (the `AnonymousParam`/`Closure` leaf
	/// cases) from being buried inside a huge match arm.
	fn collect_anon_children<'e>(
		&self,
		expr: &'e Expr,
		path: &mut Vec<&'e Expr>,
		out: &mut Vec<Occurrence<'e>>,
	) {
		match &expr.kind {
			ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
				self.collect_anon(lhs, path, out);
				self.collect_anon(rhs, path, out);
			}
			ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
				self.collect_anon(value, path, out);
			}
			ExprKind::TypeOp { lhs, .. } | ExprKind::PatternOp { lhs, .. } => {
				self.collect_anon(lhs, path, out);
			}
			ExprKind::Call { func, args, .. } => {
				self.collect_anon(func, path, out);
				for a in args {
					self.collect_anon(&a.0.value, path, out);
				}
			}
			ExprKind::MemberAccess { parent, .. } => self.collect_anon(parent, path, out),
			ExprKind::IndexAccess { parent, index, .. } => {
				self.collect_anon(parent, path, out);
				self.collect_anon(index, path, out);
			}
			ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
				if let Some(v) = value {
					self.collect_anon(v, path, out);
				}
			}
			ExprKind::While {
				condition, body, ..
			} => {
				self.collect_anon(condition, path, out);
				self.collect_anon(body, path, out);
			}
			ExprKind::For { iterable, body, .. } => {
				self.collect_anon(iterable, path, out);
				self.collect_anon(body, path, out);
			}
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => {
				self.collect_anon(condition, path, out);
				self.collect_anon(then, path, out);
				if let Some(o) = otherwise {
					self.collect_anon(o, path, out);
				}
			}
			ExprKind::Match { value, arms } => {
				self.collect_anon(value, path, out);
				for arm in arms {
					if let Some(g) = &arm.guard {
						self.collect_anon(g, path, out);
					}
					self.collect_anon(&arm.body, path, out);
				}
			}
			ExprKind::Block { body, .. } => {
				for s in body {
					match &s.0 {
						Statement::Expr(e) => self.collect_anon(e, path, out),
						Statement::Let { value, .. } => self.collect_anon(value, path, out),
					}
				}
			}
			ExprKind::Grouped(inner) => self.collect_anon(inner, path, out),
			ExprKind::List(items) | ExprKind::Tuple(items) => {
				for i in items {
					match &i.0 {
						ListItem::Expr(e) | ListItem::Spread(e) => self.collect_anon(e, path, out),
					}
				}
			}
			ExprKind::Map(entries) => {
				for e in entries {
					match &e.0 {
						MapEntry::Entry(k, v) => {
							self.collect_anon(k, path, out);
							self.collect_anon(v, path, out);
						}
						MapEntry::Spread(e) => self.collect_anon(e, path, out),
					}
				}
			}
			ExprKind::Range(kind) => match kind {
				RangeKind::From(e) | RangeKind::To(e) | RangeKind::ToInclusive(e) => {
					self.collect_anon(e, path, out);
				}
				RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
					self.collect_anon(min, path, out);
					self.collect_anon(max, path, out);
				}
			},
			ExprKind::String(parts) => {
				for p in parts {
					if let StringPart::InterpolatedExpr(e) = &p.0 {
						self.collect_anon(e, path, out);
					}
				}
			}
			// Leaves: no child `Expr`s to scan.
			ExprKind::Int(_)
			| ExprKind::UInt(_)
			| ExprKind::Float(_)
			| ExprKind::Char(_)
			| ExprKind::Boolean(_)
			| ExprKind::Identifier(_)
			| ExprKind::Continue { .. }
			| ExprKind::This => {}
			// Already short-circuited by the caller, `collect_anon`, before it
			// ever calls this function.
			ExprKind::AnonymousParam(_) | ExprKind::Closure { .. } => unreachable!(
				"collect_anon short-circuits AnonymousParam/Closure before calling collect_anon_children"
			),
		}
	}
}

/// Group `occurrences` by their CURRENT hypothesized boundary (`candidates[level]`,
/// clamped defensively to the last entry) and compute each boundary's arity —
/// the max param index among the occurrences assigned to it, plus one. Shared
/// between installing a trial round and committing the final one.
fn group_by_current_boundary(
	occurrences: &[Occurrence],
	levels: &[usize],
) -> FxHashMap<NodeId, u8> {
	let mut arities: FxHashMap<NodeId, u8> = FxHashMap::default();
	for (occ, &level) in occurrences.iter().zip(levels) {
		let level = level.min(occ.candidates.len() - 1);
		let id = occ.candidates[level].id;
		let entry = arities.entry(id).or_insert(0);
		*entry = (*entry).max(occ.idx + 1);
	}
	arities
}
