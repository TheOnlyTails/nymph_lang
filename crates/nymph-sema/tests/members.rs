//! Tests inherent methods, method-body checking, namespaced functions, and
//! top-level inherent implementations.

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
	assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
}

fn assert_error_contains(source: &str, needle: &str) {
	let errors = check(source);
	assert!(
		errors.iter().any(|e| e.contains(needle)),
		"expected an error containing {needle:?}, got: {errors:?}"
	);
}

#[test]
fn inherent_method_with_this() {
	assert_ok(
		"struct Point(x: int, y: int) {
		   func sum(): int = this.x + this.y
		 }
		 func total(p: Point): int = p.sum()",
	);
}

#[test]
fn inherent_method_omitted_return_infers_from_body() {
	// `value()` has no return annotation; its type is inferred from the body and
	// callers must see `int` (the shared return variable).
	assert_ok(
		"struct Counter(n: int) {
		   func value() = this.n
		 }
		 func read(c: Counter): int = c.value()",
	);
}

#[test]
fn generic_inherent_method_on_this() {
	assert_ok(
		"enum Opt<T> {
		   Some(value: T),
		   None

		   func is_some() = match (this) {
		     Some(...) -> true,
		     None -> false,
		   }
		 }
		 func check(o: Opt<int>): boolean = o.is_some()",
	);
}

#[test]
fn method_body_return_mismatch_is_reported() {
	assert_error_contains(
		"struct Point(x: int) {
		   func bad(): int = true
		 }",
		"mismatched types",
	);
}

#[test]
fn method_this_field_type_is_checked() {
	assert_error_contains(
		"struct Point(x: int) {
		   func wrong(): boolean = this.x
		 }",
		"mismatched types",
	);
}

#[test]
fn method_argument_type_is_checked() {
	assert_error_contains(
		"struct Point(x: int) {
		   func add(n: int): int = this.x + n
		 }
		 func f(p: Point): int = p.add(true)",
		"mismatched types",
	);
}

#[test]
fn top_level_inherent_impl() {
	assert_ok(
		"struct Point(x: int)
		 impl Point {
		   func get(): int = this.x
		 }
		 func f(p: Point): int = p.get()",
	);
}

#[test]
fn top_level_and_nested_inherent_members_share_collision_namespace() {
	assert_error_contains(
		"struct Point(x: int) { namespace func at(v: int): Point = Point(x = v) }
		 impl Point { namespace func at(v: int): Point = Point(x = v) }",
		"defined more than once",
	);
	assert_error_contains(
		"struct Point(x: int) { func at(): int = this.x }
		 impl Point { namespace func at(v: int): Point = Point(x = v) }",
		"defined more than once",
	);
}

#[test]
fn namespaced_constructor() {
	assert_ok(
		"struct Point(x: int) {
		   namespace func at(v: int): Point = Point(x = v)
		 }
		 func origin(): Point = Point.at(0)",
	);
}

#[test]
fn namespaced_function_returning_self() {
	assert_ok(
		"enum Opt<T> {
		   Some(value: T),
		   None

		   namespace func empty(): self = None
		 }
		 func none_int(): Opt<int> = Opt.empty()",
	);
}

#[test]
fn unknown_namespaced_function_is_reported() {
	assert_error_contains(
		"struct Point(x: int)
		 func f(): int = Point.nope()",
		"no namespaced function `nope`",
	);
}

#[test]
fn generic_method_with_omitted_return_generalizes() {
	// `map`'s omitted return is inferred as `Opt<R>` and generalised, so `None` (which is
	// a nullary-variant *pattern*, not a binding) and `Some(value = f(value))` agree.
	assert_ok(
		"enum Opt<T> { Some(value: T), None
		   func map<R>(f: (T) -> R) = match (this) {
		     Some(value) -> Some(value = f(value)),
		     None -> None,
		   }
		 }",
	);
}

#[test]
fn bare_nullary_variant_is_a_pattern_not_a_binding() {
	// `Red`/`Green` in the arms match variants (and are exhaustive); if they bound
	// variables instead, the second arm would be unreachable and `Green` unknown.
	assert_ok(
		"enum Color { Red, Green }
		 func code(c: Color): int = match (c) {
		   Red -> 1,
		   Green -> 2,
		 }",
	);
}

#[test]
fn negation_of_a_method_with_omitted_return() {
	// `is_off` negates `is_on`, whose return type is inferred (omitted). `!` must treat
	// the still-unresolved receiver as `boolean` rather than failing to find a `Not` impl.
	assert_ok(
		"enum Flag { On, Off
		   func is_on() = match (this) { On -> true, Off -> false }
		   func is_off() = !this.is_on()
		 }",
	);
}

// ── Duplicate inner members (checker-level collision detection) ────────────
//
// Struct/enum inner members used to be collected into one `FxHashMap` keyed
// only by name (`collect_impl_member`), with a later member silently
// overwriting an earlier same-named one and no diagnostic ever fired. The
// shadowed member's body was then never type-checked, yet the Slice 4J HIR
// lowering walks the raw AST and emits EVERY member's body regardless — an
// unchecked-body-reaches-JS soundness hole. These tests pin the fix: any such
// collision must now be reported as an error, for every kind combination, on
// both structs and enums.

#[test]
fn duplicate_func_on_struct_is_reported() {
	assert_error_contains(
		"struct Point(x: int) {
		   func get(): int = this.x
		   func get(): int = this.x
		 }",
		"defined more than once",
	);
}

#[test]
fn duplicate_func_on_enum_is_reported() {
	assert_error_contains(
		"enum Flag { On, Off
		   func is_on() = match (this) { On -> true, Off -> false }
		   func is_on() = true
		 }",
		"defined more than once",
	);
}

#[test]
fn func_and_namespace_static_same_name_is_reported() {
	assert_error_contains(
		"struct Point(x: int) {
		   func at(): int = this.x
		   namespace func at(v: int): Point = Point(x = v)
		 }",
		"defined more than once",
	);
}

#[test]
fn two_namespace_statics_same_name_is_reported() {
	assert_error_contains(
		"struct Point(x: int) {
		   namespace func at(v: int): Point = Point(x = v)
		   namespace func at(v: int): Point = Point(x = v)
		 }",
		"defined more than once",
	);
}

#[test]
fn duplicate_func_in_namespace_is_reported() {
	assert_error_contains(
		"namespace Point {
		   func at(v: int): int = v
		   func at(v: int): int = v
		 }",
		"defined more than once",
	);
}

#[test]
fn two_different_member_names_stay_clean() {
	assert_ok(
		"struct Point(x: int) {
		   func get(): int = this.x
		   namespace func at(v: int): Point = Point(x = v)
		 }",
	);
}

#[test]
fn variant_name_matching_namespaced_function_is_not_a_member_duplicate() {
	// Variant names live in a separate namespace (`defs.variants`) from the
	// per-type inherent-method map this check guards, so a namespaced function
	// sharing a variant's name is NOT flagged here. (It IS a real hazard, but a
	// different, enum-specific one caught later at lowering by
	// the runtime class's variant/static collision assertion.)
	assert_ok(
		"enum Color { Red
		   namespace func Red(): Color = Color.Red
		 }",
	);
}

#[test]
fn shadowed_body_type_error_still_surfaces_the_duplicate_diagnostic() {
	// The second `bad` must not silently shadow the first and skip its broken
	// body. The collision itself must always be reported, regardless of
	// whether the shadowed body would also have failed on its own merits.
	assert_error_contains(
		"struct Point(x: int) {
		   func bad(): int = true
		   func bad(): int = this.x
		 }",
		"defined more than once",
	);
}

#[test]
fn interface_default_method_own_generic_scopes_into_signature() {
	// A generic parameter declared on an interface DEFAULT method (`wrap<R>`) must be
	// in scope when its own signature (`value: R): R`) is lowered — this
	// failed with `cannot find type R` because only the interface's generics were
	// registered, not the method's.
	assert_ok(
		"interface Widget {
		   func base(): int
		   func wrap<R>(value: R): R = value
		 }
		 struct Thing() {
		   impl Widget {
		     func base(): int = 7
		   }
		 }
		 func run(t: Thing): int = t.wrap(42)",
	);
}

#[test]
fn interface_default_method_generic_instantiates_at_call_site() {
	// The method generic `R` must be instantiated to a fresh inference variable per
	// call, so `wrap` infers `R` from the argument at each site (here `int`, then
	// `boolean`) rather than leaking the rigid parameter or pinning it across calls.
	assert_ok(
		"interface Widget {
		   func base(): int
		   func wrap<R>(value: R): R = value
		 }
		 struct Thing() {
		   impl Widget {
		     func base(): int = 7
		   }
		 }
		 func run(t: Thing): boolean = {
		   let n: int = t.wrap(42)
		   t.wrap(true)
		 }",
	);
}

#[test]
fn positional_subpattern_on_a_single_field_variant_binds_the_sole_field() {
	// `Box(inner)` — `inner` is not the field name (`value`), but the constructor has
	// exactly one field, so it's a positional binding of that field, not an error.
	assert_ok(
		"enum Box { Full(value: int), Empty }
		 func unwrap_or(b: Box): int = match (b) {
		   Full(inner) -> inner,
		   Empty -> 0,
		 }",
	);
}

#[test]
fn positional_subpattern_on_a_multi_field_constructor_is_rejected() {
	// A positional (un-named) sub-pattern has no single field to bind to on a
	// two-field struct — the checker rejects it rather than guessing.
	assert_error_contains(
		"struct Pair(a: int, b: int)
		 func f(p: Pair): int = match (p) {
		   Pair(#(1, 2)) -> 0,
		 }",
		"struct pattern fields must be named",
	);
}

#[test]
fn named_struct_patterns_require_anonymous_trailing_omission() {
	assert_ok(
		"struct Pair(a: int, b: int)\n\
		 func first(pair: Pair): int = match (pair) { Pair(a = a, ...) -> a }",
	);
	assert_error_contains(
		"struct Pair(a: int, b: int)\n\
		 func first(pair: Pair): int = match (pair) { Pair(a = a) -> a }",
		"partial struct pattern must end with anonymous `...`",
	);
	assert_error_contains(
		"struct Pair(a: int, b: int)\n\
		 func first(pair: Pair): int = match (pair) { Pair(..., a = a) -> a }",
		"anonymous `...` must occur once at the end",
	);
}

#[test]
fn positional_struct_patterns_are_rejected_even_for_one_field() {
	assert_error_contains(
		"struct Box(value: int)\n\
		 func get(box: Box): int = match (box) { Box(0) -> 0, Box(...) -> 1 }",
		"struct pattern fields must be named",
	);
}
