# Projects and `nymph.toml`

A Nymph project is identified by a `nymph.toml` manifest. Tools discover the
nearest manifest by searching the starting directory and then each ancestor.
The directory containing the manifest is the project root.

## Creating a project

Create a binary package at a new destination with:

```sh
nymph new hello-world
```

The destination basename becomes the package name. Names must start with a
lowercase ASCII letter and may contain only lowercase ASCII letters, digits,
and hyphens. Missing parent directories are created. An existing destination
is accepted only when it is an empty directory; files and nonempty directories
are refused without modification.

The generated binary tree and exact initial source are:

```text
hello-world/
├── nymph.toml
└── src/
    └── main.nym
```

```toml
[package]
name = "hello-world"
version = "0.1.0"
```

```nym
func main(): void = {}
```

Pass `--lib` to generate `src/lib.nym` instead:

```sh
nymph new hello-lib --lib
```

```text
hello-lib/
├── nymph.toml
└── src/
    └── lib.nym
```

```nym
public func hello(): string = "Hello, world!"
```

Git is initialized by default, but no initial commit is created. Use
`--no-git` to skip repository initialization. Project creation is
noninteractive and staged before publication, so a missing or failing Git
executable and other initialization errors do not leave a partial destination.

The generated binary can be checked from its root with `nymph check`. Until
library-target metadata is part of the manifest schema, check a generated
library explicitly with `nymph check src/lib.nym`.

Discovery has three outcomes: a valid manifest selects project mode; finding
no manifest in the search chain permits loose-file mode; and finding a
manifest that cannot be read or validated is a fatal project error. A broken
nearby manifest is authoritative and must be fixed (or the source moved out of
its search chain); tools never ignore it and retry the source as a loose file.

Pass the global `--manifest <PATH>` option to select a manifest explicitly.
The selected path is authoritative: tools read exactly that file, do not
discover `nymph.toml` or fall back to loose-file mode, and report any read or
parse error for that path. There is no `--config` alias or environment-based
project configuration.

Every manifest requires a `[package]` table:

```toml
[package]
name = "hello"
version = "0.1.0"
# src = "src" # optional; defaults to src

[dependencies] # optional; defaults to empty
utilities = "^1.0"

[build] # optional
entry = "main.nym" # optional; defaults to main.nym
```

`package.src` is relative to the project root and defines the source root.
`build.entry` is a contained `.nym` path relative to that source root; it may
not be absolute or escape with `..`. Source files below the source root map to
canonical compiler module paths by removing `.nym` and joining components with
`/` (for example, `src/network/http.nym` maps to `network/http`).

## Selecting a command target

`run`, `build`, and `check` use the same target-selection rules:

| Project found? | File argument? | Selected target |
| -------------- | -------------- | --------------- |
| Yes | Omitted | The manifest's `build.entry`, relative to `package.src` |
| Yes | Explicit | That file, which must be below the project's source root |
| No | Explicit | That loose `.nym` file |
| No | Omitted | Error: pass a `.nym` file or run inside a project |

The manifest entry is an executable entry module for `build` and `check`;
other explicit project files and loose files are libraries. `run` always
requires the selected target to declare a valid `main`. Entry selection never
depends on whether a file happens to be named `main.nym`.

`check` resolves the same complete import graph and uses the same embedded
ambient core and `std/…` sources as `build` and `run`. It stops after parsing,
binding, and semantic checking: it emits no JavaScript, creates no `.mjs`
artifact, and executes neither the selected module nor Node.

From a project directory, all three commands select `build.entry`:

```sh
nymph check
nymph build
nymph run
```

An explicit project file or a standalone loose file remains supported:

```sh
nymph check src/network/http.nym
nymph build scratch.nym
nymph run script.nym
```

Because manifest fields are based on the selected manifest's directory, an
explicit manifest works from anywhere. As a global option, it may appear
before or after the subcommand:

```sh
nymph --manifest ../hello/nymph.toml check
nymph build --manifest ../hello/nymph.toml
nymph run --manifest ../hello/nymph.toml
```

An explicit source argument is still resolved within the selected manifest's
`package.src`; a source outside that root is rejected rather than causing
discovery of another project.

## Interactive evaluation

`nymph repl` starts a persistent read-eval-print loop. It discovers the nearest
`nymph.toml` from the current directory and resolves `@/…` imports from that
project's `package.src`. `--manifest <PATH>` selects exactly that manifest and
never falls back. Only the absence of a discovered manifest starts a loose
session; loose sessions still include ambient core and embedded `std/…` modules.

Declarations, imports, and their evaluated values remain available to later
submissions. A newer declaration may shadow a name without changing the
meaning of older declarations that captured the previous binding. Each
submission is staged through the normal project compiler and committed only
after its generated program executes successfully, so syntax, type, compiler,
and runtime failures preserve the last good session state.

In a terminal the primary prompt is `> ` and incomplete syntax uses `... `.
Continuation is determined by lexer/parser end-of-input state, so multiline
blocks, string interpolation, and block comments do not require an extra blank
line. Ctrl-D (EOF) exits cleanly. Values are rendered with Nymph's `Debug`
semantics, including user implementations, rather than JavaScript object
inspection.

Redirected input prints neither the banner nor prompts. This makes transcripts
deterministic and scriptable:

```sh
printf 'let x = 40\nx + 2\n' | nymph repl
# 42
```

## Formatting sources

`nymph format [FILES...]` rewrites explicitly named `.nym` files in normalized,
deterministic order. Duplicate paths are processed once. With no files it
discovers the authoritative project and formats every `.nym` file below
`package.src`, without following symlinks or entering generated/dependency
directories. Use `--manifest <PATH>` to select a project authoritatively;
explicit files outside that project's source root are rejected.

Formatting writes atomically and leaves already formatted files untouched.
`nymph format --check [FILES...]` performs the same selection without writing.
Exit status 0 means success (and, in check mode, no changes), 1 means check mode
found files that would change, and 2 means selection, I/O, or syntax errors.
Errors do not prevent other selected files from being checked or formatted.

Dependency declarations may use a version string or a table with `version`,
`path`, or `git`. Dependency fetching and resolution are not yet implemented.
