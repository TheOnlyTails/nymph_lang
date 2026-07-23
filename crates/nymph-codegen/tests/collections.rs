//! Phase-scoped tests for slice #6: boxed collection representation,
//! value-equality maps, and uniform index/iteration dispatch.

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
