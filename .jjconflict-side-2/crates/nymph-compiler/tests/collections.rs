//! Boxed collection representation, persistent-list sharing, value-equality
//! maps, and uniform index/iteration dispatch.

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
fn list_literal_constructs_a_persistent_vector_of_boxed_elements() {
	let js = emit_js("func values(): #[int] = #[1, 2]");
	assert!(
		js.contains("new NList([new NInt(1n), new NInt(2n)])"),
		"list construction preserves boxed element order: {js}"
	);
}

#[test]
fn persistent_list_updates_preserve_aliases_and_share_unchanged_branches() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
const source = new NList(Array.from({ length: 4096 }, (_, i) => new NInt(BigInt(i))));
const alias = source;
const appended = source.appended(new NInt(4096n));
const replaced = source.replaced(new NUint(1024n), new NInt(9999n));
const sharedLeaf = source.v._leafFor(0) === replaced.v._leafFor(0);
console.log(
	alias === source,
	source.v.length,
	source.v.get(1024).v,
	appended.v.length,
	replaced.v.get(1024).v,
	sharedLeaf,
);
"#,
	);
	assert_eq!(run_node(&js), "true 4096 1024n 4097 9999n true");
}

#[test]
fn deep_and_nested_slices_are_trimmed_rebased_and_share_only_overlapping_leaves() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
const source = new NList(Array.from({ length: 4096 }, (_, i) => new NInt(BigInt(i))));
const slice = source.slice(new NUint(1024n), new NUint(3072n));
const nested = slice.slice(new NUint(512n), new NUint(1024n));
const outsideLeaf = source.v._leafFor(0);
const includesReference = (node, target) =>
	node === target || (Array.isArray(node) && node.some((child) => includesReference(child, target)));
console.log(
	slice.v.length,
	slice.v.get(0).v,
	nested.v.length,
	nested.v.get(0).v,
	slice.v._leafFor(0) === source.v._leafFor(1024),
	nested.v._leafFor(0) === source.v._leafFor(1536),
	includesReference(slice.v._root, outsideLeaf),
	Object.isFrozen(slice.v),
	Object.isFrozen(slice.v._root),
	Object.isFrozen(slice.v._tail),
	typeof globalThis.NymphListTransient,
);
"#,
	);
	assert_eq!(
		run_node(&js),
		"2048 1024n 512 1536n true true false true true true undefined"
	);
}

#[test]
fn immutable_stdlib_list_apis_leave_every_branch_unchanged() {
	let source = r#"
		func branches(): #[#[int]] = {
			let original = #[1, 2]
			#[original, original.appended(3), original.replaced(0, 9), original.slice(1, 2)]
		}
	"#;
	let mut js = emit_js(source);
	js.push_str(
		"\nconsole.log(branches().v.map(list => list.v.map(item => item.v).join(',')).join('|'));\n",
	);
	assert_eq!(run_node(&js), "1,2|1,2,3|9,2|2");
}

#[test]
fn tuple_literal_uses_its_distinct_box_and_tag() {
	let mut js = emit_js("func pair(): #(int, string) = #(1, \"one\")");
	assert!(
		js.contains("new NTuple([new NInt(1n), new NString(\"one\")])")
			|| js.contains("new NTuple([new NInt(1n), new NString('one')])"),
		"{js}"
	);
	js.push_str(
		"\nconst pairValue = pair();\nconsole.log(pairValue.v[0].v, pairValue[Symbol.for(\"nymph.tag\")].description);\n",
	);
	assert_eq!(run_node(&js), "1n nymph.tuple");
}

#[test]
fn signed_list_index_dispatches_through_the_extensible_index_impl() {
	let mut js = emit_js("func second(): int = #[10, 20][1]");
	assert!(
		!js.contains(".indexDirect(new NInt(1n))"),
		"an `int` key must dispatch through `Index<int>`: {js}"
	);
	js.push_str("\nconsole.log(second().v);\n");
	assert_eq!(run_node(&js), "20n");
}

#[test]
fn list_spread_splices_the_boxed_sources_payload_and_reboxes_the_result() {
	let src = "func values(): #[int] = { let middle = #[2, 3] #[1, ...middle, 4] }";
	let mut js = emit_js(src);
	js.push_str("\nconsole.log(values().v.map(x => x.v).join(','));\n");
	assert_eq!(run_node(&js), "1,2,3,4");
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
	assert_eq!(run_node(&js), "7n");
}

#[test]
fn equal_map_key_overwrites_without_growing_the_map() {
	let src = "func values(): #{int: int} = #{1: 7, 1: 9}";
	let mut js = emit_js(src);
	js.push_str("\nconst map = values(); console.log(map.size, map.get(new NInt(1)).v);\n");
	assert_eq!(run_node(&js), "1 9n");
}

#[test]
fn separately_boxed_equal_tuple_key_retrieves_the_entry() {
	let src = "func value(): int = #{#(1, 2): 7}[#(1, 2)]";
	let mut js = emit_js(src);
	js.push_str("\nconsole.log(value().v);\n");
	assert_eq!(run_node(&js), "7n");
}

#[test]
fn hamt_collision_node_uses_key_equality() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
const key = value => {
	const result = nymphStructuralValue({}, "struct:CollisionKey", ["value"]);
	result.value = new NString(value);
	return result;
};
let root = null;
[root] = hamtSet(root, 42, key("left"), new NInt(1), 0);
[root] = hamtSet(root, 42, key("right"), new NInt(2), 0);
console.log(
	root.entries.length,
	hamtGet(root, 42, key("left"), 0).v,
	hamtGet(root, 42, key("right"), 0).v,
);
"#,
	);
	assert_eq!(run_node(&js), "2 1n 2n");
}

#[test]
fn unlawful_float_keys_fail_at_runtime_when_bypassing_sema() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
try {
	new NMap([[new NFloat(5), new NString("float")]]);
	console.log("accepted");
} catch (error) {
	console.log(error.message);
}
"#,
	);
	assert_eq!(run_node(&js), "float has no lawful structural hash");
}

#[test]
fn hamt_handles_deep_branching_and_persistent_deletion() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
let map = new NMap([]);
for (let i = 0; i < 1000; i++) map = map.with(new NInt(i), new NInt(i * 3));
let valid = map.size === 1000;
for (let i = 0; i < 1000; i++) valid &&= map.get(new NInt(i)).v === BigInt(i * 3);
for (let i = 0; i < 1000; i += 2) map = map.without(new NInt(i));
valid &&= map.size === 500;
for (let i = 1; i < 1000; i += 2) valid &&= map.get(new NInt(i)).v === BigInt(i * 3);
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
	assert_eq!(run_node(&js), "7n");
}

#[test]
fn map_pattern_accepts_lawful_uint_keys() {
	let src = r#"
		func uint_value(): int = match (#{1u: 7}) {
			#{1u: found} -> found,
			_ -> 0,
		}
	"#;
	let mut js = emit_js(src);
	js.push_str("\nconsole.log(uint_value().v);\n");
	assert_eq!(run_node(&js), "7n");
}

#[test]
fn separately_constructed_struct_and_enum_keys_use_structural_value_equality() {
	let struct_src = "struct Point(x: int, y: int)\nfunc value(): int = #{Point(x = 1, y = 2): 7}[Point(x = 1, y = 2)]";
	let mut struct_js = emit_js(struct_src);
	struct_js.push_str("\nconsole.log(value().v);\n");
	assert_eq!(run_node(&struct_js), "7n");

	let enum_src = "enum Key { Named(value: int), Empty }\nfunc value(): int = #{Key.Named(value = 1): 9}[Key.Named(value = 1)]";
	let mut enum_js = emit_js(enum_src);
	enum_js.push_str("\nconsole.log(value().v);\n");
	assert_eq!(run_node(&enum_js), "9n");
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
	assert_eq!(run_node(&js), "7n");
}

#[test]
fn structural_hash_and_persistent_hamt_obey_equality_laws() {
	let mut js = nymph_codegen::box_module_source();
	js.push_str(
		r#"
const key = value => {
	const result = nymphStructuralValue({}, "struct:Key", ["value"]);
	result.value = new NInt(BigInt(value));
	return result;
};
const signed = new NInt(42n);
const unsigned = new NUint(42n);
const crossNumeric = new NMap([[signed, new NString("value")]]);

const left = new NMap()
	.with(key(1), new NString("one"))
	.with(key(2), new NString("two"));
const right = new NMap()
	.with(key(2), new NString("two"))
	.with(key(1), new NString("one"));
const extended = left.with(key(3), new NString("three"));
const removed = extended.without(key(1));

const hiddenA = nymphStructuralValue({}, "struct:Hidden", ["visible", "hidden"]);
hiddenA.visible = new NInt(1n);
hiddenA.hidden = new NInt(2n);
const hiddenB = nymphStructuralValue({}, "struct:Hidden", ["visible", "hidden"]);
hiddenB.visible = new NUint(1n);
hiddenB.hidden = new NInt(2n);

console.log(
	nymphKeyEquals(signed, unsigned),
	nymphHash(signed) === nymphHash(unsigned),
	crossNumeric.get(unsigned).v,
	nymphKeyEquals(left, right),
	nymphHash(left) === nymphHash(right),
	left.size,
	extended.size,
	removed.size,
	left.has(key(1)),
	removed.has(key(1)),
	nymphKeyEquals(hiddenA, hiddenB),
	nymphHash(hiddenA) === nymphHash(hiddenB),
);
"#,
	);
	assert_eq!(
		run_node(&js),
		"true true value true true 2 3 2 true false true true"
	);
}
