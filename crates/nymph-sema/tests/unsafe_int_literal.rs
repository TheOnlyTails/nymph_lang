//! `TypeError::IntLiteralUnsafe`: a source `int`/`uint` literal whose magnitude
//! exceeds `Number.MAX_SAFE_INTEGER` (`2^53 - 1`) warns, since Nymph's `int`/`uint`
//! are JS doubles at runtime and can't represent it exactly.

use nymph_diagnostics::Severity;
use nymph_sema::check_module;
use nymph_syntax::parse_module;

/// Parse and check `source`, returning every diagnostic the checker produced
/// (errors AND warnings — unlike `check.rs`'s error-only helper, this crate's
/// tests need to see the warning this file is about).
fn check(source: &str) -> Vec<nymph_diagnostics::Diagnostic> {
	let parsed = parse_module(source, "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"source failed to parse: {:?}",
		parsed.diagnostics
	);
	check_module(&parsed.tree).diags
}

#[test]
fn unsafe_positive_int_literal_warns() {
	// 2^53, one past the safe-integer bound.
	let diags = check("func f(): int = 9007199254740992");
	let diag = diags
		.iter()
		.find(|d| d.message.contains("9007199254740992"))
		.unwrap_or_else(|| panic!("expected a diagnostic naming the literal, got: {diags:?}"));
	assert_eq!(diag.severity, Severity::Warning);
	assert!(
		diag
			.help
			.as_deref()
			.is_some_and(|h| h.contains("JS doubles")),
		"expected a help hint explaining Nymph ints are JS doubles, got: {:?}",
		diag.help
	);
}

#[test]
fn unsafe_uint_literal_warns() {
	let diags = check("func f(): uint = 9007199254740992u");
	let diag = diags
		.iter()
		.find(|d| d.message.contains("9007199254740992"))
		.unwrap_or_else(|| panic!("expected a diagnostic naming the literal, got: {diags:?}"));
	assert_eq!(diag.severity, Severity::Warning);
}

#[test]
fn unsafe_negative_int_literal_warns() {
	// `-9007199254740992` parses as `Negate` over the positive literal
	// `9007199254740992` (already unsafe on its own) — inferring the `Negate`
	// operand infers that inner literal first, so the warning fires there
	// without any special-casing of the surrounding `Negate`.
	let diags = check("func f(): int = -9007199254740992");
	let diag = diags
		.iter()
		.find(|d| d.message.contains("9007199254740992"))
		.unwrap_or_else(|| panic!("expected a diagnostic naming the literal, got: {diags:?}"));
	assert_eq!(diag.severity, Severity::Warning);
}

#[test]
fn exactly_the_safe_integer_bound_does_not_warn() {
	// 2^53 - 1 == Number.MAX_SAFE_INTEGER — exactly representable, no warning.
	let diags = check("func f(): int = 9007199254740991");
	assert!(
		diags.is_empty(),
		"expected no diagnostics for the exact safe-integer bound, got: {diags:?}"
	);
}

#[test]
fn negative_of_the_safe_integer_bound_does_not_warn() {
	let diags = check("func f(): int = -9007199254740991");
	assert!(
		diags.is_empty(),
		"expected no diagnostics for the exact negative safe-integer bound, got: {diags:?}"
	);
}

#[test]
fn one_past_the_bound_warns() {
	// 2^53 itself: the smallest magnitude that warns.
	let diags = check("func f(): int = 9007199254740992");
	assert!(
		diags.iter().any(|d| d.severity == Severity::Warning),
		"expected a warning at exactly 2^53, got: {diags:?}"
	);
}

#[test]
fn ordinary_int_literals_do_not_warn() {
	let diags = check("func f(): int = 42");
	assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}
