# Iteration

Nymph has one looping construct for walking over a source of values: `for (pat in src) { .. }`.
What `src` may be — and how fast the resulting loop is — depends on its type: a range, a list,
or any type implementing one of the two iteration interfaces, [Iterator](#iterators) or
[Iterable](#iterables).

## Ranges

Iterating a [range](./literals#Ranges) is the fastest path: the compiler lowers it directly to a
counting loop, with no range-value allocation and no interface dispatch. This specialization only
applies when the range expression is the direct `for` source. Storing or passing the expression
first constructs its canonical standard-library range value; iteration of escaped range values is
reserved for the future `Step`/range-iteration work.

```nym
func sum(): int = {
  let mut total = 0
  for (i in 1..=4) {
    total = total + i
  }
  total
}
```

Only a range with both a lower and an upper bound (`1..10` or `1..=4`) has this direct-loop
behavior today. The other three forms are ordinary values, not supported "iterate forever"
sources. General range-value iteration belongs to the future `Step` work.

## Lists

A `#[T]` list is iterated natively as well — the element type needs no interface at all:

```nym
func sum(): int = {
  let mut total = 0
  for (x in #[1, 2, 3, 4]) {
    total = total + x
  }
  total
}
```

## Iterators

A source whose type directly implements `Iterator<Item>` is iterated by repeatedly calling its
`next()` method until it returns `None`, binding each `Some` value to `pat` in turn.

```nymph
interface Iterator<Item> {
  mut func next(): Option<Item>
}
```

`next` is a [mut func](./mutability#mut-func): producing the next value is inherently a mutation
of the iterator's own state, not necessarily of the collection it traverses. A binding used as a
direct `Iterator` source must therefore be declared `let mut`.

```nym
struct Counter(n: int, max: int)
impl Iterator<int> for Counter {
  mut func next(): Option<int> = if (this.n > this.max) {
    None
  } else {
    let v = this.n
    this.n = this.n + 1
    Some(value = v)
  }
}

func sum_counter(): int = {
  let mut c = Counter(n = 1, max = 4)
  let mut total = 0
  for (x in c) {
    total = total + x
  }
  total
}
```

## Iterables

A source that doesn't implement `Iterator` itself, but implements `Iterable<T>`, is iterated by
first calling `.iter()` to obtain an `Iterator<T>`, then following the same protocol as above.

```nymph
interface Iterable<T> {
  func iter(): Iterator<T>
}
```

```nym
struct Counter(n: int, max: int)
impl Iterator<int> for Counter {
  mut func next(): Option<int> = if (this.n > this.max) {
    None
  } else {
    let v = this.n
    this.n = this.n + 1
    Some(value = v)
  }
}

struct Bag(lo: int, hi: int)
impl Iterable<int> for Bag {
  func iter(): Counter = Counter(n = this.lo, max = this.hi)
}

func sum_bag(): int = {
  let b = Bag(lo = 1, hi = 4)
  let mut total = 0
  for (x in b) {
    total = total + x
  }
  total
}
```

Note that `Bag` itself need not be bound `mut` — only the `Iterator` the loop steps through
(`b.iter()`'s result, held internally) needs to be, and the loop's own desugaring takes care of
that.

Maps implement `Iterable<#(K, V)>` in ordinary Nymph by returning
`this.entries().iter()`. Explicitly consuming a map's iterator and iterating it with `for` therefore
yield the same key-value tuple sequence. Repeated iteration of an unchanged map instance is stable,
but map order is otherwise unspecified: separate map instances need not have the same order, and
mutation may change it. As with every `for` source, the map expression is evaluated exactly once.

> [!NOTE] Real `Iterator`/`Iterable` come from the standard library
> The stdlib defines the real `Iterator`/`Iterable` interfaces in
> [`@/iter`](./stdlib/iter#Iterator) — the versions declared inline above are for illustration
> and exercise the exact same for-loop desugaring.

## Non-iterable sources

A `for` loop over a source that is neither a range, a list, nor a type implementing `Iterator` or
`Iterable` is rejected at compile time — there is no silent fallback and nothing is iterated at
runtime that the checker didn't first prove iterable.

```nym
struct NotIterable(n: int)

func demo(): void = {
  for (x in NotIterable(n = 1)) {} // [!code error]
}
```
