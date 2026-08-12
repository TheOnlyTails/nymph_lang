//! Integration tests for the stdlib operator-interface prelude, which the
//! prelude-default-flip slice made the DEFAULT behind `check`/`compile` (and
//! their entry-mode counterparts) — there is no longer a separate
//! `check_with_prelude`/`compile_with_prelude` facade; `check`/`compile`
//! themselves flatten `std/ops` ahead of the user module (see `pipeline.rs`
//! for programs whose behavior is prelude-invariant either way).

use nymph_compiler::{check, compile, compile_project_library};

/// Emit `src`, append a driver that logs `call`, run under Node, return
/// trimmed stdout. Local copy of `golden_programs.rs`'s `run` helper (tests
/// may not import from another crate's test files).
fn run(src: &str, call: &str) -> String {
	let mut js = compile(src, "test").expect("expected a clean compile");
	js.push_str(&format!("\nconsole.log({call});\n"));
	run_emitted_js(js)
}

/// Compile a one-module library project and execute the exact emitted symbol
/// for a zero-argument function returning an `int`.
fn run_int_entry(src: &str, entry: &str) -> String {
	let load = |path: &str| (path == "test").then(|| src.to_string());
	let compiled = compile_project_library("test", &load)
		.unwrap_or_else(|diagnostics| panic!("expected a clean compile, got: {diagnostics:?}"));
	let call = compiled.entry_symbol(entry);
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));
	run_emitted_js(js)
}

fn run_emitted_js(js: String) -> String {
	use std::io::Write;
	use std::sync::atomic::{AtomicU64, Ordering};

	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir();
	let path = dir.join(format!(
		"nymph_prelude_project_{}_{unique}.mjs",
		std::process::id()
	));
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(js.as_bytes()).unwrap();

	let output = std::process::Command::new("node")
		.arg(&path)
		.env("NO_COLOR", "1")
		.env_remove("FORCE_COLOR")
		.output()
		.expect("run node");
	let _ = std::fs::remove_file(&path);
	assert!(
		output.status.success(),
		"node failed:\n{}\n--- js ---\n{}",
		String::from_utf8_lossy(&output.stderr),
		js
	);
	String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn with_compiler_stack(test: impl FnOnce() + Send + 'static) {
	// Deeply nested ambient generic adapters exceed libtest's 2 MiB worker
	// stack in debug builds. Production CLI threads use the process stack.
	std::thread::Builder::new()
		.stack_size(8 * 1024 * 1024)
		.spawn(test)
		.expect("spawn ambient compiler test")
		.join()
		.expect("ambient compiler test panicked");
}

#[test]
fn standalone_intrinsic_and_source_options_share_one_runtime_prototype() {
	let source = r#"
		func intrinsic_option(): Option<char> = "x".char_at(0u)
		func source_option(): Option<char> = Some(value = 'x')
	"#;
	assert_eq!(
		run(
			source,
			"Object.getPrototypeOf(intrinsic_option()) === Object.getPrototypeOf(source_option())",
		),
		"true"
	);
}

#[test]
fn check_now_resolves_a_bare_user_struct_plus_impl_via_the_prelude_default() {
	// The payoff of the flip: `P` implements the stdlib's `Plus` interface
	// directly, with no local `interface Plus` declaration and no opt-in
	// call — plain `check` resolves it cleanly. The stdlib body
	// materialization slice closes the gap this test used to pin as a
	// lowering panic ("impl references unknown interface `Plus`" — the
	// interface's own declaration lives in the prelude tree, invisible to a
	// lowering that only ever walked the user's own AST): `compile` now
	// lowers and emits `P`'s `plus` cleanly too, and the emitted JS actually
	// runs the prelude-resolved `+` correctly.
	let source = "struct P(v: int)
		impl Plus<Other = P, Output = P> for P { func plus(other: P): P = P(v = this.v + other.v) }
		func add(a: P, b: P): P = a + b";
	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `P`'s `Plus` impl to resolve via the default prelude cleanly, got: {diags:?}"
	);

	let js = compile(source, "test").expect("expected lowering/emit to succeed via the prelude");
	assert!(
		js.contains("class P") && js.contains("plus("),
		"expected `P`'s `plus` method to be lowered/emitted:\n{js}"
	);
	assert_eq!(
		run(
			source,
			"add(new P({v: new NInt(2)}), new P({v: new NInt(3)})).v.v"
		),
		"5"
	);
}

#[test]
fn compile_lowers_and_runs_cleanly_when_a_user_impl_targets_a_stdlib_interface() {
	// The direct payoff pin (was `compile_panics_loudly_when_a_user_impl_targets_a_stdlib_interface`,
	// pinning the honest-scope cross-module-lowering limitation the stdlib
	// body materialization slice fixes): checking `P`'s `Plus` impl against
	// the prelude is fully clean (see
	// `check_now_resolves_a_bare_user_struct_plus_impl_via_the_prelude_default`
	// above), and lowering now feeds the prelude's own `interface`
	// declarations into the same by-name lookup `push_unoverridden_defaults`
	// already used for the user's own interfaces (gap a of that slice) — a
	// stdlib interface named by a user's own `impl … for …` resolves at
	// lowering time exactly like a local one, and the resulting class runs
	// under Node with the right runtime value.
	let source = "struct P(v: int)
		impl Plus<Other = P, Output = P> for P { func plus(other: P): P = P(v = this.v + other.v) }
		func add(a: P, b: P): P = a + b";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected checking alone to be clean via the prelude, got: {diags:?}"
	);

	assert!(
		compile(source, "test").is_ok(),
		"expected compile to succeed now that a user impl of a stdlib interface lowers cleanly"
	);
	assert_eq!(
		run(
			source,
			"add(new P({v: new NInt(10)}), new P({v: new NInt(32)})).v.v"
		),
		"42"
	);
}

#[test]
fn compile_resolves_mixed_float_int_arithmetic() {
	// Before the prelude existed this was a type error (no impl for
	// `float + int`); now that it's the default, the stdlib's
	// `impl Plus<Other = int, Output = self> for float` resolves it and it
	// still compiles to a native JS `+`.
	let source = "func f(a: float, b: int): float = a + b";
	let js = compile(source, "test").expect("should compile via the default prelude");
	assert!(js.contains('+'));
}

#[test]
fn generic_bound_operator_executes_through_the_stable_protocol() {
	let source = "func add<T: Plus<Other = T, Output = T>>(a: T, b: T): T = a + b
		func result(): int = add(20, 22)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `add` to typecheck cleanly via the prelude, got: {diags:?}"
	);
	assert_eq!(run_int_entry(source, "result"), "42");
}

#[test]
fn blanket_impl_only_equals_method_call_links_and_runs() {
	let source = "struct P(v: int)
		func result(): #(boolean, boolean) = #(
			P(v = 1).equals(P(v = 1)),
			P(v = 1).not_equals(P(v = 2)),
		)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `P.equals` to resolve cleanly via the prelude's blanket impl, got: {diags:?}"
	);

	assert_eq!(
		run(source, "result().v.map(value => value.v).join(' ')"),
		"true true"
	);
}

#[test]
fn blanket_impl_only_not_equals_method_call_links_and_runs() {
	let source = "struct P(v: int)
		func result(): #(boolean, boolean) = #(
			P(v = 1).equals(P(v = 1)),
			P(v = 1).not_equals(P(v = 2)),
		)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `P.not_equals` to resolve cleanly via the prelude's blanket impl, got: {diags:?}"
	);

	assert_eq!(
		run(source, "result().v.map(value => value.v).join(' ')"),
		"true true"
	);
}

#[test]
fn generic_bound_plain_method_executes_through_the_stable_protocol() {
	let source = "func add<T: Plus<Other = T, Output = T>>(a: T, b: T): T = a.plus(b)
		func result(): int = add(20, 22)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `add` to typecheck cleanly via the prelude, got: {diags:?}"
	);
	assert_eq!(run_int_entry(source, "result"), "42");
}

#[test]
fn interface_default_generic_arithmetic_executes_through_the_stable_protocol() {
	let source = "interface Foo<Other: Plus<Other = Other, Output = Other>> {
			func combine(other: Other): Other = other + other
		}
		struct Bar()
		impl Foo<Other = int> for Bar {}
		func combine_it(b: Bar, x: int): int = b.combine(x)
		func result(): int = combine_it(Bar(), 21)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `Bar`'s `Foo` impl to typecheck cleanly via the prelude's `Plus`, got: {diags:?}"
	);
	assert_eq!(run_int_entry(source, "result"), "42");
}

#[test]
fn ambient_iterator_default_adapter_chain_and_terminals_run() {
	with_compiler_stack(ambient_iterator_default_adapter_chain_and_terminals_body);
}

fn ambient_iterator_default_adapter_chain_and_terminals_body() {
	// This crosses the highest-risk stable ownership path removed with the
	// flattened-prelude harness: a project implementation calls ambient generic
	// defaults, those defaults materialize nested adapter types, and terminal
	// defaults repeatedly dispatch through each adapter's exact Iterator.next.
	let source = r#"
		struct Counter(current: uint, limit: uint) {
			impl Iterator<uint> {
				mut func next(): Option<uint> = if (this.current < this.limit) {
					let value = this.current
					this.current = this.current + 1u
					Some(value = value)
				} else None
			}
		}

		func values(): #[uint] = {
			let mut counter = Counter(current = 0u, limit = 20u)
			counter
				.filter((value) -> value % 2u == 0u)
				.map((value) -> value * 10u)
				.drop(1u)
				.take(3u)
				.to_list()
		}

		func count(): uint = {
			let mut counter = Counter(current = 0u, limit = 6u)
			counter.map((value) -> value + 1u).count()
		}
	"#;
	assert_eq!(
		run(
			source,
			"values().v.map(value => value.v).join(',') + '|' + count().v"
		),
		"20,40,60|6"
	);
}

#[test]
fn materialized_self_equality_substitutes_primitive_and_nominal_receivers() {
	let source = "interface IdentityDefault { func same(other: self): boolean = this == other }\n\
		struct Box(value: int)\n\
		impl IdentityDefault for int {}\n\
		impl IdentityDefault for Box {}\n\
		func result(): #(boolean, boolean) = #(21.same(21), Box(value = 1).same(Box(value = 1)))";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected materialized SelfType equality defaults to compile cleanly, got: {diags:?}"
	);
	assert_eq!(
		run(source, "result().v.map(value => value.v).join(' ')"),
		"true false"
	);
}

#[test]
fn comparable_compare_to_for_string_links_and_runs() {
	// Primitive comparison itself remains a host leaf, while the Comparable
	// implementation and sign-to-Order composition are inspectable Nymph.
	let source = "func compare(a: string, b: string): int = match (a.compare_to(b)) {
		LessThan -> -1,
		Equal -> 0,
		GreaterThan -> 1,
	}";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `string.compare_to` to resolve cleanly via the prelude, got: {diags:?}"
	);
	assert_eq!(
		run(source, "compare(new NString('a'), new NString('b')).v"),
		"-1"
	);
}

#[test]
fn list_sort_orders_primitives_without_mutating_the_source() {
	let source = r#"
		func values(): #(#[int], #[int]) = {
			let source = #[3, 1, 2]
			#(source, source.sort())
		}
	"#;
	assert_eq!(
		run(
			source,
			"values().v.map(xs => xs.v.map(x => x.v).join(',')).join('|')"
		),
		"3,1,2|1,2,3"
	);
}

#[test]
fn list_sort_dispatches_comparable_for_user_structs() {
	let source = r#"
		struct Item(key: int)
		impl Comparable<Other = Item> for Item {
			func compare_to(other: Item): Order = this.key.compare_to(other.key)
		}
		func values(): #[Item] = #[Item(key = 3), Item(key = 1), Item(key = 2)].sort()
	"#;
	assert_eq!(
		run(source, "values().v.map(item => item.key.v).join(',')"),
		"1,2,3"
	);
}

#[test]
fn list_sort_uses_compare_to_when_less_than_override_disagrees() {
	let source = r#"
		struct Item(key: int)
		impl Comparable<Other = Item> for Item {
			func compare_to(other: Item): Order = this.key.compare_to(other.key)
			func less_than(other: Item): boolean = this.key > other.key
		}
		func values(): #[Item] = #[Item(key = 3), Item(key = 1), Item(key = 2)].sort()
	"#;
	assert_eq!(
		run(source, "values().v.map(item => item.key.v).join(',')"),
		"1,2,3"
	);
}

#[test]
fn list_sort_is_stable_for_equal_keys() {
	let source = r#"
		struct Item(key: int, sequence: int)
		impl Comparable<Other = Item> for Item {
			func compare_to(other: Item): Order = this.key.compare_to(other.key)
		}
		func values(): #[Item] = #[
			Item(key = 2, sequence = 0),
			Item(key = 1, sequence = 1),
			Item(key = 2, sequence = 2),
			Item(key = 1, sequence = 3),
		].sort()
	"#;
	assert_eq!(
		run(source, "values().v.map(item => item.sequence.v).join(',')"),
		"1,3,0,2"
	);
}

#[test]
fn list_sort_by_accepts_descending_order_comparator() {
	let source = r#"
		func values(): #[int] = #[1, 3, 2].sort_by((left, right) ->
			if (left > right) { Order.LessThan }
			else if (left < right) { Order.GreaterThan }
			else { Order.Equal }
		)
	"#;
	assert_eq!(run(source, "values().v.map(x => x.v).join(',')"), "3,2,1");
}

#[test]
fn list_sort_by_preserves_source_and_equal_element_order() {
	let source = r#"
		struct Item(key: int, sequence: int)
		func compare_items(left: Item, right: Item): Order =
			if (left.key < right.key) { Order.LessThan }
			else if (left.key > right.key) { Order.GreaterThan }
			else { Order.Equal }
		func values(): #(#[Item], #[Item]) = {
			let source = #[
				Item(key = 2, sequence = 0),
				Item(key = 1, sequence = 1),
				Item(key = 2, sequence = 2),
			]
			#(source, source.sort_by(compare_items))
		}
	"#;
	assert_eq!(
		run(
			source,
			"values().v.map(xs => xs.v.map(item => item.sequence.v).join(',')).join('|')"
		),
		"0,1,2|1,0,2"
	);
}
