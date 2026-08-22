# todo-cli

A command-line task manager: `todo add "buy milk"`, `todo done 1`, `todo list`.

The focus here is the shape of a real CLI:

- **Parse arguments into a typed `Command`** — a single `match` over the argument
  list turns `#["add", ...words]`, `#["done", id]`, `#["list"]` into an `enum`,
  returning a `Result` so a bad invocation produces a clear message instead of a
  crash.
- **List patterns** — `#["add", ...words]` peels the subcommand off the front and
  binds the remainder; `#[other, ...]` catches anything unrecognized.
- **Immutable state transitions** — `Store.add` and `Store.complete` return new
  stores. Persistent list append and struct spread leave their inputs unchanged.
- **Named destinations** — enum construction and matching identify payload fields
  explicitly, for example `Some(value = n)` and `Complete(id = n)`.

```sh
nymph run
# [x] #1 write the compiler
```

**Status:** ✅ The project checks and its deterministic demonstration runs today.
The parser remains a reusable pure function; `main` supplies fixed input so its
output is suitable for exact producer tests.
