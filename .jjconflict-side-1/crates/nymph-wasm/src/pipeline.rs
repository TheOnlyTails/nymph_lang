//! Browser DTO adaptation around the stable compiler session pipeline.

use nymph_compiler::Severity;
use nymph_syntax::{lex, parse_module};

use crate::diag::{CompileResult, InspectionResult, LineIndex, StageStatus, StageView, TokenView};

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

/// Inspect the concrete outputs that are useful while debugging a compilation.
/// The final compile still goes through `nymph-compiler`; direct syntax calls only
/// expose the lexer and parser artifacts which that stable facade intentionally hides.
pub(crate) fn run_inspect(source: &str) -> InspectionResult {
	let index = LineIndex::new(source);
	let lexed = lex(source);
	let tokens = lexed
		.tokens
		.iter()
		.map(|token| {
			let span = token.1;
			let (line, col) = index.line_col(span.start);
			TokenView {
				kind: format!("{:?}", token.0),
				text: source
					.get(span.start..span.end)
					.unwrap_or_default()
					.to_owned(),
				start: span.start,
				end: span.end,
				line,
				col,
			}
		})
		.collect::<Vec<_>>();
	let lex_failed = lexed
		.diagnostics
		.iter()
		.any(|diagnostic| diagnostic.severity == Severity::Error);

	let parsed = parse_module(source, "playground");
	let parse_failed = parsed
		.diagnostics
		.iter()
		.any(|diagnostic| diagnostic.severity == Severity::Error);
	let ast = format!("{:#?}", parsed.tree);

	let report = nymph_compiler::compile_report(source, "playground");
	let compile_failed = report
		.diagnostics
		.iter()
		.any(|diagnostic| diagnostic.severity == Severity::Error);
	let diagnostics = report
		.diagnostics
		.iter()
		.map(|diagnostic| index.to_diag(diagnostic))
		.collect::<Vec<_>>();

	let syntax_failed = lex_failed || parse_failed;
	let stages = vec![
		StageView {
			name: "Lex",
			status: if lex_failed {
				StageStatus::Failed
			} else {
				StageStatus::Complete
			},
			detail: format!(
				"{} token{}",
				tokens.len(),
				if tokens.len() == 1 { "" } else { "s" }
			),
		},
		StageView {
			name: "Parse",
			status: if lex_failed {
				StageStatus::Blocked
			} else if parse_failed {
				StageStatus::Failed
			} else {
				StageStatus::Complete
			},
			detail: format!(
				"{} top-level declaration{}",
				parsed.tree.members.len(),
				if parsed.tree.members.len() == 1 {
					""
				} else {
					"s"
				}
			),
		},
		StageView {
			name: "Analyze",
			status: if syntax_failed {
				StageStatus::Blocked
			} else if compile_failed {
				StageStatus::Failed
			} else {
				StageStatus::Complete
			},
			detail: if syntax_failed {
				"waiting for valid syntax".to_owned()
			} else if compile_failed {
				"type or semantic errors found".to_owned()
			} else {
				"types and names resolved".to_owned()
			},
		},
		StageView {
			name: "Lower & emit",
			status: if report.js.is_some() {
				StageStatus::Complete
			} else {
				StageStatus::Blocked
			},
			detail: report.js.as_ref().map_or_else(
				|| "waiting for a clean analysis".to_owned(),
				|js| format!("{} bytes of JavaScript", js.len()),
			),
		},
	];

	InspectionResult {
		tokens,
		ast,
		stages,
		js: report.js,
		diagnostics,
	}
}
