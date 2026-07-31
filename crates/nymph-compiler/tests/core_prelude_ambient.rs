//! Integration tests for the core/std split, Slice A: `core` (the
//! compiler-coupled subset of the stdlib — `ops`, `default`, `option`,
//! `result`, `convert`, `iter`, `iter/iterable`, `range`) is now injected as
//! the AMBIENT prelude for every `check`/`compile` call, not just
//! `stdlib/src/ops/mod.nym` as before this slice.
//!
//! See `crates/nymph-compiler/tests/golden_programs.rs` for the two
//! pre-existing golden tests (`golden_enums_construction_and_qualification`,
//! `golden_match_variants_bindings_and_guards`) rewritten to drop their own
//! local `enum Option` in favor of this ambient one, and this file's sibling
//! `crates/nymph-sema/src/prelude.rs`'s
//! `eight_prelude_modules_all_get_pairwise_disjoint_offsets` unit test for the
//! NodeId/Span-offset-disjointness proof at the real 8-module core count.

use nymph_compiler::{check, compile};

#[test]
fn ambient_hash_interface_lowers_to_the_boxed_runtime_intrinsic() {
	let js = compile("func value(): int = 1.hash()", "hash_ambient")
		.expect("Hash should be available from the ambient ops prelude");
	assert!(!js.contains("//#region std/option"), "{js}");
	assert!(js.contains("return hash(new NInt(1))"), "{js}");
	assert_eq!(
		run("func value(): int = 1.hash()", "value()"),
		"-1852055280"
	);
}

/// Emit `src`, append a driver that logs `call`, run under Node, return
/// trimmed stdout. Local copy of `golden_programs.rs`'s `run` helper (tests
/// may not import from another crate's test files).
fn run(src: &str, call: &str) -> String {
	use std::collections::HashMap;
	use std::io::Write;
	use std::sync::atomic::{AtomicU64, Ordering};
	use std::sync::{Mutex, OnceLock};

	static COMPILED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
	let cache = COMPILED.get_or_init(|| Mutex::new(HashMap::new()));
	let cached = cache.lock().unwrap().get(src).cloned();
	let mut js = cached.unwrap_or_else(|| {
		let compiled = compile(src, "core_prelude_ambient").expect("expected a clean compile");
		cache
			.lock()
			.unwrap()
			.insert(src.to_owned(), compiled.clone());
		compiled
	});
	js.push_str(&format!("\nconsole.log(({call}).v);\n"));

	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir();
	let path = dir.join(format!(
		"nymph_core_prelude_ambient_{}_{unique}.mjs",
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

/// The headline Slice A payoff: a program using `Option` (construction +
/// `match`), `Result`, `convert.nym`'s `Option`/`Result` conversions
/// (`.ok_or(..)`, `.ok()`), and a `for`-over-list — all with **no `import`
/// anywhere** — compiles clean and runs under Node with the right values.
/// `Default`'s `T.default()` is exercised separately below so its unsupported
/// generic namespaced dispatch has a focused diagnostic regression.
#[test]
fn ambient_core_option_result_convert_and_for_over_list_run_with_no_import() {
	let src = r#"
		func classify(n: int): string = {
			let o: Option<int> = if (n > 0) { Some(value = n) } else { None }
			match (o) {
				Some(value) -> "pos",
				None -> "non-pos",
			}
		}

		func safe_div(a: int, b: int): Result<float, string> = if (b == 0) {
			Result.Error(error = "div by zero")
		} else {
			Result.Ok(value = a / b)
		}

		func sum_list(): int = {
			let mut total = 0
			for (x in #[1, 2, 3, 4]) {
				total = total + x
			}
			total
		}

		// `convert.nym`'s `Option::ok_or` (builds a `Result` from an `Option`).
		func opt_to_result(n: int): int = {
			let o: Option<int> = if (n > 0) { Some(value = n) } else { None }
			o.ok_or(-1).unwrap(-99)
		}

		// `convert.nym`'s `Result::ok` (builds an `Option` from a `Result`).
		func result_to_opt(b: int): float = safe_div(10, b).ok().unwrap(-1.0)
		"#;

	let diags = check(src, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected the ambient-core program to check cleanly with no import, got: {diags:?}"
	);

	assert_eq!(run(src, "classify(new NInt(5))"), "pos");
	assert_eq!(run(src, "classify(new NInt(-5))"), "non-pos");
	assert_eq!(run(src, "sum_list()"), "10");
	// `n = 7 > 0` -> `Some(7)` -> `.ok_or(-1)` -> `Result.Ok(7)` -> `.unwrap(-99)` -> `7`.
	assert_eq!(run(src, "opt_to_result(new NInt(7))"), "7");
	// `n = -1 <= 0` -> `None` -> `.ok_or(-1)` -> `Result.Error(-1)` -> `.unwrap(-99)` -> the
	// fallback `-99` (Result's `unwrap(default)` returns `default` on `Error`, not the error
	// value itself).
	assert_eq!(run(src, "opt_to_result(new NInt(-1))"), "-99");
	// `safe_div(10, 2)` -> `Result.Ok(5)` -> `.ok()` -> `Some(5)` -> `.unwrap(-1)` -> `5`.
	assert_eq!(run(src, "result_to_opt(new NInt(2))"), "5");
	// `safe_div(10, 0)` -> `Result.Error(..)` -> `.ok()` -> `None` -> `.unwrap(-1)` -> `-1`.
	assert_eq!(run(src, "result_to_opt(new NInt(0))"), "-1");
}

/// `std/math` (added to `CORE_SOURCES` alongside the other core modules) is
/// ambient too: `int`/`float`'s `abs`/`sqrt` methods, and the `Power<Other =
/// float, Output = float> for int` impl that makes `this ** 0.5` type-check
/// for an `int` receiver, all resolve and run with **no `import` anywhere** —
/// the headline payoff of making `math` a 9th core module. Locks in the
/// runtime behavior so a regression (e.g. dropping `("std/math", ..)` from
/// `CORE_SOURCES`, or reintroducing the `Power<Other = float, Output =
/// Complex> for int` ambiguity this slice removed from `complex.nym`) fails
/// loudly here instead of staying silently green under `stdlib_typechecks_
/// cleanly`/`docs_samples` (both compile-only, never run under Node).
#[test]
fn ambient_math_abs_sqrt_and_power_run_with_no_import() {
	let src = r#"
		func neg_abs(): int = (0 - 5).abs()
		func pos_abs(): int = (5).abs()
		func float_abs(): float = (0.0 - 2.5).abs()
		func int_sqrt(): float = (16).sqrt()
		func int_pow_frac(): float = 16 ** 0.5
		"#;

	let diags = check(src, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected the ambient-math program to check cleanly with no import, got: {diags:?}"
	);

	assert_eq!(run(src, "neg_abs()"), "5");
	assert_eq!(run(src, "pos_abs()"), "5");
	assert_eq!(run(src, "float_abs()"), "2.5");
	assert_eq!(run(src, "int_sqrt()"), "4");
	assert_eq!(run(src, "int_pow_frac()"), "4");
}

/// `std/math`'s plain top-level `let` constants (`pi`, `tau`, `e`, `phi`,
/// `max_int`, `min_int` — literal-initialized, never `external`) are ambient
/// too, and must be genuinely usable with no import: a bare reference type-
/// checks (name resolution sees every core module's names) AND runs correctly
/// under Node. Before the fix this locks in, `lower_hir_with_prelude`'s
/// materialization machinery only ever demand-materialized prelude
/// FUNCTIONS/METHODS (`try_materialize_prelude_dispatch`,
/// `materialize_referenced_prelude_enums`) — a bare identifier referencing a
/// prelude top-level `let` lowered straight through `resolve()`'s fallback
/// (`HirExpr::Local(name)` unchanged) with no corresponding `const` ever
/// emitted anywhere in the module, so `func f() = pi` compiled with ZERO
/// diagnostics to `function f() { return pi; }` — a silent-wrong-JS
/// `ReferenceError: pi is not defined` at runtime, not a loud lowering panic.
#[test]
fn ambient_math_constants_run_with_no_import() {
	let src = r#"
		func get_pi(): float = pi
		func get_tau(): float = tau
		func get_e(): float = e
		func get_phi(): float = phi
		func get_max_int(): int = max_int
		func get_min_int(): int = min_int
		"#;

	let diags = check(src, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected the ambient-math constants program to check cleanly with no import, got: {diags:?}"
	);

	// `pi`/`tau`/`e` match Rust's own `std::f64::consts` (both languages'
	// literals round to the same nearest `f64`) — use those directly rather
	// than repeat the full-precision literal `clippy::excessive_precision`/
	// `clippy::approx_constant` would flag. `phi` has no `std` counterpart, so
	// it stays a literal, truncated to `f64`'s significant digits.
	let pi: f64 = std::f64::consts::PI;
	let tau: f64 = std::f64::consts::TAU;
	let e: f64 = std::f64::consts::E;
	let phi: f64 = 1.618_033_988_749_895;
	assert_eq!(run(src, "get_pi()"), pi.to_string());
	assert_eq!(run(src, "get_tau()"), tau.to_string());
	assert_eq!(run(src, "get_e()"), e.to_string());
	assert_eq!(run(src, "get_phi()"), phi.to_string());
	// `int` lowers to a plain JS `number` (an `f64`) throughout this compiler,
	// so `2 ** 63 - 1`/`-2 ** 63` — both outside `f64`'s 53-bit exact-integer
	// range — round to the nearest representable double on both sides; assert
	// against that same rounding (`f64` arithmetic), not the exact `i64`
	// bound, so this test isn't asserting a precision guarantee the language
	// doesn't make.
	let max_int: f64 = 2f64.powi(63) - 1.0;
	let min_int: f64 = -(2f64.powi(63));
	assert_eq!(run(src, "get_max_int()"), max_int.to_string());
	assert_eq!(run(src, "get_min_int()"), min_int.to_string());
}

/// `Default` is ambient too (no `import` needed to name it as a bound), but a
/// namespaced call through a generic parameter cannot select a concrete
/// implementation in the current type-erased runtime. It must return one
/// exact typed diagnostic and no JavaScript rather than emit an unbound `T`.
#[test]
fn default_generic_bound_returns_one_exact_typed_diagnostic_and_no_js() {
	let source = "func make<T: Default>(): T = T.default()\nfunc use_it(): int = make()";

	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|d| d.is_error()),
		"expected `make` to typecheck cleanly via the ambient `Default`, got: {diags:?}"
	);
	let diagnostics = compile(source, "test")
		.expect_err("unsupported generic namespaced dispatch must not emit JavaScript");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(diagnostics[0].code, "STABLE-EMISSION-LINK");
	assert_eq!(diagnostics[0].severity, nymph_compiler::Severity::Error);
	assert_eq!(
		diagnostics[0].message,
		"stable runtime linking failed: Lowering(Unsupported { definition: DefinitionId { module: ModuleIdentity { origin: Project(\"__nymph_internal_facade_project__\"), project: \"__nymph_internal_facade_project__\", path: \"__nymph_internal_standalone_entry__\" }, key: TopLevel { category: Function, name: \"make\", duplicate: 0 } }, node: Some(BodyNodeId(0)), feature: \"namespaced call through a generic type parameter\" })"
	);
}

/// A user redefinition of an ambient core name (`Option`, here — not `ops`)
/// must never leak a `std/…` (or any prelude-internal) span to the user: the
/// `Redefinition` diagnostic is anchored entirely at the user's OWN
/// declaration, with a plain note (no span, no `std/ops`-only wording) taking
/// the place of a "first defined here" label that would otherwise point into
/// the injected core clone.
#[test]
fn a_project_option_remains_distinct_from_compiler_option() {
	let source = "enum Option<T> { Some(value: T), None }\nfunc f(): int = 1";
	let diags = check(source, "test");
	assert!(
		!diags.iter().any(|diagnostic| diagnostic.is_error()),
		"{diags:?}"
	);
	compile(source, "test").expect("project Option and compiler Option must compile distinctly");
}
