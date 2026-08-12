# `std/collections/linked_list`

A doubly-linked list, as an alternative to the array-backed [list literal](../literals#lists) — see
the note there on array lists vs. linked lists for when you'd reach for one over the other. This is
an opt-in standard-library type:

```nymph
import std/collections/linked_list with (LinkedList)

func retain<T>(list: LinkedList<T>): LinkedList<T> = list
```

## `LinkedList`

The current declarations are:

```nym
struct Node<T>(
  prev: Option<Node<T>>,
  next: Option<Node<T>>,
  value: T
) {}

public struct LinkedList<T>(
  head: Option<Node<T>>,
  tail: Option<Node<T>>,
  length: int
) {}
```

> [!NOTE] Early shape — no operations yet
> `LinkedList<T>` exists today only as this bare field shape: a `head`/`tail` pair of optional
> nodes and a `length`. There's no `push`/`pop`/`iter` (or any other method) implemented on it yet,
> so it is importable but not yet a practical general-purpose collection.
