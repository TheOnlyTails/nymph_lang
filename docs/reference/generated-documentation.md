# Generated API documentation

`nymph doc` checks a project and writes a browsable, static HTML site for every
project module reachable from the manifest's `build.entry` module.

```sh
nymph doc
```

The default output directory is `target/nymph/doc` under the project root. Open
`index.html` directly in a browser; the generated site does not need an HTTP
server or JavaScript.

## Options

Use `--output` to replace a different directory:

```sh
nymph doc --output build/api
```

Private declarations, fields, and members are omitted by default. Include them
when generating internal documentation:

```sh
nymph doc --document-private-items
```

Pass `--open` to ask the system browser to open the generated index after the
new site has been published successfully:

```sh
nymph doc --open
```

Like other project commands, `doc` accepts the global `--manifest <PATH>` option
before or after the subcommand. The selected path is authoritative: Nymph does
not search for another manifest when that path is missing or invalid. Without
`--manifest`, discovery starts in the current directory and searches its
ancestors. Unlike `check`, `build`, and `run`, documentation has no loose-file
mode because it describes a project module graph.

## Output contract

- Output is deterministic for the same checked project.
- Declaration signatures come from checked semantic interfaces. Links between
  project types use exact resolved declaration identities, including when two
  modules or declarations use the same text name.
- Module and declaration text is HTML-escaped before rendering.
- Generation happens in a sibling staging directory. The destination is
  replaced only after every page and asset has been written. A parse, type,
  resolution, or staging failure leaves the previous destination tree intact.
- `--open` runs only after publication succeeds.

The site currently documents the reachable project closure rather than every
unreferenced source file under `src`, and it does not generate pages for the
embedded standard library or third-party packages.
