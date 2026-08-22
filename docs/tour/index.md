# A tour of Nymph

This tour builds a small immutable program. Every executable sample is tagged `nym` and checked by
the compiler; the [reference](../reference/) supplies the complete rules.

## Functions, blocks, and immutable bindings

A function body is one expression. A block evaluates steps in order and returns its final value;
`let` introduces an immutable binding.

```nym
func scaled_average(a: int, b: int, scale: int): float = {
  let sum = a + b
  let scaled = sum * scale
  scaled / 2
}
```

## Structs return replacements

Struct fields do not change in place. A function can preserve the old value and return a new one.

```nym
struct Task(title: string, done: boolean)
func complete(task: Task): Task = Task(title = task.title, done = true)

func before_and_after(): #(boolean, boolean) = {
  let before = Task(title = "Dishes", done = false)
  let after = complete(before)
  #(before.done, after.done)
}
```

## Enums, static views, and matching

Enums may embed variant sets. Widening changes the nominal static view, not the underlying variant.
Embedding is not an implicit `Into` implementation.

```nym
enum Priority { Low, Medium, High }
enum Scheduling { ...Priority, Deferred }

func schedule(priority: Priority): Scheduling = priority
func weight(priority: Priority): int = match (priority) {
  Priority.Low -> 1,
  Priority.Medium -> 2,
  Priority.High -> 3,
}
```

See [Structs and enums](../reference/structs-and-enums#embedding-and-static-views) for fixed-point
sets, selected variants, pattern refinement, and static method dispatch.

## Absence and failure are values

Nymph has no `null` and no exceptions. `Option<T>` represents presence or absence;
`Result<T, E>` represents expected failure.

```nym
enum Priority { Low, Medium, High }

func priority(code: int): Result<Priority, string> = match (code) {
  1 -> Ok(value = Priority.Low),
  2 -> Ok(value = Priority.Medium),
  3 -> Ok(value = Priority.High),
  _ -> Error(error = "unknown priority: ${code}"),
}
```

## Persistent iteration

An iterator's `next()` returns nominal `Iteration<Item, self>` successor state. `for` is a dedicated
compiler operation over that protocol; it is not a mutable iterator desugaring.

```nym
struct Counter(next: int, end: int)
impl Iterator<int> for Counter {
  func next(): Iteration<int, self> = if (this.next > this.end) {
    Done
  } else {
    Yield(item = this.next, next = Counter(next = this.next + 1, end = this.end))
  }
}

func find_three(): Option<int> = for (value in Counter(next = 1, end = 4)) {
  if (value == 3) { break value }
}
```

## Immutable state loops

There is no source `while`. A state loop gives every iteration fresh immutable bindings and replaces
named values simultaneously on `continue`.

```nym
func sum_to(limit: int): int = loop (
  let next = 1
  let total = 0
) {
  if (next > limit) { break total }
  continue(next = next + 1, total = total + next)
}
```

See [Iteration](../reference/iteration) and the [migration guide](../reference/mutability).

## Observation and program output

`echo value` returns the identical value and adds no effect. In development it renders complete
structure to stderr regardless of field visibility; release emission erases the observer but keeps
operand evaluation. Intentional output uses effectful I/O.

```nym
struct Task(public title: string, private note: string)
func inspect(task: Task): Task = echo task
```

The Node launcher never prints a successful root value. `main` may return `void`, `Option<void>`, or
`Result<void, E>` (or the corresponding `Task`), with exact error and exit handling documented in
[Projects and the Node launcher](../reference/projects#executable-roots-and-the-node-launcher).

```nym
import std/io
func main(): void = io.println("Hello from immutable Nymph")
```

## Next steps

- [Expressions](../reference/expressions) covers closures, pipes, operators, and control flow.
- [Error handling](../reference/error-handling) covers `Option`, `Result`, and `?`.
- [Projects](../reference/projects) covers manifests, semantic roots, profiles, and execution.
