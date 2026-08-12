# Root result rendering and process exit policy

This note records the resolution of
[issue #92](https://github.com/TheOnlyTails/nymph_lang/issues/92). It is planning evidence for the
language-identity roadmap, not an implementation of the destination runtime.

## Decision

Entrypoint validation operates on resolved semantic types after alias normalization. The only valid
root result shapes are:

```text
void
Option<void>
Result<void, E>
Task<void>
Task<Option<void>>
Task<Result<void, E>>
```

`E` must implement `Display`. The compiler statically selects the launcher path for the declared root
shape; the runtime does not inspect an arbitrary value by duck typing.

The Node executable launcher applies this terminal policy:

| Terminal outcome                                | Standard error                                                        | Exit status |
| ----------------------------------------------- | --------------------------------------------------------------------- | ----------: |
| `void`, `Some(void)`, or `Ok(void)`             | none                                                                  |           0 |
| `None`                                          | `error: main returned None\n`                                         |           1 |
| `Error(error)`                                  | `error: `, `Display(error)`, then `\n`                                |           1 |
| Unsourced cancellation or `SIGINT` cancellation | `error: execution cancelled\n`                                        |         130 |
| `SIGTERM` cancellation                          | `error: execution cancelled\n`                                        |         143 |
| Defect                                          | runtime-owned report beginning `error: program defected: <summary>\n` |         101 |

Every task-shaped root has the same observable result as its synchronous counterpart after the root
task context joins. Root policy never writes successful values to standard output; intentional
program output remains the responsibility of effectful operations such as `print` and `println`.

For application `Error`, the launcher preserves embedded newlines from `Display` and writes one final
newline. If `Display` defects, that failure is rendered and exited as a defect rather than replacing
the application error with another expected value.

## Semantic boundaries

The Node launcher creates the root task context and directly drives a root `Task`'s memoized default
execution. It classifies the declared `Option` or `Result` layer only after the execution successfully
produces a value and the context has joined.

This keeps the three outcome layers separate:

- `None` and application `Error` are ordinary produced values interpreted only by root host policy.
- Cancellation means the root execution produced no declared value.
- A defect means the root execution produced no declared value because of a panic, activation/runtime
  failure, cleanup defect, or uncaught exception or rejection crossing trusted FFI.

In particular, generic task machinery does not treat `None` or application `Error` as cancellation or
defect. A cleanup defect during cancellation settles as a defect with cancellation context, preserving
the task-runtime policy from issues
[#88](https://github.com/TheOnlyTails/nymph_lang/issues/88) and
[#89](https://github.com/TheOnlyTails/nymph_lang/issues/89).

## Cancellation and defects

On the first `SIGINT` or `SIGTERM`, the Node launcher requests cooperative root cancellation, joins
children, and permits deterministic cleanup before exiting with the signal's conventional status. A
second termination signal may force immediate termination because an execution doing checkpoint-free
CPU work is not guaranteed to observe cancellation.

Defects use a runtime-owned renderer that invokes no Nymph `Display` or `Debug` implementation. Its
stable first line identifies a program defect; when available, subsequent diagnostics contain the
logical Nymph activation backtrace, foreign-host cause, and suppressed cleanup defects. Promise and
Node implementation frames are excluded from the ordinary Nymph trace. Raw V8 stack text may be
included as marked supplemental detail but is not stable output. If defect normalization or rendering
itself fails, the launcher emits the fallback `error: program defected\n` and still exits 101.

This root boundary catches synchronous defects and rejected runtime promises alike, so behavior does
not depend on Node's version-specific uncaught-exception or unhandled-rejection renderer.

## Compiler and host ownership

- Sema validates the six resolved root shapes and the `Display` bound for `E`.
- HIR and code generation identify the statically known root shape and emit the corresponding launcher
  adapter without exposing Node process state as language semantics.
- The task runtime produces values, cancellation, and defects and performs root joining and cleanup.
- The Node launcher alone maps those outcomes to standard streams, signals, and process statuses.
- Ordinary `nymph build` output remains an inert importable ES module. `nymph run` and future explicitly
  runnable Node artifacts own executable launch policy.

The mapping is host policy rather than `Option`, `Result`, `Task`, or expected-error semantics. A later
browser adapter can consume the same runtime outcomes without inheriting Node process concepts.

## Repository and Node evidence

The current CLI compiles in entry mode, appends a bare `main();`, and returns the child Node status
(`crates/nymph-cli/src/commands/run.rs`). It ignores a synchronous return value and does not drive a
task. Current entry checking inspects only the surface return annotation and accepts only explicit
`void`, with an unannotated inferred non-void value slipping through
(`crates/nymph-sema/src/entry.rs`). The destination therefore requires semantic root-shape validation
rather than extending that syntax-only check.

Current `Display` dispatch uses a language protocol implementation with structural fallback, while
`println` uses `Display` (`stdlib/src/display.ts`, `stdlib/src/io.ts`). This supports using `Display` for
an expected application error while reserving the runtime-owned renderer for defects.

Focused probes under Node 24.19.0 confirmed that a normal module exits 0, an uncaught throw exits 1,
and bare or top-level-awaited rejected promises exit 1 with V8-owned stack output. Those defaults do
not distinguish expected failure from a defect and do not provide a Nymph-stable diagnostic format.

## Implementation verification frontier

The eventual execution plan should test all six accepted root types and reject near misses after alias
normalization. For synchronous and task-shaped roots, CLI integration tests should assert exact
stdout, stderr, and status for success, `None`, application `Error`, cancellation, and defects. Runtime
tests should additionally cover multiline and defecting `Display`, synchronous panic, rejected FFI,
logical activation traces, cleanup defects during cancellation, first-signal cleanup, forced
second-signal termination, and inert library builds.

No additional decision ticket is needed. Parser/sema updates, launcher emission, task-root driving,
signal handling, defect diagnostics, and the verification matrix belong in the map's eventual
dependency-ordered implementation decomposition.
