//! Milestone B: inherent methods (`this`), method-body checking, namespaced
//! functions (`Type.f()`), and top-level inherent impls.

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
fn namespaced_constructor() {
	assert_ok(
		"struct Point(x: int) {
		   namespace {
		     func at(v: int): Point = Point(x = v)
		   }
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

		   namespace {
		     func empty(): self = None
		   }
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
