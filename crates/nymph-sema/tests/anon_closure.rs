//! End-to-end checker tests for anonymous closure parameters (`$`, `$0`,
//! `$1`, …) — the type-directed boundary search in `anon_closure.rs`.
//!
//! Mirrors `tests/check.rs`'s own harness exactly (parse + `check_module`,
//! assert on the resulting diagnostics).

use nymph_sema::check_module;
use nymph_syntax::parse_module;

fn check(source: &str) -> Vec<String> {
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
	check_module(&parsed.tree)
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect()
}

fn assert_ok(source: &str) {
	let errors = check(source);
	assert!(
		errors.is_empty(),
		"expected no errors, got: {errors:?}\n---\n{source}"
	);
}

fn assert_error_contains(source: &str, needle: &str) {
	let errors = check(source);
	assert!(
		errors.iter().any(|e| e.contains(needle)),
		"expected an error containing {needle:?}, got: {errors:?}\n---\n{source}"
	);
}

// ── Canonical examples ───────────────────────────────────────────────────────

#[test]
fn both_dollars_direct_call_args_share_one_boundary() {
	// `f($1, $0)`: both `$`'s immediate parent is the SAME `Call` — one
	// two-param closure, arity from the max index used (1) + 1.
	assert_ok(
		"func combine(a: int, b: int): int = a - b
		 func apply2(cb: (int, int) -> int, a: int, b: int): int = cb(a, b)
		 func g(): int = apply2(combine($1, $0), 10, 3)",
	);
}

#[test]
fn call_argument_boundary_needs_no_expansion() {
	// `apply(cb, x)`'s `cb` argument `$0 + 1` already checks as `(int) -> int`
	// at its smallest boundary (the `+` itself) — no expansion needed.
	assert_ok(
		"func apply(cb: (int) -> int, x: int): int = cb(x)
		 func g(): int = apply($0 + 1, 5)",
	);
}

#[test]
fn comparison_boundary_expands_past_the_inner_operator() {
	// THE key case: `$ % 2` alone would give `((p0) => p0 % 2) == 0` — a
	// closure compared to an int, ill-typed — so the boundary must EXPAND
	// outward to the whole `$ % 2 == 0`, which checks as `(int) -> boolean`.
	assert_ok(
		"func check_pred(cb: (int) -> boolean, x: int): boolean = cb(x)
		 func g(): boolean = check_pred($ % 2 == 0, 4)",
	);
}

#[test]
fn repeated_param_shares_one_slot() {
	// `add($0, $0)`: one param used twice — arity 1, not 2.
	assert_ok(
		"func add(a: int, b: int): int = a + b
		 func apply(cb: (int) -> int, x: int): int = cb(x)
		 func g(): int = apply(add($0, $0), 5)",
	);
}

#[test]
fn bare_dollar_as_the_whole_call_argument() {
	// `g($0)`: the smallest non-`$` enclosing expression is the `Call`
	// itself — `(p0) => g(p0)`.
	assert_ok(
		"func inc(a: int): int = a + 1
		 func apply(cb: (int) -> int, x: int): int = cb(x)
		 func g(): int = apply(inc($0), 5)",
	);
}

#[test]
fn nested_at_different_depths() {
	// `f($0, g($1))`: `$1`'s boundary is the inner `g(...)` call, `$0`'s is
	// the outer `f(...)` call — two independent, non-overlapping boundaries,
	// each committed on the very first (smallest) trial, no expansion.
	assert_ok(
		"func g(x: int): int = x * 10
		 func f(a: int, cb: (int, int) -> int): int = a + cb(a, 100)
		 func apply(h: (int) -> int, x: int): int = h(x)
		 func caller(): int = apply(f($0, g($1)), 5)",
	);
}

// ── Slot sites beyond call arguments ────────────────────────────────────────

#[test]
fn a_functions_own_body_is_a_closure_slot() {
	// A function's own body is exactly the same kind of slot a call argument
	// is — the top-level spelling of the same boundary the canonical
	// examples form as an argument.
	assert_ok(
		"func check_pred(cb: (int) -> boolean, x: int): boolean = cb(x)
		 func is_even(): (int) -> boolean = $ % 2 == 0
		 func g(): boolean = check_pred(is_even(), 4)",
	);
}

#[test]
fn a_let_initializer_is_a_closure_slot() {
	assert_ok(
		"func apply(cb: (int) -> int, x: int): int = cb(x)
		 func g(): int = {
		 	let double = $0 * 2
		 	apply(double, 21)
		 }",
	);
}

#[test]
fn a_return_operand_is_a_closure_slot() {
	assert_ok(
		"func apply(cb: (int) -> int, x: int): int = cb(x)
		 func mk(): (int) -> int = {
		 	return $0 + 1
		 }
		 func g(): int = apply(mk(), 5)",
	);
}

#[test]
fn a_constructor_field_is_a_closure_slot() {
	// `Adder(cb = $0 + 100)`: the labeled constructor argument is a
	// `check_ctor_args` slot, exercised through `resolve_anon` exactly like a
	// call argument or a let initializer.
	assert_ok(
		"struct Adder(cb: (int) -> int)
		 func apply(cb: (int) -> int, x: int): int = cb(x)
		 func g(): int = {
		 	let a = Adder(cb = $0 + 100)
		 	apply(a.cb, 5)
		 }",
	);
}

#[test]
fn an_enum_variant_constructor_field_is_a_closure_slot() {
	// Same slot, reached through `infer_variant_ctor` instead of
	// `infer_struct_ctor` — both call sites feed the same `check_ctor_args`.
	assert_ok(
		"enum Holder { With(cb: (int) -> int) }
		 func call_cb(h: Holder, x: int): int = match (h) {
		   With(cb) -> cb(x),
		 }
		 func g(): int = call_cb(With(cb = $0 + 100), 5)",
	);
}

// ── Explicit closures are a hard boundary ───────────────────────────────────

#[test]
fn dollar_inside_an_explicit_closure_does_not_escape_it() {
	// `$0` inside an explicit closure's body forms its OWN nested anon
	// closure bounded by that body — it does not escape out to the
	// enclosing call argument. The outer explicit closure's inferred type
	// becomes a CURRIED `(int) -> ((int) -> int)`, matching `run_two`'s
	// `outer` param exactly (only possible if `$0 + x` desugared to its own
	// closure rather than trying to unify `x`'s scope with some outer one).
	assert_ok(
		"func run_two(outer: (int) -> ((int) -> int), a: int, b: int): int = outer(a)(b)
		 func g(): int = run_two((x: int) -> $0 + x, 10, 5)",
	);
}

// ── Loud failure, never silent ──────────────────────────────────────────────

#[test]
fn a_closure_where_a_plain_value_is_expected_is_a_loud_type_error() {
	// No candidate boundary up to the enclosing slot can ever make a bare
	// `$0` check against a non-function expected type — reported loudly via
	// the ordinary `subtype` mismatch, never silently.
	assert_error_contains("func bad(): int = $0", "mismatched types");
}

#[test]
fn mismatched_arity_at_every_boundary_is_a_loud_type_error() {
	// `$0` used as a 1-ary closure passed where a 2-ary one is expected: no
	// boundary reconciles this, so the natural arity/type mismatch surfaces.
	assert_error_contains(
		"func apply2(cb: (int, int) -> int, a: int, b: int): int = cb(a, b)
		 func g(): int = apply2($0, 1, 2)",
		"mismatched types",
	);
}
