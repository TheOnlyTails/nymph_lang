//! The playground-facing diagnostic and result shapes, plus the byte-offset
//! → line/col index used to fill them in.

use nymph_compiler::{Diagnostic, Severity};
use serde::Serialize;

/// A single diagnostic, shaped for a JS/TS consumer (a playground UI).
///
/// `start`/`end` are byte offsets into the source (matching
/// compiler spans); `start_line`/`start_col`/`end_line`/`end_col` are
/// 1-based, precomputed here so the playground doesn't need to re-derive them
/// from the raw offsets.
#[derive(Clone, Debug, Serialize)]
pub struct Diag {
	pub severity: &'static str,
	pub message: String,
	pub code: String,
	pub start: usize,
	pub end: usize,
	pub start_line: usize,
	pub start_col: usize,
	pub end_line: usize,
	pub end_col: usize,
}

/// The result of a `compile` call: the emitted JS (present only when there
/// were no error diagnostics), plus every diagnostic produced.
#[derive(Clone, Debug, Serialize)]
pub struct CompileResult {
	pub js: Option<String>,
	pub diagnostics: Vec<Diag>,
}

/// One token produced by the lexer, including its original spelling and span.
#[derive(Clone, Debug, Serialize)]
pub struct TokenView {
	pub kind: String,
	pub text: String,
	pub start: usize,
	pub end: usize,
	pub line: usize,
	pub col: usize,
}

/// Whether a compiler stage completed, failed, or was blocked by an earlier error.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StageStatus {
	Complete,
	Failed,
	Blocked,
}

/// A compact summary used by the visual pipeline in the docs playground.
#[derive(Clone, Debug, Serialize)]
pub struct StageView {
	pub name: &'static str,
	pub status: StageStatus,
	pub detail: String,
}

/// One expression and its resolved checker state.
#[derive(Clone, Debug, Serialize)]
pub struct TypeStateView {
	pub node: u32,
	pub source: String,
	pub type_: String,
	pub dispatch: Option<String>,
	pub method: Option<String>,
	pub start: usize,
	pub end: usize,
	pub line: usize,
	pub col: usize,
}

/// Debug information for every browser-observable compiler stage.
#[derive(Clone, Debug, Serialize)]
pub struct InspectionResult {
	pub tokens: Vec<TokenView>,
	pub ast: String,
	pub types: Vec<TypeStateView>,
	pub stages: Vec<StageView>,
	pub js: Option<String>,
	pub diagnostics: Vec<Diag>,
}

fn severity_str(severity: Severity) -> &'static str {
	match severity {
		Severity::Error => "error",
		Severity::Warning => "warning",
		Severity::Info => "info",
		Severity::Hint => "hint",
	}
}

/// A byte-offset → (1-based line, 1-based column) index over a source
/// string, built once per `compile`/`check` call.
pub(crate) struct LineIndex {
	/// Byte offset of the start of each line; `starts[0]` is always `0`.
	starts: Vec<usize>,
}

impl LineIndex {
	pub(crate) fn new(source: &str) -> Self {
		let mut starts = vec![0];
		for (i, b) in source.bytes().enumerate() {
			if b == b'\n' {
				starts.push(i + 1);
			}
		}
		Self { starts }
	}

	/// 1-based `(line, col)` for a byte `offset` into the indexed source.
	pub(crate) fn line_col(&self, offset: usize) -> (usize, usize) {
		let line = match self.starts.binary_search(&offset) {
			Ok(exact) => exact,
			Err(insertion) => insertion - 1,
		};
		let col = offset - self.starts[line] + 1;
		(line + 1, col)
	}

	pub(crate) fn to_diag(&self, d: &Diagnostic) -> Diag {
		let (start_line, start_col) = self.line_col(d.span.start);
		let (end_line, end_col) = self.line_col(d.span.end);
		Diag {
			severity: severity_str(d.severity),
			message: d.message.to_string(),
			code: d.code.to_string(),
			start: d.span.start,
			end: d.span.end,
			start_line,
			start_col,
			end_line,
			end_col,
		}
	}
}
