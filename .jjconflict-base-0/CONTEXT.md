# Nymph compiler

The compiler for the Nymph language (Rust → JavaScript). This glossary fixes the project's terms for how Nymph values and types are represented and compiled.

## Language

**Ordinary value**:
An immutable, persistent Nymph value with no observable shared mutation, ownership, or borrowing. Runtime structural sharing and internal mutation are permitted only when they preserve value semantics.
_Avoid_: owned value, mutable value.

**Boxed value**:
The runtime form of every Nymph value — a wrapper object carrying its type's methods via a prototype, so `x.method(...)` dispatches uniformly for primitives and objects alike. See [ADR-0002](./docs/adr/0002-uniform-value-boxing.md).
_Avoid_: unboxed value.

**Raw value**:
The un-wrapped JavaScript form of a value (`3`, `"x"`, a native array) — appears only transiently, at a marshalling boundary or inside a condition (`if (x.v)`).
_Avoid_: primitive (a Nymph `int` is a boxed value; its raw value is a JS number).

**Canonical emission**:
Compiling each enum/struct to exactly one runtime definition, imported everywhere it is used. See [ADR-0001](./docs/adr/0001-single-canonical-type-emission.md).
_Avoid_: materialization (the superseded per-consumer scheme).

**Enum view**:
The nominal static enum type through which an unchanged single-variant value is assigned, matched, or
dispatched. Embedding adds accepted single-variant types to an enum's deduplicated set; it does not
wrap or duplicate their values. A contextual assignment or `as` changes the view and therefore the
selected methods.
_Avoid_: embedded wrapper, flattened variant.

**Single-variant type**:
The ordinary source-nameable type of one qualified enum variant, identified by its stable variant
definition and only the generic arguments used by its fields. Qualified patterns prove this type.
_Avoid_: synthetic embedding variant.

**Materialization**:
The superseded mechanism that re-emitted a prelude type into each module that used it, producing duplicate definitions. Replaced by canonical emission; the term survives only in existing code (`materialize_prelude_*`).
_Avoid_: use "canonical emission" for the current model.

**Intrinsic**:
A stdlib operation implemented directly in hand-written JS/TS and linked in (e.g. `list.get`), as opposed to one with a Nymph body the compiler lowers.
_Avoid_: builtin.

**Marshalling**:
Converting a boxed value to a raw value, or the reverse, at the JavaScript-interop boundary.
_Avoid_: conversion, casting (a cast is a Nymph-level `as` between types).

**Effect**:
A nominal, statically tracked label describing an externally observable operation a computation may perform. Effects form an idempotent, commutative set and are not runtime handlers or expected-error values.
_Avoid_: exception, capability (effects describe and constrain operations but are not a security boundary).

**Managed resource**:
Any value whose static type implements `Close`, making it eligible for deterministic lexical cleanup through `let use`. Management is explicit and non-transitive: a containing type must implement `Close` itself rather than inheriting cleanup behavior from its fields. Aliases may escape, but cleanup still runs for the registered value.
_Avoid_: owned value (Nymph does not generally track ownership), finalizable object.

**Persistent iterator**:
An immutable traversal state whose `next` operation yields either completion or an item plus another persistent iterator state. Reusing a state repeats its declared effects; only pure iterators promise deterministic replay.
_Avoid_: cursor (a cursor conventionally advances through hidden mutation), stream (external streams are managed one-shot resources).

**Iterator capability**:
A static iterator interface whose guarantees survive stepping through the `self` successor type. `ExactSizeIterator` is the first capability and reports the exact remaining item count.
_Avoid_: iterator mode, iterator flag.

**Functional state loop**:
A non-exhausting loop with immutable header bindings whose named `continue` values create the next iteration's bindings. Omitted values remain unchanged; header resources remain managed until replaced or the loop exits.
_Avoid_: `while` loop, mutable loop.

### Async model

**Task**:
A cold reusable computation recipe plus one memoized default execution. Direct await drives or observes the default execution; each explicit spawn creates a fresh independent execution.
_Avoid_: promise (a JavaScript promise is eager), process.

**Task context**:
The structured lifetime that owns spawned task executions. An async block creates one; an async function inherits its caller's context without creating another.
_Avoid_: executor (the executor is a runtime mechanism, not the structured lifetime).

**Task execution**:
One drive of a task recipe, with its own cancellation state and descendants but an inherited task context. The task's default execution and every explicit spawn are distinct executions.
_Avoid_: task context (ownership/join lifetime), task (the reusable recipe).

**Execution frame**:
The generated-JavaScript/runtime seam passed as a hidden recipe argument. It carries the inherited task context, current execution cancellation lineage, and `AbortSignal`; it is not a Nymph value and is never exposed through FFI.
_Avoid_: ambient context, `AsyncLocalStorage` (Node ambient state is not language semantics).

**Task handle**:
The reference to one running or completed task execution. Driving it to completion observes a `Result` that distinguishes a produced value from cancellation or a defect.
_Avoid_: detached task, process handle.

**Drive to completion**:
Start a cold task or wait for a running task, suspending the current logical execution until the task completes. Driving an already completed task returns its memoized result immediately.
_Avoid_: run, block (neither says whether the caller waits or the physical thread stops).

**Activation**:
The generated, defunctionalized state of one logical Nymph callable invocation: its resume state, live locals, and lexical cleanup scopes. The runtime execution-frame driver pushes activations for non-tail calls, replaces them for tail calls, and retains them across suspension.
_Avoid_: JavaScript stack frame, promise, continuation closure.

**Tail transfer**:
A proper tail call that closes the departing activation's pending lexical scopes and then replaces that activation with the destination, without growing the logical call stack. A cleanup defect prevents the destination call.
_Avoid_: tail-call optimization (proper tail calls are semantics, not an optional optimization), recursive JavaScript call.

**Run concurrently**:
Spawn a fresh execution from a task recipe as a child of the current task context, then continue the current logical execution before that child completes.
_Avoid_: drive, run in parallel (concurrency does not require parallel threads).

**Close**:
The effect-parameterized, synchronous, non-fallible, idempotent interface through which any nominal type may define deterministic cleanup. Its implementation owns shared closed-state behavior; Nymph does not add a universal lifecycle wrapper. Suspending or recoverable finalization must be performed explicitly before `Close` runs.
_Avoid_: `AsyncClose`, finalizer (garbage-collector finalization is not deterministic cleanup).

## Compiler architecture

**Compiler session**:
The compiler-owned, in-process lifetime for incremental analysis. `nymph-compiler` exclusively owns `CompilerSession`, `ProjectId`, `ModulePath`, Salsa inputs/storage, semantic identities, and the project graph. A CLI request creates a short-lived session; the LSP retains compiler sessions across document notifications (including a separate no-prelude session for standard-library sources). State is never persisted across processes.
_Avoid_: putting semantic IDs or Salsa state in `nymph-project`, or maintaining a frontend analysis cache alongside the session.

**Project filesystem policy**:
Manifest discovery/schema and lexical filesystem path conversion owned by `nymph-project`. It converts files to the compiler's canonical `ModulePath`, but does not define semantic identity, acquire live editor text, or own compiler state. CLI and LSP adapters may retain only command/protocol-specific selection and URI behavior.
_Avoid_: duplicating manifest discovery or module/file conversion in a frontend.

**Package identity**:
The compiler-owned identity of one exact resolved package instance within a project's dependency graph. Dependency aliases resolving to the same graph node share a `PackageId`; independently resolved copies do not, even when their declared names and versions match. `ProjectId` remains the compiler-session/workspace lifecycle boundary, while `PackageId` is the ownership boundary used by `internal` visibility and carried through module identities, stable interfaces, incremental analysis, and canonical emission.
_Avoid_: using package name, name plus version, source-root text, or `ProjectId` alone as package identity.

**Effective source input**:
The source text currently installed for a `(ProjectId, ModulePath)`. The LSP initially installs disk sources, replaces one with open-document text on open/change, and restores the latest disk text (or removes the module if absent) on close. Document versions are publication guards and metadata, not Salsa identity.
_Avoid_: keying analysis by source text, URI, or LSP version.

**Incremental query graph**:
The tracked parse, import, semantic-analysis, diagnostics, lowering, and emission queries rooted in a `CompilerSession`. Resolved imports form the forward project graph; reverse importers are derived from those same edges rather than synchronized separately. Salsa invalidation follows query dependencies, allowing unchanged and unrelated results to be reused.
_Avoid_: eagerly invalidating every module in a project or storing a second reverse-dependency cache.

**One-shot compiler facade**:
The public standalone/project check and compile functions. These create and populate a temporary `CompilerSession`, then call the same canonical queries used by retained clients; they are compatibility adapters, not a second compiler pipeline.
_Avoid_: implementing separate one-shot parsing, checking, or emission semantics.
