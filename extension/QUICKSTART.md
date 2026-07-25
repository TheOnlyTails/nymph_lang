# VS Code Extension - Quick Start

## Install & Test (5 minutes)

### 1. Install dependencies

```bash
cd extension
pnpm install
```

### 2. Compile extension

```bash
npm run compile
```

### 3. Build LSP server

```bash
cargo build --release --package nymph-lsp
```

### 4. Test in VS Code

1. Press `F5` in VS Code (Extension Development Host)
2. Create file: `test.nym`
3. Type:

```nymph
fn hello() {
  let msg = "world"
  return msg
}
```

4. Try:
   - **Hover** over `fn` keyword
   - **Ctrl+Space** for completions
   - **Ctrl+Shift+O** for symbols

## Development Mode

Watch for changes automatically:

```bash
# Terminal 1: Watch extension code
cd extension && npm run watch

# Terminal 2: Watch LSP server
cargo watch -x 'build --release --package nymph-lsp'

# Terminal 3: Open VS Code and press F5
```

## What Works

✅ Syntax highlighting (TextMate grammar)
✅ Hover information
✅ Code completion (keywords)
✅ Document symbols/outline
✅ Multi-file workspace

## Useful Commands

```bash
# Compile extension
npm run compile

# Watch extension
npm run watch

# Build LSP server (release)
cargo build --release --package nymph-lsp

# Build LSP server (debug - faster)
cargo build --package nymph-lsp

# Test extension in VS Code
# Press F5 in VS Code

# Package for distribution
cd extension && vsce package
```

## File Extensions

Nymph source files use the `.nym` extension.

## Troubleshooting

**"Server won't start"**

- Build server: `cargo build --release --package nymph-lsp`
- Check exists: `ls target/release/nymph-lsp`
- Make executable: `chmod +x target/release/nymph-lsp`

**"No syntax highlighting"**

- Reload VS Code: Ctrl+Shift+P → "Reload Window"
- Use the `.nym` extension

**"Hover shows nothing"**

- File must be valid Nymph code
- Hover shows basic info (can be enhanced)

## Architecture

```
Extension (TypeScript)
  ↓ (vscode-languageclient)
JSON-RPC
  ↓
LSP Server (Rust)
```

Binary locations:

- Release: `target/release/nymph-lsp` (13MB)
- Debug: `target/debug/nymph-lsp` (50MB+)

## Next Steps

- Edit `extension/src/extension.ts` for client features
- Edit `lsp/src/server.rs` for server features
- See `LSP_INTEGRATION.md` for detailed docs
- See `EXTENSION_INTEGRATION.md` in workspace root

Happy coding! 🚀
