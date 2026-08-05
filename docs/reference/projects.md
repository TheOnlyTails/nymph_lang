# Projects and `nymph.toml`

A Nymph project is identified by a `nymph.toml` manifest. Tools discover the
nearest manifest by searching the starting directory and then each ancestor.
The directory containing the manifest is the project root.

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

Dependency declarations may use a version string or a table with `version`,
`path`, or `git`. Dependency fetching and resolution are not yet implemented.
