# Slice 4F (impl-Trait Params at Call Sites) Implementation Plan

> **For agentic workers:** Executed by a single dynamic Workflow (investigate →
> implement TDD → review → adversarial refute → fix loop) in the MAIN working
> copy. The controller commits and updates the ledger.

**Goal:** Close golden-corpus finding 4 (the last `#[ignore]`d test in
`crates/nymph-compiler/tests/golden_programs.rs`): `func measure(shape: Area)`
(impl-Trait param sugar) is callable with a concrete argument
(`measure(square)`), behaving exactly like the explicit-generic spelling
`func measure<T: Area>(shape: T)`. Body-side resolution already works.

**Architecture:** Checker-only slice (no HIR/lowering/emit changes expected —
the explicit-generic spelling already lowers and runs; operator/method dispatch
on the param inside the body goes through the existing GenericBound machinery).
The fix makes call-site instantiation treat a function's synthetic impl-Trait
params like its declared generics: substitute fresh inference vars and require
the interface bound on the argument type.

## Global Constraints

- Codegen stays type-free; deferred features panic loudly in lowering.
- Rust: `cargo +nightly`, hard tabs, 2-space width, clippy clean.
- Subagents do not commit; the controller commits with jj scoped commits.

## Current state (surveyed 2026-07-13 at 38299e99)

- `Checker.synthetic_params: u32` + `synthetic_bounds: FxHashMap<ParamIdx, Vec<DefId>>`
  (check.rs:77-82): impl-Trait params mint a synthetic `Param` with a
  high-offset index (the corpus error shows `T268435456` = 0x10000000 offset)
  and record its interface bound. Body-side checking resolves methods on it
  through the bound (works — corpus confirms).
- At CALL sites, instantiation substitutes fresh vars only for the function's
  DECLARED generics; the synthetic param leaks through rigid, so unifying the
  concrete argument against `Param(synthetic)` fails with "mismatched types:
  expected T268435456, found Square".
- Acceptance test exists: the ignored `golden_finding_*impl_trait*` test in
  golden_programs.rs (read it for the exact program).

## Decisions

- **Z1 (semantics):** `func f(x: Iface)` ≡ `func f<T: Iface>(x: T)` — fresh
  instantiation per call site, bound enforced on the argument (a
  non-implementing argument gets the same diagnostic family the explicit
  spelling gets). Two impl-Trait params of the same interface are independent
  type variables.
- **Z2 (mechanics):** wherever function types are instantiated for calls
  (investigator locates it — likely the Fn-type lookup/instantiation in
  infer_call or the identifier-typing path), synthetic params carried by the
  function's signature ALSO get fresh vars, and their `synthetic_bounds`
  entries become obligations on those vars (reuse the existing
  bound-constraint machinery — however `<T: Iface>` declared-generic bounds
  are enforced at call sites today, do the same; do NOT invent a parallel
  path).
- **Z3 (interactions to verify, not break):** methods/impl funcs with
  impl-Trait params; impl-Trait params inside interface default bodies;
  an impl-Trait param used twice in one signature (same synthetic param must
  unify both uses); returning the impl-Trait param (`func id(x: Iface): ???` —
  if the return type can name it today, keep whatever the checker does,
  loudly); operator dispatch on the argument inside the body still records
  GenericBound → UserImplDefaultMethod (loud lowering deferral) — pinned by
  existing 4C-c tests.
- **Z4 (out of scope):** dynamic dispatch / dictionary passing (calls
  monomorphize nothing — the emitted JS is untyped, so the explicit-generic
  runtime behavior is already correct); `impl Trait` in return position (if
  the parser even accepts it — if it reaches the checker unchecked, make it
  loud); stdlib linkage.

## Tasks

### Task 1: call-site instantiation of synthetic params (Z1/Z2)
Files: crates/nymph-sema/src/{infer_expr,check,solve}.rs (investigator
narrows); tests: crates/nymph-sema/tests/solve.rs (accept + reject cases),
un-ignore the corpus test.
Cases: concrete arg implementing the interface → zero diags; non-implementing
arg → bound diagnostic (same family as explicit generics); two same-interface
impl-Trait params stay independent; one param referenced twice unifies; the
un-ignored corpus test passes and runs under Node.

### Task 2: interaction pins (Z3)
Files: tests only (solve.rs / operator_resolutions.rs / run_node.rs).
Cases per Z3 list; any interaction that FAILS gets reported by the workflow,
not silently patched.

### Task 3 (controller): commit, ledger, record review outcome.
