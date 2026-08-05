# Error handling

Nymph has **no exceptions** and **no `null`**. A value that might be absent, or an
operation that might fail, says so *in its type* — with `Option<T>` or
`Result<T, E>` — and the caller is made to deal with the empty/failing case before it
can reach the value. Both types live in the always-available prelude
([ambient core](./declarations#imports)), so they need no `import`.

There is no `throw`, no `try`/`catch`, and no `?`-style early-return operator: a
failure is an ordinary value that flows through the program like any other, handled
with `match` or with the combinator methods below.

## `Option`

`Option<T>` is either `Some(value)` — a present `T` — or `None`. It's how a function
signals "there might not be an answer" without reaching for a sentinel value.

```nym
struct Task(title: string, done: boolean)

func first_open(a: Task, b: Task): Option<Task> =
  if (!a.done) { Some(a) }
  else if (!b.done) { Some(b) }
  else { None }
```

`Some` carries one field, `value`; construct it positionally (`Some(t)`) or by name
(`Some(value = t)`), and qualify with the type when you want to be explicit
(`Option.Some(t)`, `Option.None`).

### Getting the value back out

The most direct way is `match` — every case is spelled out, so nothing is skipped:

```nym
func or_zero(o: Option<int>): int = match (o) {
  Some(value) -> value,
  None -> 0,
}
```

For a one-off "is it there?" test without a full `match`, use
[`is`](./expressions#as-and-is):

```nym
func present(o: Option<int>): boolean = o is Some(...)
```

### Combinators

`Option` carries a set of methods for transforming the inside without unpacking it by
hand. `map` applies a function to the `Some` value (and leaves `None` untouched);
`filter` keeps a `Some` only when a predicate holds; `and_then` chains another
`Option`-returning step; `or` supplies a fallback `Option`.

```nym
func inc(o: Option<int>): Option<int> = o.map((x: int) -> x + 1)

func keep_even(o: Option<int>): Option<int> = o.filter((x: int) -> x % 2 == 0)

func check_pos(n: int): Option<int> = if (n > 0) { Some(n) } else { None }
func positive(o: Option<int>): Option<int> = o.and_then(check_pos)

func either(a: Option<int>, b: Option<int>): Option<int> = a.or(b)
```

These shine with [anonymous-parameter closures](./expressions#anonymous-closure-parameters)
— `o.map($ + 1)` is the same as `o.map((x: int) -> x + 1)`.

### Supplying a default

`??` (the [`Unwrap`](./operators#unwrap) operator) collapses an `Option` to a plain
value by giving the `None` case a fallback:

```nym
func title_or(o: Option<string>): string = o ?? "untitled"
```

`unwrap_or_else` does the same but computes the fallback lazily from a closure, and
`unwrap_or_default` uses the element type's `Default` when it has one.

```nym
func or_compute(o: Option<int>): int = o.unwrap_or_else(() -> 0)
```

## `Result`

`Result<T, E>` is either `Ok(value)` — success carrying a `T` — or `Error(error)` —
failure carrying an `E` explaining what went wrong. Reach for it (over `Option`) when
the *reason* for a failure matters.

```nym
enum Priority { Low, Medium, High }

func from_code(n: int): Result<Priority, string> = match (n) {
  1 -> Ok(Low),
  2 -> Ok(Medium),
  3 -> Ok(High),
  _ -> Error("unknown priority: ${n}"),
}
```

> [!NOTE] The failing variant is `Error`, not `Err`
> Its field is named `error`: `Error(error = "…")`, or positionally `Error("…")`.

### Handling both sides

`match` handles the two variants explicitly:

```nym
func describe(r: Result<int, string>): string = match (r) {
  Ok(value) -> "ok: ${value}",
  Error(error) -> "failed: ${error}",
}
```

### Combinators

`map` transforms the `Ok` value; `map_err` transforms the `Error`; `and_then` chains
another fallible step, short-circuiting on the first `Error`. This is how a pipeline
of fallible operations composes without any early-return syntax — each `and_then`
runs only if the previous step succeeded.

```nym
func step(n: int): Result<int, string> =
  if (n > 0) { Ok(n - 1) } else { Error("hit zero") }

func run(start: int): Result<int, string> =
  Ok(start).and_then(step).and_then(step)
```

```nym
struct Fail(code: int)
func wrap_err(r: Result<int, int>): Result<int, Fail> =
  r.map_err((c: int) -> Fail(code = c))
```

`??` supplies a fallback for the `Error` case, exactly as it does for `Option`:

```nym
func value_or(r: Result<int, string>): int = r ?? -1
```

## Converting between them

A `Result` drops its error side with `.ok()` (keeping the success as an `Option`) or
keeps only the error with `.err()`:

```nym
func to_option(r: Result<int, string>): Option<int> = r.ok()
func error_of(r: Result<int, string>): Option<string> = r.err()
```

Going the other way — attaching an error reason to a `None` — is a plain `match`:

```nym
func to_result(o: Option<int>): Result<int, string> = match (o) {
  Some(value) -> Ok(value),
  None -> Error("missing"),
}
```

> [!NOTE] `Option.ok_or` isn't usable yet
> `Option` also declares `ok_or`/`ok_or_else` for this direction, but they currently
> infer their error type as `() -> E` instead of `E` (the internal thunk they
> delegate through leaks into the result type), so any real use fails to type-check.
> Use the explicit `match` above until that's fixed — it's a compiler gap, not a
> language rule.

## No exceptions, by design

Because failure is a value and never a hidden control-flow jump, a function's
signature is the whole truth about how it can fail: a `Result<T, E>` return is the
*only* way it reports an error, and the type system won't let a caller ignore it. See
[Pattern matching](./pattern-matching) for everything `match` can pull out of an
`Option`/`Result`, and [Operators](./operators#unwrap) for `??`.
