# Iteration

Nymph has one looping construct for walking over a source of values: `for (pat in src) { .. }`.
What `src` may be — and how fast the resulting loop is — depends on its type: a range, a list,
or any type implementing one of the two iteration interfaces, [Iterator](#iterators) or
[Iterable](#iterables).

## Ranges

Ranges advance through the fallible `Step` interface. `Step.next()` and `Step.previous()` return
`Option<Self>`, so an iterator stops at a representation boundary instead of wrapping or producing
an invalid value. The standard library implements `Step` for `int`, `uint`, and `char`; character
stepping skips the UTF-16 surrogate interval and stops at the Unicode scalar boundaries.

`Range`, `RangeInclusive`, and `RangeFrom` are forward `Iterable` values. A bounded range whose
start is greater than its end is empty; endpoint order never selects an implicit descending loop.
Call `.reversed()` explicitly for descending traversal:

```nym
func sum(): int = {
  let mut total = 0
  for (i in 1..=4) {
    total = total + i
  }
  total
}

func countdown(): int = {
  let mut digits = 0
  for (i in (1..4).reversed()) {
    digits = digits * 10 + i
  }
  digits // 321
}
```

`RangeTo` and `RangeToInclusive` have no starting value and therefore are not forward iterable.
Their explicit reversed views are iterable: `(..4).reversed()` begins at `3`, while
`(..=4).reversed()` begins at `4`. `RangeFrom` is open-ended in the other direction. Open-ended
iteration must be stopped by control flow or an iterator adapter, and also stops cleanly if `Step`
returns `None`.

Exclusive and inclusive endpoints are symmetric after reversal. For example, `(1..4).reversed()`
yields `3, 2, 1`, and `(1..=4).reversed()` yields `4, 3, 2, 1`.

A direct bounded `int` or `uint` range remains an allocation-free compiler specialization. Stored,
passed, or returned ranges use the canonical standard-library value and the ordinary
`Iterable`/`Iterator` protocol. Both paths have the same direction, endpoint, emptiness, and
boundary behavior.

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
