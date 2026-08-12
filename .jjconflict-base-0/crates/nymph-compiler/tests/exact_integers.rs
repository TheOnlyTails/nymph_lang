use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use nymph_compiler::compile;

fn emit(source: &str) -> String {
	compile(source, "exact_integers").unwrap_or_else(|diagnostics| panic!("{diagnostics:?}"))
}

fn run(source: &str, driver: &str) -> String {
	static COUNTER: AtomicU64 = AtomicU64::new(0);

	let mut js = emit(source);
	js.push('\n');
	js.push_str(driver);
	js.push('\n');
	let path = std::env::temp_dir().join(format!(
		"nymph_exact_integers_{}_{}.mjs",
		std::process::id(),
		COUNTER.fetch_add(1, Ordering::Relaxed)
	));
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(js.as_bytes()).unwrap();
	let output = std::process::Command::new("node")
		.arg(&path)
		.env("NO_COLOR", "1")
		.env_remove("FORCE_COLOR")
		.output()
		.expect("run Node");
	let _ = std::fs::remove_file(path);
	assert!(
		output.status.success(),
		"Node failed:\n{}\n--- js ---\n{js}",
		String::from_utf8_lossy(&output.stderr)
	);
	String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn min_max_literals_emit_and_run_as_exact_bigint_payloads() {
	let source = r#"
func signed_min(): int = -9223372036854775808
func signed_max(): int = 9223372036854775807
func unsigned_max(): uint = 18446744073709551615u
"#;
	let js = emit(source);
	assert!(js.contains("new NInt(-9223372036854775808n)"), "{js}");
	assert!(js.contains("new NInt(9223372036854775807n)"), "{js}");
	assert!(js.contains("new NUint(18446744073709551615n)"), "{js}");
	assert_eq!(
		run(
			source,
			r#"console.log([
typeof signed_min().v, signed_min().v,
typeof signed_max().v, signed_max().v,
typeof unsigned_max().v, unsigned_max().v,
].join("|"));"#,
		),
		"bigint|-9223372036854775808|bigint|9223372036854775807|bigint|18446744073709551615"
	);
}

#[test]
fn exact_patterns_and_interface_defaults_keep_boundary_values() {
	let source = r#"
interface BoundaryDefaults {
  func signed_default(): int = -9223372036854775808
  func unsigned_default(): uint = 18446744073709551615u
}
struct Bounds()
impl BoundaryDefaults for Bounds { }
func signed_default(): int = Bounds().signed_default()
func unsigned_default(): uint = Bounds().unsigned_default()
func signed_pattern(value: int): int = match (value) {
  -9223372036854775808 -> 1,
  9223372036854775807 -> 2,
  _ -> 0,
}
func unsigned_pattern(value: uint): int = match (value) {
  18446744073709551615u -> 3,
  _ -> 0,
}
"#;
	assert_eq!(
		run(
			source,
			r#"console.log([
signed_default().v, unsigned_default().v,
signed_pattern(new NInt(-9223372036854775808n)).v,
signed_pattern(new NInt(9223372036854775807n)).v,
unsigned_pattern(new NUint(18446744073709551615n)).v,
].join("|"));"#,
		),
		"-9223372036854775808|18446744073709551615|1|2|3"
	);
}

#[test]
fn constant_folds_reach_hir_and_emission_without_number_rounding() {
	let source = r#"
func folded_signed(): int = 9223372036854775800 + 7
func folded_unsigned(): uint = 18446744073709551600u + 15u
"#;
	let js = emit(source);
	assert!(js.contains("new NInt(9223372036854775807n)"), "{js}");
	assert!(js.contains("new NUint(18446744073709551615n)"), "{js}");
	assert!(!js.contains("9223372036854776000"), "{js}");
	assert!(!js.contains("18446744073709552000"), "{js}");
}

#[test]
fn uncertain_integer_arithmetic_traps_instead_of_wrapping() {
	let source = r#"
func add_one(value: int): int = value + 1
func add_one_unsigned(value: uint): uint = value + 1u
func remainder(value: int, divisor: int): int = value % divisor
func divide(value: int, divisor: int): float = value / divisor
func shift(value: int, count: int): int = value << count
"#;
	let driver = r#"
const caught = (run) => { try { run(); return "ok"; } catch (error) { return `${error.name}:${error.message}`; } };
console.log([
caught(() => add_one(new NInt(9223372036854775807n))),
caught(() => add_one_unsigned(new NUint(18446744073709551615n))),
caught(() => remainder(new NInt(1n), new NInt(0n))),
caught(() => divide(new NInt(1n), new NInt(0n))),
caught(() => shift(new NInt(1n), new NInt(64n))),
].join("|"));
"#;
	assert_eq!(
		run(source, driver),
		"RangeError:int overflow|RangeError:uint overflow|RangeError:Division by zero|RangeError:integer division by zero|RangeError:integer shift count must be in 0..63"
	);
}

#[test]
fn integer_number_boundaries_are_explicit_and_checked() {
	let source = r#"
func unsigned_to_signed(value: uint): int = value as int
func signed_to_unsigned(value: int): uint = value as uint
func integer_to_float(value: int): float = value as float
func float_to_integer(value: float): int = value as int
func indexed(values: #[int], index: int): int = values[index]
"#;
	let driver = r#"
const caught = (run) => { try { return String(run().v); } catch (error) { return `${error.name}:${error.message}`; } };
console.log([
caught(() => unsigned_to_signed(new NUint(18446744073709551615n))),
caught(() => signed_to_unsigned(new NInt(-1n))),
caught(() => integer_to_float(new NInt(9007199254740992n))),
caught(() => float_to_integer(new NFloat(9223372036854775808))),
caught(() => indexed(new NList([new NInt(1n)]), new NInt(9223372036854775807n))),
caught(() => float_to_integer(new NFloat(3.9))),
].join("|"));
"#;
	assert_eq!(
		run(source, driver),
		"RangeError:int overflow|RangeError:uint overflow|9007199254740992|RangeError:int overflow|RangeError:index is outside the collection|3"
	);
}

#[test]
fn proven_integer_operations_and_conversions_omit_runtime_range_checks() {
	let source = r#"
func increment(value: uint): uint = if (value < 18446744073709551615u) value + 1u else value
func positive(value: int): uint = if (value >= 0) value as uint else 0u
func uncertain(value: int): uint = value as uint
"#;
	let js = emit(source);
	assert!(
		js.lines()
			.any(|line| line.contains("NUint.direct(") && line.contains(".v + new NUint(1n).v)")),
		"{js}"
	);
	assert!(
		js.lines()
			.any(|line| line.contains("NUint.direct(") && line.contains(".v)")),
		"{js}"
	);
	assert!(
		js.lines()
			.any(|line| line.contains("new NUint(") && line.contains(".v)")),
		"{js}"
	);
	assert_eq!(
		run(
			source,
			r#"console.log([
increment(new NUint(41n)).v,
increment(new NUint(18446744073709551615n)).v,
positive(new NInt(7n)).v,
].join("|"));"#,
		),
		"42|18446744073709551615|7"
	);
}

#[test]
fn negative_indices_and_range_slices_preserve_collection_semantics() {
	let source = r#"
func last(values: #[int]): int = values[-1]
func middle(): #[int] = #[1, 2, 3, 4][-3..-1]
func inclusive(): #[int] = #[1, 2, 3, 4][1..=2]
func reversed(): #[int] = #[1, 2, 3, 4][3..1]
func unicode(): string = "A😀éB"[-4..-1]
func unicode_index(): char = "A😀"[-1]
func full_exclusive(): #[int] = #[1, 2][0..2]
func full_inclusive(): #[int] = #[1, 2][0..=1]
func negative_edge(): #[int] = #[1, 2][-2..2]
func checked(values: #[int], start: int, end: int): #[int] = values[start..end]
func checked_index(values: #[int], index: int): int = values[index]
"#;
	let driver = r#"
const caught = (run) => { try { return run(); } catch (error) { return `${error.name}:${error.message}`; } };
const values = (list) => list.v.map((item) => item.v).join(",");
console.log([
last(new NList([new NInt(10n), new NInt(20n)])).v,
values(middle()), values(inclusive()), values(reversed()), unicode().v, unicode_index().v,
values(full_exclusive()), values(full_inclusive()), values(negative_edge()),
caught(() => values(checked(new NList([new NInt(1n)]), new NInt(0n), new NInt(2n)))),
caught(() => checked_index(new NList([new NInt(1n)]), new NInt(-2n))),
].join("|"));
"#;
	let js = emit(source);
	assert!(
		js.contains(".indexDirect("),
		"safe index must use direct HIR: {js}"
	);
	assert!(
		js.contains("nymphListSlice(") && js.contains(", false, false)"),
		"safe slice must use direct HIR: {js}"
	);
	assert!(
		js.lines()
			.any(|line| line.contains(".index(") && !line.contains("indexDirect")),
		"unknown index must remain checked: {js}"
	);
	assert_eq!(
		run(source, driver),
		"20|2,3|2,3||😀é|😀|1,2|1,2|1,2|RangeError:slice bound is outside the collection|RangeError:index is outside the collection"
	);
}

#[test]
fn obvious_invalid_indices_and_slices_are_compile_errors() {
	for (source, message) in [
		(
			"func value(): int = #[1, 2][2]",
			"index is outside the collection",
		),
		(
			"func value(): #[int] = #[1, 2][0..3]",
			"slice bound is outside the collection",
		),
		(
			"func value(): #[int] = #[1, 2][0..=2]",
			"slice bound is outside the collection",
		),
		(
			"func value(): #[int] = #[1, 2][-3..2]",
			"slice bound is outside the collection",
		),
		(
			"func value(): #(int, int) = #(1, 2)[0..1]",
			"tuple slicing is unsupported",
		),
		(
			"func value(): uint = (-1) as uint",
			"integer conversion is outside the destination range",
		),
	] {
		let diagnostics = nymph_compiler::compile(source, "range_error").unwrap_err();
		assert!(
			diagnostics
				.iter()
				.any(|diagnostic| diagnostic.message.contains(message)),
			"expected {message:?}, got {diagnostics:?}"
		);
	}
}

#[test]
fn interpolation_and_map_keys_consume_exact_bigint_values() {
	let source = r#"
func signed_min_display(): string = "${-9223372036854775808}"
func unsigned_max_display(): string = "${18446744073709551615u}"
func exact_uint_key(): string = #{1u: "value"}[1u]
"#;
	assert_eq!(
		run(
			source,
			r#"console.log([
signed_min_display().v,
unsigned_max_display().v,
exact_uint_key().v,
].join("|"));"#,
		),
		"-9223372036854775808|18446744073709551615|value"
	);
}

#[test]
fn trusted_ffi_round_trips_raw_bigints_and_rejects_number_results() {
	let source = r#"
func from_length(): uint = #[11, 22].length()
func through_index(): int = match (#[11, 22].get(1u)) {
  Some(value) -> value,
  None -> 0,
}
func compare_chars(): Order = 'a'.compare_to('b')
"#;
	let js = emit(source);
	assert!(
		js.contains("const length = ($_this) => BigInt($_this.v.length)"),
		"{js}"
	);
	assert!(
		js.lines()
			.any(|line| line.contains("nymphTrustedUInt(length(") && line.contains(")")),
		"{js}"
	);
	assert!(
		js.lines()
			.any(|line| line.contains("get(") && line.contains(".v)")),
		"{js}"
	);
	assert_eq!(
		run(
			source,
			r#"const caught = (run) => { try { run(); return "ok"; } catch (error) { return `${error.name}:${error.message}`; } };
console.log([
typeof from_length().v, from_length().v,
typeof through_index().v, through_index().v,
caught(() => nymphTrustedInt(1)),
caught(() => nymphTrustedInt(9223372036854775808n)),
].join("|"));"#,
		),
		"bigint|2|bigint|22|TypeError:trusted int FFI must return BigInt|RangeError:int overflow"
	);
}
