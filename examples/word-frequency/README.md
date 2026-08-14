# word-frequency

Read a document, count how often each word appears, and print the five most common.

This is the example that best shows where iteration is headed: a **lazy pipeline**
built by chaining adapters on an iterator.

```nym
let top = counts
  .entries()
  .iter()
  .sorted_by((a, b) -> b[1].compare_to(a[1]))
  .take(5u)
```

`entries()` gives a list of `#(word, count)` tuples; `iter()` turns it into an
iterator; `sorted_by` orders the remaining entries by count descending; `take(5u)`
keeps the first five. Building the pipeline does no iteration. On the first pull,
`sorted_by` materializes and stably sorts its remaining source once, then yields
from that buffer.

Also on display:

- **`Option` + `??`** — `counts.get(word) ?? 0` reads a possibly-absent map value
  with a default; `read_file(...) ?? fallback` does the same for a fallible read.
- **Tuple-destructuring patterns** — `for (#(word, count) in top)` unpacks each
  entry directly in the loop header.
- **String methods** — `split`, `trim`, `to_lower`, `length`.

**Status:** 🚧 In flight.
- The word-tallying loop (map + `Option` default) works today.
- `read_file` needs `std/fs` (not implemented yet).
- The `entries().iter().sorted_by(...).take(...)` iterator chain is implemented.
  The missing `std/fs` module still prevents the complete example from running.
