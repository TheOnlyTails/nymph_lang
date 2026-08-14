# Ambient iteration APIs

The interfaces behind [`for` loops](../iteration), including the fallible stepping contract used
by canonical ranges. `Step`, `Iterator`, and `Iterable` are part of Nymph's ambient core: use them
directly, without an import.

## `Step`

```nymph
public interface Step: Comparable<Other = self> {
  func next(): Option<self>
  func previous(): Option<self>
}
```

`Step` is bidirectional and fallible. Its `int` and `uint` implementations return `None` rather
than crossing the exactly representable integer boundary or wrapping below zero. Its `char`
implementation skips UTF-16 surrogate code points in both directions and returns `None` before
crossing `U+0000` or `U+10FFFF`.

`Range`, `RangeInclusive`, and `RangeFrom` use `next` for forward iteration. Explicit reversed
views of bounded ranges, plus reversed `RangeTo` and `RangeToInclusive` views, use `previous`.
See [Ranges](../iteration#ranges) for endpoint and direction rules.

## `Iterator`

```nymph
public interface Iterator<Item> {
  mut func next(): Option<Item>

  func map<R>(f: (Item) -> R): Mapped<Item, R, self>
  func filter(predicate: (Item) -> boolean): Filtered<Item, self>
  func take(n: uint): Take<Item, self>
  func drop(n: uint): Drop<Item, self>
  func sorted_by(compare: (Item, Item) -> Order): SortedBy<Item, self>

  mut func for_each(f: (Item) -> void): void
  mut func fold<Acc>(initial: Acc, combine: (Acc, Item) -> Acc): Acc
  mut func to_list(): #[Item]
  mut func count(): uint
}
```

`next` is a [`mut func`](../mutability#mut-func): producing the next value is a mutation of the
iterator's own position, not necessarily a mutation of the collection it traverses. A binding used
directly as a `for` source must therefore be `let mut`. A `for` loop over a type implementing
`Iterator<Item>` calls `next()` until it returns `None`, binding each `Some`'s payload to the loop
pattern in turn. See [Iterators](../iteration#iterators) for a full worked implementation.

`map`, `filter`, `take`, and `drop` are lazy adapters. The terminal methods `for_each`, `fold`,
`to_list`, and `count` consume the iterator's remaining items. `sorted_by` is also lazy to call:
its first `next()` materializes the remaining source, performs a stable sort with an
`(Item, Item) -> Order` comparator, and then yields from that sorted buffer. Equal elements retain
their source order. If the source was partially consumed before `sorted_by` was created, only its
remaining items are sorted.

## `Iterable`

```nymph
public interface Iterable<T> {
  func iter(): Iterator<T>
}
```

A type that isn't itself an `Iterator` but can produce one — a collection, say, as opposed to the
cursor walking it — implements `Iterable<T>` instead. A `for` loop over an `Iterable` source calls
`.iter()` once to obtain an `Iterator<T>`, then follows the same protocol as above. See
[Iterables](../iteration#iterables) for a full worked implementation, including why the source
binding itself doesn't need to be `mut` even though the `Iterator` it produces does.

For maps, `Iterable<#(K, V)>.iter()` is ordinary Nymph and delegates to
`this.entries().iter()`. It consequently has the same entry sequence as map `for` iteration. That
sequence is stable when the same map instance is iterated repeatedly without mutation, but its
order is otherwise unspecified, including across distinct instances.

The [Iteration](../iteration) reference page declares equivalent interfaces inline where a worked
example needs to show their complete shape. Ordinary programs should use the ambient interfaces
instead of redeclaring or importing them.
