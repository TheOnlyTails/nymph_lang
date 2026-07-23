//! Phase-scoped tests for slice #10a: built-in arithmetic, bitwise, and
//! relational operators unwrap their boxed operands, use the native JS fast
//! path, and re-box the result in the checker-resolved output type.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use nymph_codegen::compile;

fn emit_js(src: &str) -> String {
	compile(src, "test").unwrap_or_else(|diags| panic!("unexpected diagnostics: {diags:?}"))
}

fn run_node(js: &str) -> String {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let path = std::env::temp_dir().join(format!(
		"nymph_arithmetic_{}_{unique}.mjs",
		std::process::id()
	));
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

fn run(src: &str, expr: &str) -> String {
	let mut js = emit_js(src);
	js.push_str(&format!("\nconsole.log({expr});\n"));
	run_node(&js)
}

#[test]
fn int_addition_unwraps_and_reboxes_as_nint() {
	let js = emit_js("func add(a: int, b: int): int = a + b");
	assert!(
		js.contains("new NInt(a.v + b.v)"),
		"int addition uses the boxed built-in fast path: {js}"
	);
}

#[test]
fn division_reboxes_in_the_resolved_float_output_type() {
	let js = emit_js("func divide(a: int, b: int): float = a / b");
	assert!(
		js.contains("new NFloat(a.v / b.v)"),
		"division follows its resolved float Output type: {js}"
	);
}

#[test]
fn late_resolved_integral_division_reboxes_as_nfloat() {
	let js = emit_js(
		"func divide(): float = {
			let xs = #[]
			let result = xs[0] / xs[0]
			let pin: int = xs[0]
			result
		}",
	);
	assert!(
		js.contains("new NFloat(xs.index(") && js.contains(").v / xs.index(") && js.contains(").v)"),
		"late-resolved integral division follows its float Output type: {js}"
	);
}

#[test]
fn relational_operators_rebox_as_nbool() {
	let js = emit_js("func less(a: int, b: int): boolean = a < b");
	assert!(
		js.contains("new NBool(a.v < b.v)"),
		"primitive comparison produces a boxed boolean: {js}"
	);
}

#[test]
fn non_primitive_equality_dispatches_to_equals_and_not_equals() {
	let src = "interface Equals<Other> {
		func equals(other: Other): boolean
		func not_equals(other: Other): boolean = !this.equals(other)
	}
		struct Point(x: int)
		impl Equals<Other = Point> for Point { func equals(other: Point): boolean = true }
		func distinct(): boolean = Point(x = 1) == Point(x = 1)
		func identical(): boolean = { let p = Point(x = 1) p == p }
		func different(): boolean = Point(x = 1) != Point(x = 2)";
	assert_eq!(run(src, "distinct().v"), "true");
	assert_eq!(run(src, "identical().v"), "true");
	assert_eq!(run(src, "different().v"), "false");
}

#[test]
fn explicit_not_equals_override_wins() {
	let src = "interface Equals<Other> {
		func equals(other: Other): boolean
		func not_equals(other: Other): boolean = !this.equals(other)
	}
		struct Point(x: int)
		impl Equals<Other = Point> for Point {
			func equals(other: Point): boolean = true
			func not_equals(other: Point): boolean = true
		}
		func different(): boolean = Point(x = 1) != Point(x = 1)";
	assert_eq!(run(src, "different().v"), "true");
}

#[test]
fn late_resolved_primitive_equality_compares_payloads() {
	let js = emit_js(
		"func same(): boolean = {
			let xs = #[]
			let result = xs[0] == xs[0]
			let pin: int = xs[0]
			result
		}",
	);
	assert!(
		js.contains(").v === xs.index(") && js.contains(").v)"),
		"late-resolved primitive equality compares boxed payloads: {js}"
	);
}

#[test]
fn late_resolved_adt_equality_dispatches_to_equals() {
	let js = emit_js(
		"interface Equals<Other> { func equals(other: Other): boolean }
		struct Point(x: int)
		impl Equals<Other = Point> for Point { func equals(other: Point): boolean = true }
		func same(): boolean = {
			let xs = #[]
			let result = xs[0] == xs[0]
			let pin: #[Point] = xs
			result
		}",
	);
	assert!(
		js.contains("xs.index(") && js.contains(".equals(xs.index("),
		"late-resolved ADT equality dispatches to Equals: {js}"
	);
}

#[test]
fn numeric_prefix_operators_unwrap_and_rebox() {
	let js = emit_js("func negate(x: int): int = -x\nfunc invert(x: int): int = ~x");
	assert!(
		js.contains("new NInt(-x.v)"),
		"negation reboxes as NInt: {js}"
	);
	assert!(
		js.contains("new NInt(~x.v)"),
		"bit-not reboxes as NInt: {js}"
	);
}

#[test]
fn compound_assignment_uses_the_target_type_for_reboxing() {
	let src = "func f(): int = { let mut x = 2 x += 3 x }";
	let js = emit_js(src);
	assert!(
		js.contains("x = new NInt(x.v + new NInt(3).v)"),
		"compound assignment reboxes its inner operation as the target type: {js}"
	);
	assert_eq!(run(src, "f().v"), "5");
}

#[test]
fn boxed_builtin_operators_execute_under_node() {
	assert_eq!(run("func f(): float = (5 + 3) / 2", "f().v"), "4");
	assert_eq!(run("func f(): int = if (2 < 3) 7 else 9", "f().v"), "7");
	assert_eq!(run("func f(): int = -(~5)", "f().v"), "6");
}
