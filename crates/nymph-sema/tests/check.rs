//! End-to-end checker tests: parse a small Nymph program and assert on the
//! diagnostics the type checker produces. Milestone A covers functions, generics,
//! ADTs, closures, and local inference.

use nymph_sema::check_module;
use nymph_syntax::parse_module;

/// Parse and check `source`, returning the checker's error messages. Panics if the
/// source fails to *parse* (these tests exercise the checker, not the parser).
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

/// Assert the program type-checks with no errors.
fn assert_ok(source: &str) {
	let errors = check(source);
	assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
}

/// Assert at least one error message contains `needle`.
fn assert_error_contains(source: &str, needle: &str) {
	let errors = check(source);
	assert!(
		errors.iter().any(|e| e.contains(needle)),
		"expected an error containing {needle:?}, got: {errors:?}"
	);
}

#[test]
fn simple_annotated_function() {
	assert_ok("func add(a: int, b: int): int = a + b");
}

#[test]
fn return_type_mismatch_is_reported() {
	assert_error_contains("func bad(): int = true", "mismatched types");
}

#[test]
fn wrong_argument_type_is_reported() {
	assert_error_contains(
		"func takes_int(x: int): int = x
		 func caller(): int = takes_int(true)",
		"mismatched types",
	);
}

#[test]
fn generic_identity_infers_at_call_site() {
	assert_ok(
		"func id<T>(x: T): T = x
		 func use_it(): int = id(5)",
	);
}

#[test]
fn generic_return_is_specialised_per_call() {
	// `id` used at two different types in one module must not conflate them.
	assert_ok(
		"func id<T>(x: T): T = x
		 func a(): int = id(1)
		 func b(): boolean = id(true)",
	);
}

#[test]
fn unknown_identifier_is_reported() {
	assert_error_contains("func f(): int = nope", "cannot find `nope`");
}

#[test]
fn struct_construction_and_field_access() {
	assert_ok(
		"struct Point(x: int, y: int)
		 func sum(p: Point): int = p.x + p.y
		 func make(): Point = Point(x = 1, y = 2)",
	);
}

#[test]
fn struct_field_type_mismatch() {
	assert_error_contains(
		"struct Point(x: int, y: int)
		 func make(): Point = Point(x = 1, y = true)",
		"mismatched types",
	);
}

#[test]
fn unknown_struct_field() {
	assert_error_contains(
		"struct Point(x: int, y: int)
		 func make(): Point = Point(x = 1, z = 2)",
		"unknown field `z`",
	);
}

#[test]
fn generic_enum_construction_and_match() {
	assert_ok(
		"enum Option<T> { Some(value: T), None }
		 func unwrap_or(o: Option<int>, fallback: int): int = match (o) {
		   Some(value) -> value,
		   None -> fallback,
		 }",
	);
}

#[test]
fn enum_variant_inference() {
	// Constructing `Some(1)` should infer `Option<int>`; unifying with a
	// `Option<boolean>` annotation must fail.
	assert_error_contains(
		"enum Option<T> { Some(value: T), None }
		 func f(): Option<boolean> = Some(value = 1)",
		"mismatched types",
	);
}

#[test]
fn let_inference_in_block() {
	assert_ok(
		"func f(): int = {
		   let x = 1
		   let y = x + 2
		   y
		 }",
	);
}

#[test]
fn assignment_to_immutable_is_reported() {
	assert_error_contains(
		"func f(): int = {
		   let x = 1
		   x = 2
		   x
		 }",
		"cannot assign to immutable `x`",
	);
}

#[test]
fn mutable_assignment_is_allowed() {
	assert_ok(
		"func f(): int = {
		   let mut x = 1
		   x = 2
		   x
		 }",
	);
}

#[test]
fn closure_inference_through_expected_type() {
	assert_ok(
		"func apply(f: (int) -> int, x: int): int = f(x)
		 func run(): int = apply(n -> n + 1, 10)",
	);
}

#[test]
fn if_branches_must_agree() {
	assert_error_contains(
		"func f(cond: boolean): int = if (cond) { 1 } else { true }",
		"mismatched types",
	);
}

#[test]
fn int_literal_widens_to_float_in_return_position() {
	assert_ok("func f(): float = 1");
}

#[test]
fn int_literal_widens_to_uint_in_return_position() {
	assert_ok("func f(): uint = 0");
}

#[test]
fn int_literal_widens_to_float_argument() {
	assert_ok(
		"func takes_float(x: float): float = x
		 func f(): float = takes_float(2)",
	);
}

#[test]
fn int_literal_compares_against_a_float() {
	// `x > 0`: the `0` literal widens to the `float` operand instead of clashing.
	assert_ok("func positive(x: float): boolean = x > 0");
}

#[test]
fn int_literal_equals_a_uint() {
	assert_ok("func empty(n: uint): boolean = n == 0");
}

#[test]
fn non_literal_int_still_clashes_with_float() {
	// The widening is a *literal* rule: an `int`-typed value is not silently a float.
	assert_error_contains("func f(n: int): float = n", "mismatched types");
}

#[test]
fn variants_can_share_names_across_enums() {
	// Two enums each with a `Leaf` variant coexist; qualified access disambiguates.
	assert_ok(
		"enum Tree { Leaf(v: int), Branch }
		 enum Plant { Leaf(v: int), Root }
		 func t(): Tree = Tree.Leaf(v = 1)
		 func p(): Plant = Plant.Leaf(v = 2)",
	);
}

#[test]
fn a_struct_and_a_variant_may_share_a_name() {
	// `struct Leaf` (a type) and `Tree.Leaf` (a variant) don't collide.
	assert_ok(
		"enum Tree { Leaf(v: int), Branch }
		 struct Leaf(x: int)
		 func make(): Leaf = Leaf(x = 1)
		 func tree(): Tree = Tree.Leaf(v = 2)",
	);
}

#[test]
fn ambiguous_bare_variant_is_reported() {
	assert_error_contains(
		"enum Tree { Leaf, Branch }
		 enum Plant { Leaf, Root }
		 func f() = Leaf",
		"ambiguous variant",
	);
}

#[test]
fn compound_assignment_on_a_mutable_local() {
	assert_ok(
		"func f(): int = {
		   let mut x = 1
		   x += 2
		   x
		 }",
	);
}

#[test]
fn compound_assignment_to_an_immutable_local_is_reported() {
	assert_error_contains(
		"func f(): int = {
		   let x = 1
		   x += 2
		   x
		 }",
		"cannot assign to immutable",
	);
}

#[test]
fn compound_assignment_result_must_fit_the_place() {
	// `x` is a `float`; `x *= 2` stays a `float` (the `2` literal widens) — no error.
	assert_ok(
		"func f(): float = {
		   let mut x = 1.5
		   x *= 2
		   x
		 }",
	);
}
