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

## Compiler and runtime seam

The semantic checker owns `Task<T + !E>` and `Handle<T>` types, async-context legality, and effect
charging. Creating a recipe is pure; driving its default execution or spawning a fresh execution
charges its latent row; observing a running handle is pure. Resolved effects are erased before
runtime execution.

HIR names recipe creation, default driving, fresh spawning, handle observation, nested task contexts,
lexical cleanup regions, cancellation checkpoints, and cancellable host calls. It does not expose
JavaScript promises, `AbortController`, runtime scheduling state, or cleanup stacks.

Generated JavaScript represents a recipe as a closure receiving one hidden execution frame. The frame
carries the inherited structured task context, the current execution's cancellation lineage, and its
`AbortSignal`. Async functions pass the inherited context through unchanged; async blocks replace only
the context with a nested one. Generated cleanup regions register lexical `Close` calls with runtime
helpers, and cancellable host adapters receive the frame's signal explicitly. Nymph does not use
Node's ambient `AsyncLocalStorage` as language semantics.

The host runtime owns runtime-private mutable state: the task's memoized default handle, fresh
executions, handle outcomes and observation state, context child registries, execution cancellation
lineages, deterministic settlement order, defect aggregation, selection, racing, and root driving. Its
task kernel is an embedded JavaScript module whose core requires only promises and `AbortController`;
Node-specific host operations remain adapters around that kernel. Native promises are an internal
scheduling mechanism, never the representation of a Nymph `Task`.

## Continuations and suspension

Generated Nymph callables use defunctionalized activations under the runtime execution frame. An
activation contains the callable's explicit resume state, live locals, and lexical cleanup scopes. The
runtime driver interprets HIR-level ordinary-call, tail-call, suspension, return, and cleanup
operations; native promises schedule host suspension but do not represent the language continuation.

An ordinary non-tail call pushes a logical activation. A tail call closes the departing activation's
pending lexical scopes and replaces that activation, including across direct, mutual, generic, and
dynamic dispatch. Suspension retains the current activation and resumes its named state through the
same driver. Consequently proper tail calls, async suspension, cancellation unwind, and deterministic
cleanup share one control-flow mechanism rather than composing a separate trampoline with native
`async`/`await` and `finally` stacks.

All generated Nymph callables share this activation ABI. The cold recipe closure's hidden execution
frame from the task runtime is the driver context; it is not a second continuation representation.
External host operations remain explicit adapters and receive only the frame data their ABI requires,
such as an `AbortSignal` for cancellable operations.

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
failure is expected application output. Enum set assignability and the explicit `Into` fallback make
propagating both layers ergonomic without changing the underlying error variant.

A spawned execution is an isolation boundary: its panic becomes `Err(Defected(...))` when explicitly
observed through its handle. Directly awaiting a task propagates a panic directly.

## Structured concurrency

An async block's task context owns its spawned child handles and joins every child before exposing the
block result. Running concurrency cannot escape a context; completed handles may.

Structured ownership and cancellation lineage are distinct. A spawned execution is registered with
the inherited task context, while the execution that spawned it records it as a cancellation child.
Consequently an async function can inherit its caller's context without creating a new join boundary,
while cancellation still descends through executions and joins their cleanup before the cancelling
execution closes its own resources.

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

There is no implicit suspension. Entering the next `for` or functional state-loop iteration is not a
cancellation checkpoint. CPU work with no checkpoint may be unresponsive to cancellation.

Cancellation cannot be suppressed inside the affected execution. Supervisors observe it as
`Err(HandleError.Cancelled)`.

Cancellation cleanup order is:

1. Observe cancellation.
2. Request cancellation of child executions.
3. Let children unwind their `let use` resources.
4. Join children.
5. Close current-execution resources in reverse declaration order.
6. Settle the handle as `Cancelled`, unless cleanup defects; then settle it as `Defected`.

JavaScript host operations generally require `AbortSignal` bridging. Generated code passes the
current execution frame's signal to explicitly cancellable host adapters; cancellation does not rely
on ambient Node state and does not change the ordinary external-call ABI.

## Managed resources

Managed resource bindings use synchronous, effect-parameterized `Close.close()`:

```nymph
let use file = File.open(path)?
```

```nymph
interface Close<!E> {
  func close(): void + !E
}
```

Any nominal type may implement `Close<!E>`. A value is a managed resource when its static type
satisfies that interface, and `let use` contributes `!E` to the enclosing computation. Management is
non-transitive: a type containing a managed field must implement `Close` explicitly. The compiler
warns when a type has a direct `Close` field, including a generic field with a `Close` bound, but does
not itself implement `Close`; recursive containers are not inspected and intentional non-ownership may
suppress the warning.

`close(): void + !E` is synchronous, non-fallible, and idempotent. The runtime invokes it on normal
scope completion, `?`, return, panic, and cancellation, in reverse declaration order. Fallible or
suspending finalization is an explicit operation such as `finish(): Result<...>`.

A functional state loop may carry a `let use` binding across iterations. An omitted binding remains
managed. Replacing one acquires the new value, closes the old value before the next iteration begins,
and manages the replacement thereafter. Cancellation or a defect during this transition uses the
same execution cleanup path; no replacement or successor iteration can escape it.

A resource may escape as an alias, be registered more than once, or be closed manually. Every lexical
registration still invokes `close` once. The implementation owns alias-shared closed state and exposes
safe post-close failures through its declared expected-error types; Nymph adds neither a universal
closed-resource error nor a compiler-owned lifecycle wrapper.

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
5. Returns the winner result if every loser cancels and cleans up without defecting.

First settlement may be success, cancellation, or defect. `select` observes existing handles; `race`
owns fresh executions and cancels losers.

A losing execution that defects during cancellation or cleanup defects the `race` execution after all
losers have been joined. It is not discarded in favor of a successful winner. This outer task defect
is distinct from a winner whose first settlement is an ordinary
`Err(HandleError.Defected(...))` result.

Separate `first_ok` and `try_all` families interpret application-level `Result` values. Generic task
primitives never treat an application `Err` as cancellation or execution failure.

## Entrypoints

After alias normalization, the only valid root shapes are:

```text
void
Result<void, E>
Option<void>
Task<void>
Task<Result<void, E>>
Task<Option<void>>
```

`E` must implement `Display`. A statically selected Node launcher creates and joins the root structured
task context, then applies the same policy to synchronous and task-shaped roots:

- `void`, `Some(void)`, and `Ok(void)` produce no implicit output and exit 0.
- `None` writes `error: main returned None\n` to stderr and exits 1.
- `Error(error)` writes `error: `, `Display(error)`, and one final newline to stderr and exits 1.
- Unsourced or orderly `SIGINT` cancellation writes `error: execution cancelled\n` and exits 130;
  orderly `SIGTERM` cancellation writes the same line and exits 143.
- A defect uses the runtime-owned non-user-code renderer, whose stable first line is
  `error: program defected: <summary>\n`, and exits 101. Renderer failure falls back to
  `error: program defected\n`.

Application `None` and `Error` values never become cancellation or defects. Ordinary `nymph build`
output remains an inert importable ES module; the launcher policy belongs to `nymph run` and future
explicitly runnable Node artifacts. Task machinery continues to distinguish produced values,
cancellation, and defects without interpreting application `Option` or `Result` values. The complete
contract and implementation evidence are recorded in
[`issue-92-root-result-rendering-and-exit-policy.md`](../research/issue-92-root-result-rendering-and-exit-policy.md).

## Actors

Actors are deferred to possible third-party libraries. They are not part of the core task model. A
library may build actor mailboxes, supervision, and isolated state machines over structured tasks and
handles without blocking this design.
