# AGENTS.md - Nymph Language Compiler

## Build & Test Commands

- **Build**: `cargo build` | **Check**: `cargo check` | **Format**: `cargo fmt` | **Lint**: `cargo clippy --all-targets --all-features`
- **All tests**: `cargo nextest run` | **Module tests**: `cargo test --lib types::tests` | **Single test**: `cargo test --lib types::tests::test_type_display_function`

## Architecture Overview

**Workspace**: `compiler` (library), `cli` (binary, ariadne error reporting), `lsp` (language server via tower-lsp)
**Pipeline**: lexing (chumsky) → parsing → AST → name resolution → type checking → JS transpilation (oxc)
**Compiler modules**: `lexer/`, `parser/`, `ast/` (expr, types, declarations, ops), `resolver/`, `types/` (bidirectional type checking, `TypeError` enum), `transpiler/` (emits JS via oxc AST), `db.rs` (salsa incremental DB), `queries.rs`
**Other**: `extension/` (VS Code extension), `stdlib/` (standard library), `docs/` (documentation site)

## Code Style

- **Formatting**: Hard tabs, 2-space width (rustfmt.toml). `#![warn(clippy::all)]`, inlined format args enforced (clippy.toml).
- **Naming**: snake_case functions/variables, CamelCase types. Workspace deps in root Cargo.toml.
- **Errors**: `Result<T, TypeError>` in type module, `anyhow::Result` elsewhere. Strings use `ecow::EcoString`.
- **Imports**: Group std → external crates → `crate::` internals. Tests in `#[cfg(test)] mod tests`.
