//! A WASM-loadable frontend to the Nymph compiler: compile Nymph source to a
//! JavaScript module string plus structured diagnostics, for an in-browser
//! playground.
//!
//! This is the frontend twin of the native CLI/LSP — it shares the compiler
//! stable `nymph-compiler` session pipeline in library mode (no top-level
//! `main` required — the right default for a playground compiling arbitrary
//! snippets). Its in-memory SWC backend supports browser WASM synchronously.
//!
//! The exported [`compile`], [`check`], and [`inspect`] bindings return their
//! structured results serialized to a `JsValue` via `serde-wasm-bindgen`.

mod diag;
mod pipeline;

use wasm_bindgen::prelude::*;

/// Compile Nymph `source` to a JavaScript module string.
///
/// Returns a serialized `{ js, diagnostics }`: `js` is the emitted module
/// string when `source` parses and type-checks with no errors, `null`
/// otherwise; `diagnostics` carries every diagnostic from parsing and
/// checking (errors and warnings alike).
#[wasm_bindgen]
pub fn compile(source: &str) -> JsValue {
	serde_wasm_bindgen::to_value(&pipeline::run_compile(source))
		.expect("CompileResult is always representable as a JsValue")
}

/// Parse and check Nymph `source`, returning every diagnostic with no
/// emission (`js` is always `null`).
#[wasm_bindgen]
pub fn check(source: &str) -> JsValue {
	serde_wasm_bindgen::to_value(&pipeline::run_check(source))
		.expect("CompileResult is always representable as a JsValue")
}

/// Return lexer tokens, the parsed AST, stage status, diagnostics, and emitted
/// JavaScript for an interactive visual inspection of the compilation process.
#[wasm_bindgen]
pub fn inspect(source: &str) -> JsValue {
	serde_wasm_bindgen::to_value(&pipeline::run_inspect(source))
		.expect("InspectionResult is always representable as a JsValue")
}

#[cfg(test)]
mod tests {
	use super::pipeline::{run_check, run_compile, run_inspect};

	#[test]
	fn compile_clean_program_emits_js_with_no_error_diagnostics() {
		let result = run_compile("func add(a: int, b: int): int = a + b\n");

		assert!(
			result.js.as_deref().is_some_and(|js| !js.is_empty()),
			"expected non-empty emitted JS, got {:?}",
			result.js
		);
		assert!(
			result.diagnostics.iter().all(|d| d.severity != "error"),
			"expected no error diagnostics, got {:?}",
			result.diagnostics
		);
	}

	#[test]
	fn compile_type_error_program_returns_no_js_and_an_error_diagnostic() {
		let result = run_compile("func broken(): int = {\n\tlet x: int = \"hi\"\n\tx\n}\n");

		assert!(
			result.js.is_none(),
			"expected no emitted JS for a type error, got {:?}",
			result.js
		);
		let error = result
			.diagnostics
			.iter()
			.find(|d| d.severity == "error")
			.unwrap_or_else(|| panic!("expected an error diagnostic, got {:?}", result.diagnostics));
		assert_eq!(error.start_line, 2, "the type error is on line 2");
	}

	#[test]
	fn check_returns_same_diagnostics_as_compile_but_never_emits() {
		let source = "func broken(): int = {\n\tlet x: int = \"hi\"\n\tx\n}\n";

		let checked = run_check(source);
		let compiled = run_compile(source);

		assert!(checked.js.is_none(), "check() never emits");
		assert_eq!(
			checked.diagnostics.len(),
			compiled.diagnostics.len(),
			"check() and compile() should surface the same diagnostics"
		);
		assert!(checked.diagnostics.iter().any(|d| d.severity == "error"));
	}

	#[test]
	fn inspect_exposes_real_syntax_artifacts_and_pipeline_status() {
		let result = run_inspect("func add(a: int, b: int): int = a + b\n");

		assert!(result.tokens.iter().any(|token| token.text == "func"));
		assert!(result.ast.contains("Func"));
		assert!(result.types.iter().any(|entry| entry.type_ == "int"));
		assert_eq!(result.stages.len(), 4);
		assert!(result.js.is_some());
		assert!(result.diagnostics.is_empty());
	}
}
