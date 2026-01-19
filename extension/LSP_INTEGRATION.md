# Nymph LSP Extension Integration

This document explains the VS Code extension's integration with the Nymph Language Server.

## Architecture

```
VS Code Extension (TypeScript)
    ↓ (vscode-languageclient)
JSON-RPC over stdio
    ↓
Nymph Language Server (Rust)
    ├─ Workspace management
    ├─ Document parsing
    └─ Analysis & type checking
```

## Files

### Extension Code

- **src/extension.ts** - Main extension entry point
  - Launches the LSP server as a child process
  - Configures language client options
  - Handles activation/deactivation

### Configuration

- **package.json** - VS Code extension manifest
  - Declares `.nym` and `.nymph` file extensions
  - Adds `onLanguage:nymph` activation event
  - Specifies LSP server command and transport
  - Includes dependencies (vscode-languageclient)

- **tsconfig.json** - TypeScript compiler configuration
  - Targets ES2020
  - Outputs to `out/` directory
  - Strict type checking enabled

### Build & Debug

- **.vscode/launch.json** - Debug configuration
  - "Extension Development Host" - test the extension in isolated VS Code
  - "Attach to Language Server" - debug the server process (optional)

- **.vscode/tasks.json** - Build automation
  - `npm: compile - extension` - Compile TypeScript to JavaScript
  - `npm: watch - extension` - Watch mode for development
  - `Build LSP Server (release)` - Build optimized server
  - `Build LSP Server (debug)` - Build debug server

## Getting Started

### 1. Install Dependencies

```bash
cd extension
pnpm install
```

### 2. Compile the Extension

```bash
cd extension
npm run compile
```

Or use watch mode for development:

```bash
npm run watch
```

### 3. Build the LSP Server

Debug build (faster):

```bash
cargo build --package nymph-lsp
```

Release build (optimized):

```bash
cargo build --release --package nymph-lsp
```

### 4. Test the Extension

Option A: Launch in VS Code

1. Open the workspace root in VS Code
2. Press `F5` to launch Extension Development Host
3. Open a `.nym` or `.nymph` file
4. Features should work: hover, completion, symbols, etc.

Option B: Manual testing

1. Run the LSP server directly: `./target/release/nymph-lsp`
2. Send LSP messages via stdin (for manual testing)

### 5. Debug

**Extension code:**

- Set breakpoints in `src/extension.ts`
- They'll work in the Extension Development Host

**LSP server:**

- Use `eprintln!` macros for logging to stderr
- Check "Nymph Language Server" output panel in VS Code
- Set `RUST_LOG=nymph_lsp=debug` environment variable

## How It Works

### Activation Flow

1. **VS Code starts** - reads `package.json`
2. **User opens .nymph file** - triggers `onLanguage:nymph` activation
3. **activate()** function runs:
   - Resolves LSP server binary path from build output
   - Creates `LanguageClient` with stdio transport
   - Calls `client.start()`
   - Server process launches with JSON-RPC protocol
4. **Server ready** - responds to hover, completion, etc.

### Command Execution

```
VS Code User Action (e.g., hover)
    ↓
VS Code UI -> Language Client
    ↓
JSON-RPC Request (stdout)
    ↓
LSP Server Process
    ↓
Process Handler (e.g., hover())
    ↓
JSON-RPC Response (stdout)
    ↓
Language Client -> VS Code UI
```

## Configuration

### Server Binary Discovery

The extension looks for the LSP server at:

1. **Release build** (preferred): `../target/release/nymph-lsp`
2. **Debug build** (fallback): `../target/debug/nymph-lsp`

Both are relative to the extension's directory.

### File Extensions

Supported extensions (from package.json):

- `.nym` - Primary extension
- `.nymph` - Alternative extension

To add more, update the `extensions` array in `package.json`:

```json
"extensions": [".nym", ".nymph", ".nx"]
```

### Language ID

All configurations use `nymph` as the language ID. This must match:

- `package.json` → `languages[0].id`
- `extension.ts` → `documentSelector`
- LSP server configuration in other editors

## Dependencies

### Runtime

- **vscode-languageclient** (^9.0.1) - LSP client library
  - Handles JSON-RPC protocol
  - Manages server process lifecycle
  - Provides TypeScript types for LSP

### Development

- **typescript** (^5.3.3) - Compile TypeScript
- **@types/vscode** - VS Code API types
- **@types/node** - Node.js types

## Troubleshooting

### "Failed to start Nymph Language Server"

Check the extension's output panel:

1. Press `Ctrl+Shift+U` to open output panel
2. Look for errors from "Nymph Language Server"

Common causes:

- Binary not built: `cargo build --release --package nymph-lsp`
- Binary missing: Check `target/release/nymph-lsp` exists
- Binary not executable: `chmod +x target/release/nymph-lsp`
- TypeScript not compiled: `npm run compile` in extension folder

### "No syntax highlighting"

- Ensure you opened a `.nym` or `.nymph` file
- Check that `nymph.tmLanguage.json` exists in `syntaxes/`
- Reload VS Code window (`Ctrl+Shift+P` → "Reload Window")

### "Hover shows nothing"

- File must be syntactically valid (no parse errors)
- Hover provider is enabled (see `server.rs`)
- Try hovering over a keyword like `let` or `fn`

### "No code completion"

- Completion is available (basic keyword list)
- Try `Ctrl+Space` to trigger completion
- Check output panel for errors

## Publishing

To publish the extension to VS Code Marketplace:

```bash
# Install vsce
npm install -g @vscode/vsce

# In extension directory
cd extension
vsce publish
```

This requires:

1. Publisher account on VS Code Marketplace
2. Personal Access Token (PAT) from Azure DevOps
3. Updated version in `package.json`
4. Compiled binary in `../target/release/nymph-lsp`

## Advanced

### Custom Settings

To add extension settings, update `package.json`:

```json
"configuration": [
  {
    "title": "Nymph Language Server",
    "properties": {
      "nymph.trace.server": {
        "type": "string",
        "default": "off",
        "enum": ["off", "messages", "verbose"]
      }
    }
  }
]
```

Then access in code:

```typescript
const trace = workspace.getConfiguration("nymph").get("trace.server");
```

### Environment Variables

Pass environment variables to the server:

```typescript
const serverOptions: ServerOptions = {
	run: {
		command: serverModule,
		transport: TransportKind.stdio,
		options: {
			env: {
				...process.env,
				RUST_LOG: "nymph_lsp=debug",
			},
		},
	},
	// ...
};
```

### Multiple LSP Features

The current extension supports:

- ✅ Semantic tokens (syntax highlighting)
- ✅ Hover information
- ✅ Code completion
- ✅ Document symbols
- 🔄 Go-to-definition (in development)
- 🔄 Find references (in development)

To enable new features:

1. Implement in `lsp/src/server.rs`
2. Test with LSP client
3. Optionally add UI shortcuts in extension

## References

- [VS Code Extension API](https://code.visualstudio.com/api)
- [LSP Specification](https://microsoft.github.io/language-server-protocol/)
- [vscode-languageclient Docs](https://github.com/microsoft/vscode-languageclient)
- [Nymph Language Server](../lsp/)
