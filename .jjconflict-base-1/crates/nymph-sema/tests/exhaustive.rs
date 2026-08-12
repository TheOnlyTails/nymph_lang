//! Tests match exhaustiveness and reachability.

use nymph_sema::check_module;
use nymph_syntax::parse_module;

/// Returns `(errors, warnings)` message lists.
fn diagnose(source: &str) -> (Vec<String>, Vec<String>) {
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
	let diags = check_module(&parsed.tree).diags;
	let errors = diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	let warnings = diags
		.iter()
		.filter(|d| !d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	(errors, warnings)
}

fn assert_ok(source: &str) {
	let (errors, _) = diagnose(source);
	assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
}

fn assert_error_contains(source: &str, needle: &str) {
	let (errors, _) = diagnose(source);
	assert!(
		errors.iter().any(|e| e.contains(needle)),
		"expected an error containing {needle:?}, got: {errors:?}"
	);
}

const OPT: &str = "enum Opt<T> { Some(value: T), None }\n";

#[test]
fn binding_subpatterns_are_transparent_to_exhaustiveness_and_refutability() {
	assert_ok(
		"func f(value: boolean): int = match (value) {
		   whole = true -> 1,
		   nested = false -> 0,
		 }",
	);
	assert_error_contains(
		"func f(value: boolean): int = match (value) {
		   whole = true -> 1,
		 }",
		"non-exhaustive",
	);
}

#[test]
fn missing_enum_variant_is_non_exhaustive() {
	let src = format!(
		"{OPT}
		 func f(o: Opt<int>): int = match (o) {{
		   Some(value) -> value,
		 }}",
	);
	let (errors, _) = diagnose(&src);
	assert!(
		errors
			.iter()
			.any(|e| e.contains("non-exhaustive") && e.contains("None")),
		"expected non-exhaustive/None, got: {errors:?}"
	);
}

#[test]
fn wildcard_makes_enum_exhaustive() {
	assert_ok(&format!(
		"{OPT}
		 func f(o: Opt<int>): int = match (o) {{
		   Some(value) -> value,
		   _ -> 0,
		 }}",
	));
}

#[test]
fn all_variants_is_exhaustive() {
	assert_ok(&format!(
		"{OPT}
		 func f(o: Opt<int>): int = match (o) {{
		   Some(value) -> value,
		   None -> 0,
		 }}",
	));
}

#[test]
fn boolean_match_needs_both_cases() {
	assert_error_contains(
		"func f(b: boolean): int = match (b) {
		   true -> 1,
		 }",
		"non-exhaustive",
	);
}

#[test]
fn boolean_match_with_both_cases_is_ok() {
	assert_ok(
		"func f(b: boolean): int = match (b) {
		   true -> 1,
		   false -> 0,
		 }",
	);
}

#[test]
fn int_match_without_wildcard_is_non_exhaustive() {
	assert_error_contains(
		"func f(n: int): int = match (n) {
		   1 -> 1,
		   2 -> 2,
		 }",
		"non-exhaustive",
	);
}

#[test]
fn int_match_with_wildcard_is_ok() {
	assert_ok(
		"func f(n: int): int = match (n) {
		   1 -> 1,
		   _ -> 0,
		 }",
	);
}

#[test]
fn int_ranges_can_be_exhaustive() {
	// `0`, `1..` (≥1), and `..=-1` (≤-1) together cover every `int`.
	assert_ok(
		"func sign(n: int): int = match (n) {
		   0 -> 0,
		   1.. -> 1,
		   ..=-1 -> -1,
		 }",
	);
}

#[test]
fn int_ranges_with_a_gap_are_non_exhaustive() {
	// `0` and `2..` leave `1` (and everything negative) uncovered.
	assert_error_contains(
		"func f(n: int): int = match (n) {
		   0 -> 0,
		   2.. -> 2,
		 }",
		"non-exhaustive",
	);
}

#[test]
fn int_inclusive_range_plus_tails_is_exhaustive() {
	assert_ok(
		"func f(n: int): int = match (n) {
		   ..=-1 -> -1,
		   0..=9 -> 0,
		   10.. -> 1,
		 }",
	);
}

#[test]
fn uint_match_without_wildcard_is_non_exhaustive() {
	assert_error_contains(
		"func f(n: uint): int = match (n) {
		   0u -> 0,
		   1u -> 1,
		 }",
		"non-exhaustive",
	);
}

#[test]
fn uint_match_with_wildcard_is_ok() {
	assert_ok(
		"func f(n: uint): int = match (n) {
		   0u -> 0,
		   _ -> 1,
		 }",
	);
}

#[test]
fn uint_ranges_can_be_exhaustive() {
	// `0u` and `1u..` (≥1) together cover every `uint` — there is nothing below 0,
	// so unlike `int` no negative tail is needed. This is the case that spuriously
	// demanded a `_` arm before `uint` got its own interval reasoning.
	assert_ok(
		"func f(n: uint): int = match (n) {
		   0u -> 0,
		   1u.. -> 1,
		 }",
	);
}

#[test]
fn uint_ranges_with_a_gap_are_non_exhaustive() {
	// `0u` and `2u..` leave `1u` uncovered.
	assert_error_contains(
		"func f(n: uint): int = match (n) {
		   0u -> 0,
		   2u.. -> 2,
		 }",
		"non-exhaustive",
	);
}

#[test]
fn uint_inclusive_range_plus_tail_is_exhaustive() {
	assert_ok(
		"func f(n: uint): int = match (n) {
		   0u..=9u -> 0,
		   10u.. -> 1,
		 }",
	);
}

#[test]
fn uint_non_exhaustive_message_names_uint_not_int() {
	// A dedicated `NonExhaustiveUInt`: the message must say `uint`, never `int`.
	let (errors, _) = diagnose(
		"func f(n: uint): int = match (n) {
		   0u -> 0,
		   1u -> 1,
		 }",
	);
	assert!(
		errors.iter().any(|e| e.contains("uint")),
		"expected a uint-worded non-exhaustive error, got: {errors:?}"
	);
	assert!(
		!errors.iter().any(|e| e.contains("`int`")),
		"the uint non-exhaustive message must not claim the gap is in `int`, got: {errors:?}"
	);
}

const NEST: &str = "enum Inner { C, D }\nenum Outer { A(x: Inner), B }\n";

#[test]
fn nested_variant_arms_are_reachable_and_exhaustive() {
	let (errors, warnings) = diagnose(&format!(
		"{NEST}
		 func f(o: Outer): int = match (o) {{
		   A(x = C) -> 1,
		   A(x = D) -> 2,
		   B -> 3,
		 }}",
	));
	assert!(errors.is_empty(), "unexpected errors: {errors:?}");
	assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
}

#[test]
fn duplicate_nested_arm_is_unreachable() {
	// The second `A(x = C)` is genuinely redundant — the algorithm sees the nesting.
	let (_, warnings) = diagnose(&format!(
		"{NEST}
		 func f(o: Outer): int = match (o) {{
		   A(x = C) -> 1,
		   A(x = C) -> 2,
		   A(x = D) -> 3,
		   B -> 4,
		 }}",
	));
	assert!(
		warnings.iter().any(|w| w.contains("unreachable")),
		"expected an unreachable warning, got: {warnings:?}"
	);
}

#[test]
fn nested_missing_case_is_non_exhaustive() {
	// `A(x = D)` is uncovered.
	assert_error_contains(
		&format!(
			"{NEST}
			 func f(o: Outer): int = match (o) {{
			   A(x = C) -> 1,
			   B -> 2,
			 }}",
		),
		"non-exhaustive",
	);
}

#[test]
fn boolean_tuple_match_is_exhaustive() {
	// All four `#(bool, bool)` combinations are covered (with a union arm) — no `_` needed.
	assert_ok(
		"func f(a: boolean, b: boolean): int = match (#(a, b)) {
		   #(false, true) -> 1,
		   #(false, false) | #(true, true) -> 0,
		   #(true, false) -> -1,
		 }",
	);
}

#[test]
fn boolean_tuple_match_missing_a_case_is_non_exhaustive() {
	assert_error_contains(
		"func f(a: boolean, b: boolean): int = match (#(a, b)) {
		   #(false, false) -> 0,
		   #(true, true) -> 1,
		 }",
		"non-exhaustive",
	);
}

#[test]
fn tuple_match_with_a_wildcard_column_is_exhaustive() {
	// `#(_, false)` and `#(_, true)` cover every second-element value for any first.
	assert_ok(
		"func f(a: boolean, b: boolean): int = match (#(a, b)) {
		   #(_, false) -> 0,
		   #(_, true) -> 1,
		 }",
	);
}

#[test]
fn arm_after_catch_all_is_unreachable() {
	let src = format!(
		"{OPT}
		 func f(o: Opt<int>): int = match (o) {{
		   _ -> 0,
		   Some(value) -> value,
		 }}",
	);
	let (errors, warnings) = diagnose(&src);
	assert!(errors.is_empty(), "unexpected errors: {errors:?}");
	assert!(
		warnings.iter().any(|w| w.contains("unreachable")),
		"expected an unreachable warning, got: {warnings:?}"
	);
}
