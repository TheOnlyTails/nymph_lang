# `@/collections/linked_list`

A doubly-linked list, as an alternative to the array-backed [list literal](../literals#lists) — see
the note there on array lists vs. linked lists for when you'd reach for one over the other.

```nymph
struct Node<T>(
  prev: Option<Node<T>>,
  next: Option<Node<T>>,
  value: T,
)

public struct LinkedList<T>(
  head: Option<Node<T>>,
  tail: Option<Node<T>>,
  length: int,
)
```

> [!NOTE] Early shape — no operations yet
> `LinkedList<T>` exists today only as this bare field shape: a `head`/`tail` pair of optional
> nodes and a `length`. There's no `push`/`pop`/`iter` (or any other method) implemented on it yet,
> and — like the rest of the standard library outside the ambient [operator prelude](../operators)
> — it isn't reachable via `import` from a user program yet either. Treat this page as documenting
> the intended data shape, not something to build against today.
