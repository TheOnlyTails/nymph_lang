# Parallel Incremental Compiler Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the flattened compiler with the Salsa interface-consuming compiler, delete the legacy implementation in parallel, preserve Nymph language behavior, and profile the resulting compiler for safe parallel speedups.

**Architecture:** Workspace A continues the current #78 correctness work and implements stable lowering. Workspace B starts as its sibling and removes compatibility queries, flattened AST machinery, and internal differential tests while retaining behavior tests. Merge resolution always favors the new semantic data flow.

**Tech Stack:** Rust nightly, Salsa 0.28, Jujutsu workspaces, cargo-nextest, Criterion, Rayon, Node.js, GitHub issues.

## Global Constraints

- Compiler-internal APIs may break; Nymph syntax, typing, visibility, dispatch, standard-library semantics, external ABI, and generated behavior remain stable.
- Retain CLI, LSP, diagnostics, JavaScript, runtime, standard-library, project, and golden behavior tests.
- Remove tests only when they assert deleted compatibility implementation details.
- No production or test code may select the flattened compiler after merge.
- Dependency semantic queries consume `ModuleEnvironment`, never dependency ASTs or compatibility analysis.
- Recovered environments suppress dependent cascades and are never lowerable.
- Stable IDs, diagnostic order, output, and cache behavior remain deterministic.
- Every commit is an independently valid, issue-aligned unit with focused checks green.

---

### Task 1: Create sibling workspaces and update tracker contracts

**Files:**
- Modify: `.superpowers/sdd/progress.md`
- Modify: GitHub issues `#78`, `#79`, `#80`, and parent `#71`
- Create: concurrency follow-up GitHub issue

**Interfaces:**
- Consumes: current #78 WIP `@` and documentation-only parent `@-`.
- Produces: `../nymph-interface` on current #78 and `../nymph-legacy-removal` on a sibling change.

- [ ] **Step 1: Verify revisions**

```sh
jj log -r '@ | @-' --no-graph -T 'change_id.short() ++ " " ++ commit_id.short() ++ " " ++ description.first_line() ++ "\n"'
jj diff --stat
```

Expected: `@` contains current semantic correctness work; `@-` contains only committed documentation over the green #78 foundation.

- [ ] **Step 2: Create sibling changes and workspaces**

```sh
jj new --no-edit @- -m "refactor: remove flattened compiler internals (#80)"
legacy=$(jj log -r 'description("refactor: remove flattened compiler internals (#80)")' --no-graph -T 'change_id')
jj workspace add ../nymph-interface --revision @
jj workspace add ../nymph-legacy-removal --revision "$legacy"
```

Expected: both workspace changes have the documentation revision as parent.

- [ ] **Step 3: Update issue descriptions**

Record these exact contracts with `gh issue edit`/`gh issue comment`:

```text
#78 finishes the sole interface-consuming semantic compiler.
#79 implements stable per-definition lowering only for the new compiler.
#80 deletes compatibility internals, merges both workspaces, and verifies language/performance behavior.
The concurrency follow-up profiles the sole compiler and accepts only measured parallel wins.
```

- [ ] **Step 4: Commit durable progress metadata**

```sh
jj describe -m "chore: record parallel compiler cutover workspaces (#78, #80)"
jj new
```

---

### Task 2A: Complete the interface semantic compiler

**Workspace:** `../nymph-interface`

**Files:**
- Modify: `crates/nymph-sema/src/{check,environment,interface_extract,infer_expr,infer_pattern,solve}.rs`
- Modify: `crates/nymph-compiler/src/project/queries.rs`
- Replace: `crates/nymph-compiler/tests/interface_checker_differential.rs`
- Test: `crates/nymph-sema/tests/semantic_analysis.rs`
- Test: `crates/nymph-compiler/tests/interface_invalidation.rs`

**Interfaces:**
- Consumes: `check_module_with_environment`, `SemanticEnvironment`, and `interface_module_{analysis,interface,environment}`.
- Produces: one language-complete semantic query family with interface-only acceptance tests.

- [ ] **Step 1: Replace differential comparison with interface-only acceptance**

Use this test shape while retaining all existing fixture categories:

```rust
#[test]
fn interface_language_fixture_matrix() {
	for case in matrix_cases() {
		let outcome = run_interface(&case);
		assert_eq!(outcome.diagnostics, case.expected_diagnostics, "{}", case.category);
		assert_semantics(&outcome, &case.expected_semantics);
	}
}
```

Categories remain imports/visibility, aliases, ADTs/patterns, inherent/static/mutating members, interfaces/defaults, generics/blankets, coherence, recovery, externals, and entry/library mode.

- [ ] **Step 2: Run RED acceptance suite**

```sh
cargo nextest run -p nymph-compiler --features test-support -E 'binary(interface_checker_differential)' --no-tests=fail
```

Expected: failures identify new-compiler language defects, never mangled-name or synthetic-span parity.

- [ ] **Step 3: Finalize inferred semantic types before interface extraction**

At checker completion, call deep inference resolution for all function/method parameters and returns, generalized values, struct fields, enum fields, interface methods, and impl methods. Genuine unresolved inference remains an extraction error; complete state is never converted to poison.

- [ ] **Step 4: Resolve fixtures at their owning layer**

```text
imports and aliases       -> resolved bindings + SemanticEnvironment
canonical imported types -> stable allocation and instantiation
ADTs and patterns         -> owned signatures + stable annotations
methods and defaults      -> interface/impl/inherent registries
coherence                 -> local checker diagnostics
recovery                  -> recovered instantiation + diagnostic composition
externals                 -> owned ABI facts
entry/library             -> environment-aware checker mode
```

For each category: add one exact failing assertion, run RED, apply the smallest source fix, then run GREEN.

- [ ] **Step 5: Prove isolation and invalidation**

```sh
cargo nextest run -p nymph-compiler --features test-support -E 'binary(interface_invalidation)' --no-tests=fail
```

Expected: private body edits execute zero consumer analyses; public shapes rerun only reachable consumers; equal intermediate interfaces stop propagation; no interface query executes a compatibility query.

- [ ] **Step 6: Verify and commit #78**

```sh
cargo nextest run -p nymph-sema -p nymph-compiler --no-fail-fast
cargo check -p nymph-sema -p nymph-compiler --all-targets
cargo fmt --all -- --check
git diff --check
jj describe -m "feat(sema): complete interface semantic compiler (#78)"
jj new
```

---

### Task 2B: Delete legacy internals and preserve behavior tests

**Workspace:** `../nymph-legacy-removal`

**Files:**
- Delete: `crates/nymph-compiler/src/project/compat.rs`
- Modify: `crates/nymph-compiler/src/project/{mod,queries,session}.rs`
- Modify: `crates/nymph-compiler/src/lib.rs`
- Modify: `crates/nymph-lsp/src/*.rs`
- Delete/modify: compatibility-only compiler tests
- Retain: language, CLI, LSP, codegen, runtime, stdlib, project, and golden tests

**Interfaces:**
- Consumes: #78 foundation interface query names and public facade signatures.
- Produces: no flattened semantic implementation and explicit merge handoffs for stable lowering.

- [ ] **Step 1: Inventory legacy symbols**

```sh
rg -n 'compat_|CompatibilityFlattened|SemanticPipeline|checked_module|SPAN_BASE|offset_module|prelude_slice|transitive_dependencies' crates/nymph-compiler crates/nymph-lsp crates/nymph-sema
```

Classify each match as `delete`, `replace with interface query`, or `retained standalone language utility` in `.superpowers/sdd/task-legacy-removal-report.md`.

- [ ] **Step 2: Add a failing deletion guard**

```sh
! rg -n 'compat_|CompatibilityFlattened|SemanticPipeline' crates/nymph-compiler/src crates/nymph-lsp/src
```

Expected before deletion: FAIL with legacy symbols.

- [ ] **Step 3: Delete flattened implementation**

Remove `compat.rs`, its module declaration, `CompatModuleAnalysis`, pipeline selection, differential projection, symbol rewriting, flattened dependency analysis, and compatibility emission queries. Rewire semantic facade calls to `interface_module_*`. Leave a small explicit compile-time handoff for lowering calls owned by Task 3A; do not recreate flattening.

- [ ] **Step 4: Remove only compatibility-specific tests**

Delete tests whose sole subject is query parity, mangled names, synthetic prelude spans, or selector behavior. Preserve every test that runs Nymph, checks language diagnostics, imports projects, exercises LSP behavior, or inspects generated JavaScript.

- [ ] **Step 5: Verify deletion and commit**

```sh
cargo check -p nymph-sema -p nymph-compiler -p nymph-lsp --all-targets
cargo nextest run -p nymph-sema
rg -n 'compat_|CompatibilityFlattened|SemanticPipeline' crates/nymph-compiler/src crates/nymph-lsp/src && exit 1 || true
cargo fmt --all -- --check
git diff --check
jj describe -m "refactor(compiler): remove flattened compiler internals (#80)"
jj new
```

---

### Task 3A: Implement stable per-definition lowering

**Workspace:** `../nymph-interface`

**Files:**
- Create: `crates/nymph-sema/src/runtime.rs`
- Modify: `crates/nymph-sema/src/{annotate,lower_hir}.rs`
- Modify: `crates/nymph-compiler/src/project/{queries,session}.rs`
- Create: `crates/nymph-compiler/tests/runtime_definition_invalidation.rs`

**Interfaces:**
- Consumes: stable `DefinitionId`, diagnostic-free `SemanticAnalysis`, environments, and compiler-owned core roots.
- Produces: `runtime_definition(db, key, id) -> Option<Arc<RuntimeDefinition>>` and stable module lowering without span provenance.

- [ ] **Step 1: Add runtime types**

```rust
pub struct RuntimeDefinition {
	pub id: DefinitionId,
	pub owner: ModuleIdentity,
	pub body: CheckedDefinitionBody,
	pub abi: RuntimeDefinitionAbi,
	pub dependencies: Arc<[DefinitionId]>,
}

pub enum RuntimeDefinitionAbi {
	Nymph,
	External(ExternalAbi),
}
```

`CheckedDefinitionBody` owns only the current definition's body and stable annotations; it never references dependency modules.

- [ ] **Step 2: Add per-definition invalidation RED tests**

Prove one body edit reruns only that `runtime_definition` and dependent lowering. Prove unrelated definitions, semantic consumers, external ABI, and compiler-core owners remain cached/resolvable by exact stable ID.

- [ ] **Step 3: Lower through stable lookup**

```rust
pub trait RuntimeDefinitionLookup {
	fn get(&self, id: &DefinitionId) -> Option<Arc<RuntimeDefinition>>;
}
```

Calls, fields, variants, methods, implementations, operators, indexing, and iteration resolve through stable targets. No lowering decision reads `impl_span`, `SPAN_BASE`, mangled names, or dependency ASTs.

- [ ] **Step 4: Add tracked lowering/emission queries**

Implement `lower_interface_module` and emission/bundle queries over own analysis plus stable runtime dependencies. Preserve entry metadata, external linkage, runtime ownership, and generated behavior.

- [ ] **Step 5: Verify and commit #79**

```sh
cargo nextest run -p nymph-codegen -p nymph-compiler --no-fail-fast
cargo nextest run -p nymph-cli
cargo test -p nymph-compiler --test golden_programs
cargo fmt --all -- --check
git diff --check
jj describe -m "feat(compiler): lower stable runtime definitions (#79)"
jj new
```

---

### Task 4: Merge both workspaces into the sole compiler

**Files:**
- Resolve: `crates/nymph-compiler/src/project/{mod,queries,session}.rs`
- Resolve: `crates/nymph-compiler/src/lib.rs`
- Resolve: `crates/nymph-lsp/src/*.rs`
- Resolve: retained/deleted tests

**Interfaces:**
- Consumes: completed Workspace A #78/#79 and Workspace B deletion.
- Produces: one compiler with interface checking and stable lowering only.

- [ ] **Step 1: Create merge change/workspace**

```sh
jj new <workspace-a-head> <workspace-b-head> -m "refactor: merge sole incremental compiler (#80)"
jj workspace add ../nymph-cutover-merge --revision @
```

- [ ] **Step 2: Resolve with fixed rules**

Keep interface and stable-lowering queries, legacy deletions, and behavior tests. Drop `compat_*`, selector flags, differential projections, mangled parity, synthetic spans, and flattening.

- [ ] **Step 3: Prove no legacy production seam remains**

```sh
rg -n 'compat_|CompatibilityFlattened|SemanticPipeline|check_module_with_prelude|offset_module|SPAN_BASE' crates/nymph-compiler/src crates/nymph-lsp/src
```

Expected: no production matches.

- [ ] **Step 4: Run full verification and commit**

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo nextest run --no-fail-fast
cargo test --doc
pnpm lint
git diff --check
jj describe -m "refactor(compiler): select sole incremental pipeline (#80)"
jj new
```

---

### Task 5: Verify performance gates and remove transition code

**Files:**
- Modify: `crates/nymph-compiler/benches/incremental_project.rs`
- Modify: `docs/superpowers/benchmarks/incremental-semantic-baseline.md`
- Modify: files reported dead by clippy

- [ ] **Step 1: Run paired benchmarks**

```sh
cargo bench -p nymph-compiler --features test-support --bench incremental_project -- --sample-size 20
```

- [ ] **Step 2: Enforce gates**

```text
unchanged diagnostics and analysis+type_at >= 10x fresh
private leaf body >= 3x fresh and zero dependent body checks
public signature checks only reachable consumers
clean full build <= 10% slower than recorded paired baseline
```

Inspect Salsa events before changing any threshold.

- [ ] **Step 3: Remove transition dead code**

```sh
cargo clippy --all-targets --all-features
```

Remove dead transition fields/helpers instead of suppressing warnings, except documented test instrumentation.

- [ ] **Step 4: Commit**

```sh
jj describe -m "perf(compiler): verify incremental cutover gates (#80)"
jj new
```

---

### Task 6: Profile concurrency opportunities

**Files:**
- Create: `docs/superpowers/research/compiler-concurrency-opportunities.md`
- Modify: concurrency follow-up issue
- Modify: benchmark only if instrumentation is needed

- [ ] **Step 1: Capture clean/warm CPU and query profiles**

Profile wide, mixed, deep, public-shape, and private-body operations. Record CPU occupancy, phase wall time, query counts, and critical paths.

- [ ] **Step 2: Rank candidates**

Evaluate independent parsing, dependency-ready semantic analysis, per-definition lowering, module emission, and test/Node orchestration. Reject candidates that violate Salsa access, deterministic diagnostics, stable allocation, or regress small/deep projects.

- [ ] **Step 3: Prototype the best safe candidate**

Use Rayon or scoped threads in a benchmark-only prototype. Keep it only if paired measurements materially improve wide/mixed graphs without material deep/small regression.

- [ ] **Step 4: Document, update tracker, and commit**

Record measurements, ownership boundaries, rejected candidates, and the selected next task.

```sh
jj describe -m "perf(compiler): profile parallel execution opportunities"
jj new
```

---

### Task 7: Independent review and closeout

**Files:**
- Modify: `.superpowers/sdd/progress.md`
- Modify: GitHub issues `#78`, `#79`, `#80`, `#71`, and concurrency follow-up

- [ ] **Step 1:** Independently review language semantics, stable identities, recovery, core/runtime ownership, and retained tests.
- [ ] **Step 2:** Independently review dependency isolation, invalidation, per-definition lowering, and benchmark arithmetic.
- [ ] **Step 3:** Fix every Critical/Important finding, run focused tests, and re-review.
- [ ] **Step 4:** Close each issue only with exact implementation, test, and performance evidence.
