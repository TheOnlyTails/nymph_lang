# Lists

List literals (`#[...]`) provide eager query and string-joining conveniences, plus stable,
non-mutating sorting methods.

## `any`

```nym
let has_even = #[1, 2, 3].any((value) -> value % 2 == 0)
```

`any` evaluates elements in list order and stops as soon as the predicate returns `true`.

## `join`

```nym
let words = #["one", "two", "three"].join(" | ")
let numbers = #[1, 2, 3].join(", ")
```

`join` is available for element types implementing `Into<Other = string>`. It converts elements
in list order and places the string separator only between adjacent elements. Empty lists produce
the empty string.

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

Lists intentionally do not have an eager `map`. Use `items.iter().map(f)` for a lazy result, and
append `.to_list()` when a list is required.
