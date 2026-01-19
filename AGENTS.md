# AGENTS.md - Nymph Language Compiler

## Build & Test Commands

```bash
# Build entire workspace
cargo build

# Run all tests
cargo test

# Run type checker tests only
cargo test --lib types::tests

# Run single test
cargo test --lib types::tests::test_type_display_function

# Check without building
cargo check

# Format code (rustfmt)
cargo fmt

# Lint
cargo clippy --all-targets --all-features
```

## Architecture Overview

**Workspace**: `compiler` (Rust, edition 2024) + `cli` (binary using compiler)
**Language**: Nymph - a simple language with lexing → parsing → AST → type checking

**Core Modules**:

- `compiler/src/lexer/` - Tokenization (chumsky-based)
- `compiler/src/parser/` - AST generation from tokens
- `compiler/src/ast/` - AST definitions (expr, types, declarations, ops)
- `compiler/src/types/` - Type inference, checking, resolution (1500+ lines)
- `compiler/src/resolver/` - Name/symbol resolution
- `cli/src/` - CLI entry point using ariadne for error reporting

**Types Module**: Bidirectional type checking with context lookup, interface support, constraint validation, 25+ error types, 55+ tests.

## Code Style

**Formatting**: 2-space tabs (hard tabs, see rustfmt.toml)
**Language**: Rust with `#![warn(clippy::all)]`
**Conventions**:

- Use workspace dependencies (anyhow, itertools, tokio, tracing, etc.)
- Snake_case for functions/variables, CamelCase for types
- Errors: Use `Result<T, TypeError>` in type module, `anyhow::Result` elsewhere
- Imports: Organize by std, crates, then crate modules; use `use crate::` for internal
- Comments: Doc comments for public API, inline for complex logic
- Testing: Put tests in `#[cfg(test)] mod tests` with `#[test]` attribute
