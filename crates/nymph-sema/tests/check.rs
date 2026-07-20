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
fn for_in_over_an_unbounded_generic_param_is_still_permissive() {
	// Known carve-out (see `resolve_iterable_source`): a bare, unbounded
	// generic type parameter used as a for-loop source can't be resolved
	// through the impl registry (`head_of` maps `Param` to `None`), and there
	// is no bound-checking path for it here (RR1 targets concrete ADT
	// sources) — this keeps the PRE-EXISTING permissive behavior instead of a
	// new false-positive `NotIterable`, which would otherwise regress
	// `collections/set.nym`'s `for (item in from)` over a `...from: Item`
	// spread parameter (a distinct, out-of-footprint spread-typing gap).
	assert_ok(
		"func f<Item>(from: Item): int = {
		   for (item in from) {
		     item
		   }
		   0
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
