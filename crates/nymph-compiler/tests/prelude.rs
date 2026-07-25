//! Integration tests for the stdlib operator-interface prelude, which the
//! prelude-default-flip slice made the DEFAULT behind `check`/`compile` (and
//! their entry-mode counterparts) — there is no longer a separate
//! `check_with_prelude`/`compile_with_prelude` facade; `check`/`compile`
//! themselves flatten `std/ops` ahead of the user module (see `pipeline.rs`
//! for programs whose behavior is prelude-invariant either way).

use nymph_compiler::{check, compile};

/// Emit `src`, append a driver that logs `call`, run under Node, return
/// trimmed stdout. Local copy of `golden_programs.rs`'s `run` helper (tests
/// may not import from another crate's test files).
fn run(src: &str, call: &str) -> String {
	use std::io::Write;
	use std::sync::atomic::{AtomicU64, Ordering};

	let mut js = compile(src, "test").expect("expected a clean compile");
	js.push_str(&format!("\nconsole.log({call});\n"));

	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir();
	let path = dir.join(format!("nymph_prelude_{}_{unique}.mjs", std::process::id()));
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

#[test]
fn standalone_intrinsic_and_source_options_share_one_runtime_prototype() {
	let source = r#"
		func intrinsic_option(): Option<char> = "x".char_at(0u)
		func source_option(): Option<char> = Some(value = 'x')
	"#;
	let js = compile(source, "standalone_option_owner").expect("expected a clean compile");
	assert_eq!(
		js.matches("//#region std/option").count(),
		1,
		"the ambient Option owner must be emitted exactly once: {js}"
	);
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
fn compile_panics_loudly_on_an_unlinked_generic_bound_operator() {
	// Generic bound dispatch needs a canonical host/runtime target for each
	// primitive case. Arithmetic remains native codegen rather than a linked
	// stdlib leaf, so this unsupported case must stay loud instead of emitting
	// the invalid type-erased fallback `a.plus(b)`.
	let source = "func f<T: Plus<Other = T, Output = T>>(a: T, b: T): T = a + b";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `f` to typecheck cleanly via the prelude, got: {diags:?}"
	);
	let message = catch_panic_message(|| {
		let _ = compile(source, "test");
	})
	.expect("expected lowering to reject unlinked generic arithmetic dispatch");
	assert!(
		message.contains("does not yet dispatch operator to interface default method"),
		"expected the documented generic-dispatch panic, got: {message:?}"
	);
}

#[test]
fn blanket_impl_only_equals_method_call_links_and_runs() {
	let source = "struct P(v: int)
		func eq(a: P, b: P): boolean = a.equals(b)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `P.equals` to resolve cleanly via the prelude's blanket impl, got: {diags:?}"
	);

	let js = compile(source, "test").expect("blanket equals should lower to the linked intrinsic");
	assert!(
		js.contains("//#region std/equality") && js.contains("return equals(a, b)"),
		"expected the blanket equals call to use linked equality: {js}"
	);
}

#[test]
fn blanket_impl_only_not_equals_method_call_links_and_runs() {
	let source = "struct P(v: int)
		func neq(a: P, b: P): boolean = a.not_equals(b)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `P.not_equals` to resolve cleanly via the prelude's blanket impl, got: {diags:?}"
	);

	let js =
		compile(source, "test").expect("blanket not_equals should lower to the linked intrinsic");
	assert!(
		js.contains("//#region std/equality") && js.contains("return not_equals(a, b)"),
		"expected the blanket not_equals call to use linked equality: {js}"
	);
}

#[test]
fn compile_panics_loudly_on_a_generic_bound_plain_method_call_through_a_stdlib_interface() {
	// Findings 1 & 3 (stdlib linkage groundwork review, round 2): the plain
	// dot-call twin of `compile_panics_loudly_on_a_generic_bound_through_a_stdlib_interface`
	// above. `dispatch_kind_for_method_call` (`infer_expr.rs`) used to treat
	// every `MethodSource::GenericBound` resolution as safe for a plain
	// `receiver.method(args…)` call (type erasure + duck typing lets whichever
	// concrete type instantiates `T` supply its own compiled method) — true
	// when the bound is satisfied by a *user* impl (that impl is lowered
	// along with the rest of the user's module), but false here: `T`'s `Plus`
	// bound is only known to be satisfiable through the stdlib prelude's own
	// impls, which `compile` never lowers. Before the fix, `check` reported
	// zero diagnostics and `compile` silently emitted
	// `function f(a, b) { return a.plus(b); }` — confirmed to throw
	// `TypeError: a.plus is not a function` under Node for e.g. `f(1, 2)`.
	// Lowering must now refuse loudly instead, mirroring the operator case.
	let source = "func f<T: Plus<Other = T, Output = T>>(a: T, b: T): T = a.plus(b)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `f` to typecheck cleanly via the prelude, got: {diags:?}"
	);

	let message = catch_panic_message(|| {
		let _ = compile(source, "test");
	})
	.expect("expected lowering to reject unsupported generic-bound plain method dispatch");
	assert!(
		message.contains("cannot lower generic-bound dispatch"),
		"expected a loud generic-bound dispatch panic, got: {message:?}"
	);
}

#[test]
fn compile_panics_loudly_when_an_interface_default_uses_unlinked_generic_arithmetic() {
	// The generic receiver here is not `this`, and Plus has no linked primitive
	// runtime target. Materializing the default must not silently emit
	// `other.plus(other)` for an int.
	let source = "interface Foo<Other: Plus<Other = Other, Output = Other>> {
			func combine(other: Other): Other = other + other
		}
		struct Bar()
		impl Foo<Other = int> for Bar {}
		func combine_it(b: Bar, x: int): int = b.combine(x)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `Bar`'s `Foo` impl to typecheck cleanly via the prelude's `Plus`, got: {diags:?}"
	);
	let message = catch_panic_message(|| {
		let _ = compile(source, "test");
	})
	.expect("expected lowering to reject unlinked generic arithmetic in the default body");
	assert!(
		message.contains("does not yet dispatch operator to interface default method"),
		"expected the documented generic-dispatch panic, got: {message:?}"
	);
}

#[test]
fn comparable_compare_to_for_string_links_and_runs() {
	// Primitive comparison itself remains a host leaf, while the Comparable
	// implementation and sign-to-Order composition are inspectable Nymph.
	let source = "func f(a: string, b: string): Order = a.compare_to(b)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `string.compare_to` to resolve cleanly via the prelude, got: {diags:?}"
	);
	assert_eq!(
		run(
			source,
			"f(new NString('a'), new NString('b'))[Symbol.for('nymph.tag')].description"
		),
		"Order.LessThan"
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

/// Run `f`, catching any panic and returning its message — captured from
/// *inside* a temporary panic hook rather than by downcasting the
/// `catch_unwind` payload directly, mirroring `nymph-cli`'s
/// `compile_guard::compile_guarded`: with this pipeline's dependency stack, a
/// panic's payload can arrive at `catch_unwind`'s `Err` already repackaged
/// into some other `Any` type by the time it gets there (observed: a panic
/// the hook sees as a plain formatted `String` shows up at the
/// `catch_unwind` boundary as a value that downcasts as neither `&str` nor
/// `String`), whereas the hook always sees the original payload at the
/// moment the panic is raised. Also suppresses the default hook's stderr
/// backtrace dump for the duration of the call. Returns `None` if `f` didn't
/// panic.
///
/// `panic::set_hook`/`take_hook` are *process-global*, not per-thread, so two
/// of this file's tests calling this helper concurrently (the default
/// multi-threaded `cargo test` harness runs tests in one process across
/// several threads — unlike `cargo nextest`, which isolates one test per
/// process) would otherwise race: one thread's `set_hook` can install over,
/// or get clobbered by, another's mid-call (observed directly: an
/// intermittent empty captured message under `cargo test`). A single
/// process-wide [`Mutex`] serializes the hook-swap-catch-restore critical
/// section so concurrent callers queue instead of racing.
fn catch_panic_message(f: impl FnOnce() + std::panic::UnwindSafe) -> Option<String> {
	use std::cell::RefCell;
	use std::panic;
	use std::sync::Mutex;

	static HOOK_LOCK: Mutex<()> = Mutex::new(());
	thread_local! {
		static CAPTURED: RefCell<Option<String>> = const { RefCell::new(None) };
	}

	let _guard = HOOK_LOCK
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	CAPTURED.with(|cell| *cell.borrow_mut() = None);

	let previous_hook = panic::take_hook();
	panic::set_hook(Box::new(|info| {
		let payload = info.payload();
		let message = payload
			.downcast_ref::<&str>()
			.map(|s| (*s).to_string())
			.or_else(|| payload.downcast_ref::<String>().cloned())
			.unwrap_or_else(|| "<non-string panic payload>".to_string());
		CAPTURED.with(|cell| *cell.borrow_mut() = Some(message));
	}));

	let result = panic::catch_unwind(f);

	panic::set_hook(previous_hook);

	result.err().map(|_| {
		CAPTURED
			.with(|cell| cell.borrow_mut().take())
			.unwrap_or_default()
	})
}
