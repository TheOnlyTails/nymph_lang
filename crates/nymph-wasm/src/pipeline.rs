//! Browser DTO adaptation around the stable compiler session pipeline.

use crate::diag::{CompileResult, LineIndex};

/// Parse, check, and (if error-free) lower + emit `source` to a JS module
/// string. Mirrors `nymph_compiler::compile`.
pub(crate) fn run_compile(source: &str) -> CompileResult {
	let report = nymph_compiler::compile_report(source, "playground");
	let index = LineIndex::new(source);
	let diagnostics = report
		.diagnostics
		.iter()
		.map(|d| index.to_diag(d))
		.collect();
	CompileResult {
		js: report.js,
		diagnostics,
	}
}

/// Parse and check `source`, returning every diagnostic (errors and
/// warnings alike) with no emission. Mirrors `nymph_compiler::check`.
pub(crate) fn run_check(source: &str) -> CompileResult {
	let checked = nymph_compiler::check(source, "playground");
	let index = LineIndex::new(source);
	let diagnostics = checked.iter().map(|d| index.to_diag(d)).collect();

	CompileResult {
		js: None,
		diagnostics,
	}
}
