//! A WASM-loadable frontend to the Nymph compiler: compile Nymph source to a
//! JavaScript module string plus structured diagnostics, for an in-browser
//! playground.
//!
//! This is the frontend twin of the native CLI/LSP — it shares the compiler
//! stable `nymph-compiler` session pipeline in library mode (no top-level
//! `main` required — the right default for a playground compiling arbitrary
//! snippets). Its in-memory SWC backend supports browser WASM synchronously.
//!
//! The exported [`compile`], [`check`], [`expand`], and [`inspect`] bindings return their
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

/// Return canonical fully expanded Nymph source plus structured diagnostics.
#[wasm_bindgen]
pub fn expand(source: &str) -> JsValue {
	serde_wasm_bindgen::to_value(&pipeline::run_expand(source))
		.expect("ExpandedSourceResult is always representable as a JsValue")
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
	use super::pipeline::{run_check, run_compile, run_expand, run_inspect};

	#[test]
	fn expand_returns_formatted_generated_nymph_and_imports() {
		let result = run_expand(
			"const func generate(): meta.Tokens = \\(import std/io func answer(): int = 42)\n$(generate())\n",
		);
		assert!(
			result
				.diagnostics
				.iter()
				.all(|diagnostic| diagnostic.severity != "error"),
			"{:?}",
			result.diagnostics
		);
		let source = result.source.expect("expanded source");
		assert!(source.contains("import std/io"), "{source}");
		assert!(source.contains("func answer(): int = 42"), "{source}");
		assert!(!source.contains("$("));
		assert!(!source.contains("const func"));
	}

	#[test]
	fn failed_expansion_clears_source_and_keeps_structured_diagnostics() {
		let result =
			run_expand("const func broken(): meta.Tokens = \\(func generated(): = 1)\n$(broken())\n");
		assert!(result.source.is_none());
		let diagnostic = result
			.diagnostics
			.iter()
			.find(|diagnostic| diagnostic.severity == "error")
			.expect("structured expansion diagnostic");
		assert!(!diagnostic.labels.is_empty());
		assert!(diagnostic.help.is_some());
		assert!(diagnostic.pretty.contains("playground:"));
		assert!(diagnostic.pretty.contains("func generated(): = 1"));
		assert!(diagnostic.pretty.contains("macro was defined here"));
		assert!(diagnostic.pretty.contains("Help:"));
		assert!(!diagnostic.pretty.contains('\u{1b}'));
	}

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
	fn compile_expands_const_tokens_through_the_browser_pipeline() {
		let result = run_compile(
			"const func make(value: int): meta.Tokens = \\(func answer(): int = $(value))\n$(make(42))\n",
		);

		assert!(
			result
				.diagnostics
				.iter()
				.all(|diagnostic| diagnostic.severity != "error")
		);
		assert!(
			result
				.js
				.as_deref()
				.is_some_and(|javascript| javascript.contains("answer")),
			"expected generated function in browser output, got {:?}",
			result.js
		);
	}

	#[test]
	fn compile_keeps_attached_targets_and_appends_each_sibling_output() {
		let result = run_compile(
			"const func first(target: meta.Struct): meta.Tokens = \\(func first_helper(): int = 20)\n\
			 const func second(target: meta.Struct): meta.Tokens = \\(func second_helper(): int = 22)\n\
			 $[first()] $[second()] struct Point(value: int)\n",
		);

		assert!(
			result
				.diagnostics
				.iter()
				.all(|diagnostic| diagnostic.severity != "error"),
			"unexpected diagnostics: {:?}",
			result.diagnostics
		);
		let javascript = result.js.expect("additive attachment output");
		let point = javascript.find("Point").expect("original target");
		let first = javascript.find("first_helper").expect("first output");
		let second = javascript.find("second_helper").expect("second output");
		assert!(point < first && first < second);
	}

	#[test]
	fn attached_redeclaration_diagnostics_keep_expansion_labels() {
		let result = run_check(
			"const func duplicate(target: meta.Struct): meta.Struct = target\n\
			 $[duplicate()] struct Point\n",
		);
		let diagnostic = result
			.diagnostics
			.iter()
			.find(|diagnostic| {
				diagnostic
					.message
					.contains("`Point` is defined more than once")
			})
			.unwrap_or_else(|| panic!("missing collision diagnostic: {result:?}"));
		assert!(
			diagnostic
				.labels
				.iter()
				.any(|label| label.message == "macro was defined here")
		);
		assert!(
			diagnostic
				.notes
				.iter()
				.any(|note| note.contains("expanded at"))
		);
		assert!(diagnostic.pretty.contains("macro was defined here"));
		assert!(diagnostic.pretty.contains("expanded at"));
		assert!(diagnostic.pretty.contains("Help:"));
	}

	#[test]
	fn generated_diagnostics_preserve_help_labels_and_expansion_trace() {
		let result =
			run_check("const func broken(): meta.Tokens = \\(func generated(): = 1)\n$(broken())\n");
		let diagnostic = result
			.diagnostics
			.iter()
			.find(|diagnostic| {
				diagnostic
					.notes
					.iter()
					.any(|note| note.contains("parsing compile-time expansion output"))
			})
			.unwrap_or_else(|| panic!("expected generated parse diagnostic: {result:?}"));
		assert!(diagnostic.help.is_some());
		assert!(
			diagnostic
				.labels
				.iter()
				.any(|label| label.message.contains("macro was defined here"))
		);
		assert!(
			diagnostic
				.notes
				.iter()
				.any(|note| note.contains("expanded at"))
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
		let result = run_inspect(
			"func add(a: int, b: int): int = a + b\nfunc main(): void = { let value = echo add(1, 2) }\n",
		);

		assert!(result.tokens.iter().any(|token| token.text == "func"));
		assert!(result.ast.contains("Func"));
		assert!(result.types.iter().any(|entry| entry.type_ == "int"));
		assert!(result.types.iter().any(|entry| entry.parent.is_some()));
		assert_eq!(result.stages.len(), 4);
		assert_eq!(
			result.expanded.as_deref(),
			Some(
				"func add(a: int, b: int): int = a + b\nfunc main(): void = {\n\tlet value = echo add(1, 2)\n}\n"
			)
		);
		assert!(
			result
				.expanded_tokens
				.iter()
				.any(|token| token.text == "func")
		);
		assert!(result.js.is_some());
		assert!(result.run.as_ref().is_some_and(|run| !run.task));
		assert!(result.diagnostics.is_empty());
	}

	#[test]
	fn inspect_updates_generated_expansion_and_clears_it_on_failure() {
		let expanded =
			run_inspect("const func make(): meta.Tokens = \\(func answer(): int = 42)\n$(make())\n");
		assert_eq!(
			expanded.expanded.as_deref(),
			Some("func answer(): int = 42\n")
		);
		assert!(
			expanded
				.expanded_tokens
				.iter()
				.any(|token| token.text == "answer")
		);

		let failed =
			run_inspect("const func broken(): meta.Tokens = \\(func generated(): = 1)\n$(broken())\n");
		assert!(failed.expanded.is_none());
		assert!(failed.expanded_tokens.is_empty());
		assert!(
			failed
				.diagnostics
				.iter()
				.any(|diagnostic| diagnostic.severity == "error")
		);
	}
}
