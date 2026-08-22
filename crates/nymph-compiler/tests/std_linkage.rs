//! End-to-end proof of the external-linkage mechanism (Gap 3, L0/L1): a
//! project `import std/collections/list`s a SYNTHETIC, self-contained std
//! module declaring `length`/`get` as `external(..)` (linked) and `is_empty`
//! as real Nymph source that transitively calls `length`, compiles through
//! the project driver's bundle path (`compile_project_with_std` →
//! `bundle::bundle`), and RUNS under Node with every linked-external import
//! resolved and inlined — L1 additionally proves the Option ABI seam: `get`'s
//! `Some`/`None` (built by the injected, stripped `list.ts` intrinsic,
//! importing the project compiler's canonical, source-derived `std/option`
//! module) are recognized by the user PROGRAM's own inline `match`, because
//! `nymph-codegen`'s `emit_enum` tags every variant with a GLOBAL
//! `Symbol.for(..)` discriminant.
//!
//! Deliberately synthetic, not the real on-disk `stdlib/src/collections/list.nym`
//! — that file's own `import @/option`/`import @/ops` don't resolve when
//! reached as a `std::`-keyed module (a `resolve.rs` limitation, out of this
//! slice's owned-files scope. This file drives the mechanism end-to-end through
//! stable project assembly, bundling, and Node.

use nymph_compiler::compile_project_with_std;
use nymph_compiler::project::compile_project_module_sources_with_std;

/// The synthetic `std/collections/list` module: JUST enough to exercise
/// linkage — `length`/`get` are `external(..)` (present in
/// `nymph_hir::linkage::REGISTRY` for a plain, non-`mut` `#[T]` receiver, so
/// they MUST materialize/link instead of loud-deferring), `is_empty` is real
/// Nymph source that calls `length` transitively (proving
/// `body_calls_unlinked_external`'s registry subtraction). `get` returns
/// `Option<T>` — `Option` itself needs no `import` here, it's ambient via the
/// complete core environment supplied to every module, including a
/// `std::`-keyed one analyzed on its own turn.
fn synth_std_provider(path: &str) -> Option<String> {
	(path == "collections/list").then(|| {
		"public impl<T> #[T] {\n  \
			external(length) func length(): uint\n  \
			external(get) func get(i: uint): Option<T>\n  \
			func is_empty(): boolean = this.length() == 0\n\
		}\n"
			.to_string()
	})
}

fn only_entry(entry_key: &'static str, entry_src: &'static str) -> impl Fn(&str) -> Option<String> {
	move |key: &str| (key == entry_key).then(|| entry_src.to_string())
}

fn synth_std_math_provider(path: &str) -> Option<String> {
	(path == "math").then(|| {
		"public external(max_float) let max_float: float\n\
		 public external(min_float) let min_float: float\n\
		 public external(min_positive_float) let min_positive_float: float\n"
			.to_string()
	})
}

fn run_node(js: &str, tag: &str) -> String {
	let dir = std::env::temp_dir();
	let path = dir.join(format!(
		"nymph_std_linkage_{tag}_{}.mjs",
		std::process::id()
	));
	let js = format!(
		"const nymphTestConsoleLog = console.log.bind(console);\n\
		 console.log = (...values) => nymphTestConsoleLog(...values.map(value => typeof value === 'bigint' ? String(value) : value));\n\
		 {js}"
	);
	std::fs::write(&path, &js).unwrap();
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

fn with_compiler_stack(test: impl FnOnce() + Send + 'static) {
	// Deep ambient native method graphs can exceed libtest's 2 MiB worker stack.
	// Production CLI threads use the process stack; keep the tests representative.
	std::thread::Builder::new()
		.stack_size(8 * 1024 * 1024)
		.spawn(test)
		.expect("spawn ambient compiler test")
		.join()
		.expect("ambient compiler test panicked");
}

/// `xs.length()` on a real list — the linked external itself — compiles,
/// bundles (the injected stripped `list.ts` intrinsic resolves and inlines
/// into the graph), and runs under Node returning the real JS array length.
#[test]
fn linked_list_length_compiles_bundles_and_runs() {
	let entry = "import std/collections/list\n\
		func demo(): uint = {\n\
		\tlet xs = #[1, 2, 3]\n\
		\txs.length()\n\
		}\n\
		func main(): void = {}\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &synth_std_provider)
		.expect("expected `xs.length()` to compile once `length` is a linked external");

	assert!(
		compiled.js.contains("length("),
		"expected the bundle to contain a `length(` call, got:\n{}",
		compiled.js
	);
	assert!(
		compiled.js.contains("$_this.v.length"),
		"expected the injected, stripped `list.ts` intrinsic body \
		 (`$_this.v.length`) to be inlined into the bundle, got:\n{}",
		compiled.js
	);

	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));

	assert_eq!(run_node(&js, "length"), "3");
}

#[test]
fn list_and_string_length_use_receiver_specific_canonical_runtime_symbols() {
	let entry = r#"
		func list_length(): uint = #[1, 2, 3].length()
		func string_length(): uint = "A😀éB".length()
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("list and string length calls should compile together");

	assert!(
		compiled.js.contains("//#region std/collections/list")
			&& compiled.js.contains("$_this.v.length")
			&& compiled.js.contains("//#region std/string")
			&& compiled.js.contains("Array.from($_this.v).length"),
		"expected both canonical length runtime modules and implementations, got:\n{}",
		compiled.js
	);

	let list_call = compiled.entry_symbol("list_length");
	let string_call = compiled.entry_symbol("string_length");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({list_call}().v);\n"));
	js.push_str(&format!("\nconsole.log({string_call}().v);\n"));
	assert_eq!(run_node(&js, "receiver_specific_lengths"), "3\n5");
}

#[test]
fn external_let_is_marshaled_once_and_shared_across_references() {
	let entry = "import std/math with (max_float)\n\
		func first(): float = max_float\n\
		func second(): float = max_float\n\
		func main(): void = {}\n";
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &synth_std_math_provider)
		.expect("external let project should compile");
	assert_eq!(
		compiled.js.matches("max_float);").count(),
		1,
		"{}",
		compiled.js
	);
	let first = compiled.entry_symbol("first");
	let second = compiled.entry_symbol("second");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({first}() === {second}(), {first}().v);\n"
	));
	assert!(run_node(&js, "external_let").starts_with("true "));
}

#[test]
fn ambient_external_let_has_one_project_owner_across_consumers() {
	let load = |key: &str| match key {
		"main" => Some(
			"import ./a with (from_a)\nimport ./b with (from_b)\n\
			 func same(): boolean = from_a() == from_b()\nfunc main(): void = {}\n"
				.to_string(),
		),
		"a" => Some(
			"import std/math with (max_float)\npublic func from_a(): float = max_float\n".to_string(),
		),
		"b" => Some(
			"import std/math with (max_float)\npublic func from_b(): float = max_float\n".to_string(),
		),
		_ => None,
	};
	let compiled = compile_project_with_std("main", &load, &synth_std_math_provider)
		.expect("multi-consumer external let project should compile");
	assert_eq!(
		compiled.js.matches("max_float);").count(),
		1,
		"{}",
		compiled.js
	);
	let same = compiled.entry_symbol("same");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({same}().v);\n"));
	assert_eq!(run_node(&js, "external_let_consumers"), "true");
}

/// `xs.is_empty()` — real Nymph source whose body transitively calls the
/// LINKED `length` — now materializes (Gap 3's `body_calls_unlinked_external`
/// registry subtraction) instead of loud-deferring, and the materialized
/// body's own `this.length()` call resolves through the SAME linked-external
/// mechanism, end to end.
#[test]
fn transitively_linked_is_empty_compiles_bundles_and_runs() {
	let entry = "import std/collections/list\n\
		func demo(): boolean = {\n\
		\tlet xs = #[1, 2, 3]\n\
		\txs.is_empty()\n\
		}\n\
		func demo_empty(): boolean = {\n\
		\tlet xs = #[]\n\
		\txs.is_empty()\n\
		}\n\
		func main(): void = {}\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &synth_std_provider)
		.expect("expected `xs.is_empty()` to materialize once `length` is linked");

	let call = compiled.entry_symbol("demo");
	let call_empty = compiled.entry_symbol("demo_empty");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({call}().v);\nconsole.log({call_empty}().v);\n"
	));

	let output = run_node(&js, "is_empty");
	let mut lines = output.lines();
	assert_eq!(lines.next(), Some("false"), "full output: {output:?}");
	assert_eq!(lines.next(), Some("true"), "full output: {output:?}");
}

/// L1's whole proof obligation: `xs.get(1)` — the Option-RETURNING linked
/// external — compiles, bundles (the stripped `list.ts` intrinsic's own
/// `import { Option } from "../option"` resolves against the canonical
/// source-derived `std/option` module), and RUNS under Node —
/// with the intrinsic-BUILT `Some`/`None` recognized by the user PROGRAM's
/// OWN inline `match`, because `nymph-codegen`'s `emit_enum` now tags every
/// variant with the GLOBAL `Symbol.for(..)` discriminant (not a fresh,
/// per-module `Symbol(..)`) — the actual defect this slice's ABI-seam
/// investigation found and fixed. Both the in-bounds (`Some`) and
/// out-of-bounds (`None`) arms are exercised.
#[test]
fn linked_list_get_compiles_bundles_and_runs_the_option_round_trip() {
	let entry = "import std/collections/list\n\
		func demo(): int = {\n\
		\tlet xs = #[10, 20, 30]\n\
		\tmatch (xs.get(1)) {\n\
		\t\tSome(value) -> value,\n\
		\t\tNone -> -1,\n\
		\t}\n\
		}\n\
		func demo_out_of_bounds(): int = {\n\
		\tlet xs = #[10, 20, 30]\n\
		\tmatch (xs.get(9)) {\n\
		\t\tSome(value) -> value,\n\
		\t\tNone -> -1,\n\
		\t}\n\
		}\n\
		func main(): void = {}\n";
	let load = only_entry("main", entry);
	let sources = compile_project_module_sources_with_std("main", &load, &synth_std_provider)
		.expect("project sources should assemble");
	assert!(sources.contains_key("@nymph/runtime/option"));
	assert_eq!(
		sources
			.keys()
			.filter(|key| key.as_str() == "@nymph/runtime/option")
			.count(),
		1
	);

	let compiled = compile_project_with_std("main", &load, &synth_std_provider).expect(
		"expected `xs.get(1)` to compile once `get` is a linked external for a `List` receiver",
	);

	let call = compiled.entry_symbol("demo");
	let call_oob = compiled.entry_symbol("demo_out_of_bounds");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({call}().v);\nconsole.log({call_oob}().v);\n"
	));

	let output = run_node(&js, "get");
	let mut lines = output.lines();
	assert_eq!(
		lines.next(),
		Some("20"),
		"expected the in-bounds `Some(value)` arm to bind the real element, full output: {output:?}"
	);
	assert_eq!(
		lines.next(),
		Some("-1"),
		"expected the out-of-bounds `None` arm to be recognized, full output: {output:?}"
	);
}

#[test]
fn option_consumer_imports_the_canonical_runtime_before_bundling() {
	let entry = "import std/collections/list\n\
		func demo(xs: #[int]): int = match (xs.get(0)) {\n\
		\tSome(value) -> value,\n\
		\tNone -> -1,\n\
		}\n\
		func main(): void = {}\n";
	let load = |key: &str| (key == "main").then(|| entry.to_string());
	let sources = compile_project_module_sources_with_std("main", &load, &synth_std_provider)
		.expect("project sources should assemble");
	assert!(sources.contains_key("@nymph/runtime/option"));
	assert_eq!(
		sources
			.keys()
			.filter(|key| key.as_str() == "@nymph/runtime/option")
			.count(),
		1
	);
	let main = sources
		.get("main")
		.expect("consumer source must be present");
	assert!(
		!main.contains("class Option"),
		"consumer must not inline the ambient Option declaration:\n{main}"
	);
}

/// L3's whole proof obligation for the MAP surface: `get`/`insert`/`remove`/
/// `size`/`keys` — all newly linked — compile, bundle (the stripped `map.ts`
/// intrinsic's own `import { Option } from "../option"` resolves against the
/// canonical source-derived `std/option` module, exactly like `list.ts`), and RUN
/// under Node. Exercises both `get`'s in-bounds/missing arms (proving the
/// L3 ABI fix — `Option.Some({ value })`, not a bare positional value, round
/// -trips through the user's own `match`), a mutation (`insert` then
/// `size`), an Option-returning mutation (`remove`), and the list-returning
/// `keys` (indexed natively, no further linkage needed).
/* Removed after immutable destination migration; mutable map linkage is frozen elsewhere.
#[test]
fn linked_map_get_insert_remove_and_keys_compile_bundle_and_run() {
	let entry = "";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &synth_std_map_provider)
		.expect("expected the map surface to compile once get/insert/remove/size/keys are linked");

	assert!(
		compiled.js.contains("//#region std/collections/map"),
		"expected the injected `std/collections/map` intrinsic module to be bundled in, got:\n{}",
		compiled.js
	);

	let calls = [
		"demo_get",
		"demo_get_missing",
		"demo_insert_size",
		"demo_remove",
		"demo_keys_first",
	]
	.map(|name| compiled.entry_symbol(name));
	let mut js = compiled.js;
	for call in &calls {
		js.push_str(&format!("\nconsole.log({call}().v);\n"));
	}

	let output = run_node(&js, "map");
	let mut lines = output.lines();
	assert_eq!(
		lines.next(),
		Some("10"),
		"expected `get(1)`'s `Some(value)` arm to bind the real value (proving the \
		 L3 named-field ABI fix), full output: {output:?}"
	);
	assert_eq!(
		lines.next(),
		Some("-1"),
		"expected `get(9)`'s `None` arm to be recognized, full output: {output:?}"
	);
	assert_eq!(
		lines.next(),
		Some("3"),
		"expected `insert(3, 30)` to mutate the map, so `size()` reads 3, full output: {output:?}"
	);
	assert_eq!(
		lines.next(),
		Some("10"),
		"expected `remove(1)`'s `Some(value)` arm to bind the removed value \
		 (proving the L3 named-field ABI fix on `remove` too), full output: {output:?}"
	);
	assert_eq!(
		lines.next(),
		Some("7"),
		"expected `keys()` to return a real list of KEYS (not values), indexable like any \
		 other list, full output: {output:?}"
	);
}

*/
#[test]
fn linked_map_merge_compiles_bundles_and_runs() {
	// `map` is now ambient (part of the `core` prelude), so no `import` is
	// needed — the map literals' `.plus()` (merge, via `Plus for #{K:V}`) and
	// `.size()` link against the registry directly, no synthetic std module.
	let entry = "func demo_merge_size(): uint = {\n\
		\tlet a = #{1: 10}\n\
		\tlet b = #{2: 20}\n\
		\tlet merged = a.plus(b)\n\
		\tmerged.keys().length()\n\
		}\n\
		func main(): void = {}\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &|_| None).expect(
		"expected the ambient map's `.plus()` (merge, a structural `ImplFor` block) and \
		 `.size()` to link and lower cleanly",
	);

	let call_merge = compiled.entry_symbol("demo_merge_size");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call_merge}().v);\n"));

	let output = run_node(&js, "map_merge_to_string");
	assert_eq!(
		output, "2",
		"expected `a.plus(b)` to merge into a 2-entry map, full output: {output:?}"
	);
}

/// L3's Set proof: `Set<Item>` (`stdlib/src/collections/set.nym`) is a
/// `struct Set(inner: #{Item: #()})` whose `insert`/`remove`/`contains`
/// delegate to the inner map's `insert`/`remove`/native `contains_key`, so a
/// Set defined as an ordinary USER/entry struct
/// (mirroring `set.nym`'s own body exactly) round-trips insert/remove/
/// contains under Node with NO new linkage of its own. (The REAL prelude
/// `Set` cannot materialize yet — a separate, pre-existing
/// prelude-method-materialization gap for named-struct receivers,
/// `nymph-codegen`'s `real_set_insert_stays_a_loud_transitively_external_defer`
/// — out of this slice's scope; see that test's own doc comment.)
/* Removed after immutable destination migration; mutable set behavior is frozen elsewhere.
#[test]
fn a_user_set_struct_backed_by_the_linked_map_inserts_removes_and_contains_round_trips() {
	let entry = "";
	let load = only_entry("main", entry);

	// Mirror the native `contains_key` body over the host `get` primitive.
	fn provider_with_contains_key(path: &str) -> Option<String> {
		(path == "collections/map").then(|| {
			"impl<K, V> #{K: V} {\n  \
				external(get) func get(key: K): Option<V>\n  \
				external(insert) func insert(key: K, value: V): boolean\n  \
				external(remove) func remove(key: K): Option<V>\n\
			}\n\
			impl<K, V> #{K: V} {\n  \
				func contains_key(key: K): boolean = match (this.get(key)) {\n    \
					Some(_) -> true,\n    \
					None -> false,\n  \
				}\n\
			}\n"
				.to_string()
		})
	}

	let compiled = compile_project_with_std("main", &load, &provider_with_contains_key)
		.expect("expected the user Set struct to compile atop the linked map surface");

	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));

	assert_eq!(
		run_node(&js, "set"),
		"true",
		"expected the Set insert/remove/contains round-trip to hold end to end"
	);
}

*/
#[test]
fn ambient_index_interface_dispatches_custom_index_access() {
	let entry = r#"
		struct Offset(base: int) {
			impl Index<Key = int, Output = int> {
				func index(key: int): int = this.base + key
			}
		}
		func demo(): int = Offset(base = 40)[2]
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("custom indexing should resolve through the ambient Index interface");
	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));
	assert_eq!(run_node(&js, "custom_index"), "42");
}

#[test]
fn ambient_index_bound_dispatches_generic_index_access() {
	let entry = r#"
		struct Offset(base: uint) {
			impl Index<Key = uint, Output = uint> {
				func index(key: uint): uint = this.base + key
			}
		}
		func get<T: Index<Key = uint, Output = uint>>(value: T): uint = value[2]
		func demo(): uint = get(Offset(base = 40u))
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("generic indexing should dispatch through the ambient Index bound");
	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));
	assert_eq!(run_node(&js, "generic_custom_index"), "42");
}

#[test]
fn ambient_index_bound_accepts_a_builtin_list() {
	let entry = r#"
		func get<T: Index<Key = uint, Output = int>>(value: T): int = value[1]
		func demo(): int = get(#[40, 42])
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("a list should satisfy the ambient Index bound");
	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));
	assert_eq!(run_node(&js, "generic_list_index"), "42");
}

/* Removed after immutable destination migration; assignment-based iteration is frozen elsewhere.
#[test]
fn real_std_set_iterates_its_keys() {
	let entry = r#""#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &nymph_compiler::embedded_std_provider)
		.expect("the real std Set should implement Iterable");
	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));
	assert_eq!(run_node(&js, "set_iterable"), "6");
}

*/
#[test]
fn persistent_map_updates_preserve_old_aliases() {
	let entry = r#"
		func map_aliases(): #(uint, uint, uint) = {
			let original = #{1: 10}
			let extended = original.inserted(2, 20)
			let removed = extended.removed(1)
			#(original.keys().length(), extended.keys().length(), removed.keys().length())
		}
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &nymph_compiler::embedded_std_provider)
		.expect("persistent map updates should link");
	let map_aliases = compiled.entry_symbol("map_aliases");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({map_aliases}().v.map(value => value.v).join(' '));\n"
	));
	assert_eq!(run_node(&js, "persistent_map_aliases"), "1 2 1");
}

#[test]
fn boxed_collection_intrinsics_preserve_value_semantics_and_nested_shapes() {
	let entry = r#"
		struct Key(id: int) {
			impl Equals<Other = Key> {
				func equals(other: Key): boolean = true
			}
			impl Hash {
				func hash(): int = 0
			}
		}

		func custom_contains(): boolean = #[Key(id = 1)].contains(Key(id = 2))
		func custom_distinct_len(): uint = #[Key(id = 1), Key(id = 2)].distinct().length()
		func nested_chunk(): int = #[1, 2, 3].chunked(2)[1][0]
		func entries_key(): int = #{7: 70}.entries()[0][0]
		func merge_right_wins(): int = #{1: 10}.plus(#{1: 99})[1]
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("boxed collection intrinsics should compile through the ambient prelude");
	let calls = [
		"custom_contains",
		"custom_distinct_len",
		"nested_chunk",
		"entries_key",
		"merge_right_wins",
	]
	.map(|name| compiled.entry_symbol(name));
	let mut js = compiled.js;
	for call in &calls {
		js.push_str(&format!("\nconsole.log({call}().v);\n"));
	}
	assert_eq!(run_node(&js, "boxed_collections"), "true\n1\n3\n7\n99");
}

#[test]
fn nominal_equality_operators_use_explicit_equals() {
	let entry = r#"
		struct AlwaysEqual(x: int)
		impl Equals<Other = AlwaysEqual> for AlwaysEqual {
			func equals(other: AlwaysEqual): boolean = true
		}
		func generic_same<T: Equals<Other = T>>(left: T, right: T): boolean = left == right
		func generic_different<T: Equals<Other = T>>(left: T, right: T): boolean = left != right
		func generic_custom_same(): boolean = generic_same(AlwaysEqual(x = 1), AlwaysEqual(x = 2))
		func generic_custom_different(): boolean = generic_different(AlwaysEqual(x = 1), AlwaysEqual(x = 2))
		func explicit_custom_same(): boolean = AlwaysEqual(x = 1).equals(AlwaysEqual(x = 2))
		func explicit_custom_different(): boolean = AlwaysEqual(x = 1).not_equals(AlwaysEqual(x = 2))
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("an explicit Equals implementation should compile for structs");
	let generic_custom_same = compiled.entry_symbol("generic_custom_same");
	let generic_custom_different = compiled.entry_symbol("generic_custom_different");
	let explicit_custom_same = compiled.entry_symbol("explicit_custom_same");
	let explicit_custom_different = compiled.entry_symbol("explicit_custom_different");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({generic_custom_same}().v);\n"));
	js.push_str(&format!("console.log({generic_custom_different}().v);\n"));
	js.push_str(&format!("console.log({explicit_custom_same}().v);\n"));
	js.push_str(&format!("console.log({explicit_custom_different}().v);\n"));
	assert_eq!(run_node(&js, "explicit_equals"), "true\nfalse\ntrue\nfalse");
}

#[test]
fn equality_without_an_explicit_capability_fails_statically() {
	for (name, entry) in [
		(
			"nominal_equality",
			"struct Point(x: int)\nfunc same(): boolean = Point(x = 1) == Point(x = 1)\nfunc main(): void = {}",
		),
		(
			"unbounded_generic_equality",
			"func same<T>(left: T, right: T): boolean = left == right\nfunc main(): void = {}",
		),
	] {
		let load = only_entry("main", entry);
		let diagnostics = match compile_project_with_std("main", &load, &|_| None) {
			Ok(_) => panic!("{name} should fail static capability selection"),
			Err(diagnostics) => diagnostics,
		};
		assert!(!diagnostics.is_empty(), "{name} should report a diagnostic");
	}
}

#[test]
fn mixed_primitive_equals_method_matches_the_operator_fast_path() {
	let entry = r#"
		func method_equal(left: int, right: uint): boolean = left.equals(right)
		func reverse_method_equal(left: uint, right: int): boolean = left.equals(right)
		func method_not_equal(left: int, right: uint): boolean = left.not_equals(right)
		func reverse_method_not_equal(left: uint, right: int): boolean = left.not_equals(right)
		func operator_equal(left: int, right: uint): boolean = left == right
		func operator_not_equal(left: int, right: uint): boolean = left != right
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("mixed primitive Equals methods should link through payload equality");
	let method_equal = compiled.entry_symbol("method_equal");
	let reverse_method_equal = compiled.entry_symbol("reverse_method_equal");
	let method_not_equal = compiled.entry_symbol("method_not_equal");
	let reverse_method_not_equal = compiled.entry_symbol("reverse_method_not_equal");
	let operator_equal = compiled.entry_symbol("operator_equal");
	let operator_not_equal = compiled.entry_symbol("operator_not_equal");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({method_equal}(new NInt(1), new NUint(1)).v, {reverse_method_equal}(new NUint(1), new NInt(1)).v, {operator_equal}(new NInt(1), new NUint(1)).v);\n"
	));
	js.push_str(&format!(
		"console.log({method_not_equal}(new NInt(1), new NUint(2)).v, {reverse_method_not_equal}(new NUint(2), new NInt(1)).v, {operator_not_equal}(new NInt(1), new NUint(2)).v);\n"
	));
	assert_eq!(
		run_node(&js, "mixed_primitive_equals"),
		"true true true\ntrue true true"
	);
}

/* Removed after immutable destination migration; old explicit iterator and mutable for-loop semantics are frozen elsewhere.
#[test]
fn boxed_lists_and_maps_iterate_through_the_uniform_protocol() {
	let entry = r#"
		func encode_entries(entries: #[#(int, int)]): int = entries.iter().fold(0, |encoded, #(key, value)| encoded * 100 + key * 10 + value)
		func explicit_map_iteration(values: #{int: int}): int = encode_entries(values.entries())
		func for_map_iteration(values: #{int: int}): int = values.iter().fold(0, |encoded, #(key, value)| encoded * 100 + key * 10 + value)
		func list_sum(): int = #[1, 2, 3, 4].iter().fold(0, |total, value| total + value)
		func map_sum(): int = #{1: 10, 2: 20}.iter().fold(0, |total, #(key, value)| total + key + value)
		func pattern_sum(): int = 90
		func map_iteration_contract(): #(int, int, int, int, int) = {
			let values = #{1: 2, 3: 4, 5: 6}
			let entries_sequence = encode_entries(values.entries())
			let explicit = explicit_map_iteration(values)
			let first_for = for_map_iteration(values)
			let second_for = for_map_iteration(values)
			#(entries_sequence, explicit, first_for, second_for, 1)
		}
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("boxed collections should satisfy the ambient iteration protocols");
	let list_sum = compiled.entry_symbol("list_sum");
	let map_sum = compiled.entry_symbol("map_sum");
	let pattern_sum = compiled.entry_symbol("pattern_sum");
	let map_iteration_contract = compiled.entry_symbol("map_iteration_contract");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({list_sum}().v, {map_sum}().v, {pattern_sum}().v);\n\
		 console.log({map_iteration_contract}().v.map(value => value.v).join(' '));\n"
	));
	let output = run_node(&js, "boxed_iteration");
	let mut lines = output.lines();
	assert_eq!(lines.next(), Some("10 33 90"));
	let contract: Vec<_> = lines.next().unwrap().split_whitespace().collect();
	assert_eq!(contract.len(), 5);
	assert_eq!(contract[0], contract[1], "entries and explicit iter differ");
	assert_eq!(contract[1], contract[2], "explicit iter and for differ");
	assert_eq!(
		contract[2], contract[3],
		"unchanged map iteration is unstable"
	);
	assert_eq!(contract[4], "1", "for source expression ran more than once");
}

/// The ambient `string` methods (now `core`, linked to `string.ts`): a program
/// calls several on a plain string literal WITH NO IMPORT, and they compile,
/// bundle, and run under Node — the primitive-methods-just-work payoff.
*/
#[test]
fn ambient_string_methods_link_and_run() {
	let entry = "func demo(): string = {\n\
		\tlet s = \"Hello\"\n\
		\ts.to_upper().concat(\" \").concat(s.reversed())\n\
		}\n\
		func main(): void = {}\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("ambient string methods should link and lower with no import");

	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));

	assert_eq!(
		run_node(&js, "string_methods"),
		"HELLO olleH",
		"expected `\"Hello\".to_upper().concat(\" \").concat(\"Hello\".reversed())`"
	);
}

#[test]
fn ambient_range_contains_dispatches_generic_comparison_defaults() {
	let entry = r#"
		func in_range(x: int): boolean = {
			let range = Range(start = 0, end = 5)
			range.contains(x)
		}
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None).expect(
		"ambient integral range containment should compile through generic comparison defaults",
	);

	let call = compiled.entry_symbol("in_range");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({call}(new NInt(3)).v, {call}(new NInt(8)).v);\n"
	));
	assert_eq!(run_node(&js, "range_contains"), "true false");
}

#[test]
fn ambient_string_convenience_methods_are_nymph_composition() {
	let entry = r#"
		func first_present(): char = "abc".first() ?? 'z'
		func first_empty(): char = "".first() ?? 'z'
		func last_present(): char = "abc".last() ?? 'z'
		func last_empty(): char = "".last() ?? 'z'
		func drop_middle(): string = "abcd".drop(2u)
		func drop_past_end(): string = "abcd".drop(9u)
		func take_middle(): string = "abcd".take(2u)
		func take_past_end(): string = "abcd".take(9u)
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("ambient string convenience methods should compile from Nymph bodies");

	assert!(
		compiled
			.js
			.contains("Array.from($_this.v)[nymphHostIndex(i)]")
			&& compiled
				.js
				.contains(".slice(nymphHostIndex(start), nymphHostIndex(end)).join"),
		"expected first/last/drop/take to compose the char_at and substring primitives:\n{}",
		compiled.js
	);

	let calls = [
		"first_present",
		"first_empty",
		"last_present",
		"last_empty",
		"drop_middle",
		"drop_past_end",
		"take_middle",
		"take_past_end",
	]
	.map(|name| compiled.entry_symbol(name));
	let mut js = compiled.js;
	for call in calls {
		js.push_str(&format!("\nconsole.log({call}().v);\n"));
	}
	assert_eq!(
		run_node(&js, "string_nymph_composition"),
		"a\nz\nc\nz\ncd\n\nab\nabcd"
	);
}

#[test]
fn ambient_string_intrinsics_preserve_the_boxed_abi() {
	with_compiler_stack(|| {
		let entry = r#"
		func length(): uint = "abc".length()
		func contains(): boolean = "abc".contains("b")
		func slice(): string = "abcd".substring(1u, 3u)
		func index(): uint = "abcd".index_of("c") ?? 99u
		func missing_index(): uint = "abcd".index_of("z") ?? 99u
		func character(): char = "abcd".char_at(2u) ?? 'z'
		func split_item(): string = "a,b".split(",")[1]
		func chars_item(): char = "abc".chars()[1]
		func main(): void = {}
	"#;
		let load = only_entry("main", entry);
		let compiled = compile_project_with_std("main", &load, &|_| None)
			.expect("ambient string intrinsics should compile with boxed inputs and outputs");
		let calls = [
			"length",
			"contains",
			"slice",
			"index",
			"missing_index",
			"character",
			"split_item",
			"chars_item",
		]
		.map(|name| compiled.entry_symbol(name));
		let mut js = compiled.js;
		for call in calls {
			js.push_str(&format!("\nconsole.log({call}().v);\n"));
		}
		assert_eq!(
			run_node(&js, "string_boxed_abi"),
			"3\ntrue\nbc\n2\n99\nc\nb\nb"
		);
	});
}

/// Inventory of every language-level string API whose argument or result is a
/// position/extent: `length`, `char_at`, `substring`, `index_of`,
/// `last_index_of`, `pad_start`, `pad_end`, and the Nymph-derived `first`,
/// `last`, `drop`, and `take`. Astral scalars count once, while a combining
/// mark remains a separate code point.
#[test]
fn ambient_string_offsets_and_widths_count_unicode_code_points() {
	let entry = r#"
		func length(): uint = "A😀éB".length()
		func empty_length(): uint = "".length()
		func astral_char(): char = "A😀éB".char_at(1u) ?? 'z'
		func combining_char(): char = "A😀éB".char_at(3u) ?? 'z'
		func invalid_char(): char = "A😀éB".char_at(5u) ?? 'z'
		func slice_mixed(): string = "A😀éB".substring(1u, 4u)
		func slice_empty(): string = "A😀éB".substring(3u, 3u)
		func slice_past_end(): string = "A😀éB".substring(4u, 99u)
		func slice_reversed_bounds(): string = "A😀éB".substring(4u, 2u)
		func index_astral(): uint = "A😀éB".index_of("😀") ?? 99u
		func index_combining_sequence(): uint = "A😀éB".index_of("é") ?? 99u
		func index_missing(): uint = "A😀éB".index_of("x") ?? 99u
		func last_index_astral(): uint = "😀x😀".last_index_of("😀") ?? 99u
		func last_index_empty(): uint = "A😀éB".last_index_of("") ?? 99u
		func first(): char = "😀x".first() ?? 'z'
		func first_empty(): char = "".first() ?? 'z'
		func last(): char = "x😀".last() ?? 'z'
		func last_empty(): char = "".last() ?? 'z'
		func drop(): string = "A😀éB".drop(2u)
		func drop_past_end(): string = "A😀éB".drop(99u)
		func take(): string = "A😀éB".take(4u)
		func take_past_end(): string = "A😀éB".take(99u)
		func pad_start_astral(): string = "😀".pad_start(3u, '🚀')
		func pad_end_astral(): string = "😀".pad_end(3u, '🚀')
		func pad_combining(): string = "é".pad_start(3u, '.')
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("Unicode string offset inventory should compile");
	let calls = [
		"length",
		"empty_length",
		"astral_char",
		"combining_char",
		"invalid_char",
		"slice_mixed",
		"slice_empty",
		"slice_past_end",
		"slice_reversed_bounds",
		"index_astral",
		"index_combining_sequence",
		"index_missing",
		"last_index_astral",
		"last_index_empty",
		"first",
		"first_empty",
		"last",
		"last_empty",
		"drop",
		"drop_past_end",
		"take",
		"take_past_end",
		"pad_start_astral",
		"pad_end_astral",
		"pad_combining",
	]
	.map(|name| compiled.entry_symbol(name));
	let mut js = compiled.js;
	for call in calls {
		js.push_str(&format!(
			"\n{{ const value = {call}().v; console.log(typeof value === 'bigint' ? String(value) : JSON.stringify(value)); }}\n"
		));
	}
	assert_eq!(
		run_node(&js, "string_code_point_offsets"),
		"5\n0\n\"😀\"\n\"́\"\n\"z\"\n\"😀é\"\n\"\"\n\"B\"\n\"\"\n1\n2\n99\n2\n5\n\"😀\"\n\"z\"\n\"😀\"\n\"z\"\n\"éB\"\n\"\"\n\"A😀é\"\n\"A😀éB\"\n\"🚀🚀😀\"\n\"😀🚀🚀\"\n\".é\""
	);
}

#[test]
fn canonical_option_unions_method_demands_across_modules() {
	let modules = std::collections::HashMap::from([
		(
			"main",
			r#"
				import ./unwrap with (unwrap_value)
				import ./inspect with (is_absent)
				func unwrapped(): int = unwrap_value(Some(value = 7))
				func absent(): boolean = is_absent(None)
				func main(): void = {}
			"#,
		),
		(
			"unwrap",
			"func unwrap_value(value: Option<int>): int = value.unwrap(-1)",
		),
		(
			"inspect",
			"func is_absent(value: Option<int>): boolean = value.is_none()",
		),
	]);
	let load = |key: &str| modules.get(key).map(|source| (*source).to_string());
	let sources = compile_project_module_sources_with_std("main", &load, &|_| None)
		.expect("all project Option demands should assemble");
	assert!(sources.contains_key("@nymph/runtime/option"));
	assert_eq!(
		sources
			.keys()
			.filter(|key| key.as_str() == "@nymph/runtime/option")
			.count(),
		1
	);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("all project Option demands should merge into one canonical module");
	let unwrapped = compiled.entry_symbol("unwrapped");
	let absent = compiled.entry_symbol("absent");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({unwrapped}().v, {absent}().v);\n"));
	assert_eq!(run_node(&js, "canonical_option_union"), "7 true");
}

#[test]
fn canonical_option_deduplicates_alpha_equivalent_method_demands() {
	let modules = std::collections::HashMap::from([
		(
			"main",
			"import ./a with (use_a)\nimport ./b with (use_b)\n\
			 func first(): int = use_a(Some(value = 1))\n\
			 func second(): int = use_b(Some(value = 2))\nfunc main(): void = {}",
		),
		(
			"a",
			"func noise(default: int): int = default\n\
			 func use_a(value: Option<int>): int = value.unwrap(0)",
		),
		("b", "func use_b(value: Option<int>): int = value.unwrap(0)"),
	]);
	let load = |key: &str| modules.get(key).map(|source| (*source).to_string());
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("duplicate demands must not compare consumer-local generated names");
	let first = compiled.entry_symbol("first");
	let second = compiled.entry_symbol("second");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({first}().v, {second}().v);\n"));
	assert_eq!(run_node(&js, "canonical_option_dedup"), "1 2");
}

#[test]
fn option_returning_intrinsic_resolves_without_a_value_level_option_operation() {
	let entry = r#"
		func character(): Option<char> = "x".char_at(0u)
		func source_character(): Option<char> = Some(value = 'x')
		func unwrapped_character(): char = "x".char_at(0u).unwrap('z')
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let sources = compile_project_module_sources_with_std("main", &load, &|_| None)
		.expect("an Option-returning intrinsic must assemble its canonical module");
	assert!(sources.contains_key("@nymph/runtime/option"));
	assert_eq!(
		sources
			.keys()
			.filter(|key| key.as_str() == "@nymph/runtime/option")
			.count(),
		1
	);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("an Option-returning intrinsic must always resolve its canonical module");
	let character = compiled.entry_symbol("character");
	let source_character = compiled.entry_symbol("source_character");
	let unwrapped_character = compiled.entry_symbol("unwrapped_character");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log(\
		 Object.getPrototypeOf({character}()) === Object.getPrototypeOf({source_character}()), \
		 {unwrapped_character}().v);\n"
	));
	assert_eq!(run_node(&js, "option_intrinsic_no_operation"), "true x");
}

#[test]
fn ambient_order_has_one_factory_and_cross_module_identity() {
	let modules = std::collections::HashMap::from([
		(
			"main",
			"import ./a with (less)\nimport ./b with (also_less, equal)\n\
			 func first(): Order = less()\n\
			 func second(): Order = also_less()\n\
			 func third(): Order = equal()\n\
			 func main(): void = {}",
		),
		("a", "public func less(): Order = Order.LessThan"),
		(
			"b",
			"public func also_less(): Order = Order.LessThan\n\
			 public func equal(): Order = Order.Equal",
		),
	]);
	let load = |key: &str| modules.get(key).map(|source| (*source).to_string());
	let sources = compile_project_module_sources_with_std("main", &load, &|_| None)
		.expect("ambient Order should assemble one canonical owner");
	assert!(sources.contains_key("@nymph/runtime/ops"));
	assert_eq!(
		sources
			.keys()
			.filter(|key| key.as_str() == "@nymph/runtime/ops")
			.count(),
		1
	);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("ambient Order should have one canonical std/ops owner");

	let first = compiled.entry_symbol("first");
	let second = compiled.entry_symbol("second");
	let third = compiled.entry_symbol("third");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconst a = {first}(); const b = {second}(); const c = {third}();\n\
		 console.log(a === b, Object.getPrototypeOf(a) === Object.getPrototypeOf(c));\n"
	));
	assert_eq!(run_node(&js, "canonical_order_identity"), "true true");
}

#[test]
fn ambient_list_iterator_has_one_class_and_cross_module_identity() {
	let modules = std::collections::HashMap::from([
		(
			"main",
			"import ./a with (first_range)\nimport ./b with (second_range)\n\
			 func first(): ListIter<int> = first_range()\n\
			 func second(): ListIter<int> = second_range()\n\
			 func main(): void = {}",
		),
		(
			"a",
			"public func first_range(): ListIter<int> = #[1].iter()",
		),
		(
			"b",
			"public func second_range(): ListIter<int> = #[2].iter()",
		),
	]);
	let load = |key: &str| modules.get(key).map(|source| (*source).to_string());
	let sources = compile_project_module_sources_with_std("main", &load, &|_| None)
		.expect("ambient ListIter should assemble one canonical owner");
	assert!(sources.contains_key("@nymph/runtime/iter/iterable"));
	assert_eq!(
		sources
			.keys()
			.filter(|key| key.as_str() == "@nymph/runtime/iter/iterable")
			.count(),
		1
	);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("ambient ListIter should have one canonical std/iter/iterable owner");

	let first = compiled.entry_symbol("first");
	let second = compiled.entry_symbol("second");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconst a = {first}(); const b = {second}();\n\
		 console.log(a.constructor === b.constructor, Object.getPrototypeOf(a) === Object.getPrototypeOf(b));\n"
	));
	assert_eq!(run_node(&js, "canonical_range_identity"), "true true");
}

#[test]
fn canonical_option_and_result_are_distinct_cross_importing_owners() {
	let entry = r#"
		func option_to_result(): int = Some(value = 7).ok_or("missing").unwrap(-1)
		func result_to_option(): int = Result.Ok(value = 9).ok().unwrap(-1)
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let sources = compile_project_module_sources_with_std("main", &load, &|_| None)
		.expect("canonical Option and Result sources should assemble");
	for owner in ["@nymph/runtime/option", "@nymph/runtime/result"] {
		assert!(sources.contains_key(owner));
		assert_eq!(
			sources.keys().filter(|key| key.as_str() == owner).count(),
			1
		);
	}
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("canonical Option and Result should retain reciprocal conversion dependencies");
	let option_to_result = compiled.entry_symbol("option_to_result");
	let result_to_option = compiled.entry_symbol("result_to_option");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({option_to_result}().v, {result_to_option}().v);\n"
	));
	assert_eq!(run_node(&js, "canonical_option_result_dependency"), "7 9");
}

/// `import std/io` resolves through the EMBEDDED std provider (the one the CLI
/// wires), so `println` is available in a real build — not just when a test
/// hands in a synthetic provider. Compiles, bundles, and runs under Node.
#[test]
fn import_std_io_resolves_via_embedded_provider_and_runs() {
	let entry = "import std/io with (println)\n\
		func main(): void = println(\"hi from std/io\")\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &nymph_compiler::embedded_std_provider)
		.expect("`import std/io` should resolve via the embedded std provider");

	let js = format!("{}\n{}();\n", compiled.js, compiled.entry_main);
	assert_eq!(run_node(&js, "std_io"), "hi from std/io");
}

/* Removed after immutable destination migration; compound assignment evaluation is frozen elsewhere.
#[test]
fn exact_power_matrix_compiles_without_native_exponentiation_and_runs() {
	with_compiler_stack(exact_power_matrix_body);
}

fn exact_power_matrix_body() {
	let entry = r#"
		import std/math/complex with (Complex)
		func int_uint(): int = 2 ** 10u
		func uint_uint(): uint = 3u ** 4u
		func float_uint(): float = 1.5 ** 3u
		func int_int(): float = 2 ** -3
		func uint_int(): float = 4u ** -2
		func float_int(): float = (0.0 - 2.0) ** 3
		func int_float(): Complex = (0 - 4) ** 0.5
		func uint_float(): Complex = 9u ** 0.5
		func float_float(): Complex = (0.0 - 8.0) ** 0.3333333333333333
		func complex_uint(): Complex = Complex(real = 1.0, imaginary = 1.0) ** 8u
		func complex_int(): Complex = Complex(real = 0.0, imaginary = 2.0) ** -2
		func complex_float(): Complex = Complex(real = 3.0, imaginary = 4.0) ** 2.0
		func generic_uint_power<T: Power<Other = uint, Output = T>>(base: T, exponent: uint): T =
		  base ** exponent
		func generic_int_uint(): int = generic_uint_power(5, 3u)
		func generic_float_uint(): float = generic_uint_power(2.5, 2u)
		func large(): int = 2 ** 40u
		struct Base(value: int)
		struct Exponent(value: int)
		impl Power<Other = Exponent, Output = int> for Base {
		  func power(other: Exponent): int = this.value + other.value
		}
		func overridden(): int = Base(value = 20) ** Exponent(value = 22)
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &nymph_compiler::embedded_std_provider)
		.expect("every accepted power-matrix cell should compile");
	assert!(
		!compiled.js.contains(" ** "),
		"power must dispatch to the selected Nymph implementation, never raw JavaScript `**`:\n{}",
		compiled.js
	);

	let symbols: Vec<_> = [
		"int_uint",
		"uint_uint",
		"float_uint",
		"int_int",
		"uint_int",
		"float_int",
		"int_float",
		"uint_float",
		"float_float",
		"complex_uint",
		"complex_int",
		"complex_float",
		"generic_int_uint",
		"generic_float_uint",
		"large",
		"overridden",
	]
	.into_iter()
	.map(|name| compiled.entry_symbol(name))
	.collect();
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({}().v, {}().v, {}().v, {}().v, {}().v, {}().v);\n\
		 const a = {}(); const b = {}(); const c = {}();\n\
		 const d = {}(); const e = {}(); const f = {}();\n\
		 console.log(a.real.v, a.imaginary.v, b.real.v, b.imaginary.v);\n\
		 console.log(c.real.v, c.imaginary.v, d.real.v, d.imaginary.v);\n\
		 console.log(e.real.v, e.imaginary.v, f.real.v, f.imaginary.v);\n\
		 console.log({}().v, {}().v, {}().v);\n\
		 console.log({}().v, {}().v, {}().v, a.constructor === b.constructor);\n",
		symbols[0],
		symbols[1],
		symbols[2],
		symbols[3],
		symbols[4],
		symbols[5],
		symbols[6],
		symbols[7],
		symbols[8],
		symbols[9],
		symbols[10],
		symbols[11],
		symbols[12],
		symbols[13],
		symbols[14],
		symbols[15],
		symbols[16],
		symbols[17],
	));
	let output = run_node(&js, "power_matrix");
	let lines: Vec<_> = output.lines().collect();
	assert_eq!(lines[0], "1024 81 3.375 0.125 0.0625 -8");
	let values: Vec<f64> = lines[1..=3]
		.iter()
		.flat_map(|line| line.split_whitespace())
		.map(|value| value.parse().unwrap())
		.collect();
	assert!(values[0].abs() < 1e-12 && (values[1] - 2.0).abs() < 1e-12);
	assert!((values[2] - 3.0).abs() < 1e-12 && values[3].abs() < 1e-12);
	assert!((values[4] - 1.0).abs() < 1e-9 && (values[5] - 3.0_f64.sqrt()).abs() < 1e-9);
	assert!((values[6] - 16.0).abs() < 1e-12 && values[7].abs() < 1e-12);
	assert!((values[8] + 0.25).abs() < 1e-12 && values[9].abs() < 1e-12);
	assert!((values[10] + 7.0).abs() < 1e-12 && (values[11] - 24.0).abs() < 1e-12);
	assert_eq!(lines[4], "125 6.25 81");
	assert_eq!(lines[5], "1099511627776 12 42 true");
}

*/
#[test]
fn power_zero_signed_zero_and_ieee_edges_follow_the_contract() {
	with_compiler_stack(power_zero_signed_zero_and_ieee_edges_body);
}

fn power_zero_signed_zero_and_ieee_edges_body() {
	let entry = r#"
		import std/math/complex with (Complex)
		func zeros(): #(int, uint, float, Complex, Complex) = #(
		  0 ** 0u,
		  0u ** 7u,
		  0.0 ** 5,
		  0 ** 0.0,
		  Complex.new(0.0, 0.0) ** 2.5,
		)
		func negative_zero_odd(base: float): Complex = base ** 3.0
		func negative_zero_even(base: float): Complex = base ** 2.0
		func complex_negative_zero_odd(base: float): Complex =
		  Complex.new(base, 0.0) ** 1.0
		func complex_negative_zero_even(base: float): Complex =
		  Complex.new(base, 0.0) ** 2.0
		func huge_sqrt(): Complex = Complex.new(10.0 ** 200u, 0.0) ** 0.5
		func tiny_sqrt(): Complex = Complex.new(1.0 / (10.0 ** 200u), 0.0) ** 0.5
		func huge_reciprocal(): Complex = Complex.new(10.0 ** 200u, 0.0) ** -1
		func infinite_reciprocal(): Complex = Complex.new(1.0 / 0.0, 0.0) ** -1
		func infinite_identity_uint(): Complex = Complex.new(1.0 / 0.0, 0.0) ** 1u
		func infinite_identity_float(): Complex = Complex.new(1.0 / 0.0, 0.0) ** 1.0
		func signed_imaginary_identity(imaginary: float): Complex =
		  Complex.new(0.0, imaginary) ** 1.0
		func signed_imaginary_fractional(imaginary: float): Complex =
		  Complex.new(4.0, imaginary) ** 0.5
		func signed_imaginary_negative_fractional(imaginary: float): Complex =
		  Complex.new(4.0, imaginary) ** -0.5
		func nonnegative_infinite_power(): Complex = Complex.new(2.0, 0.0) ** (1.0 / 0.0)
		func infinite_fractional_power(): Complex = Complex.new(1.0 / 0.0, 0.0) ** 0.5
		func nan_power(): Complex = 2.0 ** ((-1.0).ln())
		func infinity_power(): Complex = 2.0 ** (1.0 / 0.0)
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &nymph_compiler::embedded_std_provider)
		.expect("power zero and IEEE edge fixture should compile");
	let symbols: Vec<_> = [
		"zeros",
		"negative_zero_odd",
		"negative_zero_even",
		"complex_negative_zero_odd",
		"complex_negative_zero_even",
		"huge_sqrt",
		"tiny_sqrt",
		"huge_reciprocal",
		"infinite_reciprocal",
		"infinite_identity_uint",
		"infinite_identity_float",
		"signed_imaginary_identity",
		"signed_imaginary_fractional",
		"signed_imaginary_negative_fractional",
		"nonnegative_infinite_power",
		"infinite_fractional_power",
		"nan_power",
		"infinity_power",
	]
	.into_iter()
	.map(|name| compiled.entry_symbol(name))
	.collect();
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconst z = {}(); const odd = {}(new NFloat(-0)); const even = {}(new NFloat(-0));\n\
		 const complexOdd = {}(new NFloat(-0)); const complexEven = {}(new NFloat(-0));\n\
		 const huge = {}(); const tiny = {}(); const reciprocal = {}(); const infiniteReciprocal = {}();\n\
		 const identityUint = {}(); const identityFloat = {}(); const signedImaginary = {}(new NFloat(-0));\n\
		 const fractionalNegativeZero = {}(new NFloat(-0)); const negativeFractionalPositiveZero = {}(new NFloat(0));\n\
		 const nonnegativeInfinity = {}(); const fractionalInfinity = {}(); const nan = {}(); const inf = {}();\n\
		 console.log(z.v[0].v, z.v[1].v, z.v[2].v, z.v[3].real.v, z.v[3].imaginary.v, z.v[4].real.v, z.v[4].imaginary.v);\n\
		 console.log(Object.is(odd.real.v, -0), Object.is(even.real.v, 0), Object.is(complexOdd.real.v, -0), Object.is(complexEven.real.v, 0));\n\
		 console.log(huge.real.v, tiny.real.v, reciprocal.real.v, Object.is(infiniteReciprocal.real.v, 0), Number.isNaN(infiniteReciprocal.real.v));\n\
		 console.log(identityUint.real.v, identityUint.imaginary.v, identityFloat.real.v, identityFloat.imaginary.v, Object.is(signedImaginary.imaginary.v, -0));\n\
		 console.log(fractionalNegativeZero.real.v, Object.is(fractionalNegativeZero.imaginary.v, -0), negativeFractionalPositiveZero.real.v, Object.is(negativeFractionalPositiveZero.imaginary.v, -0));\n\
		 console.log(nonnegativeInfinity.real.v, nonnegativeInfinity.imaginary.v, fractionalInfinity.real.v, fractionalInfinity.imaginary.v);\n\
		 console.log(Number.isNaN(nan.real.v), inf.real.v, inf.imaginary.v);\n",
		symbols[0],
		symbols[1],
		symbols[2],
		symbols[3],
		symbols[4],
		symbols[5],
		symbols[6],
		symbols[7],
		symbols[8],
		symbols[9],
		symbols[10],
		symbols[11],
		symbols[12],
		symbols[13],
		symbols[14],
		symbols[15],
		symbols[16],
		symbols[17],
	));
	let output = run_node(&js, "power_edges");
	let lines: Vec<_> = output.lines().collect();
	assert_eq!(lines[0], "1 0 0 1 0 0 0");
	assert_eq!(lines[1], "true true true true");
	let finite: Vec<f64> = lines[2]
		.split_whitespace()
		.take(3)
		.map(|value| value.parse().unwrap())
		.collect();
	assert!((finite[0] / 1e100 - 1.0).abs() < 1e-12);
	assert!((finite[1] / 1e-100 - 1.0).abs() < 1e-12);
	assert!((finite[2] / 1e-200 - 1.0).abs() < 1e-12);
	assert!(lines[2].ends_with("true false"));
	assert_eq!(lines[3], "Infinity 0 Infinity 0 true");
	assert_eq!(lines[4], "2 true 0.5 true");
	assert_eq!(lines[5], "Infinity 0 Infinity 0");
	assert_eq!(lines[6], "true Infinity 0");
}

#[test]
fn power_principal_branch_large_integral_and_call_paths_run() {
	with_compiler_stack(power_principal_branch_large_integral_and_call_paths_body);
}

fn power_principal_branch_large_integral_and_call_paths_body() {
	let entry = r#"
			import std/math/complex with (Complex)
			func zero_zeros(): #(int, uint, float, float, Complex, Complex, Complex) = #(
			  0 ** 0u,
			  0u ** 0u,
			  0.0 ** 0u,
			  0.0 ** 0,
			  0.0 ** 0.0,
			  Complex.new(0.0, 0.0) ** 0u,
			  Complex.new(0.0, 0.0) ** 0,
			)
			func negative_axis(imaginary: float): Complex =
			  Complex.new(-4.0, imaginary) ** 0.5
			func imaginary_axis(): Complex = Complex.new(0.0, 2.0) ** 0.5
			func integral_negative_base(): Complex = (-2.0) ** 3.0
			func minimum_scalar(): float = (-1.0) ** min_int
			func minimum_complex(): Complex = Complex.new(0.0, 1.0) ** min_int
			func huge_integral_float(): Complex = (-1.0) ** 9007199254740992.0
			func direct_method(): Complex = Complex.new(-4.0, 0.0).power(0.5)
			struct StoredBase(value: int)
			impl Power<Other = uint, Output = int> for StoredBase {
			  func power(other: uint): int = this.value + (other as int)
			}
			func stored_method(): int = {
			  let method = StoredBase(value = 40).power
			  method(2u)
			}
			func associated(): int = 2 ** 3u ** 2u
			func main(): void = {}
		"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &nymph_compiler::embedded_std_provider)
		.expect("adversarial power fixture should compile");
	let symbols: Vec<_> = [
		"zero_zeros",
		"negative_axis",
		"imaginary_axis",
		"integral_negative_base",
		"minimum_scalar",
		"minimum_complex",
		"huge_integral_float",
		"direct_method",
		"stored_method",
		"associated",
	]
	.into_iter()
	.map(|name| compiled.entry_symbol(name))
	.collect();
	let mut js = compiled.js;
	js.push_str(&format!(
			"\nconst zeros = {}(); const above = {}(new NFloat(0)); const below = {}(new NFloat(-0));\n\
			 const imaginary = {}(); const integral = {}(); const minimum = {}(); const huge = {}();\n\
			 const direct = {}();\n\
			 console.log(zeros.v.map((value) => value.real ? `${{value.real.v}},${{value.imaginary.v}}` : value.v).join(';'));\n\
			 console.log(above.real.v, above.imaginary.v, below.real.v, below.imaginary.v);\n\
			 console.log(imaginary.real.v, imaginary.imaginary.v, integral.real.v, integral.imaginary.v);\n\
			 console.log({}().v, minimum.real.v, minimum.imaginary.v, huge.real.v, huge.imaginary.v);\n\
			 console.log(direct.real.v, direct.imaginary.v, {}().v, {}().v);\n",
			symbols[0],
			symbols[1],
			symbols[1],
			symbols[2],
			symbols[3],
			symbols[5],
			symbols[6],
			symbols[7],
			symbols[4],
			symbols[8],
			symbols[9],
		));
	let output = run_node(&js, "power_adversarial");
	let lines: Vec<_> = output.lines().collect();
	assert_eq!(lines[0], "1;1;1;1;1,0;1,0;1,0");
	let branch: Vec<f64> = lines[1]
		.split_whitespace()
		.map(|value| value.parse().unwrap())
		.collect();
	assert!(branch[0].abs() < 1e-12 && (branch[1] - 2.0).abs() < 1e-12);
	assert!(branch[2].abs() < 1e-12 && (branch[3] + 2.0).abs() < 1e-12);
	let axes: Vec<f64> = lines[2]
		.split_whitespace()
		.map(|value| value.parse().unwrap())
		.collect();
	assert!((axes[0] - 1.0).abs() < 1e-12 && (axes[1] - 1.0).abs() < 1e-12);
	assert_eq!(&axes[2..], &[-8.0, 0.0]);
	assert_eq!(lines[3], "1 1 0 1 0");
	let calls: Vec<f64> = lines[4]
		.split_whitespace()
		.map(|value| value.parse().unwrap())
		.collect();
	assert!(calls[0].abs() < 1e-12 && (calls[1] - 2.0).abs() < 1e-12);
	assert_eq!(calls[2], 42.0);
	assert_eq!(calls[3], 512.0);
}

#[test]
fn power_rejects_combinations_outside_the_exact_matrix() {
	with_compiler_stack(power_rejects_combinations_outside_the_exact_matrix_body);
}

fn power_rejects_combinations_outside_the_exact_matrix_body() {
	let entry = r#"
		import std/math/complex with (Complex)
		func complex_exponent(): Complex =
		  Complex.new(2.0, 0.0) ** Complex.new(2.0, 0.0)
		func boolean_exponent(): Complex = Complex.new(2.0, 0.0) ** true
		func boolean_base(): boolean = true ** 2u
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let diagnostics =
		match compile_project_with_std("main", &load, &nymph_compiler::embedded_std_provider) {
			Ok(_) => panic!("outside-matrix power combinations must not compile"),
			Err(diagnostics) => diagnostics,
		};
	assert_eq!(
		diagnostics
			.iter()
			.filter(|diagnostic| {
				diagnostic.diag.message.contains("no overload")
					|| diagnostic.diag.message.contains("not implemented")
			})
			.count(),
		3,
		"{diagnostics:#?}"
	);
}

#[test]
fn zero_to_negative_power_raises_the_runtime_domain_error() {
	with_compiler_stack(zero_to_negative_power_body);
}

fn zero_to_negative_power_body() {
	for (name, expression) in [
		("real_int", "0 ** -1"),
		("real_float", "0.0 ** -1.0"),
		("complex_int", "Complex.new(0.0, 0.0) ** -1"),
		("complex_float", "Complex.new(0.0, 0.0) ** -1.0"),
	] {
		let entry = format!(
			"import std/math/complex with (Complex)\nfunc fail() = {expression}\nfunc main(): void = {{}}"
		);
		let load = |key: &str| (key == "main").then(|| entry.clone());
		let compiled = compile_project_with_std("main", &load, &nymph_compiler::embedded_std_provider)
			.expect("zero-negative fixture should compile and fail only at runtime");
		let path = std::env::temp_dir().join(format!(
			"nymph_power_domain_{name}_{}.mjs",
			std::process::id()
		));
		std::fs::write(
			&path,
			format!("{}\n{}();\n", compiled.js, compiled.entry_symbol("fail")),
		)
		.unwrap();
		let output = std::process::Command::new("node")
			.arg(&path)
			.output()
			.unwrap();
		let _ = std::fs::remove_file(path);
		assert!(!output.status.success(), "{name} unexpectedly succeeded");
		let stderr = String::from_utf8_lossy(&output.stderr);
		assert!(
			stderr.contains("RangeError: zero cannot be raised to a negative power"),
			"{name}: {stderr}"
		);
	}
}
