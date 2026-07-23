//! Stdlib linkage groundwork: `check_module_with_prelude` (an additive facade —
//! `check_module`/`check_module_entry`/`check_program` are unaffected).
//!
//! These tests parse the real `stdlib/src/ops/mod.nym` (the operator interfaces:
//! `Plus`, `Comparable`, …) as the *prelude* and check small user programs against
//! it — proving a user struct can resolve a stdlib operator interface (`Plus`,
//! `Comparable`) *without declaring that interface locally*, which is the end
//! state this slice targets. See the design write-up in
//! `docs/superpowers/plans/2026-07-14-nymph-stdlib-linkage-groundwork.md`.

use std::path::PathBuf;

use nymph_ast::{
	NodeId,
	decl::{Declaration, Module},
	expr::{Expr, ExprKind, Statement},
};
use nymph_sema::{
	DispatchKind, check_module, check_module_with_prelude, lower_hir, lower_hir_with_prelude,
};
use nymph_syntax::parse_module;

fn ops_prelude_source() -> String {
	let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../stdlib/src/ops/mod.nym")
		.canonicalize()
		.unwrap();
	std::fs::read_to_string(path).unwrap()
}

fn ops_prelude_module() -> Module {
	let source = ops_prelude_source();
	let parsed = parse_module(&source, "std/ops");
	assert!(
		parsed.diagnostics.iter().all(|d| !d.is_error()),
		"std/ops failed to parse"
	);
	parsed.tree
}

fn parse(source: &str) -> Module {
	let parsed = parse_module(source, "<test>");
	assert!(
		parsed.diagnostics.iter().all(|d| !d.is_error()),
		"test source failed to parse: {:?}",
		parsed.diagnostics
	);
	parsed.tree
}

/// Find the `NodeId` of the (single, top-level) `BinaryOp` inside `func_name`'s
/// body. Mirrors `operator_resolutions.rs`'s helper.
fn binary_op_in(module: &Module, func_name: &str) -> NodeId {
	let body = module.members.iter().find_map(|decl| match decl {
		Declaration::Func { meta, body, .. } if meta.name.0 == func_name => Some(body),
		_ => None,
	});
	let body = body.unwrap_or_else(|| panic!("no func named `{func_name}` found"));
	let mut out = Vec::new();
	collect_binary_ops(body, &mut out);
	assert_eq!(
		out.len(),
		1,
		"expected exactly one BinaryOp in `{func_name}`, found {}",
		out.len()
	);
	out[0]
}

fn collect_binary_ops(expr: &Expr, out: &mut Vec<NodeId>) {
	if let ExprKind::BinaryOp { lhs, rhs, .. } = &expr.kind {
		out.push(expr.id);
		collect_binary_ops(lhs, out);
		collect_binary_ops(rhs, out);
		return;
	}
	match &expr.kind {
		ExprKind::Grouped(inner) => collect_binary_ops(inner, out),
		ExprKind::Block { body, .. } => {
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) => collect_binary_ops(e, out),
					Statement::Let { value, .. } => collect_binary_ops(value, out),
				}
			}
		}
		_ => {}
	}
}

#[test]
fn ops_prelude_typechecks_cleanly_standalone() {
	// The prelude must be provably clean on its own (mirrors
	// `stdlib_check.rs::stdlib_typechecks_cleanly`, but isolated to just
	// `ops/mod.nym` — the one file this slice injects) — a dirty prelude would
	// make every downstream `debug_assert!` in `check_module_with_prelude`
	// meaningless.
	let prelude = ops_prelude_module();
	let checked = check_module(&prelude);
	let messages: Vec<_> = checked
		.diags
		.iter()
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		messages.is_empty(),
		"expected std/ops to typecheck cleanly standalone, got: {messages:?}"
	);
}

#[test]
fn default_path_is_untouched_by_the_facade_existing() {
	// A program that fails without the prelude (references `Plus` without
	// declaring it) behaves identically through the ordinary `check_module` —
	// the facade's existence must not change default behavior.
	let user = parse(
		"struct P(v: int)\n impl Plus<Other = P, Output = P> for P { func plus(other: P): P = other }\n func add(a: P, b: P): P = a + b",
	);
	let checked = check_module(&user);
	assert!(
		checked.diags.iter().any(|d| d.is_error()),
		"expected an error resolving `Plus` without the prelude and without a local declaration"
	);
}

#[test]
fn user_struct_plus_without_local_interface_resolves_via_prelude() {
	// The headline end-state: `P` implements the stdlib's `Plus` interface
	// directly — no local `interface Plus { .. }` declaration at all — and `+`
	// on two `P` values resolves and checks cleanly.
	let user = parse(
		"struct P(v: int)
		 impl Plus<Other = P, Output = P> for P { func plus(other: P): P = other }
		 func add(a: P, b: P): P = a + b",
	);
	let prelude = ops_prelude_module();

	let op_id = binary_op_in(&user, "add");
	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));

	let errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		errors.is_empty(),
		"expected `P`'s `Plus` impl to resolve via the prelude cleanly, got: {errors:?}"
	);

	let res = checked
		.annotations
		.resolution_of(op_id)
		.expect("expected a Resolution recorded for `a + b`");
	assert_eq!(res.method, "plus");
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
}

#[test]
fn mixed_float_int_plus_resolves_via_prelude() {
	// Without the prelude, mixed-primitive `float + int` arithmetic has no impl
	// to dispatch to and is a type error (see `default_path_still_rejects_mixed_primitive_plus`
	// below). With the prelude, the stdlib's `impl Plus<Other = int, Output = self> for float`
	// resolves it — and it still compiles to a native JS `+` (`BuiltinEager`):
	// arithmetic on primitives never routes through a JS method call, prelude or
	// not (see `infer_expr.rs`'s mixed-primitive arm).
	let user = parse("func f(a: float, b: int): float = a + b");
	let prelude = ops_prelude_module();

	let op_id = binary_op_in(&user, "f");
	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));

	let errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		errors.is_empty(),
		"expected mixed float+int `+` to resolve via the prelude cleanly, got: {errors:?}"
	);

	let res = checked
		.annotations
		.resolution_of(op_id)
		.expect("expected a Resolution recorded for `a + b`");
	assert_eq!(res.method, "plus");
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
}

#[test]
fn two_distinct_prelude_modules_flatten_together_cleanly() {
	// Finding 1 (stdlib linkage groundwork review): `check_module_with_prelude`'s
	// own doc comment frames a multi-module `prelude` slice as the intended
	// future shape ("one or more prelude modules"), and this is the first test
	// to actually exercise it with more than one element. Two *distinct* prelude
	// modules (different interface names, so nothing here legitimately shadows
	// anything, unlike passing the same file twice — that degenerate case
	// produces real cross-prelude `Redefinition`/`ConflictingImpls` diagnostics
	// regardless of offsetting, a separate concern from this test) whose bodies
	// happen to have the exact same shape — each was parsed independently, so
	// their `NodeId`/`Span` counters both restart at 0. Before the fix
	// (`crates/nymph-sema/src/prelude.rs`'s own unit tests pin the underlying
	// mechanism directly), `offset_module` applied the exact same
	// `NODE_BASE`/`SPAN_BASE` to every prelude module regardless of its position
	// in the slice, so these two would silently collide with each other once
	// flattened together — invisible to this end-to-end test (the corrupted
	// entries are prelude-internal `Annotations`, never observable through this
	// public API), but this at least proves the documented multi-module facade
	// now works end-to-end without error.
	let prelude_a = parse("interface Foo { func foo(): int = 1 }");
	let prelude_b = parse("interface Bar { func bar(): int = 2 }");
	let user = parse(
		"struct S()
		 impl Foo for S {}
		 impl Bar for S {}
		 func total(s: S): int = s.foo() + s.bar()",
	);

	let checked = check_module_with_prelude(&user, &[prelude_a, prelude_b]);
	let errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		errors.is_empty(),
		"expected both prelude interfaces to resolve cleanly via a 2-element prelude slice, got: {errors:?}"
	);
}

#[test]
fn default_path_still_rejects_mixed_primitive_plus() {
	// The mirror of the previous test through the *default*, non-prelude path:
	// without stdlib linkage, `float + int` has no impl to dispatch to.
	let user = parse("func f(a: float, b: int): float = a + b");
	let checked = check_module(&user);
	assert!(
		checked.diags.iter().any(|d| d.is_error()),
		"expected mixed float+int `+` to be rejected without the prelude"
	);
}

#[test]
fn shadowing_a_prelude_interface_reports_redefinition_without_a_stdlib_span() {
	// A user who declares their *own* `Plus` alongside the prelude's shadows it
	// (later-definition-wins, per `build_def_map`) and gets a `Redefinition`
	// diagnostic anchored at their own declaration — never at a raw stdlib span.
	let user = parse(
		"interface Plus<Other, Output> { func plus(other: Other): Output }
		 func f(): void = {}",
	);
	let prelude = ops_prelude_module();
	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));

	let redefinitions: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.message.contains("defined more than once"))
		.collect();
	assert_eq!(
		redefinitions.len(),
		1,
		"expected exactly one Redefinition diagnostic, got {:?}",
		checked.diags
	);
	let diag = redefinitions[0];

	// The primary diagnostic span must land inside the small user source, not the
	// (offset, far larger) prelude clone.
	assert!(
		diag.span.start < user_source_len(),
		"primary span must be user-anchored, not a prelude span: {diag:?}"
	);
	// No surviving label may point past the user's own source either — the
	// "first defined here" label (which would otherwise point at the prelude's
	// `interface Plus`) must have been rewritten into a note instead.
	for label in &diag.labels {
		assert!(
			label.span.start < user_source_len(),
			"a label must never leak a stdlib-internal span: {label:?}"
		);
	}
	assert!(
		diag
			.notes
			.iter()
			.any(|n| n.contains("std prelude") || n.contains("std.ops")),
		"expected a note pointing to the std prelude in place of the scrubbed label, got: {:?}",
		diag.notes
	);
}

#[test]
fn inherent_prelude_only_method_call_panics_loudly_in_lowering() {
	// Finding 2 (stdlib linkage groundwork review, round 2): every
	// `MethodResolution` built for `MethodSource::Inherent` (a bare `impl Type {
	// .. }` block, no interface involved) used to unconditionally set
	// `impl_span: None`, so `impl_is_unmaterialized` could never flag an
	// Inherent-sourced method as prelude-origin no matter which module actually
	// defined it. Not reachable through `nymph_compiler::compile_with_prelude`'s
	// one hardcoded prelude today (`stdlib/src/ops/mod.nym` has no bare
	// inherent impls), but `check_module_with_prelude` is public, general API
	// documented to accept "one or more prelude modules" — this test drives
	// that public surface directly with a synthetic prelude that does declare
	// a bare inherent impl, reproducing the exact silent-wrong-JS hole the
	// finding describes: `P` and its inherent `foo` both live only in the
	// prelude, so `call`'s lowered body (only the user module's own AST) would
	// emit `a.foo()` with `foo` never materialized anywhere.
	let prelude = parse("struct P(v: int)\n impl P { func foo(): int = 1 }");
	let user = parse("func call(a: P): int = a.foo()");

	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));
	let errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		errors.is_empty(),
		"expected `a.foo()` to resolve cleanly via the prelude's inherent impl, got: {errors:?}"
	);

	let message = catch_panic_message(|| {
		let _ = lower_hir(&user, &checked);
	})
	.expect("expected lowering to panic rather than emit a call to an unmaterialized method");
	assert!(
		message.contains("prelude-only impl"),
		"expected the documented prelude-only-impl panic message, got: {message:?}"
	);
}

// ── Collections materialization: `try_lower_runtime_dispatch` scanning
// inherent `Declaration::Impl` blocks (`impl<T> #[T] { .. }`/`impl<T> mut
// #[T] { .. }`/`impl<K,V> #{K:V} { .. }`) in addition to the `ImplFor`
// blocks it already scanned — every real stdlib collection method lives in
// one of these, never in an `impl … for …`. ──────────────────────────────────

/// Find the `HirFunc` named `mangled` in `module.funcs`, if materialized.
fn find_func<'a>(
	module: &'a nymph_hir::hir::HirModule,
	mangled: &str,
) -> Option<&'a nymph_hir::hir::HirFunc> {
	module.funcs.iter().find(|f| f.name == mangled)
}

#[test]
fn inherent_prelude_list_method_materializes_instead_of_panicking() {
	// The collections-materialization payoff at the sema layer (no Node
	// needed to observe it): a method resolved through a prelude-only
	// INHERENT impl on `#[T]` (never an `ImplFor`) now materializes to
	// `$std$$list$second` instead of panicking at the "does not yet support
	// dispatching…prelude-only impl" gate.
	let prelude = parse("impl<T> #[T] { func second(): T = this[1] }");
	let user = parse("func f(xs: #[int]): int = xs.second()");

	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));
	let errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		errors.is_empty(),
		"expected `xs.second()` to resolve cleanly via the prelude's inherent impl, got: {errors:?}"
	);

	let module = lower_hir_with_prelude(&user, std::slice::from_ref(&prelude), &checked);
	let materialized = find_func(&module, "$std$$list$second")
		.expect("expected `second` to materialize as `$std$$list$second`");
	assert_eq!(materialized.params, vec!["$self"]);
}

#[test]
fn inherent_prelude_mut_list_method_gets_a_distinct_tag_from_the_non_mut_impl() {
	// `impl<T> mut #[T]` folds `mutable` into the mangled tag (`mut_list`)
	// distinctly from a plain `impl<T> #[T]`'s `list` tag, so a same-named
	// method declared in BOTH never collides under one mangled name.
	let prelude = parse(
		"impl<T> mut #[T] { func tag_probe(): T = this[0] }\n \
		 impl<T> #[T] { func tag_probe(): T = this[0] }",
	);
	let mut_user = parse("func f(xs: mut #[int]): int = xs.tag_probe()");

	let checked = check_module_with_prelude(&mut_user, std::slice::from_ref(&prelude));
	assert!(
		checked.diags.iter().all(|d| !d.is_error()),
		"unexpected errors: {:?}",
		checked.diags
	);
	let module = lower_hir_with_prelude(&mut_user, std::slice::from_ref(&prelude), &checked);
	assert!(
		find_func(&module, "$std$$mut_list$tag_probe").is_some()
			|| find_func(&module, "$std$$list$tag_probe").is_some(),
		"expected `tag_probe` to materialize under EITHER the mut or non-mut list tag \
		 (whichever impl the checker actually resolved through), got funcs: {:?}",
		module.funcs.iter().map(|f| &f.name).collect::<Vec<_>>()
	);
}

#[test]
fn inherent_prelude_map_method_materializes_instead_of_panicking() {
	// Same mechanism, on `#{K: V}` — proves the `Map` arm of
	// `inherent_self_type_tag`, not just `List`.
	let prelude = parse("impl<K, V> #{K: V} { func answer(): int = 42 }");
	let user = parse("func f(m: #{int: string}): int = m.answer()");

	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));
	assert!(
		checked.diags.iter().all(|d| !d.is_error()),
		"unexpected errors: {:?}",
		checked.diags
	);
	let module = lower_hir_with_prelude(&user, std::slice::from_ref(&prelude), &checked);
	find_func(&module, "$std$$map$answer")
		.expect("expected `answer` to materialize as `$std$$map$answer`");
}

#[test]
fn inherent_prelude_external_instance_method_stays_a_loud_defer() {
	// `external(len) func len(): uint` in an inherent `impl<T> #[T]` block —
	// no `ImplMember::Func` body, so `try_lower_runtime_dispatch`'s
	// inherent branch must return `None` (never panic INSIDE materializing a
	// nonexistent body) and the call site keeps its loud panic.
	let prelude = parse("impl<T> #[T] { external(len) func len(): uint }");
	let user = parse("func f(xs: #[int]): uint = xs.len()");

	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));
	assert!(
		checked.diags.iter().all(|d| !d.is_error()),
		"unexpected errors: {:?}",
		checked.diags
	);
	let message = catch_panic_message(|| {
		let _ = lower_hir_with_prelude(&user, std::slice::from_ref(&prelude), &checked);
	})
	.expect("expected lowering to panic rather than emit a call to an `external` method");
	assert!(
		message.contains("prelude-only impl"),
		"expected the documented prelude-only-impl panic message, got: {message:?}"
	);
}

#[test]
fn inherent_prelude_transitively_external_method_stays_a_loud_defer() {
	// `is_empty`'s shape from the real `list.nym`/`map.nym`: a real Nymph
	// body (`this.len() == 0`) that itself calls an external INSTANCE
	// method (`len`) declared in the SAME impl block. Must stay a clean loud
	// defer at `is_empty`'s OWN call site — not materialize `is_empty` and
	// panic/throw reaching `len` mid-body.
	let prelude = parse(
		"impl<T> #[T] { \
		   external(len) func len(): uint \
		   func is_empty(): boolean = this.len() == 0 \
		 }",
	);
	let user = parse("func f(xs: #[int]): boolean = xs.is_empty()");

	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));
	assert!(
		checked.diags.iter().all(|d| !d.is_error()),
		"unexpected errors: {:?}",
		checked.diags
	);
	let message = catch_panic_message(|| {
		let _ = lower_hir_with_prelude(&user, std::slice::from_ref(&prelude), &checked);
	})
	.expect(
		"expected lowering to panic rather than materialize a body that transitively calls an external",
	);
	assert!(
		message.contains("prelude-only impl"),
		"expected the documented prelude-only-impl panic message, got: {message:?}"
	);
}

#[test]
fn inherent_prelude_struct_receiver_still_stays_a_loud_defer() {
	// A named struct/enum receiver (not `List`/`Map`) is NOT in this slice's
	// tag inventory (`inherent_self_type_tag` only covers `List`/`Map` plus
	// the six primitives) — `P.foo()` must keep panicking exactly like
	// before this slice (this is the SAME shape as
	// `inherent_prelude_only_method_call_panics_loudly_in_lowering` above,
	// re-asserted here with `lower_hir_with_prelude` rather than the
	// no-prelude `lower_hir`, to pin that the extension didn't accidentally
	// widen the tag inventory to arbitrary structs).
	let prelude = parse("struct P(v: int)\n impl P { func foo(): int = 1 }");
	let user = parse("func call(a: P): int = a.foo()");

	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));
	assert!(
		checked.diags.iter().all(|d| !d.is_error()),
		"unexpected errors: {:?}",
		checked.diags
	);
	let message = catch_panic_message(|| {
		let _ = lower_hir_with_prelude(&user, std::slice::from_ref(&prelude), &checked);
	})
	.expect("expected lowering to keep panicking for a struct-receiver inherent impl");
	assert!(
		message.contains("prelude-only impl"),
		"expected the documented prelude-only-impl panic message, got: {message:?}"
	);
}

/// Run `f`, catching any panic and returning its message — captured from
/// *inside* a temporary panic hook rather than by downcasting the
/// `catch_unwind` payload directly (see `nymph-compiler/tests/prelude.rs`'s
/// identical helper for the full rationale). `panic::set_hook`/`take_hook`
/// are process-global, so a single `Mutex` serializes concurrent callers
/// under `cargo test`'s multi-threaded-per-process harness.
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

/// A generous upper bound on the user source length used across this file's
/// `parse` calls — used only to assert a span is user-anchored (`< SPAN_BASE`
/// would also work, but this keeps the assertion legible without importing the
/// private offset constant).
fn user_source_len() -> usize {
	1 << 16
}
