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
	assert_eq!(run(source, "add(new P({v: 2}), new P({v: 3})).v"), "5");
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
	assert_eq!(run(source, "add(new P({v: 10}), new P({v: 32})).v"), "42");
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
fn compile_panics_loudly_on_a_generic_bound_through_a_stdlib_interface() {
	// Honest-scope acceptance criterion: a generic function bounded by the
	// prelude's `Plus` (a required method with no default body, and — unlike
	// `Comparable`/`Equals` — no unbounded blanket impl to spuriously match a
	// still-abstract type parameter) dispatches `+` through the bound itself
	// (`MethodSource::GenericBound` -> `DispatchKind::UserImplDefaultMethod`):
	// the concrete impl is only known once `T` is instantiated, which this
	// type-erased-at-lowering compiler does not track. Lowering panics loudly
	// rather than silently miscompiling — documented, expected behavior,
	// pinned here so a future slice that fixes it does so deliberately.
	let source = "func f<T: Plus<Other = T, Output = T>>(a: T, b: T): T = a + b";

	// Checking alone is clean (no diagnostics) — the panic is purely a lowering
	// limitation, not a type error.
	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `f` to typecheck cleanly via the prelude, got: {diags:?}"
	);

	let message = catch_panic_message(|| {
		let _ = compile(source, "test");
	})
	.expect("expected lowering to panic on the generic-bound dispatch");
	assert!(
		message.contains("does not yet dispatch operator to interface default method"),
		"expected the documented `UserImplDefaultMethod` panic message, got: {message:?}"
	);
}

#[test]
fn compile_panics_loudly_on_a_blanket_impl_only_method_call() {
	// Finding 2 (stdlib linkage groundwork review): a struct satisfies `Equals`
	// *solely* through the stdlib's unconstrained blanket impl
	// (`impl<T> Equals<Other = self> for T`, `ops/mod.nym`) — no local `impl
	// Equals for P` at all — so `check` reports zero diagnostics (the blanket
	// impl really does provide `equals`, and `Equals`'s own default body
	// provides `not_equals`). But `compile` lowers only `source`'s own AST,
	// never the prelude — so neither `equals` nor `not_equals` is ever
	// materialized onto `P`'s compiled class. Before this fix, a plain method
	// call (`a.equals(b)`) never consulted the checker's resolution at all and
	// unconditionally lowered to a bare `Call { callee: Field { .. }, .. }`,
	// trusting a method that doesn't exist anywhere in the emitted JS —
	// confirmed to throw `TypeError: a.equals is not a function` at runtime
	// under Node. Lowering must now refuse loudly instead, exactly like the
	// other honest-scope deferrals in this file.
	let source = "struct P(v: int)
		func eq(a: P, b: P): boolean = a.equals(b)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `P.equals` to resolve cleanly via the prelude's blanket impl, got: {diags:?}"
	);

	let message = catch_panic_message(|| {
		let _ = compile(source, "test");
	})
	.expect("expected lowering to panic rather than emit a call to an unmaterialized method");
	assert!(
		message.contains("prelude-only impl"),
		"expected the documented prelude-only-impl panic message, got: {message:?}"
	);
}

#[test]
fn compile_panics_loudly_on_a_blanket_interface_default_method_call() {
	// The `not_equals` half of the same hazard: `Equals`'s own default body
	// (`func not_equals(other: Other): boolean = !this.equals(other)`) is
	// reached through the same blanket-only impl, so `MethodSource` here is
	// `InterfaceDefault` rather than `ImplDirect` — a different source, but the
	// matched impl is the same prelude-origin blanket impl, so it must be
	// refused just as loudly (Finding 2).
	let source = "struct P(v: int)
		func neq(a: P, b: P): boolean = a.not_equals(b)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `P.not_equals` to resolve cleanly via the prelude's blanket impl, got: {diags:?}"
	);

	let message = catch_panic_message(|| {
		let _ = compile(source, "test");
	})
	.expect("expected lowering to panic rather than emit a call to an unmaterialized method");
	assert!(
		message.contains("prelude-only impl"),
		"expected the documented prelude-only-impl panic message, got: {message:?}"
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
	.expect("expected lowering to panic on the generic-bound plain method-call dispatch");
	assert!(
		message.contains("prelude-only impl"),
		"expected the documented prelude-only-impl panic message, got: {message:?}"
	);
}

#[test]
fn compile_panics_loudly_when_an_interface_default_dispatches_on_its_own_generic_param() {
	// Stdlib body materialization review, Finding 1: `materializing_onto_class`
	// is set for the FULL duration of lowering one interface default body onto
	// a concrete class (`Foo::combine` onto `Bar` here) — it must not be
	// treated as a blanket "any dispatch reached while in here is an ordinary,
	// trustworthy class method call" signal. `combine`'s own body dispatches
	// `other + other` on `Other` (`Foo`'s OWN generic parameter, pinned to
	// `int` by `Bar`'s impl), never on `this`. Before the fix this silently
	// lowered to `other.plus(other)` in `Bar`'s emitted `combine` method — a
	// runtime `TypeError` under Node, since a JS `number` has no `plus`
	// method. The receiver not being `this` must keep this exactly as loud a
	// lowering deferral as the other honest-scope cases in this file.
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
	.expect(
		"expected lowering to panic rather than silently emit `other.plus(other)` on a non-`this` receiver",
	);
	assert!(
		message.contains("does not yet dispatch operator to interface default method"),
		"expected the documented `UserImplDefaultMethod` panic message, got: {message:?}"
	);
}

#[test]
fn compile_panics_loudly_on_comparable_compare_to_for_string() {
	// Stdlib body materialization review, Finding 2: `Comparable for string`'s
	// own `compare_to` is a genuine Nymph body (not `external` itself), so
	// `try_materialize_prelude_dispatch` used to accept it as materializable —
	// but that body calls the free `external(compare_to_string) func
	// compare_to_string(..)` (`stdlib/src/ops/mod.nym`), which "stdlib
	// linkage" (a still-future slice) has not wired up anywhere in the
	// emitted module. Before the fix this compiled clean and threw
	// `ReferenceError: compare_to_string is not defined` under Node at the
	// call site. Lowering must refuse loudly instead, exactly like the other
	// honest-scope deferrals in this file.
	let source = "func f(a: string, b: string): Order = a.compare_to(b)";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `string.compare_to` to resolve cleanly via the prelude, got: {diags:?}"
	);

	let message = catch_panic_message(|| {
		let _ = compile(source, "test");
	})
	.expect("expected lowering to panic rather than emit a call to an unlinked external function");
	assert!(
		message.contains("prelude-only impl"),
		"expected the documented prelude-only-impl panic message, got: {message:?}"
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
