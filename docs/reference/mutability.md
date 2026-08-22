# Immutability

Nymph values and local bindings are immutable. Nymph has no `mut` binding, `mut T`,
`mut func`, field assignment, index assignment, or compound assignment. Construct a new value instead
of changing an old one; persistent collections may share storage internally while preserving the old
value.

```nym
struct Counter(value: int)

func increment(counter: Counter): Counter = Counter(value = counter.value + 1)
func demo(): #(int, int) = {
  let before = Counter(value = 0)
  let after = increment(before)
  #(before.value, after.value)
}
```

## Updating repeated state

Use an immutable [state loop](./iteration#state-loops). Its header creates fresh loop-carried
bindings for each iteration, and `continue(name = value)` replaces them simultaneously. Use `fold`
when the operation is naturally a reduction.

```nym
func sum_to(limit: int): int = loop (
  let next = 1
  let total = 0
) {
  if (next > limit) { break total }
  continue(next = next + 1, total = total + next)
}
```

There is no source `while`. JavaScript emitted by the compiler may use mutation and host loops as an
unobservable optimization; that does not make either operation part of Nymph source semantics.
