# Projects and `nymph.toml`

A Nymph project is identified by a `nymph.toml` manifest. Tools discover the
nearest manifest by searching the starting directory and then each ancestor.
The directory containing the manifest is the project root.

Discovery has three outcomes: a valid manifest selects project mode; finding
no manifest in the search chain permits loose-file mode; and finding a
manifest that cannot be read or validated is a fatal project error. A broken
nearby manifest is authoritative and must be fixed (or the source moved out of
its search chain); tools never ignore it and retry the source as a loose file.

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

Dependency declarations may use a version string or a table with `version`,
`path`, or `git`. Dependency fetching and resolution are not yet implemented.
