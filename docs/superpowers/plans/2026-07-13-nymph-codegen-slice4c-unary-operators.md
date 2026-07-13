# Slice 4C-a (Unary Operator Overloading Dispatch) Implementation Plan

> **For agentic workers:** This slice is executed by a single dynamic Workflow
> (investigate → implement TDD → review → adversarial refute → fix loop), per
> the standing per-feature-workflow directive. The controller commits and
> updates the ledger.

**Goal:** `-v`, `!v`, and `~v` on user types dispatch to their `negate`/`not`/
`bit_not` impl methods in emitted JS instead of silently emitting native JS
unary operators (the KNOWN SILENT GAP recorded at the end of Slice 4B).

**Architecture:** Mirror Slice 4B's binary-operator mechanism exactly: the
checker records a `Resolution` on the `PrefixOp` node; lowering dispatches on
it. No new HIR nodes, no codegen changes, no new annotation types.

## Global Constraints

- Codegen stays type-free; every dispatch decision is baked into HIR by lowering.
- Deferred features panic loudly in lowering (never silent miscompiles).
- Rust: nightly toolchain (`cargo +nightly`), hard tabs, 2-space width, clippy clean.
- Subagents do not commit; the controller commits with jj scoped commits.

## Current state (surveyed 2026-07-13, post-ef2feb27)

- `infer_prefix` (crates/nymph-sema/src/infer_expr.rs:897-933) already routes
  non-primitive operands through `dispatch_operator(operand, "not"|"negate"|"bit_not", &[], span)`
  but **discards the returned `DispatchKind`** (the known gap is even
  documented at infer_expr.rs:1191-1193).
- `BoolNot` special case: a primitive **or still-unresolved `Infer`** operand
  is unified with `boolean` (`!` defaults to boolean). Only concrete ADTs
  dispatch to `Not`. This semantic must be preserved.
- Lowering (`crates/nymph-sema/src/lower_hir.rs:266`) lowers `PrefixOp`
  unconditionally to `HirExpr::Unary` via `lower_prefix` (:746) — the silent
  miscompile. Note `lower_prefix` currently maps only `Negate`/`BoolNot`;
  verify what `BitNot` does (missing arm? `UnOp` has `Neg`/`Not`; emit.rs:594
  maps them to JS unary `-`/`!`).
- The Slice 4B closeout added `Checker::pending_operators`
  (crates/nymph-sema/src/check.rs:101, `PendingOperatorKind` :33) drained
  per-body by `finalize_pending_operators`; `infer_inherent_return` truncates
  the queue on its discarded trial run. Unary `-`/`~` on an unresolved operand
  needs the same treatment (verify what `dispatch_operator` does today with an
  `Infer` receiver on the unary path — likely mis-diagnoses or ICEs).
- stdlib: `interface Negate<Output> { func negate(): Output }` (ops/mod.nym:25),
  `interface Not<Output> { func not(): Output }` (:61), `BitNot` analog;
  primitives have inline `negate`/`not` impls (:211, :215, :270).

## Decisions

- **U1 (shape):** Reuse `Resolution { method: EcoString, dispatch: DispatchKind }`,
  recorded on the `PrefixOp` node id via the existing `record_resolution`
  precondition pattern (type recorded first in `infer`'s interception, then
  resolution layered on — exactly like `BinaryOp`/`AssignOp`).
- **U2 (recording table):**
  - primitive operand → `BuiltinEager` (method name `negate`/`not`/`bit_not`).
  - `BoolNot` on primitive-or-`Infer` operand → `BuiltinEager` (preserve the
    unify-with-boolean semantics; no pending queue for `!`).
  - concrete user ADT → `UserImpl` (direct impl method) or
    `UserImplDefaultMethod` (interface default body).
  - generic parameter operand → through `dispatch_operator` (bound provides
    the method → `UserImplDefaultMethod`; no bound → `NotImplemented`
    diagnostic). Mirrors the 4B closeout's binary behavior.
  - unresolved `Infer` operand under `Negate`/`BitNot` → new
    `PendingOperatorKind` variant for prefix ops, finalized per body; still
    unbound at body end → `CannotInferOperandType` diagnostic.
- **U3 (lowering):** the `PrefixOp` arm reads `resolution_of(expr.id)`:
  - `BuiltinEager` → `HirExpr::Unary` via `lower_prefix` (unchanged emit).
  - `UserImpl` → `HirExpr::Call { callee: Field { recv: <lowered operand>, name: method }, args: vec![] }`.
  - `UserImplDefaultMethod` → panic (same message family as `lower_binary`).
  - `None` → panic ("no operator resolution recorded for prefix op {op:?}").
  - `BuiltinShortCircuit` is unreachable for unary — panic if seen.
- **U4 (out of scope, unchanged):** `PostfixOp` (`?`/`!` error propagation —
  Milestone B); interface dynamic dispatch; enum unary impls (covered by the
  existing non-struct impl-collection panics); user `==`/`!=` dispatch;
  stdlib linkage.

## Tasks

### Task 1: Checker records `Resolution` for prefix ops
Files: crates/nymph-sema/src/infer_expr.rs (infer_prefix + infer's interception
+ pending queue), crates/nymph-sema/src/check.rs (PendingOperatorKind variant),
tests: crates/nymph-sema/tests/operator_resolutions.rs.
Cases: `-x` on int → BuiltinEager/"negate"; `!b` on boolean → BuiltinEager/"not";
`-v` on Vec2 with `impl Negate<Output = Vec2> for Vec2` → UserImpl/"negate";
interface-default unary (construct one, e.g. an interface with a defaulted
`negate`) → UserImplDefaultMethod; `-xs[0]` with `let xs = #[]` pinned later →
late-resolved BuiltinEager (declaration-order-independent); unbounded generic
`-t` → NotImplemented diagnostic; unresolved-at-body-end →
CannotInferOperandType.

### Task 2: Lowering dispatches `PrefixOp` on the recorded resolution
Files: crates/nymph-sema/src/lower_hir.rs (reuse/extend the shared
`lower_operator` helper from the 4B closeout if it fits), tests:
crates/nymph-sema/tests/lower_hir.rs.
Cases: `-v` (Negate impl) → Call{Field{recv,"negate"},[]}; `-x` int stays
Unary{Neg}; should_panic "default method" for the interface-default case;
None-panic pinned via stripped annotations (mirror the existing test).

### Task 3: Node end-to-end
Files: crates/nymph-codegen/tests/run_node.rs (JS driver strings:
`new Vec2({ x: 1, y: 2 })`).
Cases: `-Vec2` returns componentwise negation via `.negate()`; `!boolean` and
`-int`/`-float` stay native; unary inside a method body (e.g. `negate` uses
`this.x * -1`... keep native inner ops) while the outer `-v` dispatches.

### Task 4 (controller): commit, ledger, record review outcome.
