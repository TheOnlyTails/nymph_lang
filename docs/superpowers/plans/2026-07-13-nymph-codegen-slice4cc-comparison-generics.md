# Slice 4C-c (Comparison/Equality/Logical Operators on Non-Concrete Operands) Implementation Plan

> **For agentic workers:** Executed by a single dynamic Workflow (investigate →
> implement TDD → review → adversarial refute → fix loop). The controller
> commits and updates the ledger.

**Goal:** Close the KNOWN SILENT GAP documented at the end of Slice 4C-b:
comparison (`<`, `<=`, `>`, `>=`), and possibly equality/logical, operators on
generic-parameter or still-unresolved operands record `BuiltinEager` and emit
native JS operators on objects. After this slice, every such path either
records the correct dispatch, defers to the per-body pending queue, produces a
checker diagnostic, or panics loudly in lowering — never silent native JS on
non-primitives.

**Architecture:** Bring the comparison arm of `infer_binary` up to parity with
the arithmetic arm (which already routes Param operands through
`dispatch_operator` and defers Infer operands to `pending_operators` — Slice
4B closeout + 4C-a). Equality and logical arms get explicit decisions and
pinned tests rather than incidental behavior.

## Global Constraints

- Codegen stays type-free; deferred features panic loudly in lowering.
- Rust: `cargo +nightly`, hard tabs, 2-space width, clippy clean.
- Subagents do not commit; the controller commits with jj scoped commits.

## Current state (from the 4C-b investigation, post-5b3a752d)

- Arithmetic/bitwise arm (infer_expr.rs ~1092-1150): Param → `dispatch_operator`
  (bound → `UserImplDefaultMethod` loud deferral; none → `NotImplemented`);
  Infer → pending queue (per-body finalization); ADT → dispatch. CORRECT.
- Comparison arm (~1162-1177): only `is_adt` routes to dispatch; `is_adt`
  excludes `Param` and `Infer`, so both fall to the primitive path and record
  `BuiltinEager` — probe-confirmed: `a < b` under `T: Comparable<Other = T>`
  records `Resolution { less_than, BuiltinEager }` with zero diagnostics →
  native JS `<` on objects. SILENT MISCOMPILE.
- Suspected same-family hazard (verify): late-pinned Infer comparison —
  `let xs = #[]` … `xs[0] < xs[0]` … later pinned to a user ADT — records
  BuiltinEager before the pin, never re-examined (comparisons don't use the
  pending queue).
- Equality arm (~1151-1161): ALWAYS BuiltinEager by design (native `===`;
  user equals dispatch is an accepted, documented deferral). For Param/Infer
  operands native `===` is CONSISTENT with the accepted ADT behavior — likely
  no change, but pin with tests and an explicit decision.
- Logical arm (~1184-1207): unifies operands with `boolean`. A rigid Param
  presumably fails unification → diagnostic (loud). Verify and pin.

## Decisions

- **W1 (comparison parity):** the comparison arm mirrors the arithmetic arm
  exactly: concrete primitive pair → `BuiltinEager`; ADT → `dispatch_operator`
  (existing); `Param` → `dispatch_operator` (bound → `UserImplDefaultMethod`,
  none → `NotImplemented` diagnostic); `Infer` → pending queue entry finalized
  per body (reuse `PendingOperatorKind::BinaryOp(op)` if the fallback resolver
  handles comparison methods, or extend minimally); still-unbound at body end →
  `CannotInferOperandType`. Method names via the existing `comparison_method`
  mapper.
- **W2 (equality):** stays `BuiltinEager` for ALL operand kinds — consistent
  with the accepted "==/!= is always native" design. Pin with a test
  (`a == b` under an unbounded generic records BuiltinEager and lowers to
  native `===`). Document in the ledger, not code churn.
- **W3 (logical):** verify `&&`/`||` on a Param operand produces a type
  diagnostic (cannot unify rigid Param with boolean). Pin with a test. If it
  somehow passes clean, route it to a diagnostic.
- **W4 (default bodies benefit):** with W1, `this < other` inside an interface
  default body (bounded-param `this`) now records `UserImplDefaultMethod` →
  loud lowering panic instead of silent native `<`. Pin with a lowering
  should_panic test.
- **W5 (out of scope):** actual dispatch for generic-bound operators
  (monomorphization/dynamic dispatch); user equals dispatch; `??`/`in`/`|>`;
  stdlib linkage.

## Tasks

### Task 1: Checker — comparison arm parity (W1) + pinned decisions (W2, W3)
Files: crates/nymph-sema/src/infer_expr.rs (comparison arm, fallback resolver
if it needs comparison-method awareness), tests:
crates/nymph-sema/tests/operator_resolutions.rs, tests/solve.rs (diagnostic
cases).
Cases: bounded-generic `<` → UserImplDefaultMethod; unbounded-generic `<` →
NotImplemented; late-pinned-to-int `<` → BuiltinEager (declaration-order
independent); late-pinned-to-ADT `<` → UserImpl (via the impl) or the correct
kind per what the pin reveals; still-unbound → CannotInferOperandType;
unbounded-generic `==` → BuiltinEager (pinned decision); Param `&&` →
diagnostic.

### Task 2: Lowering pins (W4)
Files: crates/nymph-sema/tests/lower_hir.rs.
Cases: bounded-generic `<` panics "default method"; `this < other` in an
interface default body panics; late-pinned ADT `<` lowers to
Call{Field{recv,"less_than"},[rhs]}; int `<` stays native Binary.

### Task 3: Node end-to-end
Files: crates/nymph-codegen/tests/run_node.rs.
Cases: late-pinned ADT comparison dispatches correctly at runtime; native
int/float comparison unchanged; `==` on user structs still native `===`
(reference semantics, pinned).

### Task 4 (controller): commit, ledger, record review outcome.
