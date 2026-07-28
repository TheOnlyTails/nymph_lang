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
//! slice's owned-files scope; see the on-disk equivalent's `run_node.rs`
//! coverage, `compile_against_real_stdlib`, which drives the REAL stdlib but
//! only through the bare, import-free `emit` harness — it can assert the
//! emitted SHAPE, never run it). This file is the one place the mechanism is
//! actually driven end-to-end through the bundle + Node.

use nymph_compiler::compile_project_with_std;
use nymph_compiler::project::compile_project_module_sources_with_std;

/// The synthetic `std/collections/list` module: JUST enough to exercise
/// linkage — `length`/`get` are `external(..)` (present in
/// `nymph_hir::linkage::REGISTRY` for a plain, non-`mut` `#[T]` receiver, so
/// they MUST materialize/link instead of loud-deferring), `is_empty` is real
/// Nymph source that calls `length` transitively (proving
/// `body_calls_unlinked_external`'s registry subtraction). `get` returns
/// `Option<T>` — `Option` itself needs no `import` here, it's ambient via the
/// `core` prelude every module (including a `std::`-keyed one, on its own
/// check/lower turn) is flattened against.
fn synth_std_provider(path: &str) -> Option<String> {
	(path == "collections/list").then(|| {
		"impl<T> #[T] {\n  \
			external(length) func length(): uint\n  \
			external(get) func get(i: uint): Option<T>\n  \
			func is_empty(): boolean = this.length() == 0\n\
		}\n"
			.to_string()
	})
}

/// The synthetic `std/collections/map` module (Gap 3, L3): just enough to
/// exercise the newly-linked map surface — `size`/`get`/`insert`/`remove` are
/// declared inside a `mut #{K:V}` impl, mirroring the real `map.nym` (whose
/// registry rows for these markers are keyed `Some("mut_map")` — the impl's
/// OWN mutability, per `inherent_self_type_tag`, not the call-site receiver);
/// `keys` sits in the non-mut `#{K:V}` impl (keyed `None`, unambiguous).
fn synth_std_map_provider(path: &str) -> Option<String> {
	(path == "collections/map").then(|| {
		"impl<K, V> mut #{K: V} {\n  \
			external(size) func size(): uint\n  \
			external(get) func get(key: K): Option<V>\n  \
			external(insert) func insert(key: K, value: V): boolean\n  \
			external(remove) func remove(key: K): Option<V>\n\
		}\n\
		impl<K, V> #{K: V} {\n  \
			external(keys) func keys(): #[K]\n\
		}\n"
			.to_string()
	})
}

fn only_entry(entry_key: &'static str, entry_src: &'static str) -> impl Fn(&str) -> Option<String> {
	move |key: &str| (key == entry_key).then(|| entry_src.to_string())
}

fn synth_std_math_provider(path: &str) -> Option<String> {
	(path == "math").then(|| "public external(max_float) let max_float: float\n".to_string())
}

fn run_node(js: &str, tag: &str) -> String {
	let dir = std::env::temp_dir();
	let path = dir.join(format!(
		"nymph_std_linkage_{tag}_{}.mjs",
		std::process::id()
	));
	std::fs::write(&path, js).unwrap();
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
#[test]
fn linked_map_get_insert_remove_and_keys_compile_bundle_and_run() {
	let entry = "import std/collections/map\n\
		func demo_get(): int = {\n\
		\tlet mut m = #{1: 10, 2: 20}\n\
		\tmatch (m.get(1)) {\n\
		\t\tSome(value) -> value,\n\
		\t\tNone -> -1,\n\
		\t}\n\
		}\n\
		func demo_get_missing(): int = {\n\
		\tlet mut m = #{1: 10}\n\
		\tmatch (m.get(9)) {\n\
		\t\tSome(value) -> value,\n\
		\t\tNone -> -1,\n\
		\t}\n\
		}\n\
		func demo_insert_size(): uint = {\n\
		\tlet mut m = #{1: 10, 2: 20}\n\
		\tm.insert(3, 30)\n\
		\tm.size()\n\
		}\n\
		func demo_remove(): int = {\n\
		\tlet mut m = #{1: 10, 2: 20}\n\
		\tmatch (m.remove(1)) {\n\
		\t\tSome(value) -> value,\n\
		\t\tNone -> -1,\n\
		\t}\n\
		}\n\
		func demo_keys_first(): int = {\n\
		\tlet m = #{7: 70}\n\
		\tm.keys()[0]\n\
		}\n\
		func main(): void = {}\n";
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

/// The map merge/to_string linkage proof, driven against the AMBIENT map (map
/// is now part of the `core` prelude, so no `import`/synthetic std module is
/// needed). `merge` (`Plus<Other=self,Output=self> for #{K:V}`) is a STRUCTURAL
/// `ImplFor` block — the shape that used to unconditionally panic in
/// `push_impl_for_methods` — so lowering the ambient map's own declarations
/// exercises it; once linked, `.plus()` actually compiles, bundles, and runs
/// under Node. `to_string` (`Into<string> for #{K:V}`'s `into`) is linked in
/// the registry the same way, but there is today no checker-accepted call site
/// that reaches it for a STRUCTURAL receiver (a bare `m.into()` is a checker
/// error — a separate, pre-existing gap), so this test only asserts the row is
/// linked and lowering never panics on it.
#[test]
fn linked_map_merge_and_to_string_compile_bundle_and_run() {
	// `map` is now ambient (part of the `core` prelude), so no `import` is
	// needed — the map literals' `.plus()` (merge, via `Plus for #{K:V}`) and
	// `.size()` link against the registry directly, no synthetic std module.
	let entry = "func demo_merge_size(): uint = {\n\
		\tlet mut a = #{1: 10}\n\
		\tlet b = #{2: 20}\n\
		\tlet merged = a.plus(b)\n\
		\tmerged.size()\n\
		}\n\
		func main(): void = {}\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &|_| None).expect(
		"expected the ambient map's `.plus()` (merge, a structural `ImplFor` block) and \
		 `.size()` to link and lower cleanly",
	);

	assert!(
		nymph_hir::linkage::lookup("to_string", Some("map")).is_some(),
		"expected `to_string` to still be linked for a `map` receiver (proving this fix \
		 didn't accidentally regress the registry row itself, only the panic while lowering \
		 its structural `ImplFor` declaration)"
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
/// delegate to the inner map's `insert`/`remove`/`contains_key` — now that
/// the map surface is linked, a Set defined as an ordinary USER/entry struct
/// (mirroring `set.nym`'s own body exactly) round-trips insert/remove/
/// contains under Node with NO new linkage of its own. (The REAL prelude
/// `Set` cannot materialize yet — a separate, pre-existing
/// prelude-method-materialization gap for named-struct receivers,
/// `nymph-codegen`'s `real_set_insert_stays_a_loud_transitively_external_defer`
/// — out of this slice's scope; see that test's own doc comment.)
#[test]
fn a_user_set_struct_backed_by_the_linked_map_inserts_removes_and_contains_round_trips() {
	let entry = "import std/collections/map\n\
		struct MySet(inner: #{int: #()}) {\n\
		\tmut func add(item: int): boolean = this.inner.insert(item, #())\n\
		\tmut func drop_it(item: int): boolean = if (this.inner.contains_key(item)) {\n\
		\t\tthis.inner.remove(item)\n\
		\t\ttrue\n\
		\t} else false\n\
		\tfunc has(item: int): boolean = this.inner.contains_key(item)\n\
		}\n\
		func demo(): boolean = {\n\
		\tlet mut s = MySet(inner = #{})\n\
		\ts.add(1)\n\
		\ts.add(2)\n\
		\tlet had = s.has(1)\n\
		\tlet dropped = s.drop_it(1)\n\
		\tlet after = s.has(1)\n\
		\tlet still2 = s.has(2)\n\
		\thad && dropped && !after && still2\n\
		}\n\
		func main(): void = {}\n";
	let load = only_entry("main", entry);

	// This synthetic `MySet` needs `contains_key` too, so the synthetic
	// provider must also declare it (unlike the get/insert/remove/size/keys
	// surface `synth_std_map_provider` already covers).
	fn provider_with_contains_key(path: &str) -> Option<String> {
		(path == "collections/map").then(|| {
			"impl<K, V> mut #{K: V} {\n  \
				external(insert) func insert(key: K, value: V): boolean\n  \
				external(remove) func remove(key: K): Option<V>\n\
			}\n\
			impl<K, V> #{K: V} {\n  \
				external(contains_key) func contains_key(key: K): boolean\n\
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

#[test]
fn real_std_set_iterates_its_keys() {
	let entry = r#"
		import std/collections/set with (Set)
		func demo(): int = {
			let set = Set(inner = #{1: #(), 2: #(), 3: #()})
			let mut total = 0
			for (item in set) { total = total + item }
			total
		}
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &nymph_compiler::embedded_std_provider)
		.expect("the real std Set should implement Iterable");
	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));
	assert_eq!(run_node(&js, "set_iterable"), "6");
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
		func ordered_distinct(): #[int] = #[3, 1, 3, 2].distinct()
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
	let ordered_distinct = compiled.entry_symbol("ordered_distinct");
	let mut js = compiled.js;
	for call in &calls {
		js.push_str(&format!("\nconsole.log({call}().v);\n"));
	}
	js.push_str(&format!(
		"\nconsole.log({ordered_distinct}().v.map(value => value.v).join(','));\n"
	));
	assert_eq!(
		run_node(&js, "boxed_collections"),
		"true\n1\n3\n7\n99\n3,1,2"
	);
}

#[test]
fn nominal_equality_operators_use_identity_without_changing_explicit_equals() {
	let entry = r#"
		struct Point(x: int)
		struct Pair(left: Point, right: Point)
		struct AlwaysEqual(x: int)
		impl Equals<Other = AlwaysEqual> for AlwaysEqual {
			func equals(other: AlwaysEqual): boolean = true
		}
		func equal(): boolean = Point(x = 1) == Point(x = 1)
		func unequal(): boolean = Point(x = 1) != Point(x = 2)
		func nested(): boolean = Pair(left = Point(x = 1), right = Point(x = 2)) == Pair(left = Point(x = 1), right = Point(x = 2))
		func generic_same<T>(left: T, right: T): boolean = left == right
		func generic_different<T>(left: T, right: T): boolean = left != right
		func generic(): boolean = generic_same(Point(x = 3), Point(x = 3))
		func generic_custom_same(): boolean = generic_same(AlwaysEqual(x = 1), AlwaysEqual(x = 2))
		func generic_custom_different(): boolean = generic_different(AlwaysEqual(x = 1), AlwaysEqual(x = 2))
		func explicit_custom_same(): boolean = AlwaysEqual(x = 1).equals(AlwaysEqual(x = 2))
		func explicit_custom_different(): boolean = AlwaysEqual(x = 1).not_equals(AlwaysEqual(x = 2))
		func main(): void = {}
	"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &|_| None)
		.expect("the ambient blanket Equals implementation should compile for structs");
	let equal = compiled.entry_symbol("equal");
	let unequal = compiled.entry_symbol("unequal");
	let nested = compiled.entry_symbol("nested");
	let generic = compiled.entry_symbol("generic");
	let generic_custom_same = compiled.entry_symbol("generic_custom_same");
	let generic_custom_different = compiled.entry_symbol("generic_custom_different");
	let explicit_custom_same = compiled.entry_symbol("explicit_custom_same");
	let explicit_custom_different = compiled.entry_symbol("explicit_custom_different");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({equal}().v);\n"));
	js.push_str(&format!("console.log({unequal}().v);\n"));
	js.push_str(&format!("console.log({nested}().v);\n"));
	js.push_str(&format!("console.log({generic}().v);\n"));
	js.push_str(&format!("console.log({generic_custom_same}().v);\n"));
	js.push_str(&format!("console.log({generic_custom_different}().v);\n"));
	js.push_str(&format!("console.log({explicit_custom_same}().v);\n"));
	js.push_str(&format!("console.log({explicit_custom_different}().v);\n"));
	assert_eq!(
		run_node(&js, "blanket_equals"),
		"false\ntrue\nfalse\nfalse\nfalse\ntrue\ntrue\nfalse"
	);
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

#[test]
fn boxed_lists_and_maps_iterate_through_the_uniform_protocol() {
	let entry = r#"
		func encode_entries(entries: #[#(int, int)]): int = {
			let mut encoded = 0
			for (#(key, value) in entries) encoded = encoded * 100 + key * 10 + value
			encoded
		}
		func explicit_map_iteration(values: #{int: int}): int = {
			let mut iterator = values.iter()
			let mut encoded = 0
			encoded = match (iterator.next()) {
				Some(#(key, value)) -> encoded * 100 + key * 10 + value,
				None -> -1,
			}
			encoded = match (iterator.next()) {
				Some(#(key, value)) -> encoded * 100 + key * 10 + value,
				None -> -1,
			}
			encoded = match (iterator.next()) {
				Some(#(key, value)) -> encoded * 100 + key * 10 + value,
				None -> -1,
			}
			match (iterator.next()) {
				Some(_) -> -2,
				None -> match (iterator.next()) {
					Some(_) -> -3,
					None -> encoded,
				},
			}
		}
		func for_map_iteration(values: #{int: int}): int = {
			let mut encoded = 0
			for (#(key, value) in values) encoded = encoded * 100 + key * 10 + value
			encoded
		}
		struct MapFactory(calls: int)
		impl mut MapFactory {
			mut func make(): #{int: int} = {
				this.calls = this.calls + 1
				#{1: 2, 3: 4}
			}
		}
		func list_sum(): int = {
			let mut total = 0
			for (value in #[1, 2, 3, 4]) total = total + value
			total
		}
		func map_sum(): int = {
			let mut total = 0
			for (#(key, value) in #{1: 10, 2: 20}) total = total + key + value
			total
		}
		func pattern_sum(): int = {
			let mut total = 0
			for (#[a, b] in #[#[1, 2], #[3, 4]]) total = total + a + b
			for (#(a, b, c) in #[#(5, 6, 7)]) total = total + a + b + c
			for (#[first, ...rest] in #[#[8, 9]]) total = total + first + rest[0]
			// This fixed tuple has no rest segment; the former `...rest` fixture was stale.
			for (#(first, middle, last) in #[#(10, 11, 12)]) total = total + first + middle + last
			for (#[1, value] in #[#[1, 2], #[9, 9]]) total = total + value
			for (#(1, value) in #{1: 10, 2: 20}) total = total + value
			total
		}
		func map_iteration_contract(): #(int, int, int, int, int) = {
			let values = #{1: 2, 3: 4, 5: 6}
			let entries_sequence = encode_entries(values.entries())
			let explicit = explicit_map_iteration(values)
			let first_for = for_map_iteration(values)
			let second_for = for_map_iteration(values)
			let mut factory = MapFactory(calls = 0)
			for (_ in factory.make()) {}
			#(entries_sequence, explicit, first_for, second_for, factory.calls as int)
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
		compiled.js.contains("Array.from($_this.v)[i.v]")
			&& compiled
				.js
				.contains("Array.from($_this.v).slice(start.v, end.v).join"),
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
		js.push_str(&format!("\nconsole.log(JSON.stringify({call}().v));\n"));
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
			"public func first_range(): ListIter<int> = ListIter(items = #[1], index = 0u)",
		),
		(
			"b",
			"public func second_range(): ListIter<int> = ListIter(items = #[2], index = 0u)",
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
