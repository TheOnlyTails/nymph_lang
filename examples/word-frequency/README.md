# word-frequency

Read a document, count how often each word appears, and print the five most common.

This is the example that best shows where iteration is headed: a **lazy pipeline**
built by chaining adapters on an iterator.

```nym
let top = counts
  .entries()
  .sorted_by((a, b) -> b[1] - a[1])
  .take(5)
```

`entries()` gives an iterator of `#(word, count)` tuples; `sorted_by` orders them by
count descending; `take(5)` keeps the first five. Because the pipeline is lazy,
these stages compose without materializing an intermediate list at each step —
nothing is computed until the `for` loop consumes `top`.

Also on display:

- **`Option` + `??`** — `counts.get(word) ?? 0` reads a possibly-absent map value
  with a default; `read_file(...) ?? fallback` does the same for a fallible read.
- **Tuple-destructuring patterns** — `for (#(word, count) in top)` unpacks each
  entry directly in the loop header.
- **String methods** — `split`, `trim`, `to_lower`, `length`.

**Status:** 🚧 In flight.
- The word-tallying loop (map + `Option` default) works today.
- `read_file` needs `std/fs` (not implemented yet).
- The `entries().sorted_by(...).take(...)` chain needs the lazy `Iterator` adapters
  (`map`/`filter`/`sorted_by`/`take` defined once on `Iterator`) — the current focus
  of standard-library work.
