//! Phase-scoped tests for slice #6: boxed collection representation,
//! value-equality maps, and uniform index/iteration dispatch.

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
		"nymph_collections_{}_{unique}.mjs",
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

#[test]
fn list_literal_boxes_a_native_array_of_boxed_elements() {
	let js = emit_js("func values(): #[int] = #[1, 2]");
	assert!(
		js.contains("new NList([new NInt(1), new NInt(2)])"),
		"list wrapper owns a native array of boxed elements: {js}"
	);
}

#[test]
fn tuple_literal_uses_its_distinct_box_and_tag() {
	let mut js = emit_js("func pair(): #(int, string) = #(1, \"one\")");
	assert!(
		js.contains("new NTuple([new NInt(1), new NString(\"one\")])")
			|| js.contains("new NTuple([new NInt(1), new NString('one')])"),
		"{js}"
	);
	js.push_str(
		"\nconst pairValue = pair();\nconsole.log(pairValue.v[0].v, pairValue[Symbol.for(\"nymph.tag\")].description);\n",
	);
	assert_eq!(run_node(&js), "1 nymph.tuple");
}

#[test]
fn boxed_list_index_returns_the_already_boxed_element() {
	let mut js = emit_js("func second(): int = #[10, 20][1]");
	assert!(
		js.contains(".index(new NInt(1))"),
		"index dispatches through the list box: {js}"
	);
	js.push_str("\nconsole.log(second().v);\n");
	assert_eq!(run_node(&js), "20");
}

#[test]
fn list_spread_splices_the_boxed_sources_payload_and_reboxes_the_result() {
	let src = "func values(): #[int] = { let middle = #[2, 3] #[1, ...middle, 4] }";
	let mut js = emit_js(src);
	js.push_str("\nconsole.log(values().v.map(x => x.v).join(','));\n");
	assert_eq!(run_node(&js), "1,2,3,4");
}

#[test]
fn mutable_list_index_assignment_updates_the_payload() {
	let src = "func replace(): int = { let mut values = #[1, 2] values[1] = 9 values[1] }";
	let mut js = emit_js(src);
	js.push_str("\nconsole.log(replace().v);\n");
	assert_eq!(run_node(&js), "9");
}

#[test]
fn map_spread_consumes_boxed_tuple_entries() {
	let src =
		"func values(): #{int: string} = { let pairs = #[#(1, \"a\"), #(2, \"b\")] #{...pairs} }";
	let mut js = emit_js(src);
	js.push_str("\nconsole.log([...values().values()].map(x => x.v).join(','));\n");
	assert_eq!(run_node(&js), "a,b");
}

#[test]
fn separately_boxed_equal_map_key_retrieves_the_entry() {
	let src = "func value(): int = #{1: 7}[1]";
	let mut js = emit_js(src);
	js.push_str("\nconsole.log(value().v);\n");
	assert_eq!(run_node(&js), "7");
}

#[test]
fn equal_map_key_overwrites_without_growing_the_map() {
	let src = "func values(): #{int: int} = #{1: 7, 1: 9}";
	let mut js = emit_js(src);
	js.push_str("\nconst map = values(); console.log(map.size, map.get(new NInt(1)).v);\n");
	assert_eq!(run_node(&js), "1 9");
}

#[test]
fn separately_boxed_equal_tuple_key_retrieves_the_entry() {
	let src = "func value(): int = #{#(1, 2): 7}[#(1, 2)]";
	let mut js = emit_js(src);
	js.push_str("\nconsole.log(value().v);\n");
	assert_eq!(run_node(&js), "7");
}

#[test]
fn hamt_collision_node_uses_key_equality() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
class CollisionKey extends NBox {
	hash() { return new NInt(42); }
	equals(other) { return new NBool(this.v === other.v); }
}
const map = new NMap([
	[new CollisionKey("left"), new NInt(1)],
	[new CollisionKey("right"), new NInt(2)],
]);
console.log(map.size, map.get(new CollisionKey("left")).v, map.get(new CollisionKey("right")).v);
"#,
	);
	assert_eq!(run_node(&js), "2 1 2");
}

#[test]
fn map_assignment_and_deletion_use_value_equal_keys() {
	let mut js =
		emit_js("func values(): #{int: int} = { let mut map = #{1: 7, 2: 8} map[1] = 9 map }");
	js.push_str(
		"\nconst map = values(); const removed = map.delete(new NInt(2)); console.log(map.get(new NInt(1)).v, removed, map.size);\n",
	);
	assert_eq!(run_node(&js), "9 true 1");
}

#[test]
fn numeric_tags_keep_int_and_float_keys_distinct() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
const map = new NMap([
	[new NInt(5), new NString("int")],
	[new NFloat(5), new NString("float")],
]);
console.log(map.size, map.get(new NInt(5)).v, map.get(new NFloat(5)).v);
"#,
	);
	assert_eq!(run_node(&js), "2 int float");
}

#[test]
fn hamt_handles_deep_branching_and_in_place_deletion() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
const map = new NMap([]);
for (let i = 0; i < 1000; i++) map.set(new NInt(i), new NInt(i * 3));
let valid = map.size === 1000;
for (let i = 0; i < 1000; i++) valid &&= map.get(new NInt(i)).v === i * 3;
for (let i = 0; i < 1000; i += 2) valid &&= map.delete(new NInt(i));
valid &&= map.size === 500;
for (let i = 1; i < 1000; i += 2) valid &&= map.get(new NInt(i)).v === i * 3;
console.log(valid ? "ok" : "failed");
"#,
	);
	assert_eq!(run_node(&js), "ok");
}

#[test]
fn map_pattern_uses_boxed_value_equal_keys() {
	let src = r#"
		func value(): int = match (#{1: 7, 2: 8}) {
			#{1: found} -> found,
			_ -> 0,
		}
	"#;
	let mut js = emit_js(src);
	js.push_str("\nconsole.log(value().v);\n");
	assert_eq!(run_node(&js), "7");
}

#[test]
fn map_pattern_preserves_uint_and_float_key_tags() {
	let src = r#"
		func uint_value(): int = match (#{1u: 7}) {
			#{1u: found} -> found,
			_ -> 0,
		}
		func float_value(): int = match (#{1.0: 9}) {
			#{1.0: found} -> found,
			_ -> 0,
		}
	"#;
	let mut js = emit_js(src);
	js.push_str("\nconsole.log(uint_value().v, float_value().v);\n");
	assert_eq!(run_node(&js), "7 9");
}

#[test]
fn separately_constructed_struct_and_enum_keys_use_structural_value_equality() {
	let struct_src = "struct Point(x: int, y: int)\nfunc value(): int = #{Point(x = 1, y = 2): 7}[Point(x = 1, y = 2)]";
	let mut struct_js = emit_js(struct_src);
	struct_js.push_str("\nconsole.log(value().v);\n");
	assert_eq!(run_node(&struct_js), "7");

	let enum_src = "enum Key { Named(value: int), Empty }\nfunc value(): int = #{Key.Named(value = 1): 9}[Key.Named(value = 1)]";
	let mut enum_js = emit_js(enum_src);
	enum_js.push_str("\nconsole.log(value().v);\n");
	assert_eq!(run_node(&enum_js), "9");
}

#[test]
fn separately_constructed_equal_maps_can_be_map_keys() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
const inner = () => new NMap([
	[new NInt(1), new NString("one")],
	[new NInt(2), new NString("two")],
]);
const outer = new NMap([[inner(), new NInt(7)]]);
console.log(outer.get(inner()).v);
"#,
	);
	assert_eq!(run_node(&js), "7");
}

#[test]
fn equal_maps_with_custom_equal_keys_have_equal_hashes() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
class CustomKey extends NBox {
	hash() { return new NInt(42); }
	equals(other) { return new NBool(other instanceof CustomKey); }
}
const inner = value => new NMap([[new CustomKey(value), new NInt(1)]]);
const outer = new NMap([[inner("left"), new NInt(7)]]);
console.log(outer.get(inner("right")).v);
"#,
	);
	assert_eq!(run_node(&js), "7");
}
