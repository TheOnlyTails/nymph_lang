//! Milestone B: interface solving, operator overloading, method resolution, blanket
//! impls, and associated-generic (`Output`) outputs.

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

const PLUS: &str = "interface Plus<Other, Output> { func plus(other: Other): Output }\n";

#[test]
fn mixed_primitive_arithmetic_picks_the_output_type() {
	// `1 + 1.0` selects `impl Plus<Other = float, Output = float> for int` and so is
	// a `float`, not an `int`. This is the headline associated-generic case.
	assert_ok(&format!(
		"{PLUS}
		 impl Plus<Other = float, Output = float> for int {{
		   func plus(other: float): float = other
		 }}
		 func f(): float = 1 + 1.0",
	));
}

#[test]
fn mixed_arithmetic_result_type_is_enforced() {
	assert_error_contains(
		&format!(
			"{PLUS}
			 impl Plus<Other = float, Output = float> for int {{
			   func plus(other: float): float = other
			 }}
			 func f(): int = 1 + 1.0",
		),
		"mismatched types",
	);
}

#[test]
fn same_primitive_arithmetic_needs_no_impl() {
	// Built-in fast path: `int + int` works with no `Plus` impl in scope.
	assert_ok("func f(a: int, b: int): int = a + b");
}

#[test]
fn user_type_operator_overload() {
	assert_ok(&format!(
		"{PLUS}
		 struct Vec2(x: int, y: int)
		 impl Plus<Other = Vec2, Output = Vec2> for Vec2 {{
		   func plus(other: Vec2): Vec2 = other
		 }}
		 func add(a: Vec2, b: Vec2): Vec2 = a + b",
	));
}

#[test]
fn missing_operator_impl_is_reported() {
	assert_error_contains(
		&format!(
			"{PLUS}
			 struct Vec2(x: int, y: int)
			 func add(a: Vec2, b: Vec2): Vec2 = a + b",
		),
		"plus",
	);
}

#[test]
fn unbounded_generic_operator_operand_is_reported() {
	// Finding 2: `a + b` on two values of an unbounded generic parameter `T` used
	// to type-check with zero diagnostics (the old fallback silently accepted any
	// non-ADT operand via a best-effort `unify`), then ICE in lowering — a valid
	// program should never reach an unrecoverable compiler panic. `T` has no bound
	// providing `plus`, so this is now a proper `NotImplemented` diagnostic, exactly
	// like the concrete `missing_operator_impl_is_reported` case above.
	assert_error_contains("func f<T>(a: T, b: T): T = a + b", "plus");
}

#[test]
fn bounded_generic_operator_operand_resolves_through_the_bound() {
	// A `T: Plus<...>` bound provides `plus`, so this type-checks with zero
	// diagnostics — `dispatch_operator` resolves it via `resolve_param_method`
	// (`MethodSource::GenericBound`), the same path `method_resolves_through_generic_bound`
	// above exercises for an ordinary method call.
	assert_ok(&format!(
		"{PLUS}
		 func f<T: Plus<Other = T, Output = T>>(a: T, b: T): T = a + b",
	));
}

#[test]
fn pending_operator_finalization_is_declaration_order_independent() {
	// `Checker::pending_operators` entries whose operand is a still-unbound
	// inference variable used to all be retried once at module end
	// (`finalize_pending_operators`), by which point `Checker::param_bounds` held
	// only the *last*-checked function's bounds (`param_bounds` is a single shared
	// map that each body's checking clears and rebuilds). `a`'s operator resolves
	// through its own bound (`T: Plus<...>`) only if `a`'s bounds are still live
	// when its pending entry is retried -- if an unrelated `b` is checked
	// afterward and finalization still happens at module end, `a`'s pending entry
	// gets retried against `b`'s (empty) bounds and spuriously fails. Both
	// declaration orders of the same valid program must produce identical (zero)
	// diagnostics.
	let a_then_b = format!(
		"{PLUS}
		 func a<T: Plus<Other = T, Output = T>>(): T = {{
		   let xs = #[]
		   let y = xs[0] + xs[0]
		   let z: T = xs[0]
		   y
		 }}
		 func b(): int = 1",
	);
	let b_then_a = format!(
		"{PLUS}
		 func b(): int = 1
		 func a<T: Plus<Other = T, Output = T>>(): T = {{
		   let xs = #[]
		   let y = xs[0] + xs[0]
		   let z: T = xs[0]
		   y
		 }}",
	);
	let errors_a_then_b = check(&a_then_b);
	let errors_b_then_a = check(&b_then_a);
	assert!(
		errors_a_then_b.is_empty(),
		"expected no errors (a then b), got: {errors_a_then_b:?}"
	);
	assert!(
		errors_b_then_a.is_empty(),
		"expected no errors (b then a), got: {errors_b_then_a:?}"
	);
}

#[test]
fn function_valued_operator_operand_is_reported() {
	// Finding 2: the fallback's three-way match (primitive / ADT-or-Param / still-
	// `Infer`) didn't enumerate a *resolved* type with no operator support at all --
	// concretely, a first-class function value. `resolve_fallback_operand` returned
	// `None` for it (not primitive, not ADT, not `Param`), and the diagnostic guard
	// only fired for a still-unresolved `Infer` var, so this used to type-check with
	// zero diagnostics and then ICE in `lower_hir` on `None => panic!(..)` -- the
	// exact "valid program reaches an unrecoverable panic" pathology this closes.
	assert_error_contains(
		"func g(x: int): int = x
		 func h(x: int): int = x
		 func f(): int = {
		   let a = g
		   let b = h
		   let c = a + b
		   1
		 }",
		"plus",
	);
}

#[test]
fn method_call_resolves_through_interface() {
	assert_ok(
		"interface Show { func show(): string }
		 struct P(x: int)
		 impl Show for P { func show(): string = \"p\" }
		 func render(p: P): string = p.show()",
	);
}

#[test]
fn blanket_impl_applies_to_any_type() {
	assert_ok(
		"interface Equals<Other> { func equals(other: Other): boolean }
		 impl<T> Equals<Other = T> for T { func equals(other: T): boolean = true }
		 struct P(x: int)
		 func same(a: P, b: P): boolean = a == b",
	);
}

#[test]
fn unwrap_operator_resolves_output() {
	assert_ok(
		"enum Option<T> { Some(value: T), None }
		 interface Unwrap<Output> { func unwrap(default: Output): Output }
		 impl<T> Unwrap<Output = T> for Option<T> {
		   func unwrap(default: T): T = default
		 }
		 func get(o: Option<int>): int = o ?? 5",
	);
}

#[test]
fn unwrap_default_type_is_enforced() {
	assert_error_contains(
		"enum Option<T> { Some(value: T), None }
		 interface Unwrap<Output> { func unwrap(default: Output): Output }
		 impl<T> Unwrap<Output = T> for Option<T> {
		   func unwrap(default: T): T = default
		 }
		 func get(o: Option<int>): int = o ?? true",
		"mismatched types",
	);
}

#[test]
fn ambiguous_method_across_interfaces_is_reported() {
	assert_error_contains(
		"interface A { func m(): int }
		 interface B { func m(): int }
		 struct P(x: int)
		 impl A for P { func m(): int = 1 }
		 impl B for P { func m(): int = 2 }
		 func f(p: P): int = p.m()",
		"ambiguous",
	);
}

#[test]
fn unknown_method_is_reported() {
	assert_error_contains(
		"struct P(x: int)
		 func f(p: P): int = p.nope()",
		"no method `nope`",
	);
}

#[test]
fn int_literal_widens_in_method_argument_position() {
	// `x.close_to(0)`: the `0` literal widens to the `float` parameter, as it would in
	// check position — method arguments get the same literal rule.
	assert_ok(
		"interface Near { func close_to(other: float): boolean }
		 impl Near for float { func close_to(other: float): boolean = true }
		 func test(x: float): boolean = x.close_to(0)",
	);
}

#[test]
fn non_literal_int_argument_still_clashes() {
	assert_error_contains(
		"interface Near { func close_to(other: float): boolean }
		 impl Near for float { func close_to(other: float): boolean = true }
		 func test(x: float, n: int): boolean = x.close_to(n)",
		"mismatched types",
	);
}

#[test]
fn namespaced_call_through_generic_bound() {
	// `T.default()` resolves through `T`'s `Default` bound and yields a `T`.
	assert_ok(
		"interface Default { func default(): self }
		 impl Default for int { func default() = 0 }
		 func make<T: Default>(): T = T.default()",
	);
}

#[test]
fn namespaced_call_without_the_bound_is_reported() {
	// No `Default` bound on `T`, so `T.default()` has nothing to resolve against.
	assert_error_contains(
		"interface Default { func default(): self }
		 func make<T>(): T = T.default()",
		"no namespaced function `default`",
	);
}

#[test]
fn duplicate_impl_is_a_coherence_error() {
	assert_error_contains(
		"interface Show { func show(): string }
		 struct P(x: int)
		 impl Show for P { func show(): string = \"a\" }
		 impl Show for P { func show(): string = \"b\" }",
		"conflicting implementations",
	);
}

#[test]
fn argument_directed_overloads_do_not_conflict() {
	// Same self type (`int`), same interface, but disjoint `Other` bindings — these are
	// legitimate overloads, not a coherence violation.
	assert_ok(&format!(
		"{PLUS}
		 impl Plus<Other = int, Output = int> for int {{
		   func plus(other: int): int = other
		 }}
		 impl Plus<Other = float, Output = float> for int {{
		   func plus(other: float): float = other
		 }}",
	));
}

#[test]
fn method_resolves_through_generic_bound() {
	// `t.show()` has no `Show` impl to assemble against, but `T`'s declared bound provides
	// the method signature.
	assert_ok(
		"interface Show { func show(): string }
		 func render<T: Show>(t: T): string = t.show()",
	);
}

#[test]
fn method_resolves_through_impl_trait_parameter() {
	// `s: Show` is sugar for a generic parameter bounded by `Show`; `s.show()` resolves
	// through that synthetic bound.
	assert_ok(
		"interface Show { func show(): string }
		 func render(s: Show): string = s.show()",
	);
}

#[test]
fn impl_trait_parameter_method_return_is_enforced() {
	// `show()` returns `string`, so using it where an `int` is expected is a mismatch —
	// proving the bound's method return type flows to the call site.
	assert_error_contains(
		"interface Show { func show(): string }
		 func render(s: Show): int = s.show()",
		"mismatched types",
	);
}

const INTO: &str = "interface Into<Other> { func into(): Other }\n";

#[test]
fn cast_between_scalars_is_built_in() {
	// `n as float` is a built-in numeric conversion — no `Into` impl required.
	assert_ok("func f(n: int): float = n as float");
}

#[test]
fn cast_via_into_impl_is_allowed() {
	assert_ok(&format!(
		"{INTO}
		 struct P(x: int)
		 impl Into<string> for P {{ func into(): string = \"p\" }}
		 func f(p: P): string = p as string",
	));
}

#[test]
fn cast_without_into_impl_is_reported() {
	// No `Into<Other = string>` impl for `P`, so the cast has nothing to resolve against.
	assert_error_contains(
		&format!(
			"{INTO}
			 struct P(x: int)
			 func f(p: P): string = p as string",
		),
		"cannot cast",
	);
}

#[test]
fn inner_impl_operator_resolves() {
	// An `impl Plus { … }` nested in the struct body is collected like a top-level
	// `impl … for`, so `a + b` resolves. `plus` omits its return type, which defaults to
	// the interface's `Output = Vec2`, so the sum is usable as a `Vec2`.
	assert_ok(&format!(
		"{PLUS}
		 struct Vec2(x: int, y: int) {{
		   impl Plus<Other = Vec2, Output = Vec2> {{
		     func plus(other: Vec2) = other
		   }}
		 }}
		 func add(a: Vec2, b: Vec2): Vec2 = a + b",
	));
}

#[test]
fn impl_for_omitted_return_uses_interface_return() {
	// A top-level `impl … for` method that omits its return type inherits the interface's
	// declared return (`Output = Vec2`), not `void` — so the sum is a `Vec2`. Before this
	// defaulting the return was `void` and `add` would fail to typecheck.
	assert_ok(&format!(
		"{PLUS}
		 struct Vec2(x: int, y: int)
		 impl Plus<Other = Vec2, Output = Vec2> for Vec2 {{
		   func plus(other: Vec2) = other
		 }}
		 func add(a: Vec2, b: Vec2): Vec2 = a + b",
	));
}

#[test]
fn inner_impl_body_is_checked() {
	// The nested impl's method body is verified: `true` (a `boolean`) does not match the
	// declared `Vec2` return.
	assert_error_contains(
		&format!(
			"{PLUS}
			 struct Vec2(x: int, y: int) {{
			   impl Plus<Other = Vec2, Output = Vec2> {{
			     func plus(other: Vec2): Vec2 = true
			   }}
			 }}",
		),
		"mismatched types",
	);
}

#[test]
fn blanket_and_concrete_do_not_conflict() {
	// A blanket impl and a concrete impl of the same interface overlap by construction,
	// but concrete-beats-blanket specificity disambiguates them, so this is allowed.
	assert_ok(
		"interface Equals<Other> { func equals(other: Other): boolean }
		 impl<T> Equals<Other = T> for T { func equals(other: T): boolean = true }
		 struct P(x: int)
		 impl Equals<Other = P> for P { func equals(other: P): boolean = false }",
	);
}
