use nymph_diagnostics::{Diagnostic, Severity};
use nymph_sema::check_module;
use nymph_syntax::parse_module;

fn check(source: &str) -> Vec<Diagnostic> {
	let parsed = parse_module(source, "test");
	assert!(
		!parsed.diagnostics.iter().any(Diagnostic::is_error),
		"source failed to parse: {:?}",
		parsed.diagnostics
	);
	check_module(&parsed.tree).diags
}

fn assert_ok(source: &str) {
	let diags = check(source);
	assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

fn assert_out_of_range(source: &str) {
	let diags = check(source);
	assert!(
		diags
			.iter()
			.any(|diag| { diag.severity == Severity::Error && diag.message.contains("out of range") }),
		"expected an out-of-range error, got: {diags:?}"
	);
}

#[test]
fn signed_integer_source_boundaries() {
	assert_ok("func f(): int = 9223372036854775807");
	assert_ok("func f(): int = -9223372036854775808");
	assert_out_of_range("func f(): int = 9223372036854775808");
	assert_out_of_range("func f(): int = -9223372036854775809");
}

#[test]
fn unsigned_integer_source_boundaries() {
	assert_ok("func f(): uint = 18446744073709551615u");
	assert_out_of_range("func f(): uint = 18446744073709551615");
}

#[test]
fn integers_above_the_old_javascript_safe_bound_are_exact_and_quiet() {
	assert_ok("func f(): int = 9007199254740992");
	assert_ok("func f(): uint = 9007199254740992u");
}

#[test]
fn valid_bare_literals_still_widen() {
	assert_ok("func as_float(): float = 9223372036854775807");
	assert_ok("func as_uint(): uint = 9223372036854775807");
}

#[test]
fn constant_integer_arithmetic_is_checked_at_fixed_width_boundaries() {
	for (source, message) in [
		(
			"func value(): int = 9223372036854775807 + 1",
			"overflows `int`",
		),
		(
			"func value(): uint = 18446744073709551615u + 1u",
			"overflows `uint`",
		),
		("func value(): int = 1 % 0", "division by zero"),
		("func value(): int = (1 / 0) + 1", "division by zero"),
		(
			"func value(): int = 1 << 64",
			"shift count must be in 0..63",
		),
	] {
		let diagnostics = check(source);
		assert!(
			diagnostics
				.iter()
				.any(|diagnostic| diagnostic.message.contains(message)),
			"expected {message:?} for {source:?}, got {diagnostics:?}"
		);
	}
	assert!(check("func value(): int = 9223372036854775806 + 1").is_empty());
	assert!(check("func value(): uint = 18446744073709551614u + 1u").is_empty());
}
