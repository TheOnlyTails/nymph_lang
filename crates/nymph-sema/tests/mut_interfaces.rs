//! Tests interface method mutability, implementation-kind mismatches,
//! bound-mediated receiver gates, and mut-qualified implementation self types.

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

// ── interface `mut func` is the source of truth ────────────────────────

const STACK: &str = "interface Stack<E> {
	mut func push(x: E): void
	func peek(): E
}
struct Buf(n: int) {}
impl Stack<E = int> for Buf {
	mut func push(x: int): void = { this.n = x }
	func peek(): int = this.n
}
";

#[test]
fn mut_func_interface_method_is_callable_through_a_mut_receiver() {
	assert_ok(&format!(
		"{STACK}
		 func f(): void = {{
		   let mut b = Buf(n = 0)
		   b.push(1)
		 }}",
	));
}

#[test]
fn mut_func_interface_method_on_a_plain_receiver_is_rejected() {
	assert_error_contains(
		&format!(
			"{STACK}
			 func f(): void = {{
			   let b = Buf(n = 0)
			   b.push(1)
			 }}",
		),
		"requires a `mut` receiver",
	);
}

#[test]
fn plain_interface_method_stays_callable_on_a_plain_receiver() {
	// `peek` is a plain `func` on the interface — a non-`mut` `Buf` still has it,
	// only `push` (the `mut func`) requires a `mut` receiver.
	assert_ok(&format!(
		"{STACK}
		 func f(): int = {{
		   let b = Buf(n = 0)
		   b.peek()
		 }}",
	));
}

// ── an impl restating a mismatched kind is a diagnostic ────────────────

#[test]
fn impl_restating_a_mut_func_as_plain_func_is_reported() {
	assert_error_contains(
		"interface Stack<E> {
		   mut func push(x: E): void
		   func peek(): E
		 }
		 struct Buf(n: int) {}
		 impl Stack<E = int> for Buf {
		   func push(x: int): void = {}
		   func peek(): int = this.n
		 }",
		"restated",
	);
}

#[test]
fn impl_restating_a_plain_func_as_mut_func_is_reported() {
	assert_error_contains(
		"interface Stack<E> {
		   mut func push(x: E): void
		   func peek(): E
		 }
		 struct Buf(n: int) {}
		 impl Stack<E = int> for Buf {
		   mut func push(x: int): void = { this.n = x }
		   mut func peek(): int = this.n
		 }",
		"restated",
	);
}

#[test]
fn impl_matching_the_interfaces_kind_is_clean() {
	assert_ok(STACK);
}

// ── a `T: A` bound's `mut func` requirement is gated through the bound ─

const STACK_BOUND: &str = "interface Stack<E> {
	mut func push(x: E): void
	func peek(): E
}
";

#[test]
fn bound_mut_func_on_a_plain_generic_param_is_rejected() {
	assert_error_contains(
		&format!(
			"{STACK_BOUND}
			 func use_it<T: Stack<E = int>>(x: T): void = x.push(1)",
		),
		"requires a `mut` receiver",
	);
}

#[test]
fn bound_mut_func_on_a_mut_generic_param_is_accepted() {
	assert_ok(&format!(
		"{STACK_BOUND}
		 func use_it<T: Stack<E = int>>(x: mut T): void = x.push(1)",
	));
}

#[test]
fn bound_plain_func_stays_callable_on_a_plain_generic_param() {
	assert_ok(&format!(
		"{STACK_BOUND}
		 func use_it<T: Stack<E = int>>(x: T): int = x.peek()",
	));
}

// ── `impl A for mut B` / `impl mut A for B` bound satisfaction ─────

const A_FOR_MUT_B: &str = "interface A { func touch(): int }
struct B(n: int) {}
impl A for mut B { func touch(): int = 1 }
";

#[test]
fn impl_a_for_mut_b_matches_a_mut_receiver_directly() {
	assert_ok(&format!(
		"{A_FOR_MUT_B}
		 func f(): int = {{
		   let mut b = B(n = 0)
		   b.touch()
		 }}",
	));
}

#[test]
fn impl_a_for_mut_b_does_not_match_a_plain_receiver_directly() {
	// Stripping `mut` before matching makes a `Mut(B)`-self-typed impl unable
	// to match any receiver.
	assert_error_contains(
		&format!(
			"{A_FOR_MUT_B}
			 func f(): int = {{
			   let b = B(n = 0)
			   b.touch()
			 }}",
		),
		"no method",
	);
}

#[test]
fn impl_a_for_mut_b_bound_is_satisfied_by_a_mut_argument() {
	assert_ok(&format!(
		"{A_FOR_MUT_B}
		 func f<T: A>(x: T): int = x.touch()
		 func caller(): int = {{
		   let mut b = B(n = 0)
		   f(b)
		 }}",
	));
}

#[test]
fn impl_a_for_mut_b_bound_rejects_a_plain_argument_with_a_hint() {
	assert_error_contains(
		&format!(
			"{A_FOR_MUT_B}
			 func f<T: A>(x: T): int = x.touch()
			 func caller(): int = {{
			   let b = B(n = 0)
			   f(b)
			 }}",
		),
		"does not implement",
	);
}

#[test]
fn impl_a_for_mut_b_bound_hint_names_the_mut_type() {
	assert_error_contains(
		&format!(
			"{A_FOR_MUT_B}
			 func f<T: A>(x: T): int = x.touch()
			 func caller(): int = {{
			   let b = B(n = 0)
			   f(b)
			 }}",
		),
		"mut B",
	);
}

#[test]
fn impl_a_for_mut_b_bound_rejects_mixed_mut_and_plain_arguments() {
	// With A implemented ONLY for `mut B`, a mixed `(mut B, B)` call fails
	// because the PLAIN `b2` doesn't satisfy the bound — reported with the
	// precise "`B` does not implement `A`; `mut B` does" hint (the design's
	// intended message), not a vague "mixed arguments" one. (The false-positive
	// case — an ORDINARY plain `impl A for B`, where mixed args are FINE — is
	// pinned by `mixed_args_are_fine_when_a_plain_impl_satisfies_the_bound`.)
	assert_error_contains(
		&format!(
			"{A_FOR_MUT_B}
			 func f<T: A>(x: T, y: T): int = x.touch() + y.touch()
			 func caller(): int = {{
			   let mut b1 = B(n = 0)
			   let b2 = B(n = 0)
			   f(b1, b2)
			 }}",
		),
		"does not implement",
	);
}

#[test]
fn a_plain_impl_and_a_mut_impl_of_one_interface_is_a_coherence_error() {
	// `impl A for B` and `impl A for mut B` both apply
	// to a `mut B` receiver, so they OVERLAP and coherence must reject them at
	// the impl declarations — not let both coexist and surface later as a
	// confusing `AmbiguousCall` at a `mut`-receiver call site. `impls_overlap`
	// so overlap checking peels `mut` off both self types.
	assert_error_contains(
		"interface A { func touch(): int }
		 struct B(n: int)
		 impl A for B { func touch(): int = 1 }
		 impl A for mut B { func touch(): int = 2 }",
		"conflicting implementations",
	);
}

#[test]
fn mixed_args_are_fine_when_a_plain_impl_satisfies_the_bound() {
	// A plain `impl A for B` satisfies the bound for both `B` and `mut B`, so
	// mixed-mutability arguments are valid for `f<T: A>(x: T, y: T)`.
	assert_ok(
		"interface A { func touch(): int }
		 struct B(n: int)
		 impl A for B { func touch(): int = 1 }
		 func f<T: A>(x: T, y: T): int = x.touch() + y.touch()
		 func caller(): int = {
		   let mut b1 = B(n = 0)
		   let b2 = B(n = 0)
		   f(b1, b2)
		 }",
	);
}

#[test]
fn ordinary_plain_impl_bound_still_holds_for_a_mut_argument() {
	// An ordinary `impl A for B` satisfies a `T: A` bound for a `mut B`
	// argument. The mut-only-impl check preserves the `mut B <: B` rule.
	assert_ok(
		"interface A { func touch(): int }
		 struct B(n: int) {}
		 impl A for B { func touch(): int = 1 }
		 func f<T: A>(x: T): int = x.touch()
		 func caller(): int = {
		   let mut b = B(n = 0)
		   f(b)
		 }",
	);
}

#[test]
fn impl_mut_a_for_b_is_equivalent_to_impl_a_for_mut_b() {
	// `impl mut A for B` is the SAME feature as `impl A for mut B` under a
	// different spelling (design ruling: "mut applies to BOTH A and B — same
	// effect: only mut B").
	assert_ok(
		"interface A { func touch(): int }
		 struct B(n: int) {}
		 impl mut A for B { func touch(): int = 1 }
		 func f<T: A>(x: T): int = x.touch()
		 func caller(): int = {
		   let mut b = B(n = 0)
		   f(b)
		 }",
	);
	assert_error_contains(
		"interface A { func touch(): int }
		 struct B(n: int) {}
		 impl mut A for B { func touch(): int = 1 }
		 func f<T: A>(x: T): int = x.touch()
		 func caller(): int = {
		   let b = B(n = 0)
		   f(b)
		 }",
		"does not implement",
	);
}

// ── Owned collection literal → `mut` coercion ──────────────────────────────
//
// A `#{…}`/`#[…]` literal is a uniquely-owned temporary — nothing else can
// alias it — so it may satisfy an expected `mut` collection type directly,
// the same way `check_let_statement` already lets a plain-typed initializer
// satisfy an explicit `mut T`-annotated `let`. `try_coerce_owned_literal_to_mut`
// (coerce.rs) adds this at `check_dispatch` (covers ctor fields, block/if/match
// branches, and the `List` check arm) and `check_call_arg` (free-function
// arguments) — every `check`/`check_call_arg`-routed site.

#[test]
fn a_fresh_map_literal_satisfies_a_mut_map_parameter() {
	assert_ok(
		"func take(m: mut #{int: int}): boolean = true
		 func t(): boolean = take(#{1: 2})",
	);
}

#[test]
fn a_fresh_list_literal_satisfies_a_mut_list_parameter() {
	assert_ok(
		"func take(xs: mut #[int]): boolean = true
		 func t(): boolean = take(#[1, 2, 3])",
	);
}

#[test]
fn a_fresh_map_literal_satisfies_a_mut_struct_ctor_field() {
	assert_ok(
		"struct Box(inner: mut #{int: int}) {}
		 func t(): Box = Box(inner = #{1: 2})",
	);
}

#[test]
fn a_fresh_list_literal_satisfies_a_mut_struct_ctor_field() {
	assert_ok(
		"struct Box(inner: mut #[int]) {}
		 func t(): Box = Box(inner = #[1, 2, 3])",
	);
}

#[test]
fn an_empty_map_literal_satisfies_a_mut_map_parameter() {
	// The stdlib's own `Set.new` shape (`let mut inner: #{Item: #()} = #{}`)
	// initializes a mut binding from an EMPTY literal; this pins the same
	// empty-literal case going straight to a `mut`-typed parameter instead.
	assert_ok(
		"func take(m: mut #{int: int}): boolean = true
		 func t(): boolean = take(#{})",
	);
}

#[test]
fn a_mut_named_binding_still_satisfies_a_mut_map_parameter() {
	// Literal-only coercion must preserve the ordinary `mut T <: mut T` path
	// for a named binding
	// that already carries `mut`.
	assert_ok(
		"func take(m: mut #{int: int}): boolean = true
		 func t(): boolean = {
		   let mut m: mut #{int: int} = #{1: 2}
		   take(m)
		 }",
	);
}

#[test]
fn a_named_immutable_binding_is_still_rejected_at_a_mut_map_parameter() {
	// Invariant (must NOT regress): the literal-only coercion is keyed off the
	// EXPRESSION being a `Map`/`List` literal, not off the expected type alone
	// — a NAMED immutable binding (`ExprKind::Identifier`, never matched by
	// `try_coerce_owned_literal_to_mut`) must still fail the ordinary one-way
	// `mut T <: T` `subtype` check.
	assert_error_contains(
		"func take(m: mut #{int: int}): boolean = true
		 func t(): boolean = {
		   let m: #{int: int} = #{1: 2}
		   take(m)
		 }",
		"mut",
	);
}

#[test]
fn a_named_immutable_binding_is_still_rejected_at_a_mut_list_parameter() {
	assert_error_contains(
		"func take(xs: mut #[int]): boolean = true
		 func t(): boolean = {
		   let xs: #[int] = #[1, 2, 3]
		   take(xs)
		 }",
		"mut",
	);
}

#[test]
fn a_named_immutable_binding_is_still_rejected_at_a_mut_struct_ctor_field() {
	assert_error_contains(
		"struct Box(inner: mut #{int: int}) {}
		 func t(): Box = {
		   let inner: #{int: int} = #{1: 2}
		   Box(inner = inner)
		 }",
		"mut",
	);
}

// ── Unannotated if/block-bodied inherent method return type ──────────────
//
// `infer_inherent_return` infers the body, and `infer_block` returns the type
// of its last expression. This test exercises the `Set.remove` shape.

#[test]
fn an_unannotated_inherent_method_with_an_if_block_body_infers_the_branches_common_type() {
	assert_ok(
		"struct Wrapper(flag: boolean) {}
		 impl Wrapper {
		   mut func toggle(cond: boolean) = if (cond) {
		     this.flag = true
		     true
		   } else false
		 }
		 func f(): boolean = {
		   let mut w = Wrapper(flag = false)
		   w.toggle(true)
		 }",
	);
}

#[test]
fn omitted_return_trials_do_not_leak_bound_argument_mutability() {
	assert_ok(
		"interface A { func touch(): int }
		 struct B(n: int)
		 impl A for mut B { func touch(): int = 1 }
		 func require<T: A>(value: T): int = value.touch()
		 struct Calls {
		   func generic<T: A>(value: T) = require(value)
		   func mutable(value: mut B) = require(value)
		 }",
	);
}

// ── Nested owned-literal → `mut` coercion misses a free-function-call argument
// whose OWN top-level type isn't `mut` but contains a nested `mut`-expected
// element/value ─────────────────────────────────────────────────────
//
// `check_call_arg`'s Map/List guard arm only fires when the ARGUMENT'S OWN
// top-level type is `mut T`; falling through to the ordinary
// `_` arm, which calls a blind `self.infer(expr)` instead of `self.check(expr,
// pty)` — so nested elements/values never got the expected-type propagation
// that would let THEM reach `try_coerce_owned_literal_to_mut` in turn.

#[test]
fn a_nested_list_literal_satisfies_a_nested_mut_list_element_in_a_free_function_call() {
	assert_ok(
		"func take(xs: #[mut #[int]]): boolean = true
		 func t(): boolean = take(#[#[1, 2]])",
	);
}

#[test]
fn a_nested_list_literal_satisfies_a_nested_mut_map_value_in_a_free_function_call() {
	assert_ok(
		"func take(m: #{int: mut #[int]}): boolean = true
		 func t(): boolean = take(#{1: #[2, 3]})",
	);
}

#[test]
fn a_nested_map_literal_satisfies_a_nested_mut_map_value_in_a_free_function_call() {
	assert_ok(
		"func take(m: #{int: mut #{int: int}}): boolean = true
		 func t(): boolean = take(#{1: #{2: 3}})",
	);
}

#[test]
fn a_nested_named_immutable_binding_is_still_rejected_at_a_nested_mut_list_element() {
	// Nested-literal coercion must stay keyed off the nested
	// EXPRESSION being a literal, not off the nested expected type alone — a
	// nested NAMED immutable binding must still be rejected.
	assert_error_contains(
		"func take(xs: #[mut #[int]]): boolean = true
		 func t(): boolean = {
		   let inner: #[int] = #[1, 2]
		   take(#[inner])
		 }",
		"mut",
	);
}

// ── Owned-literal → `mut` coercion doesn't extend to method-call arguments
// ───────────────────────────────────────────────────────────────────
//
// `receiver.method(args…)` infers each argument's type directly and hands the
// results to `resolve_method` → `commit_method`, which validates the chosen
// candidate's params against `arg_tys` via `unify_arg` — a path the owned-
// literal-to-mut coercion never reached.

#[test]
fn a_fresh_map_literal_satisfies_a_mut_map_parameter_through_a_method_call() {
	assert_ok(
		"struct Box() {}
		 impl Box {
		   func take(m: mut #{int: int}): boolean = true
		 }
		 func t(): boolean = {
		   let b = Box()
		   b.take(#{1: 2})
		 }",
	);
}

#[test]
fn a_fresh_list_literal_satisfies_a_mut_list_parameter_through_a_method_call() {
	assert_ok(
		"struct Box() {}
		 impl Box {
		   func take(xs: mut #[int]): boolean = true
		 }
		 func t(): boolean = {
		   let b = Box()
		   b.take(#[1, 2, 3])
		 }",
	);
}

#[test]
fn a_named_immutable_binding_is_still_rejected_at_a_mut_map_parameter_through_a_method_call() {
	// As with free-function calls, a
	// NAMED immutable binding must still be rejected at a `mut` method
	// parameter; only a literal argument gets the coercion.
	assert_error_contains(
		"struct Box() {}
		 impl Box {
		   func take(m: mut #{int: int}): boolean = true
		 }
		 func t(): boolean = {
		   let b = Box()
		   let m: #{int: int} = #{1: 2}
		   b.take(m)
		 }",
		"mut",
	);
}
