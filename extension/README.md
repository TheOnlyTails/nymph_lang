# Nymph for Visual Studio Code

The Nymph extension provides syntax highlighting and language-server features for `.nym` source
files. It requires VS Code 1.100 or newer.

> [!WARNING]
> Nymph is still in development. Syntax and features may change. Please report problems in the
> [Nymph issue tracker](https://github.com/theonlytails/nymph_lang/issues).

## Install

Install the target-specific VSIX that matches both your operating system and CPU architecture. Each
package includes exactly one matching `nymph-lsp` executable:

| Operating system | Architecture  | VS Code target | Packaged executable    |
| ---------------- | ------------- | -------------- | ---------------------- |
| Linux            | x64           | `linux-x64`    | `server/nymph-lsp`     |
| Linux            | ARM64         | `linux-arm64`  | `server/nymph-lsp`     |
| Windows          | x64           | `win32-x64`    | `server/nymph-lsp.exe` |
| Windows          | ARM64         | `win32-arm64`  | `server/nymph-lsp.exe` |
| macOS            | Intel         | `darwin-x64`   | `server/nymph-lsp`     |
| macOS            | Apple silicon | `darwin-arm64` | `server/nymph-lsp`     |

For example, install a downloaded Linux x64 package with:

```bash
code --install-extension nymph-linux-x64.vsix
```

There is no universal VSIX. Production installation does not require Rust, a separate language
server installation, or network access when the extension starts.

## Startup

Opening a `.nym` file activates the extension. It selects the executable for the current host from
the installed package and starts it over stdio. The package uses `nymph-lsp.exe` on Windows and
`nymph-lsp` on Linux and macOS.

An unrecognized operating-system/architecture pair stops activation and displays an `Unsupported
Nymph LSP host` error directing the user to one of the six target-specific packages. A missing or
non-executable payload also stops activation and displays a specific reinstall or permissions
message; the extension does not silently search the workspace for another server.

Compiler diagnostics appear in the editor and the **Problems** panel. Language-client logs and server
stderr are available in the **Nymph Language Server** channel in the **Output** panel (**View →
Output**). To inspect protocol traffic, run **Developer: Set Log Level**, select **Nymph Language
Server**, and choose **Trace**. Startup failures appear as VS Code error notifications.

Hover uses the same checked snapshot as diagnostics. In a project, that includes project imports and
aliases, the embedded `std/...` modules, the ambient prelude, inferred generic substitutions, and
unsaved overlays for every open dependency. A saved `.nym` file outside a project is checked as a
one-file library with the ambient prelude; it has no project import graph. The extension currently
registers language-server features for `file:` documents only, not untitled editor buffers. Closing
a project file discards its overlay and refreshes diagnostics from the current disk source, including
affected importers; closing a loose or non-file document clears its diagnostics without reading it
from disk.

After initialization, the language server asks clients that support dynamic watched-file
registration to watch `**/*.nym` and `**/nymph.toml`. Creating, changing, or deleting an unopened
project source refreshes the compiler snapshot and affected diagnostics; manifest changes rerun
project discovery, including source-root and project-membership transitions. An open editor overlay
always remains authoritative over watcher events for the same module, including equivalent URI
spellings, until the overlay is closed. Clients without dynamic registration continue to support
normal open/change/close behavior, but cannot report external filesystem changes to the server.

Completion for ordinary identifiers uses the latest immutable project analysis snapshot. It offers
nearest lexical names first, then visible imported names (including aliases and unsaved dependency
overlays), same-module declarations, and keywords. Prefix filtering applies within those tiers.
Completion after `.` intentionally returns no members yet; member completion and auto-import edits
are not currently supported. Files outside a project retain lexical and same-file completion.

**Find All References** follows compiler-resolved semantic identity rather than spelling. It searches
every `.nym` file in the detected project, including unopened files, and uses unsaved open-buffer
overlays as authoritative source. Imports and aliases, value and type positions, enum patterns, and
qualified uses participate when they resolve to the selected declaration; shadowed or unrelated
same-named symbols do not. VS Code's include-declaration request setting is honored. A file outside a
project is limited to its isolated one-file analysis universe.

**Rename Symbol** uses that same compiler-resolved identity and edits the declaration and every use
in the project, without matching unrelated same-spelled or shadowed names. Import aliases participate
as references to the imported declaration: the source import token, alias token, alias uses, and
declaration are renamed together. User-written declarations and local bindings are renameable;
module names, builtins, prelude/synthetic symbols, unresolved or
ambiguous names, keywords, literals, and non-symbol labels are rejected. The replacement must lex as
exactly one Nymph identifier (so keywords, `_`, malformed, empty, and multi-token names are invalid).
Open authoritative buffers carry their current document versions in the workspace edit, while closed
files are unversioned and are reread from disk before any edit is returned. If an open overlay or a
closed project source changes while rename is being computed, the stale result is not published.

**Go to Symbol in Workspace** searches visible top-level declarations in synchronized manifest
projects, including unopened modules and unsaved open-file overlays. It excludes private items,
locals, implementation members, compiler-generated symbols, dependencies, and loose files. Matching
is case-sensitive: exact names rank before prefixes, followed by Jaro-Winkler fuzzy matches scoring
at least 0.70; equal results are ordered by URI, declaration range, and name. Searches return at most
100 results, while an empty query provides a deterministic project overview limited to 50.

Semantic highlighting consumes that same immutable project snapshot. Imported and ambient
functions, values, types, enum variants, and aliases therefore keep the same semantic kind as their
declarations, including when an unsaved dependency overrides its on-disk source. Highlighting is
best-effort for malformed projects: lexical tokens, comments, and string interpolations remain
available when semantic resolution is incomplete. The server advertises only full-document tokens
with its fixed legend; range and delta requests are not supported.

**Format Document** and **Format Selection** use Nymph's canonical style and the authoritative open
editor buffer, including unsaved changes. Formatting options such as tab size do not override that
style, and the language server never writes the source file. Malformed or incomplete input safely
produces no edits. VS Code's standard `editor.formatOnSave` setting can be enabled for Nymph files.

## Nymph files

`.nym` is the only Nymph source-file suffix. Functions use
`func name(params): ReturnType = body` syntax:

```nym
func add(a: int, b: int): int = a + b
```

See the
[function reference](https://github.com/TheOnlyTails/nymph_lang/blob/main/docs/reference/functions.md)
for parameters, inferred return types, blocks, methods, and closures.

## Troubleshooting

### The language server does not start

1. Open **View → Output** and select **Nymph Language Server**.
2. If the error reports a missing packaged executable, reinstall the VSIX for the target in the
   table above. Do not install the package for a different architecture.
3. If a Unix payload is not executable, reinstall the VSIX. Use `chmod +x` only for a local
   development override.
4. If the host is unsupported, no published package can run there. Use a supported host or the
   development override below with a compatible locally built server.

### Language features do not appear

- Confirm the file name ends in `.nym` and the status bar identifies the language as Nymph.
- Fix syntax errors shown in the editor or the **Problems** panel.
- Run **Developer: Reload Window** after reinstalling or changing development settings.

## Extension development

The following commands and settings are for contributors, not end users.

Install dependencies and compile the extension from the repository root:

```bash
pnpm install
pnpm --filter nymph compile
```

To run against a local language server, build it and set the machine-scoped `nymph.server.path`
setting to its absolute path:

```bash
cargo build \
  --package nymph-lsp
```

```json
{
	"nymph.server.path": "/absolute/path/to/nymph-lsp"
}
```

Use an `.exe` path on Windows. The override must exist and, on Linux or macOS, be executable. An
override is resolved before packaged-host selection, so it can also be used while developing on a
host that has no published VSIX. Clear the setting to test the packaged executable. Press `F5` from
the repository workspace to launch the Extension Development Host.

Run the static extension and documentation regression checks with:

```bash
pnpm --filter nymph test:unit
pnpm --filter nymph test:docs
```

### Build a target-specific VSIX

Cross-build a release server for one supported Rust target, stage that target's executable, and
pass the corresponding VS Code target to `vsce`. For Linux x64, from the repository root:

```bash
cargo build --release \
  -p nymph-lsp \
  --target x86_64-unknown-linux-gnu
pnpm --filter nymph stage:server linux-x64 ../target/x86_64-unknown-linux-gnu/release/nymph-lsp
cd extension
pnpm exec vsce package --no-dependencies --target linux-x64 --out nymph-linux-x64.vsix
node scripts/verify-vsix.cjs nymph-linux-x64.vsix linux-x64
```

The stage command removes any previous payload before copying the selected executable, and the
verification command checks that the VSIX contains exactly that executable with the required Unix
permissions. Use the Rust/VS Code target pairing in the
[packaging workflow](https://github.com/TheOnlyTails/nymph_lang/blob/main/.github/workflows/vscode.yml)
for the other supported packages.
