//! Integration tests for the `nymph-compiler` facade: `compile` and `check`.

use nymph_compiler::{check, compile};

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
