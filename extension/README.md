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
