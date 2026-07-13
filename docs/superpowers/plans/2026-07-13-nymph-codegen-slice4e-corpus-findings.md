# Slice 4E (Corpus Findings: return, shadowing, module lets) Implementation Plan

> **For agentic workers:** Executed by a single dynamic Workflow (investigate →
> implement TDD → review → adversarial refute → fix loop) in the MAIN working
> copy. The controller commits and updates the ledger.

**Goal:** Close golden-corpus findings 1–3 — the three `#[ignore]`d tests in
`crates/nymph-compiler/tests/golden_programs.rs` become the acceptance tests
and get un-ignored: (1) `return` no longer ICEs, (2) `let` shadowing no longer
emits invalid JS, (3) top-level `let` no longer vanishes from the module.

**Architecture:** Three semi-independent fixes in HIR/lowering/emit. No checker
changes expected (all three programs already type-check with zero diagnostics).

## Global Constraints

- Codegen stays type-free; deferred features panic loudly in lowering (or emit)
  — never silent wrong JS.
- Rust: `cargo +nightly`, hard tabs, 2-space width, clippy clean.
- Subagents do not commit; the controller commits with jj scoped commits.

## Current state (surveyed 2026-07-13 at ed5c9509)

- HIR has NO Return construct; `ExprKind::Return` hits lowering's slice-2a
  catch-all panic (~lower_hir.rs:499) on a zero-diagnostic program.
- Match/if in subexpression position is wrapped in an IIFE by EMIT (not
  lowering) — a JS `return` inside that IIFE would return from the IIFE, not
  the enclosing function: the hazard case for fix 1.
- `let x = 1; let x = x + 1` in one block emits two `const x` declarations in
  one JS scope → SyntaxError at load. NOTE: shadowing in NESTED blocks is
  legal JS (`const` is block-scoped) — verify only SAME-JS-scope
  redeclaration is broken.
- Top-level `let` declarations never reach lowered output (the module lowering
  ignores them entirely); block-level `Statement::Let` works
  (lower_hir.rs:833).

## Decisions

- **Y1 (return):** add a Return construct to HIR (statement-flavored:
  `HirStmt::Return(Option<HirExpr>)`, or expression-flavored if the
  investigator finds block lowering makes that cleaner) and emit a JS `return`.
  SCOPE GUARD: `return` is only supported where the emitted `return` provably
  targets the enclosing function — i.e. NOT (transitively) inside a match/if
  that emit wraps in an IIFE. The investigator maps how lowering/emit can know
  this (e.g. lowering tracks statement-vs-subexpression position, or emit
  panics when it encounters a Return while emitting an IIFE body). Unsupported
  positions PANIC LOUDLY with a slice-4e message; the supported case is plain
  `return` (with and without a value) in statement position of a function or
  method body, including inside statement-position if/while blocks.
- **Y2 (shadowing):** scope-aware rename during lowering: a per-JS-scope
  binding map; a redeclaration of a name already bound in the SAME JS scope
  gets a fresh emitted name (`x`, then `x$1`, `x$2`, …) and subsequent
  identifier references resolve through the innermost binding. Verify `$` (or
  chosen separator) cannot appear in Nymph identifiers so no collision with
  user names; also must not collide with codegen's `_tN` gensym temps. Nested-
  block shadowing keeps its source name (already-legal JS) — only same-scope
  redeclarations rename. Method/function params count as bindings in the body
  scope.
- **Y3 (module lets):** module-level `let` lowers into the HIR module (new
  `HirModule` field or a const-kind item) and emits as module-scope `const`
  declarations, in source order relative to each other, placed before function
  declarations use them at module-init time (JS function hoisting makes
  placement relative to functions safe; placement among lets preserves source
  order). Mutability (`let mut`) at top level: if the checker accepts it,
  emit `let`; if it never reaches lowering, note and skip. Export behavior:
  mirror whatever functions currently do (verify — if functions are exported,
  export consts the same way).
- **Y4 (out of scope):** corpus finding 4 (`impl Trait` param call sites — a
  checker/solver slice); `return` inside lambdas/closures (closures are
  deferred wholesale); `?`/`!` postfix propagation.

## Tasks

### Task 1: `return` (Y1)
Files: crates/nymph-hir/src/hir.rs, crates/nymph-sema/src/lower_hir.rs,
crates/nymph-codegen/src/emit.rs; tests: crates/nymph-sema/tests/lower_hir.rs,
crates/nymph-codegen/tests/run_node.rs, un-ignore the corpus test.
Cases: early return with value (the corpus `abs` shape) runs correctly under
Node; bare `return` in a void function; return inside statement-position
if/else; should_panic for return inside a match-as-subexpression arm.

### Task 2: shadowing (Y2)
Files: crates/nymph-sema/src/lower_hir.rs (rename infrastructure); tests:
lower_hir.rs, run_node.rs, un-ignore the corpus test.
Cases: same-scope re-let referencing the prior binding (`let x = 1; let x = x + 1`)
computes 2 under Node; triple shadowing; nested-block shadow keeps both
values distinct; shadowed name inside a method body; a user variable literally
named like the rename scheme (e.g. `x$1` if legal — if `$` is not legal in
Nymph identifiers, document and drop this case).

### Task 3: module lets (Y3)
Files: crates/nymph-hir/src/hir.rs, crates/nymph-sema/src/lower_hir.rs,
crates/nymph-codegen/src/emit.rs; tests: run_node.rs, un-ignore the corpus
test.
Cases: top-level `let` referenced by a function runs under Node; two lets
where the second references the first; a let referencing a function result.

### Task 4 (controller): commit, ledger, record review outcome.
