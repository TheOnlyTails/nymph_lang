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

fn has_emitted_line(js: &str, parts: &[&str]) -> bool {
	js.lines()
		.any(|line| parts.iter().all(|part| line.contains(part)))
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
		has_emitted_line(&js, &["new NInt(", ".v + ", ".v)"]),
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
		has_emitted_line(&js, &["new NUint(", ".v - new NUint(1n).v)"]),
		"a right-hand literal inherits the uint operand type: {js}"
	);
	assert!(
		has_emitted_line(&js, &["new NUint(new NUint(1n).v + ", ".v)"]),
		"a left-hand literal inherits the uint operand type: {js}"
	);
	assert!(
		has_emitted_line(
			&js,
			&["new NFloat(nymphCheckedDivide(", ".v, new NUint(2n).v))"],
		),
		"uint division by an inherited literal still produces a float: {js}"
	);
	assert!(
		has_emitted_line(&js, &["new NFloat(", ".v) + ", "new NFloat(1).v)"],),
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
		has_emitted_line(&js, &["new NInt(", ".v + new NInt(-1n).v)"]),
		"a negated literal remains signed: {js}"
	);
	assert!(
		has_emitted_line(&js, &["new NInt(", ".v + ", ".v)"]),
		"an int value remains signed: {js}"
	);
}

#[test]
fn division_reboxes_in_the_resolved_float_output_type() {
	let js = emit_js("func divide(a: int, b: int): float = a / b");
	assert!(
		has_emitted_line(&js, &["new NFloat(nymphCheckedDivide(", ".v, ", ".v))"]),
		"division follows its resolved float Output type: {js}"
	);
}

#[test]
fn late_resolved_integral_division_reboxes_as_nfloat() {
	let js = emit_js(
		"func defer<T>(): T = defer()
		func divide(): float = {
			let xs = #[defer()]
			let result = xs[0u] / xs[0u]
			let pin: int = xs[0u]
			result
		}",
	);
	assert!(
		has_emitted_line(
			&js,
			&[
				"new NFloat(nymphCheckedDivide(",
				".indexDirect(new NUint(0n)).v"
			],
		),
		"late-resolved integral division follows its float Output type: {js}"
	);
}

#[test]
fn relational_operators_rebox_as_nbool() {
	let js = emit_js("func less(a: int, b: int): boolean = a < b");
	assert!(
		has_emitted_line(&js, &["new NBool(", ".v < ", ".v)"]),
		"primitive comparison produces a boxed boolean: {js}"
	);
}

#[test]
fn non_primitive_equality_uses_equals() {
	let src = "struct Point(x: int)
		impl Equals<Other = Point> for Point { func equals(other: Point): boolean = true }
		func distinct(): boolean = Point(x = 1) == Point(x = 1)
		func identical(): boolean = { let p = Point(x = 1) p == p }
		func different(): boolean = Point(x = 1) != Point(x = 2)";
	assert_eq!(run(src, "distinct().v"), "true");
	assert_eq!(run(src, "identical().v"), "true");
	assert_eq!(run(src, "different().v"), "false");
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
		"func defer<T>(): T = defer()
		func same(): boolean = {
			let xs = #[defer()]
			let result = xs[0u] == xs[0u]
			let pin: int = xs[0u]
			result
		}",
	);
	assert!(
		has_emitted_line(&js, &["new NBool(", ".indexDirect(", ".v === ", ".v)"]),
		"late-resolved primitive equality compares boxed payloads: {js}"
	);
}

#[test]
fn late_resolved_adt_equality_uses_equals() {
	let js = emit_js(
		"func defer<T>(): T = defer()
		struct Point(x: int)
		impl Equals<Other = Point> for Point { func equals(other: Point): boolean = true }
		func same(): boolean = {
			let xs = #[defer()]
			let result = xs[0u] == xs[0u]
			let pin: #[Point] = xs
			result
		}",
	);
	assert!(
		js.contains(".indexDirect(") && has_emitted_line(&js, &["nymphPush(", ".equals"]),
		"late-resolved ADT equality dispatches through Equals: {js}"
	);
}

#[test]
fn numeric_prefix_operators_unwrap_and_rebox() {
	let js = emit_js("func negate(x: int): int = -x\nfunc invert(x: int): int = ~x");
	assert!(
		has_emitted_line(&js, &["new NInt(-", ".v)"]),
		"negation reboxes as NInt: {js}"
	);
	assert!(
		has_emitted_line(&js, &["new NInt(~", ".v)"]),
		"bit-not reboxes as NInt: {js}"
	);
}

#[test]
fn boxed_builtin_operators_execute_under_node() {
	assert_eq!(run("func f(): float = (5 + 3) / 2", "f().v"), "4");
	assert_eq!(run("func f(): int = if (2 < 3) 7 else 9", "f().v"), "7n");
	assert_eq!(run("func f(): int = -(~5)", "f().v"), "6n");
}
