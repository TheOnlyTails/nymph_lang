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

// ── Static tuple spreads ───────────────────────────────────────────────────

#[test]
fn tuple_spread_flattens_to_the_exact_result_type() {
	assert_ok("func f(): #(int, boolean, string) = #(1, ...#(true, \"x\"))");
}

#[test]
fn tuple_spread_supports_multiple_interleaved_and_empty_sources() {
	assert_ok(
		"func f(): #(int, boolean, string, uint) = #(1, ...#(), ...#(true, \"x\"), 2u, ...#())",
	);
}

#[test]
fn tuple_spread_preserves_nested_tuple_elements() {
	assert_ok("func f(): #(#(int, boolean), string) = #(...#(#(1, true)), \"x\")");
}

#[test]
fn tuple_spread_rejects_a_list_source_with_a_precise_diagnostic() {
	assert_error_contains(
		"func f(xs: #[int]): #() = #(...xs)",
		"tuple spread requires a statically shaped tuple, found `#[int]`",
	);
}

#[test]
fn tuple_spread_rejects_a_dynamically_shaped_generic_source() {
	assert_error_contains(
		"func f<T>(xs: T): #() = #(...xs)",
		"tuple spread requires a statically shaped tuple, found `T`",
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
		"Plus",
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
	assert_error_contains("func f<T>(a: T, b: T): T = a + b", "Plus");
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
		"Plus",
	);
}

// ── Slice 4C-c, Task 1: comparison/logical diagnostics (W1, W3) ─────────────

#[test]
fn unbounded_generic_less_than_is_reported() {
	// W1: an unbounded generic parameter has no `Comparable` bound to dispatch
	// `<` to — a `NotImplemented` diagnostic, not a silent native `<` on
	// still-generic operands (the pre-4C-c behavior).
	assert_error_contains("func f<T>(a: T, b: T): boolean = a < b", "Comparable");
}

#[test]
fn never_pinned_infer_less_than_reports_cannot_infer_operand_type() {
	// W1: `xs[0] < xs[0]` whose element type never gets pinned down by the end of
	// the body reports `CannotInferOperandType`, exactly like the arithmetic
	// arm's `unresolved_prefix_operand_reports_cannot_infer_operand_type`-style
	// guard — this is a *new* diagnostic on a program that used to compile clean
	// with a silently wrong `BuiltinEager` resolution (see the 4C-c investigation
	// brief's "corrections" note).
	assert_error_contains(
		"func f(): boolean = {
		   let xs = #[]
		   xs[0] < xs[0]
		 }",
		"cannot infer",
	);
}

#[test]
fn bounded_generic_logical_and_is_reported() {
	// W3: `&&`/`||` on a rigid, still-generic `Param` operand fails to unify
	// against `boolean` — a plain `mismatched types` diagnostic. No routing
	// change was needed here; this pins the already-loud behavior.
	assert_error_contains(
		"func f<T>(a: T, b: T): boolean = a && b",
		"mismatched types",
	);
}

#[test]
fn struct_logical_and_operand_is_reported_with_a_help_hint() {
	// The language decision this test pins: `&&`/`||` are never overloadable
	// (mirroring Rust), so a struct operand is a plain mismatch against
	// `boolean` — never an `And`/`Or` interface dispatch — but the diagnostic
	// carries a dedicated help hint explaining *why* there's no overload to
	// reach for, rather than reading like a missing-impl bug.
	let parsed = parse_module(
		"struct P(x: int)
		 func f(a: P, b: boolean): boolean = a && b",
		"test",
	);
	assert!(
		parsed.diagnostics.iter().all(|d| !d.is_error()),
		"source failed to parse: {:?}",
		parsed.diagnostics
	);
	let checked = check_module(&parsed.tree);
	let diag = checked
		.diags
		.iter()
		.find(|d| d.is_error())
		.expect("expected an error");
	assert!(
		diag.message.contains("mismatched types"),
		"unexpected message: {}",
		diag.message
	);
	assert_eq!(
		diag.help.as_deref(),
		Some(
			"logical operators are not overloadable — `&&`/`||` always take booleans and short-circuit"
		)
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
fn coherence_skips_distinct_heads_without_hiding_same_head_conflicts() {
	let errors = check(
		"interface Show { func show(): string }
		 type Number = int
		 impl Show for Number { func show(): string = \"number\" }
		 impl Show for string { func show(): string = \"string\" }
		 impl Show for int { func show(): string = \"int\" }
		 impl Show for boolean { func show(): string = \"boolean\" }
		 impl Show for string { func show(): string = \"other string\" }",
	);
	let conflicts = errors
		.iter()
		.filter(|error| error.contains("conflicting implementations"))
		.count();
	assert_eq!(conflicts, 2, "expected exactly two conflicts: {errors:?}");
}

#[test]
fn coherence_checks_an_unknown_head_against_a_concrete_head() {
	assert_error_contains(
		"interface Show { func show(): string }
		 impl Show for _ { func show(): string = \"unknown\" }
		 impl Show for int { func show(): string = \"int\" }",
		"conflicting implementations",
	);
}

#[test]
fn coherence_checks_matching_generic_nominal_heads() {
	assert_error_contains(
		"interface Show { func show(): string }
		 struct Box<T>(value: T)
		 impl<T> Show for Box<T> { func show(): string = \"generic\" }
		 impl Show for Box<int> { func show(): string = \"int\" }",
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
fn partial_implementation_cannot_supply_an_unimplemented_required_member() {
	assert_error_contains(
		"interface Comparable<Other> {
		   func compare_to(other: Other): int
		   func minmax(other: Other): int = 0
		 }
		 impl<T> Comparable<Other = T> for T {
		   func minmax(other: T): int = 1
		 }
		 func compare(value: int): int = value.compare_to(value)",
		"no method `compare_to` found",
	);
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
fn bare_method_value_is_ambiguous_across_receiver_applicable_bounds_despite_arity() {
	assert_error_contains(
		"interface A { func m(): int }
		 interface B { func m(value: int): int }
		 func f<T: A + B>(value: T): int = { let m = value.m m() }",
		"ambiguous call",
	);
}

#[test]
fn bare_generic_inherent_method_value_instantiates_and_remains_callable() {
	assert_ok(
		"struct Box(value: int) { func keep<T>(value: T): T = value }
		 func f(box: Box): string = { let keep = box.keep keep(\"ok\") }",
	);
}

#[test]
fn bare_generic_bound_method_value_instantiates_and_remains_callable() {
	assert_ok(
		"interface Keep { func keep<T>(value: T): T }
		 func f<K: Keep>(keeper: K): string = { let keep = keeper.keep keep(\"ok\") }",
	);
}

#[test]
fn bare_inherited_generic_default_method_value_instantiates_and_remains_callable() {
	assert_ok(
		"interface Keep { func keep<T>(value: T): T = value }
		 struct Box(value: int)
		 impl Keep for Box {}
		 func f(box: Box): string = { let keep = box.keep keep(\"ok\") }",
	);
}

#[test]
fn bare_bounded_generic_inherent_method_value_enforces_its_obligation() {
	assert_error_contains(
		"interface Area { func area(): int }
		 struct Box(value: int) { func apply<T: Area>(value: T): int = value.area() }
		 func f(box: Box): int = { let apply = box.apply apply(1) }",
		"does not implement `Area`",
	);
}

#[test]
fn mut_typed_argument_is_accepted_through_an_interface_impl_method_call() {
	// `commit_method` (the interface-impl dispatch path) checks arguments via
	// `unify_arg`, same as the inherent-method path — NN3's one-way `mut T <:
	// T` must hold here too.
	assert_ok(
		"interface Adds { func plus(other: int): int }
		 struct P(base: int)
		 impl Adds for P { func plus(other: int): int = this.base + other }
		 func f(): int = {
		   let mut n = 1
		   let p = P(base = 0)
		   p.plus(n)
		 }",
	);
}

#[test]
fn mut_typed_argument_is_accepted_through_a_generic_bound_method_call() {
	// `resolve_param_method` (the generic-bound dispatch path) also checks
	// arguments via `unify_arg`.
	assert_ok(
		"interface Adds { func plus(other: int): int }
		 func f<T: Adds>(t: T): int = {
		   let mut n = 1
		   t.plus(n)
		 }",
	);
}

#[test]
fn generic_bound_method_call_enforces_method_owned_bound() {
	assert_error_contains(
		"interface Area { func area(): int }
		 interface Mapper { func apply<T: Area>(value: T): int }
		 func f<M: Mapper>(mapper: M): int = mapper.apply(1)",
		"does not implement `Area`",
	);
}

#[test]
fn generic_bound_method_call_opaque_return_retains_exact_bound() {
	assert_ok(
		"interface Area { func area(): int }
		 interface Factory { func make(): Area }
		 func f<F: Factory>(factory: F): int = factory.make().area()",
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

// ── Slice 4F: call-site instantiation of `impl Trait` param sugar ──────────
//
// Body-side resolution of `s.show()` above already worked before this slice.
// What didn't: *calling* such a function with a concrete argument, because the
// synthetic `Param` minted for the sugar never got a fresh variable at the
// call site (only declared generics did) and so stayed rigid, making the
// concrete argument fail to unify against it.

#[test]
fn impl_trait_parameter_accepts_a_concrete_implementing_argument() {
	assert_ok(
		"interface Area { func area(): int }
		 struct Square(side: int)
		 impl Area for Square { func area(): int = this.side * this.side }
		 func measure(shape: Area): int = shape.area()
		 func total(s: Square): int = measure(s)",
	);
}

#[test]
fn impl_trait_parameter_via_function_value_reference_also_instantiates() {
	// A bare identifier reference to the function (not a direct call) types
	// through the same `type_of_def` -> `fn_type_of` path as a call, so it
	// must get the same fresh-per-use-site treatment.
	assert_ok(
		"interface Area { func area(): int }
		 struct Square(side: int)
		 impl Area for Square { func area(): int = this.side * this.side }
		 func measure(shape: Area): int = shape.area()
		 func total(s: Square): int = {
		   let m = measure
		   m(s)
		 }",
	);
}

#[test]
fn two_impl_trait_parameters_of_the_same_interface_are_independent() {
	// Every mention of an interface name in type position mints its own
	// synthetic `Param` (they are anonymous — no surface syntax can refer back
	// to one), so two `Area`-sugared params here are two distinct synthetics;
	// each must independently freshen and unify with its own argument's type.
	assert_ok(
		"interface Area { func area(): int }
		 struct Square(side: int)
		 impl Area for Square { func area(): int = this.side * this.side }
		 struct Rect(w: int, h: int)
		 impl Area for Rect { func area(): int = this.w * this.h }
		 func combine(a: Area, b: Area): int = a.area() + b.area()
		 func total(s: Square, r: Rect): int = combine(s, r)",
	);
}

#[test]
fn impl_trait_return_position_stays_rejected_when_it_disagrees_with_the_body() {
	// A param-position and a return-position mention of the same interface
	// name mint *different* opaque types. The callee cannot promise one stable
	// concrete return implementor by forwarding an arbitrary argument, so the
	// body check rejects it through the opaque return's interface obligation.
	assert_error_contains(
		"interface Area { func area(): int }
		 func id(x: Area): Area = x",
		"does not implement `Area`",
	);
}

#[test]
fn impl_trait_parameter_non_implementing_argument_parity_with_explicit_generic() {
	// Slice 4G closes the KNOWN PRE-EXISTING GAP the previous version of this
	// test documented: `int` does not implement `Area`, so both the declared
	// generic (`T: Area`) and the `impl Trait` sugar spelling now diagnose at
	// the call site instead of silently accepting a value that would crash
	// (`shape.area is not a function`) at Node runtime. `impl Trait` sugar goes
	// through the exact same substitution/obligation machinery as a declared
	// generic (Z1's equivalence), so both get identical treatment here too.
	assert_error_contains(
		"interface Area { func area(): int }
		 func measure_explicit<T: Area>(shape: T): int = shape.area()
		 func total(n: int): int = measure_explicit(n)",
		"does not implement `Area`",
	);
	assert_error_contains(
		"interface Area { func area(): int }
		 func measure_sugar(shape: Area): int = shape.area()
		 func total(n: int): int = measure_sugar(n)",
		"does not implement `Area`",
	);
}

// ── Slice 4G: call-site bound enforcement ───────────────────────────────────
//
// Closes the soundness hole above: a generic function call whose argument does
// not implement the declared bound now diagnoses instead of type-checking and
// then crashing at JS runtime.

#[test]
fn bound_violation_via_declared_generic_is_reported() {
	assert_error_contains(
		"interface Area { func area(): int }
		 func measure<T: Area>(shape: T): int = shape.area()
		 func total(): int = measure(3)",
		"does not implement `Area`",
	);
}

#[test]
fn bound_violation_via_impl_trait_sugar_is_reported() {
	assert_error_contains(
		"interface Area { func area(): int }
		 func measure(shape: Area): int = shape.area()
		 func total(): int = measure(3)",
		"does not implement `Area`",
	);
}

#[test]
fn bound_satisfying_argument_stays_clean_both_spellings() {
	assert_ok(
		"interface Area { func area(): int }
		 struct Square(side: int)
		 impl Area for Square { func area(): int = this.side * this.side }
		 func measure_explicit<T: Area>(shape: T): int = shape.area()
		 func measure_sugar(shape: Area): int = shape.area()
		 func total(s: Square): int = measure_explicit(s) + measure_sugar(s)",
	);
}

#[test]
fn generic_to_generic_forwarding_stays_clean() {
	// `outer`'s own `T: Area` bound satisfies `measure`'s identical requirement:
	// the forwarded var resolves (at drain time) to `outer`'s own rigid `Param`,
	// whose `param_bounds` entry (live during `outer`'s own body drain) already
	// records `Area`.
	assert_ok(
		"interface Area { func area(): int }
		 func measure<T: Area>(shape: T): int = shape.area()
		 func outer<T: Area>(x: T): int = measure(x)",
	);
}

#[test]
fn generic_to_generic_forwarding_without_the_bound_is_reported() {
	// The mirror image of the case above: `outer`'s own `T` is unbounded, so
	// forwarding it into `measure`'s `T: Area` requirement is unsound and must
	// now be reported, not silently accepted.
	assert_error_contains(
		"interface Area { func area(): int }
		 func measure<T: Area>(shape: T): int = shape.area()
		 func outer<T>(x: T): int = measure(x)",
		"does not implement `Area`",
	);
}

#[test]
fn argful_bound_violation_is_reported() {
	// `T: Comparable<Other = T>` requires `A`'s own `Comparable` impl to compare
	// against itself — but `A` only implements `Comparable<Other = B>`. A
	// bare-interface-only check would wrongly accept this (`A` does implement
	// `Comparable`, just not with the required argument); full argful fidelity
	// (substituting the call-site subst into the bound's args before deferring)
	// correctly rejects it.
	assert_error_contains(
		"interface Comparable<Other> { func compare_to(other: Other): int }
		 struct A(v: int)
		 struct B(v: int)
		 impl Comparable<Other = B> for A { func compare_to(other: B): int = 0 }
		 func cmp<T: Comparable<Other = T>>(a: T): int = a.compare_to(a)
		 func use_it(a: A): int = cmp(a)",
		"does not implement `Comparable`",
	);
}

#[test]
fn argful_bound_satisfying_argument_stays_clean() {
	assert_ok(
		"interface Comparable<Other> { func compare_to(other: Other): int }
		 struct A(v: int)
		 impl Comparable<Other = A> for A { func compare_to(other: A): int = 0 }
		 func cmp<T: Comparable<Other = T>>(a: T): int = a.compare_to(a)
		 func use_it(a: A): int = cmp(a)",
	);
}

#[test]
fn generic_impl_body_uses_its_implemented_interface_bound() {
	assert_ok(
		"interface Comparable<Other> { func compare_to(other: Other): int }
		 impl<T> Comparable<Other = T> for T {
		   func minmax(other: T): int = this.compare_to(other)
		 }",
	);
}

#[test]
fn implemented_interface_bound_precedes_a_same_interface_constraint() {
	assert_ok(
		"interface Comparable<Other> { func compare_to(other: Other): int }
		 impl<T: Comparable<Other = int>> Comparable<Other = T> for T {
		   func minmax(other: T): int = this.compare_to(other)
		 }",
	);
}

#[test]
fn generic_impl_body_substitutes_self_in_implemented_interface_arguments() {
	assert_ok(
		"interface Comparable<Other> { func compare_to(other: Other): int }
		 impl<T> Comparable<Other = self> for T {
		   func minmax(other: T): int = this.compare_to(other)
		 }",
	);
}

// ── Slice 4G-b: method own-generic & constructor bound enforcement ─────────
//
// Closes the remaining holes 4G's ledger documented: an inherent method's own
// generic bound, and a struct/enum constructor's declared generic bound, were
// both unenforced — a call/construction whose argument didn't implement the
// bound type-checked and then crashed at JS runtime.

#[test]
fn method_own_generic_bound_violation_is_reported() {
	// `Box.apply<U: Area>` requires its argument to implement `Area` — `3` (an
	// `int`, with no `Area` impl in scope) must now be reported, not silently
	// accepted (this used to be a zero-diagnostic program, per the 4G ledger).
	assert_error_contains(
		"interface Area { func area(): int }
		 struct Box(v: int) { func apply<U: Area>(u: U): int = u.area() }
		 func total(b: Box): int = b.apply(3)",
		"does not implement `Area`",
	);
}

#[test]
fn method_own_generic_bound_satisfying_argument_stays_clean() {
	assert_ok(
		"interface Area { func area(): int }
		 struct Square(side: int)
		 impl Area for Square { func area(): int = this.side * this.side }
		 struct Box(v: int) { func apply<U: Area>(u: U): int = u.area() }
		 func total(b: Box, s: Square): int = b.apply(s)",
	);
}

#[test]
fn method_own_generic_bound_forwarding_stays_clean() {
	// The caller's own `T: Area` bound satisfies `apply`'s identical requirement
	// — the classic generic-to-generic forwarding case, now for methods too.
	assert_ok(
		"interface Area { func area(): int }
		 struct Box(v: int) { func apply<U: Area>(u: U): int = u.area() }
		 func outer<T: Area>(b: Box, x: T): int = b.apply(x)",
	);
}

#[test]
fn method_own_generic_bound_forwarding_without_the_bound_is_reported() {
	assert_error_contains(
		"interface Area { func area(): int }
		 struct Box(v: int) { func apply<U: Area>(u: U): int = u.area() }
		 func outer<T>(b: Box, x: T): int = b.apply(x)",
		"does not implement `Area`",
	);
}

#[test]
fn interface_impl_method_own_generic_stays_loud() {
	// CC3: interface-impl method own-generics are inexpressible (not a bound
	// enforcement gap — `finish_interface_impl` never pushes them into scope),
	// so a USED own-generic must still error loudly, not silently miscompile.
	assert_error_contains(
		"interface Mapper { func extra(): int }
		 interface Area { func area(): int }
		 struct Square(side: int)
		 impl Mapper for Square { func extra<U: Area>(u: U): int = u.area() }",
		"cannot find type",
	);
}

#[test]
fn struct_ctor_bound_violation_is_reported() {
	assert_error_contains(
		"interface Area { func area(): int }
		 struct Container<T: Area>(value: T)
		 func make(): Container<int> = Container(value = 3)",
		"does not implement `Area`",
	);
}

#[test]
fn struct_ctor_bound_satisfying_argument_stays_clean() {
	assert_ok(
		"interface Area { func area(): int }
		 struct Square(side: int)
		 impl Area for Square { func area(): int = this.side * this.side }
		 struct Container<T: Area>(value: T)
		 func make(s: Square): Container<Square> = Container(value = s)",
	);
}

#[test]
fn enum_ctor_bound_violation_is_reported() {
	assert_error_contains(
		"interface Area { func area(): int }
		 enum Holder<T: Area> { Some(value: T), Empty }
		 func make(): Holder<int> = Holder.Some(value = 3)",
		"does not implement `Area`",
	);
}

#[test]
fn enum_ctor_bound_satisfying_argument_stays_clean() {
	assert_ok(
		"interface Area { func area(): int }
		 struct Square(side: int)
		 impl Area for Square { func area(): int = this.side * this.side }
		 enum Holder<T: Area> { Some(value: T), Empty }
		 func make(s: Square): Holder<Square> = Holder.Some(value = s)",
	);
}

#[test]
fn nullary_enum_ctor_bound_violation_is_reported() {
	// A nullary variant reference still instantiates the enum's generics (see
	// `variant_value`), so it needs the same enforcement as a labeled
	// construction — even though `T` is never actually witnessed by a runtime
	// value in the `Empty` case, the annotation pins it to `int` (which does not
	// implement `Area`).
	assert_error_contains(
		"interface Area { func area(): int }
		 enum Holder<T: Area> { Some(value: T), Empty }
		 func make(): Holder<int> = Holder.Empty",
		"does not implement `Area`",
	);
}

#[test]
fn pattern_position_does_not_enforce_ctor_bounds() {
	// CC3: destructuring an existing (already validly constructed) value must
	// not re-check the bound — `peek`'s own `T` is unbounded, and a pattern-site
	// obligation would wrongly reject a program that never itself constructs an
	// unbounded `Holder`.
	assert_ok(
		"interface Area { func area(): int }
		 enum Holder<T: Area> { Some(value: T), Empty }
		 func peek<T>(h: Holder<T>): int = match (h) {
		   Some(value) -> 1,
		   Empty -> 0,
		 }",
	);
}

#[test]
fn unapplied_constrained_function_value_may_remain_underdetermined() {
	// Representation-parity test for Stage 4: merely taking a constrained generic
	// function as a value leaves its fresh type variable underdetermined. Finalizing
	// that typed obligation must preserve the existing silent recovery behavior.
	assert_ok(
		"interface Area { func area(): int }
		 func measure<T: Area>(shape: T): int = shape.area()
		 func keep(): void = { let unapplied = measure }",
	);
}

#[test]
fn stdlib_range_style_bound_via_blanket_impl_stays_clean() {
	// Mirrors stdlib's `Range<Idx: Comparable<Idx>>`: constructing with `int`
	// satisfies the bound through the unconstrained blanket impl fallback (Slice
	// 4G's `holds` fallback for the concrete-type case), so this must stay
	// zero-diagnostic — the canary that this slice does not regress
	// `stdlib_typechecks_cleanly`.
	assert_ok(
		"interface Comparable<Other> { func compare_to(other: Other): int }
		 impl<T> Comparable<Other = T> for T { func compare_to(other: T): int = 0 }
		 struct Range<Idx: Comparable<Idx>>(start: Idx, end: Idx)
		 func make(): Range<int> = Range(start = 0, end = 10)",
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
fn cast_with_no_into_interface_in_scope_is_reported() {
	// Slice 4K: `check_cast` used to return silently whenever `Into` isn't even
	// declared in the module (every real `nymph-compiler::compile()` program, since
	// it checks standalone with no stdlib linkage) — a non-scalar cast type-checked
	// completely unchecked and only died later at lowering's unresolved-cast panic.
	// This is now a loud, distinct diagnostic from `cast_without_into_impl_is_reported`
	// above (which has `Into` in scope, just no matching impl).
	assert_error_contains(
		"struct P(x: int)
		 func f(p: P): string = p as string",
		"no `Into` interface is in scope",
	);
}

#[test]
fn cast_between_int_and_uint_is_built_in() {
	assert_ok("func f(n: int): uint = n as uint");
}

#[test]
fn cast_from_char_to_int_is_built_in() {
	assert_ok("func f(c: char): int = c as int");
}

#[test]
fn cast_from_int_to_char_is_built_in() {
	assert_ok("func f(n: int): char = n as char");
}

#[test]
fn invalid_numeric_literals_cannot_be_cast_to_char() {
	for literal in ["-1", "55296", "57343u", "1114112", "-1.9", "1114112.9"] {
		assert_error_contains(
			&format!("func f(): char = {literal} as char"),
			"not a valid Unicode scalar value",
		);
	}
}

#[test]
fn valid_numeric_literals_can_be_cast_to_char_after_truncation() {
	for literal in ["0", "-0.9", "65.9", "55295", "57344", "128512", "1114111"] {
		assert_ok(&format!("func f(): char = {literal} as char"));
	}
}

#[test]
fn identity_cast_needs_no_into_impl_even_without_into_in_scope() {
	// `Foo as Foo` is the identity case (`src == target_r`) and short-circuits
	// before the `Into` lookup entirely — no diagnostic even though this module
	// declares no `Into` interface at all.
	assert_ok(
		"struct P(x: int)
		 func f(p: P): P = p as P",
	);
}

#[test]
fn cast_via_an_into_interface_with_a_custom_method_name_type_checks() {
	// Defect 1 regression: `Into` is looked up purely by the NAME "Into" (`self.defs
	// .get("Into")`), so a local interface literally called `Into` whose sole method
	// isn't named `into` (e.g. `convert`) is a legal, checker-visible shape. This must
	// type-check clean — the checker itself has no reason to reject it, since `holds`
	// only checks the interface args, not any particular method name.
	assert_ok(
		r#"
		interface Into<Other> { func convert(): Other }
		struct P(x: int)
		impl Into<string> for P { func convert(): string = "p" }
		func f(p: P): string = p as string
		"#,
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
fn nested_impl_with_duplicate_method_name_is_reported() {
	// HH3 (Slice 4K): before this, a nested `impl Iface { .. }` block declaring the
	// same method name twice type-checked completely clean (silent last-wins in
	// `finish_interface_impl`'s `methods.insert`), and the first (shadowed, never
	// checked) body still reached `lower_hir.rs`'s `assert_no_duplicate_methods`,
	// which panicked — this is the ledgered ICE probe turned into a diagnostic test.
	assert_error_contains(
		&format!(
			"{PLUS}
			 struct Vec2(x: int, y: int) {{
			   impl Plus<Other = Vec2, Output = Vec2> {{
			     func plus(other: Vec2): Vec2 = other
			     func plus(other: Vec2): Vec2 = other
			   }}
			 }}",
		),
		"defined more than once",
	);
}

#[test]
fn top_level_impl_for_with_duplicate_method_name_is_reported() {
	// The same insert point (`finish_interface_impl`) is shared by top-level `impl
	// … for` blocks, so the guard closes this shape too, not just the nested one.
	assert_error_contains(
		&format!(
			"{PLUS}
			 struct Vec2(x: int, y: int)
			 impl Plus<Other = Vec2, Output = Vec2> for Vec2 {{
			   func plus(other: Vec2): Vec2 = other
			   func plus(other: Vec2): Vec2 = other
			 }}",
		),
		"defined more than once",
	);
}

#[test]
fn nested_impl_method_colliding_with_inherent_method_is_reported() {
	// HH3's actual named scenario: an inherent method (declared directly in the
	// struct body) and a nested `impl Iface { .. }` method of the SAME name. This
	// used to type-check clean (the two collection passes — `members.rs`'s inherent
	// map and `iface.rs`'s `finish_interface_impl` — never compared notes) and then
	// panic in `lower_hir.rs`'s `assert_no_duplicate_methods`.
	assert_error_contains(
		&format!(
			"{PLUS}
			 struct Vec2(x: int, y: int) {{
			   func plus(other: Vec2): Vec2 = other
			   impl Plus<Other = Vec2, Output = Vec2> {{
			     func plus(other: Vec2): Vec2 = other
			   }}
			 }}",
		),
		"defined more than once",
	);
}

#[test]
fn top_level_impl_for_method_colliding_with_inherent_method_is_reported() {
	// Same collision, top-level `impl … for` shape instead of nested.
	assert_error_contains(
		&format!(
			"{PLUS}
			 struct Vec2(x: int, y: int) {{
			   func plus(other: Vec2): Vec2 = other
			 }}
			 impl Plus<Other = Vec2, Output = Vec2> for Vec2 {{
			   func plus(other: Vec2): Vec2 = other
			 }}",
		),
		"defined more than once",
	);
}

#[test]
fn nested_impl_method_does_not_collide_with_a_same_named_namespaced_static() {
	// Defect 2: a `namespace func plus` static and a nested `impl Iface { .. }`
	// instance method sharing the name `plus` are DIFFERENT JS slots (a class
	// static vs. a prototype/instance method — see `collect_adt_methods`, which
	// keeps namespaced members in a wholly separate `statics` list from `methods`,
	// each independently checked by `assert_no_duplicate_methods`). This must check
	// clean, unlike the genuine inherent-vs-impl collision above.
	assert_ok(&format!(
		"{PLUS}
		 struct Vec2(x: int, y: int) {{
		   namespace func plus(a: Vec2, b: Vec2): Vec2 = a
		   impl Plus<Other = Vec2, Output = Vec2> {{
		     func plus(other: Vec2): Vec2 = other
		   }}
		 }}",
	));
}

#[test]
fn top_level_impl_for_method_does_not_collide_with_a_same_named_namespaced_static() {
	// Same non-collision, top-level `impl … for` shape instead of nested.
	assert_ok(&format!(
		"{PLUS}
		 struct Vec2(x: int, y: int) {{
		   namespace func plus(a: Vec2, b: Vec2): Vec2 = a
		 }}
		 impl Plus<Other = Vec2, Output = Vec2> for Vec2 {{
		   func plus(other: Vec2): Vec2 = other
		 }}",
	));
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

// ── Resolver precedence for `Param` receivers (MM1/MM2/MM3) ──
//
// A still-generic `TyKind::Param` receiver used to lose a method name declared
// by the param's OWN bound to an unrelated blanket impl of the same name:
// `head_of(Param)` is `None`, so phase 1's impl-index search in
// `resolve_method` (`solve.rs`) only ever finds each interface's blanket
// bucket, and a blanket impl unifies with *any* receiver. The fix consults the
// param's declared bounds (`resolve_param_method`) FIRST, falling through to
// the ordinary blanket search only when none of the bounds provide the method
// — never returning eagerly on a bounds miss, which would break unconstrained
// blanket dispatch (see the second test below). `tests/operator_resolutions.rs`
// pins the same two shapes against the real stdlib prelude.

#[test]
fn param_bound_method_wins_over_unrelated_blanket_impl() {
	// Collision: `T`'s own bound `Mine::pick` (returns `int`) must beat the
	// unrelated blanket `Blanket::pick` (returns `boolean`) in scope — before the
	// fix, the blanket's receiver-matches-everything unify made phase 1 commit it
	// first, and `f` (declared to return `int`) failed with a `boolean` mismatch.
	assert_ok(
		"interface Blanket<Other> { func pick(other: Other): boolean }
		 impl<T> Blanket<Other = T> for T { func pick(other: T): boolean = true }
		 interface Mine<Other> { func pick(other: Other): int }
		 func f<T: Mine<Other = T>>(a: T, b: T): int = a.pick(b)",
	);
}

#[test]
fn unconstrained_param_still_dispatches_through_blanket_impl() {
	// The blanket-dispatch behavior the fix must preserve: `T` has no bound at
	// all, so `resolve_param_method` finds nothing and control must fall through
	// (never return early) to the ordinary blanket-bucket search, which resolves
	// `a.equals(b)` through the blanket `Equals` impl. A prior, reverted fix
	// attempt returned eagerly on this bounds-miss and broke exactly this shape
	// with a spurious "no method `equals`" error.
	assert_ok(
		"interface Equals<Other> { func equals(other: Other): boolean }
		 impl<T> Equals<Other = T> for T { func equals(other: T): boolean = true }
		 func same<T>(a: T, b: T): boolean = a.equals(b)",
	);
}

#[test]
fn two_conflicting_bounds_deterministically_pick_the_first_declared() {
	// MM3: when two bounds on one param both declare the same method name with
	// different return types, `resolve_param_method` iterates
	// `param_bounds`/`synthetic_bounds` in declaration order and returns on the
	// FIRST bound that provides the name — deterministic, and silent (no
	// ambiguity diagnostic). This is type-sound from the resolution's own
	// standpoint: a genuine clash surfaces loudly as a return-type mismatch (the
	// next test), never as silent-wrong-JS from the pick itself.
	assert_ok(
		"interface A<Other> { func m(other: Other): int }
		 interface B<Other> { func m(other: Other): string }
		 func f<T: A<Other = T> + B<Other = T>>(a: T, b: T): int = a.m(b)",
	);
}

#[test]
fn conflicting_bound_pick_surfaces_loudly_as_a_type_mismatch() {
	// Same two bounds, `A` declared first again, but `f` now expects `B`'s
	// `string` return — the deterministic first-bound pick still resolves to
	// `A` (returning `int`), so this is a loud "mismatched types" error, never a
	// silently wrong resolution.
	assert_error_contains(
		"interface A<Other> { func m(other: Other): int }
		 interface B<Other> { func m(other: Other): string }
		 func f<T: A<Other = T> + B<Other = T>>(a: T, b: T): string = a.m(b)",
		"mismatched types",
	);
}

#[test]
fn declaration_order_of_bounds_controls_the_deterministic_pick() {
	// Reversing the bound order (`B` first) flips which interface wins: same
	// method name, same param, but now `B`'s `string` return resolves cleanly
	// because `B` is declared first this time.
	assert_ok(
		"interface A<Other> { func m(other: Other): int }
		 interface B<Other> { func m(other: Other): string }
		 func f<T: B<Other = T> + A<Other = T>>(a: T, b: T): string = a.m(b)",
	);
}
