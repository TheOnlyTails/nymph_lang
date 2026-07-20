# todo-cli

A command-line task manager: `todo add "buy milk"`, `todo done 1`, `todo list`.

The focus here is the shape of a real CLI:

- **Parse arguments into a typed `Command`** — a single `match` over the argument
  list turns `#["add", ...words]`, `#["done", id]`, `#["list"]` into an `enum`,
  returning a `Result` so a bad invocation produces a clear message instead of a
  crash.
- **List patterns** — `#["add", ...words]` peels the subcommand off the front and
  binds the remainder; `#[other, ...]` catches anything unrecognized.
- **State behind methods** — `Store` holds a `mut #[Task]`; `add`/`complete` are
  `mut func`s that update it. `complete` rebuilds the list functionally with `map`.
- **Two-level dispatch** — `main` matches the `Result` and hands the inner `Command`
  to `dispatch`, which matches each variant at the top level. Constructor patterns
  bind fields **by name** (`Add(title)`, `Complete(id)` use the real field names),
  and the `field = binding` form renames as it binds — `Ok(value = command)` pulls
  the `Result`'s `value` field out under the clearer name `command`.

```sh
todo add write the compiler
# added #1: write the compiler
todo list
# [ ] #1 write the compiler
todo done 1
# completed #1
```

**Status:** 🚧 Aspirational.
- `args()` needs `std/os`; `id.to_int()` needs a string→int parse on the stdlib
  string type — neither exists yet.
- The `.any(...)` / `.map(...)` / `.join(...)` list methods depend on the iterator
  adapters currently being built.
- As written, state lives only for one invocation. A real version would persist the
  task list to a file (see [`word-frequency`](../word-frequency) for `std/fs`, and a
  future `std/json` for serialization).
