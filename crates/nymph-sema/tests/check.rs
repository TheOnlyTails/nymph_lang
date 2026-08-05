//! End-to-end checker tests: parse a small Nymph program and assert on the
//! diagnostics the type checker produces. Milestone A covers functions, generics,
//! ADTs, closures, and local inference.

use nymph_sema::check_module;
use nymph_syntax::parse_module;

#[test]
fn anonymous_closure_returns_are_checked_against_the_closure_result() {
	let parsed = parse_module(
		"func value(): int = { let closure: (int) -> boolean = { if ($0 > 0) { return 1 } true }\nif (closure(1)) 1 else 0 }",
		"test",
	);
	let checked = check_module(&parsed.tree);
	assert!(
		!checked.diags.is_empty(),
		"a return from the anonymous closure was checked against the enclosing function"
	);
}

#[test]
fn forward_generic_alias_substitutes_its_owned_target() {
	let parsed = parse_module(
		"func identity(value: Later<int>): int = value\ntype Later<T> = T",
		"test",
	);
	let checked = check_module(&parsed.tree);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
}

#[test]
fn recursive_alias_keeps_the_recursive_reference_span() {
	let source = "type Loop = Loop\nfunc use(value: Loop): void = {}";
	let parsed = parse_module(source, "test");
	let checked = check_module(&parsed.tree);
	let recursive = checked
		.diags
		.iter()
		.find(|diagnostic| {
			diagnostic
				.message
				.contains("type alias expands recursively")
		})
		.expect("expected recursive alias diagnostic");
	assert_eq!(recursive.span.start, source.find("Loop\nfunc").unwrap());
}

#[test]
fn external_let_linkage_errors_are_structured_diagnostics() {
	for (source, message) in [
		(
			"external(missing_value) let value: float",
			"is not registered",
		),
		(
			"external(println) let value: float",
			"registered as a function",
		),
		(
			"external(max_float) let mut value: float",
			"external lets are immutable",
		),
		(
			"external(max_float) let value: boolean",
			"incompatible declared type",
		),
		(
			"external(max_float) func value(): float",
			"registered as a value, not a function",
		),
	] {
		let parsed = parse_module(source, "test");
		let checked = nymph_sema::check_module(&parsed.tree);
		assert!(
			checked
				.diags
				.iter()
				.any(|diag| diag.message.contains(message)),
			"{source}: {:?}",
			checked.diags
		);
	}
}

#[test]
fn value_markers_are_rejected_for_external_functions_in_every_member_shape() {
	for source in [
		"struct S { external(max_float) func bad(): float }",
		"enum E { A external(max_float) func bad(): float }",
		"namespace N { external(max_float) namespace func bad(): float }",
		"struct S {} impl S { external(max_float) func bad(): float }",
		"interface I { func bad(): float } struct S {} impl I for S { external(max_float) func bad(): float }",
	] {
		let parsed = parse_module(source, "test");
		assert!(
			!parsed.diagnostics.iter().any(|diag| diag.is_error()),
			"parse errors for {source}: {:?}",
			parsed.diagnostics
		);
		let checked = check_module(&parsed.tree);
		assert!(
			checked.diags.iter().any(|diag| diag
				.message
				.contains("registered as a value, not a function")),
			"{source}: {:?}",
			checked.diags
		);
	}
}

#[test]
fn external_let_linkage_uses_resolved_declaration_type() {
	for source in [
		"type Scalar = float\nexternal(max_float) let value: Scalar",
		"external(max_float) let value: (float)",
	] {
		let parsed = parse_module(source, "test");
		let checked = check_module(&parsed.tree);
		assert!(checked.diags.is_empty(), "{source}: {:?}", checked.diags);
	}

	let source = "type Scalar = boolean\nexternal(max_float) let value: Scalar";
	let parsed = parse_module(source, "test");
	let checked = check_module(&parsed.tree);
	assert!(
		checked
			.diags
			.iter()
			.any(|diag| diag.message.contains("incompatible declared type")),
		"{source}: {:?}",
		checked.diags
	);
}

#[test]
fn implementation_external_let_linkage_uses_resolved_member_type() {
	for source in [
		"type Scalar = float\nstruct Box {}\nimpl Box { external(max_float) let value: Scalar }",
		"type Scalar = float\ninterface Limit { let value: float }\nstruct Box {}\nimpl Limit for Box { external(max_float) let value: Scalar }",
	] {
		let parsed = parse_module(source, "test");
		let checked = check_module(&parsed.tree);
		assert!(checked.diags.is_empty(), "{source}: {:?}", checked.diags);
	}

	for source in [
		"type Scalar = boolean\nstruct Box {}\nimpl Box { external(max_float) let value: Scalar }",
		"type Scalar = boolean\ninterface Limit { let value: boolean }\nstruct Box {}\nimpl Limit for Box { external(max_float) let value: Scalar }",
	] {
		let parsed = parse_module(source, "test");
		let checked = check_module(&parsed.tree);
		assert!(
			checked
				.diags
				.iter()
				.any(|diag| diag.message.contains("incompatible declared type")),
			"{source}: {:?}",
			checked.diags
		);
	}
}

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
fn binding_subpattern_exposes_outer_and_nested_types() {
	assert_ok(
		"func sum(value: #(int, int)): int = match (value) {
		   whole = #(left, right) -> whole[0] + left + right,
		 }",
	);
}

#[test]
fn binding_subpattern_duplicate_names_are_rejected_recursively() {
	assert_error_contains(
		"func bad(value: #(int, int)): int = match (value) {
		   same = #(same, _) -> 0,
		 }",
		"bound more than once",
	);
}

#[test]
fn binding_subpattern_union_requires_consistent_bindings() {
	assert_error_contains(
		"func bad(value: int): int = match (value) {
		   (left = 1 | right = 2) -> 0,
		   _ -> 1,
		 }",
		"same names",
	);
}

#[test]
fn binding_subpattern_union_requires_compatible_binding_types() {
	assert_error_contains(
		"enum Value { Number(value: int), Text(value: string) }
		 func bad(value: Value): int = match (value) {
		   Number(x) | Text(x) -> 0,
		 }",
		"mismatched types for union pattern binding `x`",
	);
	assert_error_contains(
		"enum Value { Mutable(value: mut #[int]), Immutable(value: #[int]) }
		 func bad(value: Value): int = match (value) {
		   Mutable(x) | Immutable(x) -> 0,
		 }",
		"mismatched types for union pattern binding `x`",
	);
}

#[test]
fn binding_subpattern_union_accepts_reordered_and_nested_shared_bindings() {
	assert_ok(
		"func select(value: #(int, int)): #(int, int) = match (value) {
		   (#(x = 1, y) | #(y, x = 2)) | #(x, y = 3) -> #(x, y),
		   _ -> #(0, 0),
		 }",
	);
}

#[test]
fn binding_subpattern_nested_union_rejects_missing_and_duplicate_names() {
	assert_error_contains(
		"func bad(value: #(int, int)): int = match (value) {
		   (#(x, y) | #(x, y)) | #(x, _) -> 0,
		   _ -> 1,
		 }",
		"same names",
	);
	assert_error_contains(
		"func bad(value: #(int, int)): int = match (value) {
		   (#(x, x) | #(x, _)) -> 0,
		   _ -> 1,
		 }",
		"bound more than once",
	);
}

#[test]
fn positional_nullary_variant_unions_bind_no_names() {
	assert_ok(
		"enum Inner { None, Zero }
		 enum Outer { Wrap(value: Inner) }
		 func classify(value: Outer): int = match (value) {
		   Wrap(None) | Wrap(Zero) -> 1,
		 }",
	);
}

#[test]
fn return_type_mismatch_is_reported() {
	assert_error_contains("func bad(): int = true", "mismatched types");
}

#[test]
fn closure_return_is_checked_against_the_closure_not_the_outer_function() {
	assert_error_contains(
		"func f(): int = { let g: (boolean) -> boolean = (b: boolean) -> { if (b) { return 1 } true } 1 }",
		"mismatched types",
	);
}

#[test]
fn nested_closure_return_contexts_restore_to_the_nearest_callable() {
	assert_ok(
		"func f(): int = { let outer: (boolean) -> string = (b: boolean) -> { let inner: () -> boolean = () -> { return true } if (b) { return \"outer\" } \"tail\" } return 7 }",
	);
}

#[test]
fn return_typechecks_in_general_expression_positions_and_callable_kinds() {
	assert_ok(
		r#"
		func id(value: int): int = value
		func positions(flag: boolean): int = id(1 + if (flag) return 2 else #[3][0])
		struct Value(value: int) {
			func get(flag: boolean): int = this.value + if (flag) return 4 else 1
		}
		interface DefaultValue {
			func default_value(flag: boolean): int = 1 + if (flag) return 5 else 2
		}
		impl DefaultValue for Value {}
		"#,
	);
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
fn field_variant_as_value_is_rejected() {
	// A field-carrying variant used as a first-class value has no representable
	// constructor in the current value ABI, so it is rejected (not miscompiled).
	assert_error_contains(
		"enum Opt { Some(value: int), None }
		 func f(): Opt = Some",
		"cannot be used as a value",
	);
}

#[test]
fn nullary_variant_as_value_is_ok() {
	// A nullary variant, by contrast, is a perfectly good value of the enum type.
	assert_ok(
		"enum Opt { Some(value: int), None }
		 func f(): Opt = None
		 func g(): Opt = Opt.None",
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
fn plain_let_with_an_explicit_mut_annotation_keeps_the_mut_type() {
	// NN2 (annotation-position `mut`) + NN4 (`let mut` sugar) are independent
	// axes: a plain `let` (no `let mut`) with an explicit `mut T` annotation
	// must still bind at `mut T` — the annotation is the authority here, not
	// the `let mut` keyword, which is a SEPARATE way to reach the same `mut`
	// type. Previously the plain-let branch unconditionally stripped `mut`
	// (only `let mut` ever applied it), silently discarding the annotation.
	assert_ok(
		"struct Counter(n: int)
		 func f(c: mut Counter): int = {
		   let x: mut Counter = c
		   x.n = x.n + 1
		   x.n
		 }",
	);
}

#[test]
fn let_mut_with_an_explicit_mut_annotation_accepts_a_fresh_plain_value() {
	// `let mut c: mut Counter = Counter(n = 0)` must type-check: the freshly
	// constructed `Counter(n = 0)` is plain (non-`mut`), but initializing a
	// `mut`-annotated binding only requires the value be assignable to the
	// PLAIN inner type (`mut` is a capability layer the binding gains, not a
	// runtime distinction the initializer must already carry) — mirrors the
	// un-annotated `let mut` form, which already accepts a plain value and
	// wraps it in `mut` after the fact.
	assert_ok(
		"struct Counter(n: int)
		 func f(): int = {
		   let mut c: mut Counter = Counter(n = 0)
		   c.n = c.n + 1
		   c.n
		 }",
	);
}

#[test]
fn field_slot_assign_through_an_immutable_receiver_is_rejected() {
	// NN5 headline enforcement: `p.field = v` requires a `mut` receiver. Through
	// an immutable parameter it is the `AssignFieldThroughImmutable` diagnostic —
	// the error path (the happy `mut`-receiver path is the run_node e2e tests).
	assert_error_contains(
		"struct Counter(n: int)
		 func f(c: Counter): int = {
		   c.n = c.n + 1
		   c.n
		 }",
		"through immutable",
	);
}

#[test]
fn cast_on_a_mut_typed_value_uses_the_builtin_path() {
	// Regression: `mut` is transparent to casting — a `let mut`/`mut`-param
	// scalar's identity or scalar cast must reach the built-in path, not fall
	// through to requiring an `Into` impl ("cannot cast `mut int` to `int`").
	assert_ok(
		"func f(x: int): float = {
		   let mut y = x
		   let same = y as int
		   same as float
		 }",
	);
}

#[test]
fn for_in_over_a_mut_list_types_the_element() {
	// Regression: iterating a `mut #[int]` must still bind `int` elements, not an
	// unconstrained fresh var — so a type error in the loop body is still caught.
	assert_error_contains(
		"func f(): int = {
		   let mut xs = #[1, 2, 3]
		   for (x in xs) {
		     let z: boolean = x
		     z
		   }
		   0
		 }",
		"mismatched types",
	);
}

#[test]
fn for_in_over_an_iterator_directly_types_the_element() {
	// RR1: a for-loop source that itself implements `Iterator<Item>` is used
	// directly (no `.iter()` hop) — the element type must flow through as
	// `Item`, not an unconstrained fresh var, so a body type error is caught.
	assert_error_contains(
		"struct Counter(n: int)
		 interface Iterator<Item> { func next(): Item }
		 impl Iterator<int> for Counter {
		   func next(): int = this.n
		 }
		 func f(): int = {
		   let c = Counter(n = 0)
		   for (x in c) {
		     let z: boolean = x
		     z
		   }
		   0
		 }",
		"mismatched types",
	);
}

#[test]
fn for_in_over_an_iterator_directly_requires_a_mut_receiver() {
	// The mut-safety gate (`MutMethodNeedsMutReceiver`) that an EXPLICIT
	// `c.next()` call would hit for a `mut func next` on a non-`mut` `c` must
	// also fire for the implicit `next()` call the `for`-loop desugar
	// generates — `resolve_iterable_source` resolves the `Iterator` impl but
	// must gate it exactly like `resolve_method` does for any other mutating
	// call, or a non-`mut` binding's fields get mutated through the loop.
	assert_error_contains(
		"struct Counter(n: int)
		 interface Iterator<Item> { mut func next(): Item }
		 impl Iterator<int> for Counter {
		   mut func next(): int = { this.n = this.n + 1 this.n }
		 }
		 func f(): int = {
		   let c = Counter(n = 0)
		   for (x in c) {
		     x
		   }
		   0
		 }",
		"mut",
	);
}

#[test]
fn for_in_over_an_iterator_directly_accepts_a_mut_receiver() {
	// The same program as above, but with `c` declared `mut`, must type-check
	// cleanly — the gate must not reject a receiver that IS mut.
	assert_ok(
		"struct Counter(n: int)
		 interface Iterator<Item> { mut func next(): Item }
		 impl Iterator<int> for Counter {
		   mut func next(): int = { this.n = this.n + 1 this.n }
		 }
		 func f(): int = {
		   let mut c = Counter(n = 0)
		   for (x in c) {
		     x
		   }
		   0
		 }",
	);
}

#[test]
fn for_in_over_an_iterable_via_iter_types_the_element() {
	// RR1: a for-loop source that implements `Iterable<T>` (not `Iterator`
	// itself) is desugared through `.iter()` — the element type must flow
	// through as `T`, read off the matched `Iterable` impl's own argument
	// rather than by typing `iter()`'s return (which loses `Item` through
	// `mint_synthetic_param`).
	assert_error_contains(
		"struct Counter(n: int)
		 interface Iterator<Item> { func next(): Item }
		 interface Iterable<T> { func iter(): Iterator<T> }
		 impl Iterator<int> for Counter {
		   func next(): int = this.n
		 }
		 struct Bag(c: Counter)
		 impl Iterable<int> for Bag {
		   func iter(): Iterator<int> = this.c
		 }
		 func f(): int = {
		   let b = Bag(c = Counter(n = 0))
		   for (x in b) {
		     let z: boolean = x
		     z
		   }
		   0
		 }",
		"mismatched types",
	);
}

#[test]
fn for_in_over_generic_iterable_bound_types_the_element() {
	assert_ok(
		"enum Option<T> { Some(value: T), None }
		 interface Iterator<Item> { mut func next(): Option<Item> }
		 interface Iterable<Item> { func iter(): Iterator<Item> }
		 func first_sum<T: Iterable<Item = int>>(items: T): int = {
		   let mut total = 0
		   for (item in items) { total = total + item }
		   total
		 }",
	);
}

#[test]
fn opaque_iterator_returns_accept_concrete_generic_implementors() {
	assert_ok(
		"interface Iterator<Item> { func item(): Item }
		 struct ListIter<Item>(item: Item)
		 impl<Item> Iterator<Item> for ListIter<Item> {
		   func item(): Item = this.item
		 }
		 struct MapLike<K, V>(entry: #(K, V))
		 impl<K, V> MapLike<K, V> {
		   func iter(): Iterator<#(K, V)> = ListIter(item = this.entry)
		 }
		 struct SetLike<Item>(item: Item)
		 impl<Item> SetLike<Item> {
		   func iter(): Iterator<Item> = ListIter(item = this.item)
		 }",
	);
}

#[test]
fn opaque_iterator_returns_preserve_interface_arguments() {
	assert_error_contains(
		"interface Iterator<Item> { func item(): Item }
		 struct StringIter
		 impl Iterator<string> for StringIter {
		   func item(): string = \"wrong\"
		 }
		 func iter(): Iterator<int> = StringIter",
		"does not implement `Iterator`",
	);
}

#[test]
fn for_in_over_a_non_iterable_source_is_diagnosed() {
	// RR1: a source that implements neither `Iterator` nor `Iterable` is a
	// hard error now, not a silent `self.fresh()` accept that let the loop
	// body escape type-checking entirely.
	assert_error_contains(
		"struct Foo(n: int)
		 func f(): int = {
		   let x = Foo(n = 1)
		   for (y in x) { y }
		   0
		 }",
		"not iterable",
	);
}

#[test]
fn for_in_over_an_unbounded_generic_param_is_diagnosed() {
	assert_error_contains(
		"func f<Item>(from: Item): int = {
		   for (item in from) {
		     item
		   }
		   0
		 }",
		"not iterable",
	);
}

#[test]
fn canonical_range_bound_rejects_an_element_without_its_capability() {
	assert_error_contains(
		"interface Step {}\nstruct Range<T: Step>(start: T, end: T)\nfunc f(): Range<float> = 0.5..2.5",
		"does not implement `Step`",
	);
}

#[test]
fn canonical_range_bound_accepts_a_step_element() {
	assert_ok(
		"interface Step {}\nimpl Step for char {}\nstruct Range<T: Step>(start: T, end: T)\nfunc f(): Range<char> = 'a'..'z'",
	);
}

#[test]
fn all_range_forms_have_their_canonical_nominal_types() {
	let source = "struct Range<T>(start: T, end: T)\nstruct RangeFrom<T>(start: T)\nstruct RangeTo<T>(end: T)\nstruct RangeInclusive<T>(start: T, end: T)\nstruct RangeToInclusive<T>(end: T)\nfunc a(): Range<int> = 1..2\nfunc b(): RangeFrom<int> = 1..\nfunc c(): RangeTo<int> = ..2\nfunc d(): RangeInclusive<int> = 1..=2\nfunc e(): RangeToInclusive<int> = ..=2";
	let parsed = parse_module(source, "test");
	let checked = check_module(&parsed.tree);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
}

#[test]
fn index_access_without_an_index_impl_is_diagnosed() {
	assert_error_contains(
		"struct Box(value: int)
		 func f(): int = Box(value = 1)[0]",
		"no method `index`",
	);
}

#[test]
fn assignment_through_a_custom_index_impl_is_diagnosed() {
	assert_error_contains(
		"interface Index<Key, Output> { func index(key: Key): Output }
		 struct Box(value: int) {
		   impl Index<Key = int, Output = int> {
		     func index(key: int): int = this.value + key
		   }
		 }
		 func f(): void = {
		   let mut box = Box(value = 1)
		   box[0] = 2
		 }",
		"cannot assign to `custom index access`",
	);
}

#[test]
fn mutating_method_on_a_custom_index_result_accepts_the_temporary() {
	assert_ok(
		"interface Index<Key, Output> { func index(key: Key): Output }
		 struct Counter(value: int) {
		   mut func increment(): int = { this.value = this.value + 1 this.value }
		 }
		 struct Store(value: Counter) {
		   impl Index<Key = int, Output = Counter> {
		     func index(key: int): Counter = this.value
		   }
		 }
		 func f(): int = Store(value = Counter(value = 0))[0].increment()",
	);
}

#[test]
fn method_generic_shadowing_does_not_change_an_owner_bound() {
	assert_ok(
		"interface Echo<X> { func echo(value: X): X }
		 struct Box<T: Echo<X = T>>(value: T) {
		   func apply<T>() = this.value.echo(this.value)
		 }",
	);
}

#[test]
fn nested_impl_generic_shadowing_does_not_change_an_owner_bound() {
	assert_ok(
		"interface Echo<X> { func echo(value: X): X }
		 interface Marker { func apply(): boolean }
		 struct Box<T: Echo<X = T>>(value: T) {
		   impl<T> Marker {
		     func apply(): boolean = { this.value.echo(this.value) true }
		   }
		 }",
	);
}

// ── SS1: smart literal spread ───────────────────────────────────────────────

#[test]
fn list_spread_over_a_native_list_source_types_the_element() {
	// The common case: a same-kind `#[T]` literal source still unifies its
	// element with the surrounding list's.
	assert_ok(
		"func f(): #[int] = {
		   let xs = #[1, 2, 3]
		   #[...xs, 4]
		 }",
	);
}

#[test]
fn list_spread_over_a_native_list_source_element_mismatch_is_reported() {
	assert_error_contains(
		"func f(): #[int] = {
		   let xs = #[true, false]
		   #[...xs, 4]
		 }",
		"mismatched types",
	);
}

#[test]
fn list_spread_over_a_user_iterator_types_the_element() {
	// SS1: a spread source need not be a same-kind list literal — ANY
	// `Iterator<T>`/`Iterable<T>` whose element matches is accepted, reusing
	// Track A's own iterable resolution.
	assert_ok(
		"enum Option<T> { Some(value: T), None }
		 interface Iterator<Item> { mut func next(): Option<Item> }
		 struct Counter(n: int, max: int)
		 impl Iterator<int> for Counter {
		   mut func next(): Option<int> = if (this.n > this.max) {
		     None
		   } else {
		     let v = this.n
		     this.n = this.n + 1
		     Some(value = v)
		   }
		 }
		 func f(): #[int] = {
		   let mut c = Counter(n = 1, max = 3)
		   #[...c, 99]
		 }",
	);
}

#[test]
fn list_spread_over_a_user_iterator_element_mismatch_is_reported() {
	assert_error_contains(
		"enum Option<T> { Some(value: T), None }
		 interface Iterator<Item> { mut func next(): Option<Item> }
		 struct Counter(n: int, max: int)
		 impl Iterator<boolean> for Counter {
		   mut func next(): Option<boolean> = None
		 }
		 func f(): #[int] = {
		   let mut c = Counter(n = 1, max = 3)
		   #[...c, 99]
		 }",
		"mismatched types",
	);
}

#[test]
fn list_spread_over_a_non_iterable_source_is_diagnosed() {
	assert_error_contains(
		"struct Foo(n: int)
		 func f(): #[int] = {
		   let x = Foo(n = 1)
		   #[...x]
		 }",
		"not iterable",
	);
}

#[test]
fn map_spread_over_a_native_map_source_types_the_entry() {
	assert_ok(
		"func f(): #{int: string} = {
		   let m = #{1: \"a\"}
		   #{...m, 2: \"b\"}
		 }",
	);
}

#[test]
fn map_spread_over_a_native_map_source_value_mismatch_is_reported() {
	assert_error_contains(
		"func f(): #{int: string} = {
		   let m = #{1: true}
		   #{...m, 2: \"b\"}
		 }",
		"mismatched types",
	);
}

#[test]
fn map_spread_over_a_non_map_iterable_of_pairs_types_the_entry() {
	// SS1: `Map` has no stdlib `Iterable` impl, so a non-map spread source must
	// resolve through the ordinary `Iterator`/`Iterable` protocol as an
	// iterable of `#(K, V)` pairs.
	assert_ok(
		"enum Option<T> { Some(value: T), None }
		 interface Iterator<Item> { mut func next(): Option<Item> }
		 struct Pairs(n: int, max: int)
		 impl Iterator<#(int, string)> for Pairs {
		   mut func next(): Option<#(int, string)> = if (this.n > this.max) {
		     None
		   } else {
		     let v = this.n
		     this.n = this.n + 1
		     Some(value = #(v, \"x\"))
		   }
		 }
		 func f(): #{int: string} = {
		   let mut p = Pairs(n = 1, max = 3)
		   #{...p, 9: \"z\"}
		 }",
	);
}

#[test]
fn map_spread_over_a_native_list_of_pairs_source_types_the_entry() {
	// A native `#[#(K, V)]` list is a JS-array source, same as a list-spread's
	// own list fast path (`infer_iterable_element`) — `List` implements
	// neither `Iterator` nor `Iterable`, so without its own fast path here the
	// checker would reject this natural "merge a list of entry pairs into a
	// map" shape with a spurious `NotIterable`.
	assert_ok(
		"func f(): #{int: string} = {
		   let pairs = #[#(1, \"a\"), #(2, \"b\")]
		   #{...pairs, 9: \"z\"}
		 }",
	);
}

#[test]
fn map_spread_over_a_non_iterable_source_is_diagnosed() {
	assert_error_contains(
		"struct Foo(n: int)
		 func f(): #{int: string} = {
		   let x = Foo(n = 1)
		   #{...x}
		 }",
		"not iterable",
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
fn bare_variant_pattern_is_disambiguated_by_the_scrutinee_type() {
	// `Leaf`/`Branch` are ambiguous by bare name alone, but `match`ing a `Tree`-typed
	// scrutinee pins the enum, so the bare arms need no `Tree.` prefix.
	assert_ok(
		"enum Tree { Leaf, Branch }
		 enum Plant { Leaf, Root }
		 func d(t: Tree): int = match (t) { Leaf -> 0, Branch -> 1 }",
	);
}

#[test]
fn bare_variant_construction_is_disambiguated_by_a_let_annotation() {
	assert_ok(
		"enum Tree { Leaf, Branch }
		 enum Plant { Leaf, Root }
		 func f(): int = {
		   let t: Tree = Leaf
		   0
		 }",
	);
}

#[test]
fn bare_variant_construction_is_disambiguated_by_a_return_type() {
	assert_ok(
		"enum Tree { Leaf, Branch }
		 enum Plant { Leaf, Root }
		 func make(): Tree = Leaf",
	);
}

#[test]
fn bare_variant_nested_in_a_generic_constructor_field_is_disambiguated() {
	// `Option<Tree>`'s `value` field pins `Tree` unambiguously, even though the
	// disambiguation only reaches the field through a fresh, still-unbound generic
	// instantiation of `Option` at the moment the nested `Leaf` argument is checked.
	assert_ok(
		"enum Tree { Leaf, Branch }
		 enum Plant { Leaf, Root }
		 enum Option<T> { Some(value: T), None }
		 func wrap(): Option<Tree> = Some(value = Leaf)",
	);
}

#[test]
fn bare_variant_nested_in_a_list_literal_is_disambiguated() {
	// `#[Tree]`'s own return-type annotation must reach each element's `check` call,
	// not just get unified against a fresh, still-unbound element type var after the
	// fact.
	assert_ok(
		"enum Tree { Leaf, Branch }
		 enum Plant { Leaf, Root }
		 func make(): #[Tree] = #[Leaf]",
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

#[test]
fn tuple_rest_pattern_binds_the_middle_subtuple() {
	// `#(a, ...rest, c)` against a known 3-tuple: `rest` binds the single-element
	// middle sub-tuple (boolean) sliced from the scrutinee's own element types.
	assert_ok(
		"func mid(t: #(int, boolean, char)): int = match (t) {
			#(a, ...rest, c) -> a,
		 }",
	);
}

#[test]
fn tuple_rest_pattern_with_too_many_fixed_elements_is_reported() {
	// The pattern names 4 fixed elements (2 prefix + 2 suffix) against a 2-tuple:
	// a genuine arity mismatch, reported like any other, not a lowering panic.
	assert_error_contains(
		"func f(t: #(int, boolean)): int = match (t) {
			#(a, b, ...rest, c, d) -> 1,
		 }",
		"mismatched types",
	);
}
