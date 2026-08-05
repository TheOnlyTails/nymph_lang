# Nymph compiler

The compiler for the Nymph language (Rust → JavaScript). This glossary fixes the project's terms for how Nymph values and types are represented and compiled.

## Language

**Boxed value**:
The runtime form of every Nymph value — a wrapper object carrying its type's methods via a prototype, so `x.method(...)` dispatches uniformly for primitives and objects alike. See [ADR-0002](./docs/adr/0002-uniform-value-boxing.md).
_Avoid_: unboxed value.

**Raw value**:
The un-wrapped JavaScript form of a value (`3`, `"x"`, a native array) — appears only transiently, at a marshalling boundary or inside a condition (`if (x.v)`).
_Avoid_: primitive (a Nymph `int` is a boxed value; its raw value is a JS number).

**Canonical emission**:
Compiling each enum/struct to exactly one runtime definition, imported everywhere it is used. See [ADR-0001](./docs/adr/0001-single-canonical-type-emission.md).
_Avoid_: materialization (the superseded per-consumer scheme).

**Materialization**:
The superseded mechanism that re-emitted a prelude type into each module that used it, producing duplicate definitions. Replaced by canonical emission; the term survives only in existing code (`materialize_prelude_*`).
_Avoid_: use "canonical emission" for the current model.

**Intrinsic**:
A stdlib operation implemented directly in hand-written JS/TS and linked in (e.g. `list.get`), as opposed to one with a Nymph body the compiler lowers.
_Avoid_: builtin.

**Marshalling**:
Converting a boxed value to a raw value, or the reverse, at the JavaScript-interop boundary.
_Avoid_: conversion, casting (a cast is a Nymph-level `as` between types).

## Compiler architecture

**Compiler session**:
The compiler-owned, in-process lifetime for incremental analysis. `nymph-compiler` exclusively owns `CompilerSession`, `ProjectId`, `ModulePath`, Salsa inputs/storage, semantic identities, and the project graph. A CLI request creates a short-lived session; the LSP retains one across document notifications. State is never persisted across processes.
_Avoid_: putting semantic IDs or Salsa state in `nymph-project`, or maintaining a frontend analysis cache alongside the session.

**Project filesystem policy**:
Manifest discovery/schema and lexical filesystem path conversion owned by `nymph-project`. It converts files to the compiler's canonical `ModulePath`, but does not define semantic identity, acquire live editor text, or own compiler state. CLI and LSP adapters may retain only command/protocol-specific selection and URI behavior.
_Avoid_: duplicating manifest discovery or module/file conversion in a frontend.

**Effective source input**:
The source text currently installed for a `(ProjectId, ModulePath)`. The LSP initially installs disk sources, replaces one with open-document text on open/change, and restores the latest disk text (or removes the module if absent) on close. Document versions are publication guards and metadata, not Salsa identity.
_Avoid_: keying analysis by source text, URI, or LSP version.

**Incremental query graph**:
The tracked parse, import, semantic-analysis, diagnostics, lowering, and emission queries rooted in a `CompilerSession`. Resolved imports form the forward project graph; reverse importers are derived from those same edges rather than synchronized separately. Salsa invalidation follows query dependencies, allowing unchanged and unrelated results to be reused.
_Avoid_: eagerly invalidating every module in a project or storing a second reverse-dependency cache.

**One-shot compiler facade**:
The public standalone/project check and compile functions. These create and populate a temporary `CompilerSession`, then call the same canonical queries used by retained clients; they are compatibility adapters, not a second compiler pipeline.
_Avoid_: implementing separate one-shot parsing, checking, or emission semantics.
