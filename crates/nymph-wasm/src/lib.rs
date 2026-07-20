//! A WASM-loadable frontend to the Nymph compiler: compile Nymph source to a
//! JavaScript module string plus structured diagnostics, for an in-browser
//! playground.
//!
//! This is the frontend twin of the native CLI/LSP — it shares the compiler
//! crates (`nymph-syntax`, `nymph-sema`, `nymph-codegen`, `nymph-diagnostics`,
//! `nymph-ast`), not `nymph-compiler` itself (which pulls a native bundler +
//! async runtime that don't target `wasm32-unknown-unknown`; see
//! [`pipeline`]). The pipeline is a faithful replication of
//! `nymph_compiler::compile`/`check` in library mode (no top-level `main`
//! required — the right default for a playground compiling arbitrary
//! snippets).
//!
//! The two exported bindings, [`compile`] and [`check`], return a
//! [`diag::CompileResult`] serialized to a `JsValue` via `serde-wasm-bindgen`.

mod diag;
mod pipeline;
mod prelude;

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

#[cfg(test)]
mod tests {
	use super::pipeline::{run_check, run_compile};

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
}
