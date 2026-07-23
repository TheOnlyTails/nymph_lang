# A tour of Nymph

This is a linear, hands-on tour: a sequence of small steps, each adding one new
idea on top of the last, until they assemble into one complete little program — a
tiny **task tracker** with an enum, a struct, a couple of methods, a `match`, and a
`main()` that ties it all together.

Every step is self-contained: its snippet compiles on its own, so you can run any
one of them in isolation. Read top to bottom the first time through; after that,
jump to whatever you want to revisit. When you want the full rules behind a step,
each one links into the [Reference](../reference/).

If you've never seen the language at all, the [Getting started](../guide/) guide is a
gentler, less linear overview; this tour is the "type along and watch it grow" path.

## Step 1 — Functions and expressions

A Nymph program is a flat list of top-level declarations, and the simplest is a
function: a name, typed parameters, a return type, and a body that is a **single
expression**. There's no `return` for the common case — the body's value *is* the
result. `${…}` inside a string splices an expression into it.

```nym
func greet(name: string): string = "Hello, ${name}!"
```

**Shows:** the `func name(params): Type = expr` shape and string interpolation. More
in [Functions](../reference/functions).

## Step 2 — Blocks and `let`

When one expression isn't enough, a **block** — `{ … }` — runs several steps in order
and evaluates to its last expression. `let` introduces an immutable local binding
along the way.

```nym
func scaled_average(a: int, b: int, scale: int): float = {
  let sum = a + b
  let scaled = sum * scale
  scaled / 2
}
```

**Shows:** blocks as expressions and `let` locals. More in
[Expressions](../reference/expressions#blocks).

## Step 3 — Immutability and `mut`

Everything is immutable by default: a plain `let` can never be reassigned. Opt into
change with `mut`, on both the binding and — later — the values and methods that
need it.

```nym
func countdown(): int = {
  let mut n = 3
  n = n - 1
  n = n - 1
  n
}
```

**Shows:** `let mut` for a reassignable binding. The full rules — `mut` as a *type*,
one-way coercion, mutating methods — are in [Mutability](../reference/mutability).

## Step 4 — Structs

A `struct` groups named, typed fields under one name. Construct one by calling its
name with **named** arguments — order doesn't matter when every field is named.

```nym
struct Task(title: string, priority: int, done: boolean)

func sample(): Task = Task(title = "Dishes", priority = 1, done = false)
```

**Shows:** declaring and constructing a struct. More in
[Structs and enums](../reference/structs-and-enums#structs).

## Step 5 — Methods

A method lives in the struct body (or a separate `impl` block). Inside it, `this` is
the receiver. A plain `func` reads; a `mut func` is allowed to change its receiver's
fields — and so may only be called on a `mut` binding.

```nym
struct Task(title: string, priority: int, done: boolean) {
  func is_open(): boolean = !this.done
  mut func complete(): void = { this.done = true }
}

func finish(): boolean = {
  let mut t = Task(title = "Dishes", priority = 1, done = false)
  t.complete()
  t.is_open()
}
```

**Shows:** `func`/`mut func` methods and `this`. More in
[Structs and enums](../reference/structs-and-enums#methods) and
[Mutability](../reference/mutability#mut-func).

## Step 6 — Enums

An `enum` is a fixed set of named variants — a *sum type*. Here priorities become
real, distinct values instead of bare integers. A variant with no fields is written
(and constructed) just by name.

```nym
enum Priority { Low, Medium, High }

func default_priority(): Priority = Low
```

**Shows:** declaring an enum and using a nullary variant. More in
[Structs and enums](../reference/structs-and-enums#enums).

## Step 7 — Pattern matching

`match` takes a value apart, arm by arm, running the first that matches. It's an
expression, so every arm agrees on one result type — here, turning a `Priority` into
a number.

```nym
enum Priority { Low, Medium, High }

func weight(p: Priority): int = match (p) {
  Low -> 1,
  Medium -> 2,
  High -> 3,
}
```

**Shows:** `match` over an enum's variants. The full pattern grammar (ranges, structs,
lists, guards, and more) is in [Pattern matching](../reference/pattern-matching).

## Step 8 — Interfaces and operators

Operators like `<` and `>` are backed by interfaces from an always-available
**prelude**. Implement `Comparable` for `Priority` — just the one `compare_to`
method — and all four comparison operators start working, so `High > Low` becomes a
real question you can ask.

```nym
enum Priority { Low, Medium, High }

impl Priority {
  func weight(): int = match (this) {
    Low -> 1,
    Medium -> 2,
    High -> 3,
  }
}

impl Comparable<Other = Priority> for Priority {
  func compare_to(other: Priority): Order = {
    let a = this.weight()
    let b = other.weight()
    if (a < b) { Order.LessThan }
    else if (a > b) { Order.GreaterThan }
    else { Order.Equal }
  }
}

func hotter(a: Priority, b: Priority): boolean = a > b
```

**Shows:** implementing an interface to overload operators. More in
[Interfaces and impls](../reference/interfaces-and-impls) and
[Operators](../reference/operators#comparison).

## Step 9 — `Option`: a value that might be absent

Nymph has no `null` and no exceptions. "Might not be there" is spelled out in the
type with `Option<T>` — either `Some(value)` or `None` — so the caller can't forget
to handle the empty case. Both `Option` and `Result` (next step) are ambient: no
import needed.

```nym
struct Task(title: string, priority: int, done: boolean)

func first_open(a: Task, b: Task): Option<Task> =
  if (!a.done) { Some(a) }
  else if (!b.done) { Some(b) }
  else { None }
```

**Shows:** returning `Option<T>` instead of a nullable value. More in
[Error handling](../reference/error-handling#option).

## Step 10 — `Result`: an operation that might fail

When an operation can fail *with a reason*, `Result<T, E>` carries it — `Ok(value)`
or `Error(error)`. Validation returns one instead of throwing.

```nym
enum Priority { Low, Medium, High }

func from_code(n: int): Result<Priority, string> = match (n) {
  1 -> Ok(Low),
  2 -> Ok(Medium),
  3 -> Ok(High),
  _ -> Error("unknown priority: ${n}"),
}
```

**Shows:** `Result<T, E>` for fallible work, with no exceptions. More in
[Error handling](../reference/error-handling#result).

## Step 11 — Closures and anonymous parameters

A closure is an inline anonymous function, `params -> body`. When it's short, an
**anonymous parameter** — `$` for the first argument (`$0`, `$1`, … for more) — drops
the header entirely. Here `map` transforms the value inside an `Option` only when
there is one.

```nym
struct Task(title: string, priority: int, done: boolean)

func open_title(a: Task, b: Task): Option<string> =
  first_open(a, b).map($.title)

func first_open(a: Task, b: Task): Option<Task> =
  if (!a.done) { Some(a) } else if (!b.done) { Some(b) } else { None }
```

**Shows:** `Option.map` with an anonymous-parameter closure. More in
[Closures](../reference/expressions#closures).

## Step 12 — Pipes

`a |> f` calls `f` with `a` — chained left to right, so a sequence of steps reads in
the order it runs. `??` (from the prelude) supplies a fallback for an `Option`,
turning "maybe a title" into "definitely a string".

```nym
struct Task(title: string, priority: int, done: boolean)

func headline(a: Task, b: Task): string =
  first_open(a, b).map($.title) ?? "all done"

func first_open(a: Task, b: Task): Option<Task> =
  if (!a.done) { Some(a) } else if (!b.done) { Some(b) } else { None }
```

**Shows:** `|>` for left-to-right flow and `??` for an `Option` fallback. More in
[Pipe](../reference/expressions#pipe) and [Operators](../reference/operators#unwrap).

## Step 13 — The whole program

Every piece so far, assembled: `Priority` (ordered via `Comparable`), a `Task` with a
reading method and a mutating one, a couple of free functions returning `Task` and
`Option<Task>`, and a `main()` that builds two tasks, completes one, and computes a
summary string.

```nym
enum Priority { Low, Medium, High }

impl Priority {
  func weight(): int = match (this) {
    Low -> 1,
    Medium -> 2,
    High -> 3,
  }
}

impl Comparable<Other = Priority> for Priority {
  func compare_to(other: Priority): Order = {
    let a = this.weight()
    let b = other.weight()
    if (a < b) { Order.LessThan }
    else if (a > b) { Order.GreaterThan }
    else { Order.Equal }
  }
}

struct Task(title: string, priority: Priority, done: boolean) {
  func is_urgent(): boolean = this.priority > Medium && !this.done
  mut func complete(): void = { this.done = true }
}

func more_urgent(a: Task, b: Task): Task =
  if (b.priority > a.priority) { b } else { a }

func first_open(a: Task, b: Task): Option<Task> =
  if (!a.done) { Some(a) }
  else if (!b.done) { Some(b) }
  else { None }

func main() = {
  let mut dishes = Task(title = "Dishes", priority = Low, done = false)
  let deploy = Task(title = "Ship release", priority = High, done = false)

  dishes.complete()

  let focus = more_urgent(dishes, deploy)
  let headline = first_open(dishes, deploy).map($.title) ?? "all done"
  let summary = "Focus: ${focus.title} - urgent: ${focus.is_urgent()}, next open: ${headline}"
}
```

**Shows:** enums, operator overloading, struct methods (reading and mutating),
`Option`, closures, and pipes working together in one `main()`.

> [!NOTE] Where's the output?
> `main()` is the program's [entry point](../guide/), but this tour's `main` only
> *computes* `summary` rather than printing it: reaching the standard library's
> `io` for `println` needs `import`, which doesn't link into a running program yet
> (see the note on [Declarations](../reference/declarations#imports)). Everything
> above type-checks and runs; printing is the one piece still on the way.

## Where to go next

- The [Reference](../reference/) covers every construct this tour sampled in full.
- [Error handling](../reference/error-handling) goes deep on `Option`/`Result` — the
  `map`/`and_then`/`filter` combinators, `??`, and converting between the two.
- [Expressions](../reference/expressions) has the complete story on closures,
  anonymous parameters, pipes, and the operator set.
