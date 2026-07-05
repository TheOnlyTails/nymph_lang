# Interface Solver Progress

## Goals

- Make interface members directly accessible on implementing values.
- Detect ambiguous interface member lookup and report actionable diagnostics.
- Treat intersections as logical AND everywhere in the type-checker.
- Preserve enough progress notes to explain each implementation phase.

## Progress

- [x] Audited the existing type-checker, interface representation, and impl handling.
- [x] Identified the current intersection bug: the checker was treating target intersections like OR.
- [x] Add regression tests for direct interface member lookup, interface extensions, ambiguity, and intersections.
- [x] Replace the shallow interface lookup with a shared solver.
- [x] Register top-level impls and interface extensions in the typing context.
- [x] Validate interface impl compatibility and reject conflicting impls.
- [x] Upgrade diagnostics to show candidate interfaces and conflicting impl sites.
- [x] Run formatting and targeted tests.

## Notes

- The transpiler/emitter follow-up is intentionally deferred for a later pass.
- Explicit interface disambiguation syntax is also deferred until the language design is approved.
- Direct member access now always falls back to the interface solver, so primitive receivers can resolve trait members like `1.to_string()` through registered impls.
