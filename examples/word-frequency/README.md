# word-frequency

Build a persistent word-count map and print its five most common entries.

The example builds its counts through persistent map updates: each binding is a
new map, while every prior map remains valid.

```nym
let counts: #{string: int} = #{}
let counts = counts.inserted("the", 4)
let counts = counts.inserted("fox", 2)
```

The bounded sample uses deterministic input and output so its runtime check does
not depend on files, locale, or unstable map traversal order.

Also on display:

- **Persistent maps** — each `inserted` call returns the next map while the source
  map remains valid and unchanged.
- **Deterministic output** — the example has exact expected stdout and status.

**Status:** ✅ Runs today with deterministic built-in input.
