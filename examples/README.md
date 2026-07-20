# Nymph examples

A gallery of small, idiomatic Nymph projects — the kind of thing you'd reach for
the language to build. They double as a north star for the language and standard
library: they show the *surface we're aiming at*, written the way we want Nymph to
read.

> [!IMPORTANT] These are aspirational
> Not every example compiles or runs today. Some lean on standard-library modules
> that aren't implemented yet (`std/os`, `std/fs`, `std/http`, `std/json`, …) or on
> language features still in flight (lazy iterator adapters like `map`/`filter`/
> `fold` on `Iterator`). Each example's README calls out what it depends on and how
> much of it works right now. Treat them as design targets, not a test suite.

## Project layout

Every example is a self-contained project: a `nymph.toml` manifest at the root and
sources under `src/`, with `src/main.nym` as the entry module. Its `main()` function
(no arguments, no return) is the program's entry point.

```
todo-cli/
  nymph.toml        # name, version, dependencies
  src/
    main.nym        # func main() = { … }
```

Run one (once the toolchain supports it) with:

```sh
nymph run            # from inside the project directory
```

## The examples

| Example | What it shows | Ambient today? |
| ------- | ------------- | -------------- |
| [`hello-world`](./hello-world) | The smallest program — `println` from `std/io`. | ✅ runs |
| [`fizzbuzz`](./fizzbuzz) | Ranges, `match`, guards, string interpolation — no imports beyond `std/io`. | ✅ runs |
| [`shapes`](./shapes) | Enums, interfaces + `impl`, generics, exhaustive `match`. Pure language. | ✅ runs |
| [`word-frequency`](./word-frequency) | An iterator pipeline: `split` → `filter` → `fold` → `sorted`. File I/O. | 🚧 iterators/`std/fs` in flight |
| [`todo-cli`](./todo-cli) | A real CLI: argument parsing, subcommands, mutable state, `Result`. | 🚧 `std/os` aspirational |
| [`http-server`](./http-server) | A routed HTTP service with typed requests/responses and JSON. | 🚧 `std/http`/`std/json` aspirational |

## Language features on display

- **No `null`, no exceptions** — absence is `Option<T>`, failure is `Result<T, E>`,
  handled with `match` or combinators (`map`, `and_then`, `??`).
- **Sum types + exhaustive matching** — `enum`s with per-variant fields, checked so
  every case is handled.
- **Interfaces & operator overloading** — behavior attached with `impl … for …`,
  including the ambient operator prelude (`Plus`, `Comparable`, …).
- **Mutability as a view** — `let mut`, `mut func`, and `mut T` parameters make
  in-place mutation explicit and local.
- **Lazy iteration** — `map`/`filter`/`take`/`fold` defined once on `Iterator`,
  composing without intermediate allocation.
- **Compiles to clean JavaScript** — the whole thing runs on any JS runtime.
