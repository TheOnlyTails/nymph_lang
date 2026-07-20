//! The single-module compile pipeline, replicated from
//! `nymph-compiler`'s `compile`/`check` (library mode — no top-level `main`
//! required, the right default for a playground compiling arbitrary
//! snippets).
//!
//! Deliberately does NOT depend on `nymph-compiler` (it pulls a native
//! bundler + async runtime that don't target `wasm32-unknown-unknown`);
//! instead this depends directly on `nymph-syntax`/`nymph-sema`/
//! `nymph-codegen`/`nymph-diagnostics`/`nymph-ast` and walks the same four
//! stages: parse → check → (lower → emit, only when error-free).

use nymph_diagnostics::Diagnostic;

use crate::diag::{CompileResult, Diag, LineIndex};
use crate::prelude::ops_prelude;

fn to_diags(index: &LineIndex, batches: [&[Diagnostic]; 2]) -> Vec<Diag> {
	batches
		.into_iter()
		.flatten()
		.map(|d| index.to_diag(d))
		.collect()
}

/// Parse, check, and (if error-free) lower + emit `source` to a JS module
/// string. Mirrors `nymph_compiler::compile`.
pub(crate) fn run_compile(source: &str) -> CompileResult {
	let parsed = nymph_syntax::parse_module(source, "playground");
	let prelude = std::slice::from_ref(ops_prelude());
	let checked = nymph_sema::check_module_with_prelude(&parsed.tree, prelude);

	let has_errors = parsed.diagnostics.iter().any(Diagnostic::is_error)
		|| checked.diags.iter().any(Diagnostic::is_error);

	let index = LineIndex::new(source);
	let diagnostics = to_diags(&index, [&parsed.diagnostics, &checked.diags]);

	let js = (!has_errors).then(|| {
		nymph_codegen::emit(&nymph_sema::lower_hir_with_prelude(
			&parsed.tree,
			prelude,
			&checked,
		))
	});

	CompileResult { js, diagnostics }
}

/// Parse and check `source`, returning every diagnostic (errors and
/// warnings alike) with no emission. Mirrors `nymph_compiler::check`.
pub(crate) fn run_check(source: &str) -> CompileResult {
	let parsed = nymph_syntax::parse_module(source, "playground");
	let prelude = std::slice::from_ref(ops_prelude());
	let checked = nymph_sema::check_module_with_prelude(&parsed.tree, prelude);

	let index = LineIndex::new(source);
	let diagnostics = to_diags(&index, [&parsed.diagnostics, &checked.diags]);

	CompileResult {
		js: None,
		diagnostics,
	}
}
