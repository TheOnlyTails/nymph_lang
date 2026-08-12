//! Tests for condition and logical-operator lowering under uniform value boxing
//! (ADR-0002). Because every value is boxed,
//! `ToBoolean(box)` is unconditionally `true`: every `if`/`while`/`for`
//! condition and every `&&`/`||`/`!` must read or produce the raw `.v` payload.
//!
//! Two axes:
//! * **Emission-shape** — the condition/logical rewrites emit the decided JS
//!   shapes (`.v` unwrap, the `a.v ? b : a` operand-reuse ternary, `new NBool`).
//! * **Runtime** — the emitted `.mjs` runs under Node and the boolean logic
//!   produces the correct result, including termination of a boolean-flag
//!   loop. These deliberately isolate boolean behavior from arithmetic
//!   and relational operators.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use nymph_compiler::compile;

/// Compile a Nymph source module to its JS, panicking on any diagnostic.
fn emit_js(src: &str) -> String {
	compile(src, "test").unwrap_or_else(|diags| panic!("unexpected diagnostics: {diags:?}"))
}

/// Write `js` to a temporary `.mjs`, run it under Node, and return trimmed
/// stdout. Mirrors `boxing.rs`/`run_node.rs`'s node-invocation helper.
fn run_node(js: &str) -> String {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let path = std::env::temp_dir().join(format!("nymph_cond_{}_{unique}.mjs", std::process::id()));
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(js.as_bytes()).unwrap();

	let output = Command::new("node")
		.arg(&path)
		.env("NO_COLOR", "1")
		.env_remove("FORCE_COLOR")
		.output()
		.expect("run node");
	let _ = std::fs::remove_file(&path);
	assert!(
		output.status.success(),
		"node failed:\n{}\n--- js ---\n{js}",
		String::from_utf8_lossy(&output.stderr),
	);
	String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Emit `src`, append a driver that logs `expr`, and run it under Node.
fn run(src: &str, expr: &str) -> String {
	let mut js = emit_js(src);
	js.push_str(&format!("\nconsole.log(String({expr}));\n"));
	run_node(&js)
}

// ── Emission-shape ───────────────────────────────────────────────────────────

#[test]
fn an_if_condition_reads_the_raw_v_payload() {
	let js = emit_js("func f(b: boolean): int = if (b) 1 else 2");
	assert!(
		js.lines()
			.any(|line| line.contains(".resumeState = ") && line.contains(".v ? ")),
		"a user-boolean `if` condition unwraps its box via `.v`: {js}"
	);
}

#[test]
fn logical_not_unwraps_negates_and_reboxes() {
	let js = emit_js("func f(b: boolean): boolean = !b");
	assert!(
		js.lines()
			.any(|line| line.contains("new NBool(!") && line.contains(".v)")),
		"`!b` → `new NBool(!b.v)` (unwrap, negate, re-box): {js}"
	);
}

#[test]
fn logical_and_lowers_to_the_operand_reuse_ternary() {
	// A local operand is side-effect-free, so it is re-emitted directly — no IIFE.
	let js = emit_js("func f(a: boolean, b: boolean): boolean = a && b");
	assert!(
		js.lines()
			.any(|line| line.contains(".v ? ") && line.contains(" : ")),
		"`a && b` → `a.v ? b : a` (short-circuit + boxed result): {js}"
	);
}

#[test]
fn logical_or_lowers_to_the_operand_reuse_ternary() {
	let js = emit_js("func f(a: boolean, b: boolean): boolean = a || b");
	assert!(
		js.lines()
			.any(|line| line.contains(".v ? ") && line.contains(" : ")),
		"`a || b` preserves operand-reuse short-circuiting: {js}"
	);
}

#[test]
fn a_side_effecting_and_operand_is_stored_once_in_the_activation() {
	// The activation stores the non-trivial left operand once before it branches.
	let js = emit_js("func g(z: boolean): boolean = z\nfunc f(x: boolean): boolean = g(x) && x");
	let occurrences = js.matches(" = g;").count();
	assert_eq!(
		occurrences, 1,
		"a side-effecting `&&` left operand is stored once in the activation: {js}"
	);
	assert!(
		js.contains("nymphPush("),
		"the call is pushed into the activation: {js}"
	);
}

#[test]
fn an_is_type_test_produces_a_boxed_nbool() {
	// `is` takes a pattern (here the literal `5`); the desugar yields boxed-`NBool`
	// arm bodies while the internal pattern test stays raw.
	let js = emit_js("func f(x: int): boolean = x is 5");
	assert!(
		js.contains("new NBool("),
		"`x is 5` produces a boxed NBool result: {js}"
	);
}

// ── Runtime ──────────────────────────────────────────────────────────────────

#[test]
fn a_false_if_condition_takes_the_else_branch() {
	// The headline correctness point: a boxed `false` is truthy as an object, so
	// without the `.v` unwrap this takes the THEN branch. It must take `else`.
	assert_eq!(run("func f(): int = if (false) 1 else 2", "f().v"), "2");
}

#[test]
fn logical_and_short_circuits_and_yields_the_right_operand() {
	assert_eq!(
		run("func f(): int = if (true && false) 1 else 2", "f().v"),
		"2"
	);
	assert_eq!(
		run("func f(): int = if (true && true) 1 else 2", "f().v"),
		"1"
	);
}

#[test]
fn logical_or_short_circuits_and_yields_a_truthy_operand() {
	assert_eq!(
		run("func f(): int = if (false || true) 1 else 2", "f().v"),
		"1"
	);
	assert_eq!(
		run("func f(): int = if (false || false) 1 else 2", "f().v"),
		"2"
	);
}

#[test]
fn logical_not_flips_the_raw_boolean() {
	assert_eq!(run("func f(): int = if (!false) 1 else 2", "f().v"), "1");
	assert_eq!(run("func f(): int = if (!true) 1 else 2", "f().v"), "2");
}

#[test]
fn logical_and_short_circuit_avoids_evaluating_the_right_operand() {
	// `boom()` infinitely recurses (stack overflow → non-zero exit). Correct
	// short-circuit means `false && boom()` never calls it, so the program runs
	// to completion and prints `false`.
	let src = r#"
		func boom(): boolean = boom()
		func f(): boolean = false && boom()
	"#;
	assert_eq!(run(src, "f().v"), "false");
}

#[test]
fn a_match_guard_reads_the_raw_v_payload() {
	// The guard is the lone user-boolean slot inside `match`; it must read `.v`
	// (pattern tests stay raw). A boxed `false` guard, without the unwrap, is
	// truthy and would wrongly commit the guarded arm.
	// A call-expression guard (`pick(..)`) is a boxed `NBool`, so it exercises the
	// `.v` unwrap; a bare-identifier guard would parse as a closure (`take ->`).
	// Nullary entry points keep the boxed calling convention (a top-level driver
	// passing raw JS args would defeat the point).
	let src = r#"
		func pick(b: boolean): boolean = b
		func taken(): int = match (0) {
			n if pick(true) -> 1,
			_ -> 2,
		}
		func skipped(): int = match (0) {
			n if pick(false) -> 1,
			_ -> 2,
		}
	"#;
	assert_eq!(run(src, "taken().v"), "1");
	assert_eq!(run(src, "skipped().v"), "2");
}
