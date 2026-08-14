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
  `mut func`s that update it. `complete` rebuilds the list through lazy
  `iter().map(...)`, then collects it with `to_list()`.
- **Nested positional patterns** — `main` matches both levels at once:
  `Ok(Add(title))` matches the `Result`'s `Ok` and, positionally, its sole payload
  against the `Command` variant. A single-field constructor accepts an un-named
  sub-pattern, so the `Result` wrapper melts away and every case is one arm.

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
- Direct list `.any(...)` and `.join(...)` are implemented; transformations use
  `.iter().map(...).to_list()` because lists do not have an eager `.map(...)`.
- As written, state lives only for one invocation. A real version would persist the
  task list to a file (see [`word-frequency`](../word-frequency) for `std/fs`, and a
  future `std/json` for serialization).
