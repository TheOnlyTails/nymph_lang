//! Integration tests for the `nymph-compiler` facade: `compile` and `check`,
//! plus their entry-mode counterparts `compile_entry` and `check_entry`.

use nymph_compiler::{check, check_entry, compile, compile_entry};

#[test]
fn compiles_a_valid_program() {
	let result = compile("func double(n: int): int = n * 2", "test");
	let js = result.expect("valid program should compile");
	assert!(js.contains("double"));
	assert!(js.contains("n * 2"));
}

#[test]
fn reports_type_errors() {
	let result = compile("func f(): int = true", "test");
	let diags = result.expect_err("type-mismatched program should not compile");
	assert!(!diags.is_empty());
}

#[test]
fn reports_parse_errors() {
	let result = compile("func f(: int = 1", "test");
	assert!(result.is_err());
}

#[test]
fn check_returns_all_diagnostics() {
	let diags = check("func f(): int = true", "test");
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.is_error()));
}

#[test]
fn check_is_clean_for_a_valid_program() {
	let diags = check("func double(n: int): int = n * 2", "test");
	assert!(!diags.iter().any(|d| d.is_error()));
}

// ── Entry mode (`check_entry` / `compile_entry`) ────────────────────────────
//
// GG1: entry mode is additive — the same source that's clean under library
// mode (`check`/`compile`) can error under entry mode if it has no valid
// top-level `main`, and vice versa is never true (entry mode is strictly more
// demanding than library mode).

#[test]
fn library_mode_is_clean_for_a_source_with_no_main() {
	let diags = check("func double(n: int): int = n * 2", "test");
	assert!(!diags.iter().any(|d| d.is_error()));
}

#[test]
fn entry_mode_errors_on_the_same_source_with_no_main() {
	let diags = check_entry("func double(n: int): int = n * 2", "test");
	assert!(
		diags.iter().any(|d| d.is_error()),
		"expected an entry-mode error for a source with no `main`, got: {diags:?}"
	);
}

#[test]
fn entry_mode_is_clean_for_a_valid_main() {
	let diags = check_entry("func main(): void = {}", "test");
	assert!(!diags.iter().any(|d| d.is_error()), "diags: {diags:?}");
}

#[test]
fn compile_entry_compiles_a_program_with_a_valid_main() {
	let result = compile_entry("func main(): void = {}", "test");
	let js = result.expect("valid entry program should compile");
	assert!(js.contains("main"));
}

#[test]
fn compile_entry_errors_on_a_program_with_no_main() {
	let result = compile_entry("func double(n: int): int = n * 2", "test");
	let diags = result.expect_err("a program with no `main` should not compile in entry mode");
	assert!(!diags.is_empty());
}

#[test]
fn compile_still_succeeds_without_a_main_in_library_mode() {
	// The same source `compile_entry` rejects above compiles fine in plain
	// library mode: `compile`/`check` are unaffected by entry validation.
	let result = compile("func double(n: int): int = n * 2", "test");
	assert!(result.is_ok(), "library mode should not require `main`");
}
