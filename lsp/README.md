# Nymph Language Server

A Language Server Protocol (LSP) implementation for the Nymph programming language.

## Features

- **Document Management**: Track open/close and updates to Nymph source files
- **Hover Information**: Display type information and symbol details on hover
- **Semantic Tokenization**: Syntax highlighting with semantic token types
  - Keywords, functions, variables, types, interfaces, and more
  - Token modifiers for declarations, definitions, builtins, and mutability
- **Symbol Navigation**: Document symbol provider for outline and go-to-symbol
- **Code Completion**: Basic keyword and built-in completions
- **Parsing Integration**: Full integration with the Nymph compiler's parser

## Architecture

### Modules

- **`server.rs`** - Main LSP server implementation using tower-lsp
- **`document.rs`** - Document management and parsing
- **`workspace.rs`** - Workspace state management
- **`analyzer.rs`** - Semantic analysis for symbol resolution
- **`semantic_tokens.rs`** - Semantic tokenization for highlighting
- **`symbols.rs`** - Symbol information types

### How It Works

1. **Document Lifecycle**: When a document is opened/changed, it's stored in the workspace and parsed using the Nymph compiler
2. **Analysis**: The semantic analyzer extracts symbols and type information from the parsed AST
3. **Tokenization**: Semantic tokens are generated for syntax highlighting
4. **LSP Responses**: The server responds to client requests with type hints, symbols, completions, etc.

## Building

```bash
cargo build --release --package nymph-lsp
```

The binary will be at `target/release/nymph-lsp`

## Running

```bash
./target/release/nymph-lsp
```

The server communicates via stdin/stdout using the LSP protocol.

## Supported LSP Features

### Server Capabilities

- ✅ Text Document Synchronization (full document sync)
- ✅ Hover Provider
- ✅ Semantic Tokens (legend + full document)
- ✅ Document Symbol Provider
- ✅ Completion Provider
- ✅ Definition Provider (stub)

### In Progress

- Symbol resolution and hover type information
- Improved semantic tokenization with AST integration
- Incremental text document sync
- Go-to-definition functionality
- Workspace symbol search

## Extension Integration

### VS Code

To use with VS Code, create a `.vscode/extensions/nymph-lsp/` directory and configure the extension client:

```json
{
	"language": "nymph",
	"scheme": "file",
	"initializationOptions": {},
	"documentSelector": [{ "language": "nymph", "scheme": "file" }]
}
```

Configure the server path in your LSP client extension.

### Neovim

For Neovim with `nvim-lspconfig`:

```lua
require('lspconfig').nymph.setup {
  cmd = {"/path/to/nymph-lsp"},
  filetypes = {"nymph"},
  root_dir = require('lspconfig.util').root_pattern(".git", "Cargo.toml"),
}
```

## Testing

Run the test suite:

```bash
cargo test --package nymph-lsp
```

## Dependencies

- `tower-lsp` - LSP protocol framework
- `tokio` - Async runtime
- `serde` / `serde_json` - JSON serialization
- `nymph-compiler` - Nymph language compiler
- `ecow` - Efficient copy-on-write strings
- `url` - URL parsing for file URIs

## Future Enhancements

- [ ] Full semantic analysis with type checking
- [ ] Format provider
- [ ] Rename provider
- [ ] References provider
- [ ] Call hierarchy
- [ ] Signature help
- [ ] Document/workspace diagnostics
- [ ] Incremental text sync
- [ ] Multi-file workspace support
- [ ] Macro expansion support
