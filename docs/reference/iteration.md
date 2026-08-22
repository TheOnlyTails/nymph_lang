# Iteration

Nymph iteration is immutable. `for (pattern in source) { … }` is the traversal construct; general
source `while` does not exist. Accumulate with iterator terminals such as `fold`, or use a
[state loop](#state-loops) when several named values must advance together.

## Nominal successor state

`Iteration` is a nominal enum, not `Option` and not a `{ value, done }` host convention. An iterator
is persistent: `next` returns either `Done` or an item together with the immutable successor.

```nymph
enum Iteration<Item, Next> {
  Done,
  Yield(item: Item, next: Next),
}

interface Iterator<Item + !E> {
  func next(): Iteration<Item, self> + !E
}

interface Iterable<Item + !E> {
  func iter(): Iterator<Item + !E>
}
```

These declarations are illustrative grammar because the nominal interfaces are ambient. Programs
use them without an import. `Iterable.iter()` is pure and called once; its returned iterator carries
the latent effects of stepping.

```nym
struct Counter(next: int, end: int)

impl Iterator<int> for Counter {
  func next(): Iteration<int, self> = if (this.next > this.end) {
    Done
  } else {
    Yield(item = this.next, next = Counter(next = this.next + 1, end = this.end))
  }
}

func first(counter: Counter): int = match (counter.next()) {
  Yield(item, next) -> item,
  Done -> 0,
}
```

Saving `counter` above preserves that position. Calling `next()` again replays the step; it does not
advance `counter` in place.

## `for`

`for` accepts an iterator, an iterable, a list, or a supported range. It is a dedicated compiler HIR
operation, not source-level sugar for another loop. The source expression and `iter()` each evaluate
once. Every iteration calls `next()` once and saves the successor before entering the body, so
`continue` resumes from that successor and every other departure performs no extra step.

```nym
func first_even(): Option<int> = for (value in 1..=6) {
  if (value % 2 == 0) { break value }
}
```

Natural exhaustion of a `for` with valued breaks is `None`; an executed `break value` is `Some`.
A bare break makes the loop `void`, and bare and valued breaks cannot be mixed. Labels use
`for@outer` and `break@outer`/`continue@outer`.

Ranges iterate forward. A reversed endpoint order is empty rather than implicitly descending; use
`.reversed()` explicitly. Lists are directly iterable. Iterator adapters are lazy, and predictable
callbacks execute sequentially in source order.

## State loops

A state loop carries one or more immutable bindings:

```nym
func sum_to(limit: int): int = loop (
  let next = 1
  let total = 0
) {
  if (next > limit) { break total }
  continue(next = next + 1, total = total + next)
}
```

Header declarations evaluate once from left to right. Each iteration receives fresh bindings.
Replacement expressions evaluate left to right against the old bindings, then install together;
omitted names retain their old values. Closures therefore retain the iteration they captured.
Fallthrough is equivalent to continuing without replacements.

State loops cannot exhaust, so `break value` has type `T`, not `Option<T>`. Labels use `loop@outer`
and `continue@outer(name = value)`. Header `let use` declarations participate in normal managed
resource cleanup when replaced or when the loop exits.

```nym
func swap_twice(): #(int, int) = loop (
  let left = 1
  let right = 2
  let step = 0
) {
  if (step == 2) { break #(left, right) }
  continue(left = right, right = left, step = step + 1)
}
```

The compiler implements continuation without growing the stack. See
[Immutability](./mutability) for the rules governing loop-carried values.
