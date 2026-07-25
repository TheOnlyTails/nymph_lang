# Lists

List literals (`#[...]`) provide stable, non-mutating sorting methods. Both methods allocate a new
list and leave the source list unchanged.

## `sort`

```nym
let sorted = #[3, 1, 2].sort()
```

`sort` is available when `T` implements `Comparable<Other = T>`. It orders values according to
`Comparable.compare_to`, including user-defined structs. Elements that compare as `Order.Equal`
retain their relative source order.

## `sort_by`

```nym
let descending = #[1, 3, 2].sort_by((left, right) ->
  if (left > right) { Order.LessThan }
  else if (left < right) { Order.GreaterThan }
  else { Order.Equal }
)
```

`sort_by` accepts any element type. The comparator receives two elements and returns
`Order.LessThan`, `Order.Equal`, or `Order.GreaterThan`. Returning `Order.Equal` preserves the
elements' source order, so custom descending or key-based orderings remain stable.
