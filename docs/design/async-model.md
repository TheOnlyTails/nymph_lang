# Draft async model

This document records Nymph's current async task design. It is design documentation rather than the
implemented language reference. The broader type-system direction is documented in
[`language-identity.md`](./language-identity.md).

## Syntax and effects

```nymph
async func load(): User + !Network = {
  fetch().await
}

let operation: Task<int + !Io> = async {
  todo
}
```

`async` and `.await` are compiler syntax. Awaiting is permitted only inside an async function or
block. An `async func ...: T + !E` returns a cold `Task<T + !E>`.

An async block creates a nested structured task context. An async function does not create a context;
it inherits the context in which its body is driven.

Effects belong to the computation recipe:

```text
Task<T + !E>.await  -> T and performs !E
Task<T + !E>.spawn  -> Handle<T> and performs !E
Handle<T>.await     -> Result<T, HandleError> and is pure
```

Application effects occur when a task is driven or spawned. Joining, selecting, or supervising an
already-running handle does not repeat those effects. Handle source types retain only the outcome type,
though runtime diagnostics may preserve provenance.

## Tasks and executions

A `Task<T + !E>` contains two things:

1. A reusable computation recipe.
2. One memoized default execution handle.

Calling an async function or evaluating an async block creates the cold recipe; it does not execute
the body.

Direct await drives or observes the default execution:

```nymph
let task = fetch()
let first = task.await  // Starts and caches the default execution.
let second = task.await // Observes the same memoized result.
```

Operationally, first await is shorthand for spawning and awaiting a default instance, with that
implicit handle retained by the task.

Explicit spawn always creates a fresh independent execution from the recipe:

```nymph
let first = task.spawn()
let second = task.spawn()
```

The handles are independent. Cancelling one does not affect the other or the task's default
execution. Aliasing one handle aliases that one execution.

This distinction supersedes the earlier design in which a task represented only one shared execution.

## Handle outcomes

```nymph
enum HandleError {
  Cancelled,
  Defected(defect: Defect),
}
```

Driving a handle to completion returns an ordinary `Result`:

```nymph
let result: Result<T, HandleError> = handle.await
```

For `Handle<Result<T, E>>`, the result remains nested:

```text
Result<Result<T, E>, HandleError>
```

The layers are not flattened. Outer failure means the execution produced no declared value; inner
failure is expected application output. Enum embedding and generated `Into` conversions make
propagating both layers ergonomic.

A spawned execution is an isolation boundary: its panic becomes `Err(Defected(...))` when explicitly
observed through its handle. Directly awaiting a task propagates a panic directly.

## Structured concurrency

An async block's task context owns its spawned child handles and joins every child before exposing the
block result. Running concurrency cannot escape a context; completed handles may.

Ordinary `Result` propagation through `?` does not cancel siblings. `Result` remains ordinary data, so
the context joins children normally. Explicit fail-fast utilities request cancellation.

An unobserved child panic cannot disappear silently:

1. The panic requests cancellation of unfinished siblings.
2. The context waits for sibling cleanup and joins them.
3. The panic propagates to the owning task if no handle explicitly observes it.
4. An explicitly observed handle reports `Err(HandleError.Defected(...))`.

## Cancellation

Cancellation is cooperative and belongs to a handle or running execution. It is observed only at:

- `.await`
- Cancellable host operations
- Explicit `Task.checkpoint().await` or `Task.yield().await`

There is no implicit suspension. CPU work with no checkpoint may be unresponsive to cancellation.

Cancellation cannot be suppressed inside the affected execution. Supervisors observe it as
`Err(HandleError.Cancelled)`.

Cancellation cleanup order is:

1. Observe cancellation.
2. Request cancellation of child executions.
3. Let children unwind their `let use` resources.
4. Join children.
5. Close current-execution resources in reverse declaration order.
6. Settle the handle as `Cancelled`, unless cleanup defects; then settle it as `Defected`.

JavaScript host operations generally require `AbortSignal` bridging.

## Managed resources

Managed resource bindings use synchronous `Close.close()`:

```nymph
let use file = File.open(path)?
```

```nymph
interface Close {
  func close(): void
}
```

`close(): void` is synchronous, non-fallible, and idempotent. The runtime invokes it on normal scope
completion, `?`, return, panic, and cancellation, in reverse declaration order. Fallible or suspending
finalization is an explicit operation such as `finish(): Result<...>`.

A resource may escape as an alias, but the lexical `let use` scope still closes the underlying
resource. Later operations fail safely with a closed-resource error.

If a spawned child captures a managed resource whose lexical scope closes before the child's actual
joining context, emit a warning showing the declaration, capture, close, and join boundaries. This is
warning-oriented lifetime analysis, not ownership enforcement.

Cleanup always attempts every close. A body panic remains primary and cleanup panics are attached as
suppressed defects. A cleanup panic after normal completion defects the task; a cleanup panic during
cancellation produces a defect carrying cancellation context. Multiple cleanup defects are retained
in close order.

## Selection

Low-level selection is non-owning:

```nymph
struct Selection<T>(
  index: uint,
  result: Result<T, HandleError>,
)
```

```nymph
Handle.select(handles): Task<Selection<T>>
```

It observes the first terminal handle, preserves its input index, does not cancel or own other
handles, and performs no application effect. If several inputs are already settled, the lowest input
index wins deterministically.

## Racing tasks

```nymph
Task.race(tasks): Task<Result<T, HandleError> + !E>
```

`race`:

1. Spawns fresh independent executions from every input recipe.
2. Observes the first settlement.
3. Requests cancellation of losing executions.
4. Joins losers after their cleanup.
5. Returns the winner result.

First settlement may be success, cancellation, or defect. `select` observes existing handles; `race`
owns fresh executions and cancels losers.

Separate `first_ok` and `try_all` families interpret application-level `Result` values. Generic task
primitives never treat an application `Err` as cancellation or execution failure.

## Entrypoints

The runtime supplies the root task context and drives task-shaped `main` results. Valid root shapes
remain:

```text
void
Result<void, E>
Option<void>
Task<void>
Task<Result<void, E>>
Task<Option<void>>
```

The exact rendering of root errors and process exit-code policy remain separate CLI decisions.

## Actors

Actors are deferred to possible third-party libraries. They are not part of the core task model. A
library may build actor mailboxes, supervision, and isolated state machines over structured tasks and
handles without blocking this design.
