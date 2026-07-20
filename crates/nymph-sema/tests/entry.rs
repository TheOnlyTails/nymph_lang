//! Checker-phase validation of the program's entry point (`main`).
//!
//! `check_module_entry` is strictly additive: plain `check_module` (library
//! mode) never requires a `main` at all — every existing corpus/test caller of
//! `check_module` stays unaffected. Entry mode requires a top-level `func
//! main` taking no parameters, declaring no generics, and declaring no return
//! type other than `void`.

use nymph_sema::{check_module, check_module_entry};
use nymph_syntax::parse_module;

/// Parse and check `source` in entry mode, returning the checker's error
/// messages. Panics if the source fails to *parse* (these tests exercise the
/// checker, not the parser).
fn check_entry(source: &str) -> Vec<String> {
	let parsed = parse_module(source, "test");
	let parse_errors: Vec<_> = parsed
		.diagnostics
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		parse_errors.is_empty(),
		"source failed to parse: {parse_errors:?}\n---\n{source}"
	);
	check_module_entry(&parsed.tree)
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect()
}

fn assert_ok(source: &str) {
	let errors = check_entry(source);
	assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
}

fn assert_error_contains(source: &str, needle: &str) {
	let errors = check_entry(source);
	assert!(
		errors.iter().any(|e| e.contains(needle)),
		"expected an error containing {needle:?}, got: {errors:?}"
	);
}

#[test]
fn accepts_a_void_main_with_no_params() {
	assert_ok("func main(): void = {}");
}

#[test]
fn accepts_a_main_with_no_return_annotation() {
	assert_ok("func main() = {}");
}

#[test]
fn accepts_a_grouped_void_return_type() {
	assert_ok("func main(): (void) = {}");
}

#[test]
fn accepts_a_main_with_no_annotation_and_a_non_void_inferring_body() {
	// AST-declared-annotation-only rule: an unannotated `main` is accepted even
	// though its body infers a non-void type — only an explicit non-`void`
	// annotation is rejected.
	assert_ok("func main() = 42");
}

#[test]
fn rejects_a_missing_main() {
	assert_error_contains(
		"func add(a: int, b: int): int = a + b",
		"no `main` function found",
	);
}

#[test]
fn rejects_params_on_main() {
	assert_error_contains("func main(x: int): void = {}", "parameter");
}

#[test]
fn rejects_a_non_void_return_type() {
	assert_error_contains("func main(): int = 0", "void");
}

#[test]
fn rejects_a_generic_main() {
	assert_error_contains("func main<T>(): void = {}", "generic");
}

#[test]
fn does_not_accept_a_struct_method_named_main() {
	assert_error_contains(
		"struct Foo(x: int) { func main(): int = this.x }",
		"no `main` function found",
	);
}

#[test]
fn does_not_accept_an_external_func_named_main() {
	// An `external` func has no body to run, so it does not satisfy the
	// entry-point requirement even though it's a top-level declaration named
	// `main`.
	assert_error_contains(
		"external(main) func main(): void",
		"no `main` function found",
	);
}

#[test]
fn later_definition_does_not_shadow_a_top_level_main() {
	// `build_def_map`'s "later definition wins" rule means a later top-level
	// `struct main` sharing the name `main` claims the def-map slot away from
	// the earlier `func main` (and rightly reports a `Redefinition` error for
	// the name collision) -- but entry-main resolution must not be affected
	// by that: it walks `module.members` directly rather than consulting
	// `defs.by_name`, so the top-level `func main` is still found and
	// validated. A regression that resolved entry-main through
	// `defs.by_name` instead would resolve "main" to the struct (the def map's
	// winner) and incorrectly report "no `main` function found" here.
	let errors = check_entry("func main(): void = {}\nstruct main(x: int)");
	assert!(
		errors.iter().any(|e| e.contains("defined more than once")),
		"expected the name collision itself to be reported as a redefinition, got: {errors:?}"
	);
	assert!(
		!errors
			.iter()
			.any(|e| e.contains("no `main` function found")),
		"entry-main resolution incorrectly failed to find the top-level `func main`, got: {errors:?}"
	);
}

#[test]
fn library_mode_never_requires_main() {
	let parsed = parse_module("func add(a: int, b: int): int = a + b", "test");
	let diags = check_module(&parsed.tree).diags;
	assert!(!diags.iter().any(|d| d.is_error()));
}

#[test]
fn missing_main_diagnostic_carries_a_help_hint() {
	let parsed = parse_module("func add(a: int, b: int): int = a + b", "test");
	let checked = check_module_entry(&parsed.tree);
	let diag = checked
		.diags
		.iter()
		.find(|d| d.is_error())
		.expect("expected an error");
	assert!(
		diag.help.is_some(),
		"expected a help hint on the missing-main diagnostic"
	);
}

#[test]
fn missing_main_diagnostic_renders_without_panicking_on_an_empty_source() {
	// The missing-main diagnostic has no AST span to anchor on, so it is
	// anchored at Span::new(0, 0) — verify this renders cleanly (ariadne
	// panics if a diagnostic's span end exceeds the source length) even for
	// the degenerate empty-file case.
	let parsed = parse_module("", "test");
	let checked = check_module_entry(&parsed.tree);
	let rendered = nymph_diagnostics::render("test", "", &checked.diags);
	assert!(rendered.contains("main"), "rendered was: {rendered}");
}

#[test]
fn params_and_non_void_return_are_both_reported_when_both_present() {
	let errors = check_entry("func main(x: int): int = x");
	assert!(
		errors.iter().any(|e| e.contains("parameter")),
		"expected a parameters error, got: {errors:?}"
	);
	assert!(
		errors.iter().any(|e| e.contains("void")),
		"expected a return-type error, got: {errors:?}"
	);
}
