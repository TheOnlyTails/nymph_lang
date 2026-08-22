# Issue 91: source-aware echo and release-build policy

Status: planning resolution, 2026-08-17. This note records the agreed destination behavior for
`echo`; it does not implement the destination. It is based on `language-identity-plan@origin`
(`56f05139c160`) and the exact resolved `PackageId` and complete-interface contract resolved by
issue 90.

## Current implementation facts

- `echo` is design-only. There is no token, AST expression, semantic operation, HIR node, or emitter
  path for it (`crates/nymph-ast/src/expr.rs`, `crates/nymph-hir/src/hir.rs`).
- `Debug` is an ordinary pure interface with a blanket external implementation
  (`stdlib/src/ops/mod.nym:98-104`, `479-481`). Its runtime protocol first calls `$nymph$debug`, then
  falls back to context-free structural rendering (`stdlib/src/display.ts:12-15`,
  `crates/nymph-codegen/src/hashmap_runtime.js:176-222`).
- That structural fallback enumerates `Object.keys`, so it renders every emitted field and is unsafe
  for arbitrary host objects: property reads can invoke getters or proxies.
- Stable complete module interfaces retain field names, visibility, types, and declaring identities
  (`crates/nymph-sema/src/interface.rs:382-405`). Contextual semantic environments currently project
  fields before consumer checking (`crates/nymph-sema/src/environment.rs:875-910`). Issue 90 requires
  complete private/internal structure to remain compiler-owned while contextual projection uses exact
  package and module identity.
- HIR already owns executable evaluation order, and codegen emits call operands once in source order
  (`crates/nymph-hir/src/hir.rs:229-320`, `crates/nymph-codegen/src/emit.rs:2000-2035`). A dedicated HIR
  expression can therefore preserve one evaluation without synthesizing source-visible bindings.
- `CompilerSession` project keys contain entry mode and emission-name options but no development or
  release profile (`crates/nymph-compiler/src/project/session.rs:187-207`). CLI build/run/check and LSP
  use the same compiler pipeline, with no release configuration.
- Diagnostics already support warnings, stable codes, labels, notes, and help. Warnings do not block
  compilation unless promoted to errors (`crates/nymph-diagnostics/src/lib.rs:12-100`,
  `crates/nymph-cli/src/commands/check.rs:7-35`).
- The CLI owns module-to-filesystem mapping through `nymph-project`; the compiler owns canonical
  module identity and spans. `run` inherits Node stdio, while the REPL already detects terminal input
  and mediates a persistent Node worker (`crates/nymph-cli/src/commands/run.rs:93-124`,
  `crates/nymph-cli/src/commands/repl.rs:1-105`).

## Resolved language contract

`echo expression` is a privileged compiler observation expression:

```text
type(echo expression) = type(expression)
effects(echo expression) = effects(expression)
```

It evaluates its operand exactly once and returns that identical value. `echo` adds no `!Io`; its
stderr observation is outside program semantics. Concurrent observations are individually atomic,
but their relative order is unspecified.

Unlike the earlier draft, echo is intentionally visibility-insensitive. It recursively renders the
complete structure of ordinary Nymph values, including private and internal fields. Field visibility
remains source access control, not secrecy from development terminals, CI logs, host capture, or
observation-enabled artifacts. This is a deliberate development-time disclosure boundary.

Echo never dispatches `Debug`, including for nested values. Explicit and generated `Debug`
implementations remain ordinary language APIs rather than hooks into privileged compiler observation.
Compiler-generated nominal `Debug` implementations are `internal` and owned by the nominal declaring
module, following issue 90's generated-implementation ownership rule. A type author may instead
provide an ordinary `public impl Debug` for public `.debug()` behavior; it still does not affect echo.

Deep structural rendering is restricted to compiler-recognized ordinary Nymph values: boxed scalars,
persistent collections, tuples/maps, and canonical emitted structs/enums. Functions, managed
resources, and opaque external references render as inert type-tagged placeholders. Echo must not
invoke getters, proxies, `toString`, `Debug`, or any user/host callback. A renderer defect produces an
inert placeholder rather than throwing into or otherwise changing program control flow. Authors can
snapshot external state into an ordinary Nymph value when they need structural observation.

## Compiler and runtime seam

Keep echo explicit through syntax, checked semantics, and HIR:

```text
source Echo(operand)
  -> checked Echo { operand, same type/effects, source site }
  -> HIR Echo { operand, site }
```

Do not desugar it to `.debug()` or `println`. In an observation-enabled development artifact, codegen
emits one call equivalent to `nymphEcho(operand, site)`. JavaScript argument evaluation evaluates the
operand once; the helper writes one line and returns the same object/value. In release emission,
codegen emits only `operand`. This preserves its evaluation and effects while removing observation.

The echo site carries a compiler-owned site ID, canonical module identity, and source span. A frontend
may additionally supply an opaque absolute source URI using its own module-to-filesystem policy. This
keeps filesystem paths out of semantic identity. Echo needs no field-visibility projection or
visibility metadata: its ordinary-value renderer deliberately uses complete runtime structure.

An observation line is written to stderr with a one-based source location and value:

```text
main.nym:12:5: Credential(secret: "...")
```

Only the filename is displayed. When stderr is a terminal and a source URI is available, the location
text is wrapped in an OSC 8 hyperlink to that URI and line/column. Redirected output retains the same
plain prefix without terminal escapes. Virtual inputs use unlinked `<repl>` or `<expr>` locations.
Absolute source URIs may be embedded in development artifacts; release artifacts contain no echo site
or source-URI metadata.

## Build profile, lint, and ownership policy

The compiler owns `BuildProfile::{Development, Release}` and
`LintLevel::{Allow, Warn, Deny}` as project/session query inputs. Frontends select and configure those
inputs; they do not scan syntax or reinterpret diagnostics independently.

- Development emits observations and no `echo-in-release` diagnostic.
- Release erases every echo observation and emits one `echo-in-release` diagnostic per applicable
  source site. Its default level is `Warn`; `Allow` suppresses it and `Deny` makes it an error that
  prevents release emission.
- The warning is anchored at `echo` and directs intentional output to `println` or telemetry.
- Persistent configuration belongs in the package manifest's general lint table:

  ```toml
  [lints]
  echo-in-release = "warn"
  ```

- `nymph build`, `nymph run`, and `nymph check` default to development and accept `--release`.
  Release `check` evaluates the same lint without emitting. REPL is always development.
- LSP defaults to development and exposes a workspace setting for release-profile diagnostics while
  consuming the same manifest lint level.
- Release erasure applies to every compiled package. The warning applies only to the root or
  workspace-owned package selected for analysis, determined by exact `PackageId`. Dependency, `std`,
  and compiler-module echoes are erased silently; each dependency owns its lint when built as a root.

Profile and lint inputs participate in incremental query identity so changing either invalidates the
affected diagnostics and emission. Release demand analysis must omit the echo helper and all site/URI
metadata. It must not remove ordinary `Debug` runtime support when actual `.debug()` calls demand it.

## Required verification at implementation time

- Parser/formatter/LSP coverage for prefix and pipeline-position echo syntax.
- Semantic tests proving operand type/effect identity and the absence of an added `!Io`.
- Runtime tests proving one evaluation and identity preservation for values with operand effects.
- Development snapshots for complete private/internal nested fields and for explicit `Debug` being
  ignored at both root and nested positions.
- Safety tests proving functions/resources/externals are opaque and getters, proxies, `toString`, and
  custom `Debug` are never invoked; renderer failures cannot alter control flow.
- Atomic stderr tests and deterministic noninteractive `filename:line:column` output, plus a targeted
  TTY test for OSC 8 links and virtual-source fallback.
- Release emission tests proving operand effects remain while observer calls, helper demand, absolute
  URIs, and site metadata are absent.
- `Allow`/`Warn`/`Deny`, CLI profile, manifest, LSP profile, and exact-package warning-scope tests.
- Incremental tests showing profile/lint changes invalidate diagnostics/emission without invalidating
  unrelated semantic interfaces.

## Consequence for governing design and roadmap

The destination must replace `language-identity.md`'s statement that only source-visible fields are
rendered. Echo is source-located but deliberately renders complete ordinary structure. Explicit
`Debug` controls `.debug()` only, not echo. No new decision ticket is required: parser/HIR rollout,
manifest/profile plumbing, runtime observation, diagnostics, formatter/LSP, docs, and migration tests
belong in the map's eventual dependency-ordered execution decomposition.
