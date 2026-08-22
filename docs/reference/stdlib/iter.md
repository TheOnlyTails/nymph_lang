# Ambient iteration APIs

The interfaces behind [`for` loops](../iteration), including the fallible stepping contract used
by canonical ranges. `Step`, `Iteration`, `Iterator`, `ExactSizeIterator`, and `Iterable` are part
of Nymph's ambient core: use them directly, without an import.

## `Step`

```nymph
public interface Step: Comparable<Other = self> {
  func successor(): Option<self>
  func previous(): Option<self>
}
```

`Step` is bidirectional and fallible. Its `int` and `uint` implementations return `None` rather
than crossing the exactly representable integer boundary or wrapping below zero. Its `char`
implementation skips UTF-16 surrogate code points in both directions and returns `None` before
crossing `U+0000` or `U+10FFFF`.

`Range`, `RangeInclusive`, and `RangeFrom` use `successor` for forward iteration. Explicit reversed
views of bounded ranges, plus reversed `RangeTo` and `RangeToInclusive` views, use `previous`.
See [Ranges](../iteration#ranges) for endpoint and direction rules.

## `Iterator`

```nymph
public enum Iteration<Item, Next> {
  Done,
  Yield(item: Item, next: Next),
}

public interface Iterator<Item + !E> {
  func next(): Iteration<Item, self> + !E

  func map<R + !F>(f: (Item) -> R + !F): Mapped<Item = Item, R = R, E = !E, F = !F, S = self>
  func filter<!F>(predicate: (Item) -> boolean + !F): Filtered<Item = Item, E = !E, F = !F, S = self>
  func take(n: uint): Take<Item = Item, E = !E, S = self>
  func drop(n: uint): Drop<Item = Item, E = !E, S = self>
  func sorted_by(compare: (Item, Item) -> Order): SortedBy<Item = Item, E = !E, S = self>

  func for_each<!F>(f: (Item) -> void + !F): void + !E + !F
  func fold<Acc + !F>(initial: Acc, combine: (Acc, Item) -> Acc + !F): Acc + !E + !F
  func to_list(): #[Item] + !E
  func count(): uint + !E
}

public interface ExactSizeIterator<Item + !E>: Iterator<Item + !E> {
  func remaining(): uint
}
```

Iterator states are immutable. A successful `next()` returns both an item and the complete
successor state, so a pure state can be replayed or branched. A `for` loop saves that successor
before running the body; `continue` uses it, while every exit abandons it without an extra step.
The effect row is latent: creating an adapter is pure and consuming it charges source and callback
effects in source order.

`map`, `filter`, `take`, and `drop` are lazy adapters. The terminal methods `for_each`, `fold`,
`to_list`, and `count` consume the iterator's remaining items. `sorted_by` is also lazy to call:
its first `next()` materializes the source, performs a stable sort with a pure
`(Item, Item) -> Order` comparator, and then yields from that sorted buffer. Equal elements retain
their source order. `ExactSizeIterator.remaining()` reports the exact number of future yields;
adapters preserve that capability only when they can prove an exact result.

## `Iterable`

```nymph
public interface Iterable<Item + !E> {
  func iter(): Iterator<Item + !E>
}
```

A type that isn't itself an `Iterator` but can produce one — a collection, say, as opposed to the
state walking it — implements `Iterable<T>` instead. A `for` loop evaluates its source once and
calls `.iter()` once before following the same persistent successor protocol.

For maps, `Iterable<#(K, V)>.iter()` is ordinary Nymph and delegates to
`this.entries().iter()`. It consequently has the same entry sequence as map `for` iteration. That
sequence is stable when the same map instance is iterated repeatedly without mutation, but its
order is otherwise unspecified, including across distinct instances.

The [Iteration](../iteration) reference page declares equivalent interfaces inline where a worked
example needs to show their complete shape. Ordinary programs should use the ambient interfaces
instead of redeclaring or importing them.
