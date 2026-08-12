# Nymph examples

A gallery of small, idiomatic Nymph projects — the kind of thing you'd reach for
the language to build. They double as a north star for the language and standard
library: they show the _surface we're aiming at_, written the way we want Nymph to
read.

Every checked-in example is a compiler-checked project. Programs use immutable
values and explicit executable roots; runnable examples are deterministic, and the
service-shaped example is deliberately bounded so smoke checks always terminate.

## Project layout

Every example is a self-contained project: a `nymph.toml` manifest at the root and
sources under `src/`, with `src/main.nym` as the entry module. Its `main()` function
(no arguments, no return) is the program's entry point.

```
todo-cli/
  nymph.toml        # name, version, dependencies
  src/
    main.nym        # func main(): void = { … }
```

Run one from its project directory with:

```sh
nymph check           # selects build.entry from nymph.toml
nymph build
nymph run
```

You can also run an example from outside its directory by selecting its
manifest exactly (the option may appear before or after the subcommand):

```sh
nymph --manifest examples/hello-world/nymph.toml check
nymph build --manifest examples/hello-world/nymph.toml
nymph run --manifest examples/hello-world/nymph.toml
```

## The examples

| Example                              | What it shows                                                               | Ambient today?         |
| ------------------------------------ | --------------------------------------------------------------------------- | ---------------------- |
| [`hello-world`](./hello-world)       | The smallest program — `println` from `std/io`.                             | ✅ runs                |
| [`fizzbuzz`](./fizzbuzz)             | Ranges, `match`, guards, string interpolation — no imports beyond `std/io`. | ✅ runs                |
| [`shapes`](./shapes)                 | Enums, interfaces + `impl`, generics, exhaustive `match`. Pure language.    | ✅ runs                |
| [`word-frequency`](./word-frequency) | Persistent maps and a lazy iterator pipeline with stable sorting.           | ✅ runs                |
| [`todo-cli`](./todo-cli)             | Typed command parsing and immutable state transitions.                      | ✅ runs                |
| [`http-server`](./http-server)       | A bounded routing/service smoke.                                            | ✅ runs and terminates |

## Language features on display

- **No `null`, no exceptions** — absence is `Option<T>`, failure is `Result<T, E>`,
  handled with `match` or combinators (`map`, `and_then`, `??`).
- **Sum types + exhaustive matching** — `enum`s with per-variant fields, checked so
  every case is handled.
- **Interfaces & operator overloading** — behavior attached with `impl … for …`,
  including the ambient operator prelude (`Plus`, `Comparable`, …).
- **Persistent updates** — collection updates and struct spreads return new values;
  existing values remain unchanged.
- **Lazy iteration** — `map`/`filter`/`take`/`fold` defined once on `Iterator`,
  composing without intermediate allocation.
- **Compiles to clean JavaScript** — the whole thing runs on any JS runtime.
