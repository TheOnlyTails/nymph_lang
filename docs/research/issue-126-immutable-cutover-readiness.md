# Immutable cutover tooling and migration readiness

This is the durable verification record for
[issue #126](https://github.com/TheOnlyTails/nymph_lang/issues/126). It verifies the integrated R13
candidate; it does not publish R14, add a compatibility mode, or make a new language decision.

## Candidate identity and environment

The candidate is based directly on commit
`c09e3d51e5c85a17b72938964c65d4542eec8214`, tree
`2a7e74ef1eac2d463b6116fa6ce232066d7e46c4`. Git's ancestry check confirms prerequisite
`56f05139c16025ed57f81748e22db414af4b4329` is an ancestor. The transferred manager bundle was
reassembled, checked at SHA-256
`e76b0d6b72edc3edcd31bbe691556ee2d1f0dd0ce1a8d89ccf52c3ecfc037431`, accepted by
`git bundle verify`, and imported as `refs/issue-transfer/manager-125` at that exact parent.

The verification orb used Linux x86-64, Node 24.19.0, pnpm 11.15.0, nightly Cargo 1.100.0,
cargo-nextest 0.9.143, and Jujutsu 0.44.0. Node is therefore new enough for the
[repository-wide gate](./issue-99-diagnostics-tooling-migration-rollout.md#repository-wide-cutover-gate).

## Repository-wide command matrix

All positive commands below ran against one unchanged candidate tree. Warnings did not hide errors:
Rust Clippy and JavaScript lint both exited zero, and the JavaScript lint result was 65 warnings and
zero errors.

| Command | Result |
| --- | --- |
| `cargo fmt --check` | Passed. |
| `cargo clippy --all-targets --all-features` | Passed with the repository's visible warning backlog. |
| `cargo nextest run` | Passed; 1,927 tests. |
| `cargo test --doc` | Passed for all 12 crates; no doctests are currently defined. |
| `pnpm lint` | Passed; 65 warnings, zero errors. |
| `pnpm --filter nymph-docs build` | Passed; VitePress rendered the site and generated the sitemap. |
| `pnpm --filter nymph compile` | Passed. |
| `pnpm --filter nymph test:unit` | Passed; 23 tests. |
| `pnpm --filter nymph test:configuration` | Passed; two tests. |
| `pnpm --filter nymph test:smoke` | Passed under bounded `timeout 180s xvfb-run -a`; VS Code exited zero. |
| `python3 scripts/check-language-identity-cutover.py --check` | Passed; 13 reviewed file/rule entries are stable. |

Additional extension gates passed: `test:docs`, `bundle`, target-specific Linux x64 staging,
target-specific VSIX packaging, and `verify:vsix`. The smoke test used the staged candidate
`nymph-lsp`; it did not start an unbounded development server. The HTTP example is itself a bounded,
terminating router smoke and is covered by the exact-output example test.

## #99 ownership and focused-gate mapping

This table maps every row in the
[#99 component ownership table](./issue-99-diagnostics-tooling-migration-rollout.md#component-ownership)
to the candidate proof. The complete focused command suites run under `cargo nextest run`; the named
tests below make the ownership explicit rather than replacing the full run.

| Owner | Candidate proof |
| --- | --- |
| Canonical diagnostics and edits | `nymph-diagnostics` edit validation and snapshots pass in the full suite. The frozen migration cases prove exact code, spans, applicability, atomic edits, and manual no-edit outcomes. |
| Removed grammar and recovery | `nymph-syntax` positive/negative parser, recovery, and visitor suites pass. The cutover inventory rejects accepted legacy AST symbols and destination-source legacy spellings. |
| Sema and interface diagnostics | Complete/recovered interface, visibility, effects, enum, range, iterator, root, and lint suites pass. This gate corrected dropped effect-row arguments on generic interface bounds and added focused forwarding/excess-effect regressions. |
| Stable lowering, HIR, and codegen | Stable lowering, invalidation, HIR, emitter, and compiler project-fold suites pass, including source-anchored invariant tests and Node execution. The inventory rejects legacy stable/HIR/emitter paths. |
| Formatter | Formatter fixtures, malformed/recovered rejection, range formatting, semantic fingerprinting, and corpus idempotence pass in the full suite. Migration remains separate from formatting. |
| Retained session and LSP | Compiler session and LSP diagnostic/code-action, UTF-16, cancellation, normal/no-prelude, overlay/importer, profile, and language-feature suites pass on retained snapshots. |
| Extension | TypeScript compilation, 23 static tests, two configuration tests, bounded VS Code smoke, documentation check, bundle, target staging, and VSIX verification pass. Destination, retired, Markdown injection, TextMate fallback, and semantic-token parity are covered by the static fixtures. |
| CLI and manifests | CLI schema/profile/launcher suites pass. All six example manifests check. The migration test proves `--check`/`--write` ownership, atomicity, current-version handling, and manual status. Explicit `migrate --check --manifest` also reports no migration edits for all six example projects. |
| Reference and guide | Executable-fence compiler/Node suites, extension documentation checker, internal-link/orphan checks, and VitePress production build pass. Inventory scanning covers executable `nym` fences and generated source producers. |
| Examples | `every_example_manifest_checks` covers all six manifests. `deterministic_examples_have_exact_output_and_status` executes all six with exact stdout, stderr, and status, including the terminating HTTP router smoke. README commands and sources are checked by the migration/inventory and docs gates. |
| Standard library | Embedded-module ambient registry, linkage, persistent collections, equality/hash, exact integers, iteration/effects, cleanup/resources, tasks/activation, echo, FFI, roots, and std I/O Node suites pass. Ordinary iterator/list/string behavior remains in Nymph; TypeScript declarations/adapters provide only virtual or host/runtime primitives. |
| Compatibility and removal | The 13-entry reviewed inventory and frozen corpus prove there is no dual accepted syntax or semantic compatibility path. Legacy recognition remains confined to recovery and the temporary migration window settled by #99. |

The corresponding
[#99 focused component gates](./issue-99-diagnostics-tooling-migration-rollout.md#focused-component-gates)
are therefore covered as follows:

- diagnostics, parser/AST, sema/interfaces, lowering/HIR/codegen, formatter, and LSP are covered by
  their complete Rust suites, not a hand-picked subset;
- extension compile/unit/configuration/smoke plus bundle and VSIX verification all ran explicitly;
- manifest/CLI behavior is covered by the CLI suite and all six explicit example migration checks;
- docs/reference is covered by executable-fence tests, the extension docs checker, and VitePress;
- examples are checked and executed with exact output, including the bounded HTTP smoke;
- stdlib/runtime is compiled, linked, and executed under the full compiler Node matrix; and
- the frozen migration corpus applies safe groups through parse/format/check/lower/execute while
  preserving manual fixtures.

## Cutover-specific acceptance proofs

### Node absence fails rather than skips

The normal full run executes Node-backed suites with Node 24.19.0. A negative control runs the already
built `nymph-compiler::run_node` `runs_arithmetic` test with a `PATH` that contains Cargo/Rust tools but
no `node`. It exits nonzero at `Command::new("node").expect("run node")` with `NotFound`; there is no
skip branch. Restoring the normal `PATH` makes the same suite pass.

### Frozen corpus and removed paths

`python3 scripts/check-language-identity-cutover.py --check` verifies the machine-readable frozen
legacy corpus and all reviewed hashes, scans accepted source, Rust-produced source strings, executable
documentation fences, extension snippets/grammars, stable compiler layers, runtime adapters, release
echo bytes, and inert build output, and reports exactly 13 stable reviewed entries. The inventory file
was regenerated only from the script's exact `--print-inventory` output and then checked; it was not
passed through `oxfmt`.

### Migration ownership and producers

The six example manifests return success from both `migrate --check --manifest` and `check --manifest`.
The HTTP example emits its pre-existing unreachable-arm warning but no migration edit or error. The
stdlib manifest intentionally has no build entry and its embedded virtual modules are compiler-owned,
so treating it as a standalone CLI application would incorrectly seek `src/main.nym`; stdlib migration
ownership is instead proven by the embedded-module, inventory, linkage, and execution suites. Generated
source producers are scanned directly by the inventory.

## Corrections and diagnosis

Verification exposed candidate regressions, and only those settled-contract mismatches were corrected:

- checked interface constraints discarded effect arguments, so imported ambient modules and generic
  iterator bounds reported false excess effects; stable/recovered interface constraints now preserve
  and instantiate those rows, and generic-bound method calls charge their substituted rows;
- implementation effect contracts canonicalized interface method parameters in the wrong definition
  scope; canonicalization now uses the implementation method scope;
- migrated stdlib list/string indexing still relied on the removed implicit `uint`-to-`int` bridge,
  and iterator adapters still used receiver-field calls or recursive control shapes no longer accepted
  by the destination contracts; explicit casts, local callable bindings, and state loops correct them;
- a range test/reference retained the removed Option-style iterator method, and exact-integer and
  activation-machine and project-backdating assertions retained pre-cutover behavior; they now assert
  the settled successor, BigInt, activation, stable-fingerprint, and aggregate-emission contracts; and
- docs/stdlib TypeScript analysis lacked declarations for CSS and compiler virtual modules; declaration
  files now describe those existing imports; and
- one Node test still called removed mutable `List.push`, while immutable `Map.get` retained the old
  mutable receiver tag in the external-link registry; the test now proves `appended` preserves its
  source and map/list `get` link to their distinct immutable adapters.

The following were environment or harness failures, not candidate semantic regressions:

- extension configuration tests cannot run concurrently with extension compilation because compilation
  replaces `out`; the authoritative matrix runs them sequentially;
- the graphical smoke initially lacked an X server; bounded `xvfb-run` supplied the test environment,
  and staging the already-built candidate LSP supplied the package payload expected by the smoke;
- the extension documentation checker initially could not find Cargo because this orb's default shell
  path omitted `$HOME/.cargo/bin`; restoring the toolchain path made its snippet check pass;
- two deep compiler fixtures exceeded nextest's 2 MiB worker-thread stack but passed with an 8 MiB
  stack; only those tests are wrapped in bounded 8 MiB threads; and
- packaging a 526 MiB debug LSP exceeded VSCE's secret-scanner string limit. The release LSP used by
  the real packaging path packages and verifies successfully.

No suite was skipped for these conditions. Pre-existing lint warnings remain visible. The remaining
risk is platform coverage: this orb directly packaged and smoked Linux x64; static target mapping and
workflow tests cover all six declared VS Code targets, while CI remains responsible for executing each
cross-platform package job.
