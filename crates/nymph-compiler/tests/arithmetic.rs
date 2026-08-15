//! Phase-scoped tests for slice #10a: built-in arithmetic, bitwise, and
//! relational operators unwrap their boxed operands, use the native JS fast
//! path, and re-box the result in the checker-resolved output type.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use nymph_compiler::compile;

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
fn uint_arithmetic_retypes_an_unsuffixed_int_literal() {
	let js = emit_js(
		"func previous(index: uint): uint = index - 1
		 func next(index: uint): uint = 1 + index
		 func half(value: uint): float = value / 2
		 func increment(value: float): float = value + 1",
	);
	assert!(
		js.contains("new NUint(index.v - new NUint(1).v)"),
		"a right-hand literal inherits the uint operand type: {js}"
	);
	assert!(
		js.contains("new NUint(new NUint(1).v + index.v)"),
		"a left-hand literal inherits the uint operand type: {js}"
	);
	assert!(
		js.contains("new NFloat(value.v / new NUint(2).v)"),
		"uint division by an inherited literal still produces a float: {js}"
	);
	assert!(
		js.contains("new NFloat(value.v + new NFloat(1).v)"),
		"an unsuffixed literal inherits a float operand type: {js}"
	);
}

#[test]
fn negative_literals_and_int_values_stay_signed_in_uint_arithmetic() {
	let js = emit_js(
		"func offset(index: uint): int = index + -1
		 func add_offset(index: uint, offset: int): int = index + offset",
	);
	assert!(
		js.contains("new NInt(index.v + new NInt(-new NInt(1).v).v)"),
		"a negated literal remains signed: {js}"
	);
	assert!(
		js.contains("new NInt(index.v + offset.v)"),
		"an int value remains signed: {js}"
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
			let result = xs[0u] / xs[0u]
			let pin: int = xs[0u]
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
fn non_primitive_equality_uses_identity() {
	let src = "struct Point(x: int)
		impl Equals<Other = Point> for Point { func equals(other: Point): boolean = true }
		func distinct(): boolean = Point(x = 1) == Point(x = 1)
		func identical(): boolean = { let p = Point(x = 1) p == p }
		func different(): boolean = Point(x = 1) != Point(x = 2)";
	assert_eq!(run(src, "distinct().v"), "false");
	assert_eq!(run(src, "identical().v"), "true");
	assert_eq!(run(src, "different().v"), "true");
}

#[test]
fn explicit_not_equals_call_uses_the_override() {
	let src = "struct Point(x: int)
		impl Equals<Other = Point> for Point {
			func equals(other: Point): boolean = true
			func not_equals(other: Point): boolean = true
		}
		func different(): boolean = {
			let point = Point(x = 1)
			point.not_equals(point)
		}";
	assert_eq!(run(src, "different().v"), "true");
}

#[test]
fn late_resolved_primitive_equality_compares_payloads() {
	let js = emit_js(
		"func same(): boolean = {
			let xs = #[]
			let result = xs[0u] == xs[0u]
			let pin: int = xs[0u]
			result
		}",
	);
	assert!(
		js.contains(").v === xs.index(") && js.contains(").v)"),
		"late-resolved primitive equality compares boxed payloads: {js}"
	);
}

#[test]
fn late_resolved_adt_equality_uses_identity() {
	let js = emit_js(
		"struct Point(x: int)
		impl Equals<Other = Point> for Point { func equals(other: Point): boolean = true }
		func same(): boolean = {
			let xs = #[]
			let result = xs[0u] == xs[0u]
			let pin: #[Point] = xs
			result
		}",
	);
	assert!(
		js.contains("xs.index(") && js.contains(" === xs.index("),
		"late-resolved ADT equality compares identity: {js}"
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
fn compound_assignment_preserves_the_checker_selected_uint_type() {
	let src = "func f(): uint = { let mut x = 2u x += 3u x }";
	let js = emit_js(src);
	assert!(
		js.contains("x = new NUint(x.v + new NUint(3).v)"),
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
