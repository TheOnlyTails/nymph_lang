# Immutable language cutover release-candidate evidence

This is the durable verification record for
[issue #130](https://github.com/TheOnlyTails/nymph_lang/issues/130). It integrates and verifies the
ordered R14 cutover; it does not publish a release, reaccept retired syntax, or change a settled
language decision.

## Candidate identity and environment

The candidate is based directly on the authoritative #129 commit
`3c57f3cb8c5c8992ed267c23ad42357113cc3065`, tree
`909aee883a5bfcb882ca904fbf90849c93a644e7`, not `origin/main`. Git's ancestry check confirms
prerequisite `56f05139c16025ed57f81748e22db414af4b4329` is an ancestor. The manager transfer was
reassembled from both supplied parts, matched SHA-256
`80794cad55560326c26610936ea7d7251afb07a869464b1a731c8078ac3cb781`, passed
`git bundle verify`, and imported at `refs/issue-transfer/manager-129` with the exact commit and tree
above.

The verification orb used Linux x86-64, Node 24.19.0, pnpm 11.15.0, nightly Rust/Cargo 1.100.0,
cargo-nextest 0.9.143, and Jujutsu 0.44.0. Node therefore exceeds the Node 24 repository gate.

## R0-R14 graph and acceptance mapping

The complete closed graph from #82 through #129 was audited together with #95, #99, #126, the R14
completion records, the roadmap, ADRs, inventory contract, and repository guidance. Native GitHub
blockers remain authoritative. The implementation line is:

| Stage | Tickets | Integrated contract proven here |
| --- | --- | --- |
| Map and decisions | #82-#100 | Immutable values; checked effects; exact numbers; persistent collections; activation-owned tasks/resources; package visibility; echo/profile; roots; range proofs; FFI; compiler rollout; structs/enums; iteration; diagnostics and migration are settled and consistently assigned. |
| R0 | #101-#103 | Stable complete/recovered interfaces and retained-session parity, canonical atomic edits, frozen legacy corpus, and reviewed removed-path inventory. |
| R1 | #104 | Exact package/module/definition identity through compiler and tooling. |
| R2 | #105-#106 | Canonical finite checked-effect rows, inference, syntax, recovery, tooling, and external ABI vocabulary. |
| R3 | #107-#108 | Exact BigInt-backed 64-bit integers and body-local proof-directed range/index operations. |
| R4 | #109-#110 | Persistent lists/slices/maps/sets with lawful structural equality and hashing. |
| R5-R6 | #111-#112 | Named immutable structs with visibility-aware clone/update and fixed-point enum views with deep propagation. |
| R7 | #113 | One cleanup-aware activation machine for generated calls and tail transfer. |
| R8 | #114-#115 | Cold structured task executions plus async syntax, lowering, runtime, and tooling. |
| R9 | #116-#117 | Managed-resource cleanup and opaque, fingerprinted Node FFI adapters. |
| R10 | #118-#119 | Nominal persistent successor-state iteration, dedicated `For`, and immutable state loops. |
| R11 | #120-#121 | Compiler-owned profiles/lints and visibility-independent development echo with release erasure. |
| R12 | #122 | Semantic root validation, inert builds, and the exact separate Node launcher/status policy. |
| R13 | #123-#126 | Canonical CLI/LSP migration clients; formatter/LSP/extension parity; migrated docs, examples, and source producers; whole-repository readiness evidence. |
| R14 | #127-#130 | Recovery-only old syntax, deletion of mutable AST/sema/stable paths, deletion of legacy HIR/emitter/runtime/stdlib adapters, and this final integrated release-candidate proof. |
| Conditional follow-up | #131 | Temporary migration recognition and clients retire only after their documented support-window condition; #130 does not perform that work early. |

The ordered R14 ancestry is exact:

1. #127 `b81ee593542783bea4ec0438383499088cc591a4` switches retired syntax to bounded recovery;
2. #128 `4a4eecf29746f6a74e1e6fd1498ee20ba0942d6c` removes mutable AST, semantic, interface, and stable paths;
3. #129 `3c57f3cb8c5c8992ed267c23ad42357113cc3065` removes legacy lowering, HIR, emitter, runtime, and stdlib adapters; and
4. #130 applies only verified release-candidate corrections and this evidence on that exact parent.

## Repository-wide command matrix

All authoritative positive commands passed on the candidate. Warnings remained visible: Clippy reports
the existing warning backlog, and JavaScript lint reports 57 warnings and zero errors.

| Command | Result |
| --- | --- |
| `cargo fmt --check` | Passed. |
| `cargo clippy --all-targets --all-features` | Passed with the existing visible warnings. |
| `cargo nextest run` | Passed; 1,886 tests, with the repository's two configured skips. |
| `cargo test --doc` | Passed for all workspace crates; no doctests are currently defined. |
| `pnpm lint` | Passed; 57 warnings, zero errors. |
| `pnpm --filter nymph-docs build` | Passed; VitePress rendered the production site and sitemap. |
| `pnpm --filter nymph compile` | Passed. |
| `pnpm --filter nymph test:unit` | Passed; 23 tests. |
| `pnpm --filter nymph test:configuration` | Passed; two tests. |
| `timeout 180s xvfb-run -a pnpm --filter nymph test:smoke` | Passed; VS Code and the staged candidate LSP exited zero. |
| `python3 scripts/check-language-identity-cutover.py --check` | Passed; 13 reviewed file/rule entries are stable. |
| `git diff --check` | Passed. |

The extension's additional release gates also passed: `test:docs` checked three Markdown files, one
Nymph snippet, and all six package targets; `bundle` produced the extension bundle; the optimized
Linux x64 `nymph-lsp` built and staged; `vsce package --no-dependencies --target linux-x64` produced a
12-file VSIX with the single 35.86 MB matching executable; and `verify-vsix.cjs` accepted its contents
and executable mode. No package was published.

All six example manifests (`fizzbuzz`, `hello-world`, `http-server`, `shapes`, `todo-cli`, and
`word-frequency`) separately passed bounded `migrate --check`, `check`, and `run` commands: 18 commands,
all status zero. Exact-output repository tests also execute every example. The HTTP example's existing
unreachable-arm warning remained visible and its router smoke terminated normally.

## #99 ownership and focused gates

The full Rust run executes complete crate/package suites rather than replacing them with selected
tests. Named examples below locate each #99 owner in that integrated proof.

| #99 owner | Integrated proof |
| --- | --- |
| Canonical diagnostics and edits | Diagnostic edit validation, structured/terminal/LSP snapshots, UTF-8/UTF-16 ranges, applicability, stale versions, and deterministic ordering pass. The 16-fixture corpus pins every edit/manual outcome. |
| Removed grammar and recovery | Syntax parser/recovery/visitor suites accept destination forms and produce canonical causal diagnostics without accepted old AST nodes. Inventory `syntax-ast` and accepted `source` are zero. |
| Sema and interfaces | Complete/recovered interfaces, visibility, effects, enum fixed points, range facts, iterator capability, roots, profile lints, and retained invalidation suites pass. |
| Stable lowering, HIR, and codegen | Per-definition stable contracts, source anchors, invalidation, destination HIR, activation/cleanup, external ABI, and Node execution pass. Inventory `sema-stable` and `hir-emitter` are zero. |
| Formatter | Exact fixtures, recovered-input refusal, semantic fingerprints, second-pass idempotence, and stdlib/example corpus formatting pass. Formatting remains separate from migration. |
| Retained session and LSP | One-shot/retained parity, normal/no-prelude graphs, overlays/importers, cancellation, diagnostics/actions, completion, hover, symbols, rename, formatting, profiles, and semantic tokens pass. |
| Extension | Compile, 23 unit tests, two configuration tests, docs, lexical/semantic fixtures, bundle, bounded graphical smoke, target staging, package, and VSIX verification pass. |
| CLI and manifests | Schema/profile/release/root/launcher/migration suites pass. All six example manifests pass explicit migration, check, and execution. |
| Reference and guide | Executable fences compile and selected claims run under Node; docs checks, links/orphans, and VitePress production build pass. |
| Examples | Every manifest checks and runs; deterministic programs have exact output, while the service example uses its bounded terminating smoke. |
| Standard library/runtime | Embedded modules compile/link and execute persistent values, equality/hash, exact integers, effects, structs/enums, iteration, activation/tasks, resources, FFI, echo, and roots. Ordinary behavior remains in Nymph where practical. |
| Compatibility and removal | Frozen recovery evidence plus zero accepted-source, syntax/AST, sema/stable, HIR/emitter, runtime, generated-source, docs-source, and inert-build inventories prove one destination with no executable compatibility path. |

## Cutover-specific acceptance evidence

### Node absence fails rather than skips

The full run executes Node-backed tests with Node 24.19.0. As a negative control, the already-built
`nymph-compiler::run_node` binary ran `runs_arithmetic` with `PATH=/usr/bin:/bin`, which has no Node.
It exited 101 and failed at `Command::new("node").expect("run node")` with `NotFound`; no test skipped.
The same exact test with the normal path passed one of one.

### Frozen legacy corpus and migration window

The manifest SHA-256 is `586cf637b66d388970c1c2c140dc99ade83e4a23d7bf60f760f7bdecfbaee800`.
Its 16 fixtures cover 21 migration classes: eight safe inputs pin canonical diagnostics and atomic
edits, and eight manual inputs pin diagnostics, bytes, hashes, and unchanged no-edit outcomes. The
corpus test proves rejected old input cannot become an accepted old AST, applies safe groups, then
parses, formats twice, checks, lowers, and emits destination results. Manual fixtures remain frozen.
The temporary recovery recognizer and CLI/LSP migration clients remain only for #99's documented
window; they do not execute retired semantics and are owned by conditional #131.

### Inventory, removed paths, and inert builds

The checked inventory was compared byte-for-byte with exact `--print-inventory` output; it was not
passed through `oxfmt`. Its SHA-256 is
`7599ab4e400c13d93e84491b73c0cc3f66df2f8c94136e009a91bca4c827d8f3`.

Zero-occurrence categories are accepted source, syntax/AST, sema/stable, HIR/emitter, runtime,
generated source, docs source, and inert build output. Remaining retired spellings occur only in the
frozen corpus (20 occurrences/eight files), explicit parser/formatter/compiler recovery fixtures (15
occurrences/three files), and extension retired-token fixtures (four occurrences/two files). Private
runtime mutation is exactly 181 reviewed occurrences across the six named activation, echo, HAMT,
persistent-list, and task runtime/test files. The six reviewed release-echo source sites in four files
are producers checked for erasure, not bytes allowed in release output.

`ordinary_build_is_an_inert_importable_module_without_node_launcher_policy`, release-echo erasure
tests, inventory `inert-build = 0`, and removed-symbol scans together prove emitted ordinary/release
modules contain no launcher, echo observer, or legacy helper. The launcher remains a separate CLI host
policy only.

### Cross-language semantic agreement

The integrated suites prove the same contract through parser, semantic interfaces, stable facts, HIR,
emission, runtime, CLI, retained LSP, extension, stdlib, docs, and examples:

- ordinary bindings and values are immutable, with persistent list/map/set/string operations and
  alias-preserving structural sharing;
- effects, exact signed/unsigned 64-bit numbers, range facts, lawful equality/hash, named structs,
  fixed-point enums, and explicit conversion are preserved across modules and Node boundaries;
- generated calls use one activation machine; cold tasks, cancellation, cleanup-owned resources, and
  opaque FFI adapters retain their settled ownership and marshalling contracts;
- nominal successor-state iteration, dedicated `For`, and immutable state loops replace mutating
  iterators and source `while`; and
- development echo observes complete ordinary structure independent of visibility, release output
  erases every observer byte, ordinary builds stay inert, and the separate root launcher uses exact
  0/1/130/143/101 policy.

## Candidate corrections and diagnoses

Verification found and corrected only release-candidate regressions left by the ordered removals:

- eight stable external-alias/lowering fixtures still required the removed blanket `Display`
  constraint or external display shim; they now use the destination unconstrained print ABI and
  supported immutable string-length or primitive-equality adapters, including exact BigInt output;
- the external-link registry retained reachable `mut_list` rows for `length` and `get`; those rows and
  stale mutable registry documentation are removed, with negative lookup proof;
- removing those rows correctly changed the complete/recovered stable interface fingerprints, so the
  exact pinned values were updated after structural equality and focused retained-session parity passed;
- one root near-miss test prepended its canonical root fixtures twice, creating redefinition errors
  before it could check the intended root. The duplicate harness prefix is removed.

The initial full test run stopped after 750 tests with four stale compiler external-fixture failures.
A subsequent run exposed the two expected fingerprint pin changes; the next pass exposed the root
fixture duplication; and a no-fail-fast audit found four equivalent stale sema lowering fixtures.
Focused tests passed after each diagnosis, and the final full run passed without skipped failures.

Environment and harness failures were diagnosed rather than skipped:

- this orb initially lacked Xvfb, so the graphical smoke could not start; installing the bounded Xvfb
  harness and staging the optimized candidate LSP made the same smoke command pass;
- the extension documentation subprocess initially lacked `$HOME/.cargo/bin` on its shell path;
  restoring the pinned Rust toolchain path made the unchanged checker pass;
- repeated full Rust builds filled the orb's 64 GB root filesystem with 59.2 GiB of generated Cargo
  output, so one rebuild failed while writing its incremental dependency graph. `cargo clean` removed
  only generated output, and the complete suite reran from a clean non-incremental build;
- all toolchains and JavaScript dependencies were installed from repository guidance before the final
  matrix. No missing Node condition was converted into a skip.

## Release-candidate compatibility statement

This candidate is the one coherent immutable Nymph language cutover: ordinary values and source
bindings are immutable; retired mutable, assignment, `while`, positional, wrapper, and old iterator
forms are rejected through canonical recovery diagnostics and have no executable AST, semantic,
stable, HIR, emitter, runtime, or standard-library compatibility path. Effects, exact integers,
persistent values, structs/enums, activation/tasks/resources/FFI, iteration/state loops,
visibility-independent development echo with release erasure, and exact root/launcher policy use one
compiler/runtime/tooling contract. Frozen recovery recognition and the temporary migration driver
remain only for the documented migration window and do not execute old semantics. This is
release-candidate evidence only; no release was performed.

## Residual risk

This orb directly built, packaged, verified, and smoked Linux x64. Static extension target mapping,
documentation checks, and workflow tests cover all six declared targets; CI remains responsible for
executing each cross-platform package job. Existing Rust and JavaScript warnings remain visible. The
temporary recognizer and migration clients intentionally remain until #131's support-window condition;
that bounded follow-up cannot reaccept old syntax.
