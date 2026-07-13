# Slice 4G (Call-Site Bound Enforcement) Implementation Plan

> **For agentic workers:** Executed by a single dynamic Workflow (investigate →
> implement TDD → review → adversarial refute → fix loop) in the MAIN working
> copy. The controller commits and updates the ledger.

**Goal:** Close the KNOWN SOUNDNESS HOLE documented in the Slice 4F ledger
entry: a generic function call whose argument does NOT implement the declared
bound — `func measure<T: Area>(shape: T): int = shape.area()` called as
`measure(3)`, or the `impl Trait` spelling — currently type-checks with zero
diagnostics and crashes at JS runtime (`shape.area is not a function`). After
this slice, both spellings diagnose at the call site.

**Architecture:** Checker-only. Function signatures capture their generics'
bounds at signature-lowering time; call-site instantiation attaches a deferred
obligation (fresh var × bound) for every bound on every minted var (declared
generics AND impl-Trait synthetics via the existing `synthetic_bounds`);
obligations drain per body alongside `finalize_pending_operators`, checking
`holds` once the var has resolved. Mirrors the pending-operators deferral
pattern exactly (including the trial-inference truncation in
`infer_inherent_return` and the module-end drained-empty debug_assert).

## Global Constraints

- Codegen stays type-free; deferred features panic loudly in lowering.
- Rust: `cargo +nightly`, hard tabs, 2-space width, clippy clean.
- Subagents do not commit; the controller commits with jj scoped commits.
- New `TypeError` variants go at the END of the enum (error-code stability).

## Current state (from the 4F investigation, at 40d68563)

- `FuncSig` (def.rs:245) stores generic NAMES only — no bounds. Declared
  bounds exist in the AST generics list and are recorded into the transient
  per-body `param_bounds` only for body-side method resolution.
- `fresh_subst` (check.rs:421) mints unconstrained vars; nothing at any call
  site checks bounds. `holds`/`constraints_hold` (solve.rs:58/:105) exist but
  are eager (need a resolvable type) — used for impl where-clauses and casts.
- Synthetic impl-Trait params: `synthetic_bounds: FxHashMap<ParamIdx, Vec<DefId>>`
  (check.rs:82); 4F freshens them in `fn_type_of` (param position only).
- Deferral prior art: `pending_operators` (check.rs:115) drained per body
  (`finalize_pending_operators`), truncated on `infer_inherent_return`'s
  discarded trial, debug_assert-empty at module end.
- Interface bounds carry ARGUMENTS (`Comparable<Other = T>`); `iface.rs::Bound
  { ty, interface, args }` is the existing shape; `holds` takes bindings.

## Decisions

- **AA1 (capture bounds in FuncSig):** signature lowering records, per declared
  generic, its bounds — interface DefId + argument bindings, reusing/mirroring
  `iface.rs::Bound` (args lowered in the signature's own param index space so
  they can be substituted by the same call-site subst map). If full argument
  fidelity is disproportionate, the investigator reports what's feasible;
  bare-interface bounds (`T: Area`) are the required minimum, argful bounds
  (`T: Comparable<Other = T>`) SHOULD work by substituting the call-site subst
  into the bound's args before deferring.
- **AA2 (deferred obligations):** new `pending_bounds: Vec<(Span, Ty, BoundLike)>`
  on `Checker`, pushed by `fn_type_of` at instantiation (one per bound per
  minted var — declared generics from AA1, synthetics from `synthetic_bounds`),
  drained in the same per-body finalization pass as pending operators:
  shallow-resolve the var; concrete type → `holds` → diagnostic if unsatisfied;
  `Error` type → skip silently (other diagnostics exist); still-`Infer` → skip
  (the var virtually always unifies with the concrete argument at the call —
  the investigator probes whether a still-unbound case is reachable from a
  zero-diagnostic program; if it is, note it in the report rather than
  inventing a diagnostic). Trial-inference truncation mirrored.
- **AA3 (diagnostic):** a new `TypeError` variant at the enum end (e.g.
  `BoundNotSatisfied { ty, interface }`, message style consistent with
  `NotImplemented`), emitted at the call-site span. Same variant for both
  spellings.
- **AA4 (scope):** PLAIN FUNCTION CALLS ONLY (including function-as-value
  references — same `fn_type_of` path). Method call sites (`commit_method`,
  `resolve_inherent`, etc.) keep today's behavior: their impl selection already
  runs `constraints_hold` eagerly, and method own-generic bounds (if
  expressible) are out of scope — the investigator documents the method
  landscape so the ledger records what remains. Nothing may REGRESS methods.
- **AA5 (acceptance):** solve.rs reject tests (`measure<T: Area>(3)` → the new
  diagnostic; same for `impl Trait` spelling; argful bound violated →
  diagnostic if AA1 full fidelity landed); accept tests (implementing args stay
  zero-diagnostic, both spellings; generic-to-generic forwarding
  `func outer<T: Area>(x: T): int = measure(x)` stays clean — the forwarded
  var's bound is satisfied by the caller's param bound: the investigator
  determines how `holds` sees a rigid Param with a recorded `param_bounds`
  entry, since body-side param_bounds ARE live during the body's own drain).
  The full 347-test suite (incl. `stdlib_typechecks_cleanly` — heavy generic
  code) is the regression net.

## Tasks

### Task 1: capture + defer + drain (AA1–AA3)
Files: crates/nymph-sema/src/{def,lower,check,infer_expr,solve,errors}.rs
(investigator narrows); tests: crates/nymph-sema/tests/solve.rs.

### Task 2: acceptance + interaction pins (AA5)
Files: tests only (solve.rs; a run_node.rs or golden_programs.rs positive case
confirming a bound-satisfying program still runs; corpus stays 0-ignored).

### Task 3 (controller): commit, ledger, record review outcome.
