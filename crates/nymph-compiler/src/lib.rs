//! The Nymph compiler pipeline: a thin facade over the individual compiler
//! crates (`nymph-syntax`, `nymph-sema`, `nymph-codegen`) that exposes the
//! top-level API for parsing, checking, and compiling Nymph source.
//!
//! The pipeline runs in four stages:
//!
//! 1. **Parse** ([`nymph_syntax::parse_module`]) — source text to an AST
//!    ([`nymph_ast::decl::Module`]), plus any lex/parse diagnostics.
//! 2. **Check** ([`nymph_sema::check_module`]) — name resolution and type
//!    checking over the AST, producing diagnostics and type annotations.
//! 3. **Lower** ([`nymph_sema::lower_hir`]) — the checked AST to HIR.
//! 4. **Emit** ([`nymph_codegen::emit`]) — HIR to a JavaScript module string.
//!
//! [`compile`] runs the full pipeline and only lowers/emits when parsing and
//! checking are error-free. [`check`] runs just the parse and check stages
//! and returns every diagnostic (errors and warnings alike), which is the
//! entry point tooling such as an LSP should use.

pub use nymph_diagnostics::{Diagnostic, Severity};

/// Compile Nymph `source` to a JavaScript module string.
///
/// `path` is the module path used to anchor diagnostics (e.g. for rendering
/// or LSP URIs) — it does not need to correspond to a real file.
///
/// Runs the full pipeline: parse → check → lower → emit. If parsing or
/// checking produces any error diagnostics, lowering and emission are
/// skipped and this returns `Err` with those diagnostics (parse errors
/// followed by check errors). Warnings do not prevent compilation and are
/// discarded here — use [`check`] to observe them.
///
/// # Errors
///
/// Returns `Err` with all error-severity diagnostics from parsing and
/// checking if the source fails to parse or type-check.
pub fn compile(source: &str, path: &str) -> Result<String, Vec<Diagnostic>> {
	let parsed = nymph_syntax::parse_module(source, path);
	let mut diags: Vec<Diagnostic> = parsed
		.diagnostics
		.iter()
		.filter(|d| d.is_error())
		.cloned()
		.collect();

	let checked = nymph_sema::check_module(&parsed.tree);
	diags.extend(checked.diags.iter().filter(|d| d.is_error()).cloned());

	if !diags.is_empty() {
		return Err(diags);
	}

	Ok(nymph_codegen::emit(&nymph_sema::lower_hir(
		&parsed.tree,
		&checked,
	)))
}

/// Parse and check Nymph `source`, returning every diagnostic produced.
///
/// `path` is the module path used to anchor diagnostics (e.g. for rendering
/// or LSP URIs) — it does not need to correspond to a real file.
///
/// Unlike [`compile`], this does not filter by severity and does not lower
/// or emit: it runs parse and check only, and returns all diagnostics from
/// both stages (parse diagnostics followed by check diagnostics), including
/// warnings. This is the entry point tooling and language servers should use
/// to surface the full diagnostic picture for a source file.
pub fn check(source: &str, path: &str) -> Vec<Diagnostic> {
	let parsed = nymph_syntax::parse_module(source, path);
	let mut diags = parsed.diagnostics;

	let checked = nymph_sema::check_module(&parsed.tree);
	diags.extend(checked.diags);

	diags
}
