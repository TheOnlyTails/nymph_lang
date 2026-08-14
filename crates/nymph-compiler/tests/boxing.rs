//! Phase-scoped tests for uniform value boxing (slice #2, the keystone): the
//! runtime box representation and literal boxing. Two axes:
//!
//! * **Emission-shape** — compiling a Nymph literal emits `new N…(…)` of the
//!   right wrapper class, with the box class definitions available in the module.
//! * **Runtime-unit** — the emitted `.mjs` (and the box classes on their own)
//!   run under Node with the decided `.v` payload / `[TAG]` discriminant shape.
//!
//! This slice is intentionally RED mid-branch for the whole-program `run_node`
//! suite (downstream ops — conditions, arithmetic, match — are not adapted to
//! boxed operands until later slices); these tests pin ONLY the box
//! representation and literal boxing, which must be green now.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use nymph_compiler::compile;

/// Compile a Nymph source module to its JS, panicking on any diagnostic.
fn emit_js(src: &str) -> String {
	compile(src, "test").unwrap_or_else(|diags| panic!("unexpected diagnostics: {diags:?}"))
}

/// Write `js` to a temporary `.mjs`, run it under Node, and return trimmed
/// stdout. Mirrors `run_node.rs`'s node-invocation helper (isolated filename per
/// call, `NO_COLOR` pinned) so a boxing runtime-unit test can observe real
/// V8 behavior of the emitted representation.
fn run_node(js: &str) -> String {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let path = std::env::temp_dir().join(format!("nymph_box_{}_{unique}.mjs", std::process::id()));
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

// ── Emission-shape ───────────────────────────────────────────────────────────

#[test]
fn boxes_an_int_literal_as_new_nint_with_the_classes_inlined() {
	let js = emit_js("func f(): int = 5");
	assert!(
		js.contains("new NInt(5)"),
		"int literal boxes as NInt: {js}"
	);
	// The wrapper-class definitions travel inline in the single-module facade.
	assert!(
		js.contains("NInt = class extends NBox"),
		"box classes inlined so the module is self-contained: {js}"
	);
}

#[test]
fn boxes_a_uint_literal_as_new_nuint() {
	// Syntactic `5u`.
	let js = emit_js("func f(): uint = 5u");
	assert!(
		js.contains("new NUint(5)"),
		"uint literal boxes as NUint: {js}"
	);
}

#[test]
fn implicit_uint_to_int_conversion_reboxes_the_value() {
	let js = emit_js("func f(n: uint): int = n");
	assert!(
		js.contains("new NInt(n.v)"),
		"implicit uint-to-int conversion must produce an int box: {js}"
	);

	let call_js = emit_js(
		"func take(n: int): int = n\n\
		 struct Box { func take(n: int): int = n }\n\
		 func direct(n: uint): int = take(n)\n\
		 func method(box: Box, n: uint): int = box.take(n)",
	);
	assert!(
		call_js.matches("new NInt(n.v)").count() >= 2,
		"free and method arguments must both rebox implicit conversions: {call_js}"
	);
}

#[test]
fn boxes_a_float_literal_as_new_nfloat() {
	let js = emit_js("func f(): float = 5.0");
	assert!(
		js.contains("new NFloat(5)"),
		"float literal boxes as NFloat: {js}"
	);
}

#[test]
fn boxes_a_char_literal_as_new_nchar() {
	let js = emit_js("func f(): char = 'a'");
	assert!(
		js.contains("new NChar('a')") || js.contains("new NChar(\"a\")"),
		"char literal boxes as NChar: {js}"
	);
}

#[test]
fn boxes_a_bool_literal_as_new_nbool() {
	let js = emit_js("func f(): boolean = true");
	assert!(
		js.contains("new NBool(true)"),
		"bool literal boxes as NBool: {js}"
	);
}

#[test]
fn boxes_a_string_literal_as_new_nstring() {
	let js = emit_js("func f(): string = \"hi\"");
	assert!(
		js.contains("new NString('hi')") || js.contains("new NString(\"hi\")"),
		"string literal boxes as NString: {js}"
	);
}

#[test]
fn numeric_kind_is_threaded_from_the_checker_not_the_syntax() {
	// The literal `0` is SYNTACTICALLY an int, but the return type makes the
	// checker infer it as `uint` — the box class must follow the checker's
	// inferred type, the crux of the slice's numeric-type threading.
	let js = emit_js("func f(): uint = 0");
	assert!(
		js.contains("new NUint(0)"),
		"checker-inferred uint wins over the syntactic int form: {js}"
	);
	assert!(
		!js.contains("new NInt(0)"),
		"must NOT box as NInt when the checker inferred uint: {js}"
	);
}

#[test]
fn a_coerced_int_literal_call_argument_boxes_by_the_checker_kind() {
	// Regression (slice #2 review): a free-function call argument takes
	// `check_call_arg`, whose int-literal coercion arm used to record nothing —
	// so `g(5)` where `g(x: float)` fell back to the syntactic `int` and misboxed
	// as `NInt` even though the checker widened `5` to `float`. The arm now
	// records the coerced type, matching `check`'s own arm, so the argument boxes
	// as `NFloat`. Mirror the same for a `uint` parameter.
	let js = emit_js("func g(x: float): float = x\nfunc f(): float = g(5)");
	assert!(
		js.contains("new NFloat(5)") && !js.contains("new NInt(5)"),
		"a coerced int-literal call arg boxes by the checker-inferred float kind: {js}"
	);
	let js_u = emit_js("func g(x: uint): uint = x\nfunc f(): uint = g(5)");
	assert!(
		js_u.contains("new NUint(5)") && !js_u.contains("new NInt(5)"),
		"a coerced int-literal call arg boxes by the checker-inferred uint kind: {js_u}"
	);
}

#[test]
fn a_module_with_no_boxed_value_keeps_no_box_preamble() {
	// A function whose body is only an unboxed internal loop desugar / raw
	// arithmetic over params constructs no box, so no preamble is prepended.
	let js = emit_js("func f(a: int, b: int): int = a");
	assert!(
		!js.contains("class NBox"),
		"no box preamble when nothing is boxed: {js}"
	);
}

// ── Runtime-unit ─────────────────────────────────────────────────────────────

#[test]
fn boxed_values_carry_the_decided_payload_and_tag_shape_under_node() {
	// Drive the box classes on their own (the importable module source), asserting
	// the `.v` payload convention and the per-type global `[TAG]` discriminant —
	// including that int and float are DISTINCT despite both being JS numbers.
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
const TAG = Symbol.for("nymph.tag");
const checks = {
	int_payload: new NInt(5).v === 5,
	int_tag: new NInt(5)[TAG] === Symbol.for("nymph.int"),
	float_payload: new NFloat(5).v === 5,
	float_tag: new NFloat(5)[TAG] === Symbol.for("nymph.float"),
	int_float_distinct: new NInt(5)[TAG] !== new NFloat(5)[TAG],
	uint_tag: new NUint(5)[TAG] === Symbol.for("nymph.uint"),
	str_payload: new NString("hi").v === "hi",
	str_tag: new NString("hi")[TAG] === Symbol.for("nymph.string"),
	char_payload: new NChar("a").v === "a",
	char_tag: new NChar("a")[TAG] === Symbol.for("nymph.char"),
	bool_payload: new NBool(true).v === true,
	bool_tag: new NBool(true)[TAG] === Symbol.for("nymph.bool"),
};
const bad = Object.entries(checks).filter(([, ok]) => !ok).map(([k]) => k);
console.log(bad.length === 0 ? "ok" : "FAILED: " + bad.join(","));
"#,
	);
	assert_eq!(run_node(&js), "ok");
}

#[test]
fn an_emitted_boxed_literal_runs_and_unwraps_under_node() {
	// End-to-end through the single-module facade: the emitted module (with its
	// inline box preamble) actually executes under Node, and the boxed int both
	// unwraps to its payload and reports its global type tag.
	let mut js = emit_js("func f(): int = 5");
	js.push_str("\nconsole.log(f().v, f()[Symbol.for(\"nymph.tag\")].description);\n");
	assert_eq!(run_node(&js), "5 nymph.int");
}

#[test]
fn a_boxed_string_literal_runs_and_unwraps_under_node() {
	let mut js = emit_js("func f(): string = \"hi\"");
	js.push_str("\nconsole.log(f().v, f()[Symbol.for(\"nymph.tag\")].description);\n");
	assert_eq!(run_node(&js), "hi nymph.string");
}

#[test]
fn a_struct_value_carries_a_tag_after_boxing() {
	// Structs keep their `class` emission; this documents that a struct value is
	// still tag-bearing post-boxing (its fields are now boxed primitives). The
	// struct's own prototype tag lands in a later slice; here we assert the field
	// payloads round-trip as boxes.
	let mut js = emit_js("struct P(x: int, y: int)\nfunc mk(): P = P(x = 1, y = 2)");
	js.push_str(
		"\nconst p = mk();\nconsole.log(p.x.v, p.y.v, p.x[Symbol.for(\"nymph.tag\")].description);\n",
	);
	assert_eq!(run_node(&js), "1 2 nymph.int");
}
