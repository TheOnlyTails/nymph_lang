//! End-to-end proof of the external-linkage mechanism (Gap 3, L0/L1): a
//! project `import std/collections/list`s a SYNTHETIC, self-contained std
//! module declaring `length`/`get` as `external(..)` (linked) and `is_empty`
//! as real Nymph source that transitively calls `length`, compiles through
//! the project driver's bundle path (`compile_project_with_std` →
//! `bundle::bundle`), and RUNS under Node with every linked-external import
//! resolved and inlined — L1 additionally proves the Option ABI seam: `get`'s
//! `Some`/`None` (built by the injected, stripped `list.ts` intrinsic,
//! importing the injected `std/option` virtual module) are recognized by the
//! user PROGRAM's own inline `match`, because `nymph-codegen`'s `emit_enum`
//! now tags every variant with a GLOBAL `Symbol.for(..)` discriminant.
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
/// `import { Option } from "../option"` resolves against the injected
/// `std/option` virtual module, per `nymph_codegen::strip_ts_to_js`'s
/// `import_rewrites` and `nymph_compiler::intrinsics`), and RUNS under Node —
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

	let compiled = compile_project_with_std("main", &load, &synth_std_provider).expect(
		"expected `xs.get(1)` to compile once `get` is a linked external for a `List` receiver",
	);

	// Rolldown bundles the whole graph into ONE chunk (no `import` statement
	// survives — that's the point), so the resolution proof is that the
	// injected `std/option` virtual module's own source made it in at all
	// (rather than the bundle failing outright on `list.ts`'s originally
	// unresolvable `"../option"` specifier).
	assert!(
		compiled.js.contains("//#region std/option") && compiled.js.contains("SOME_TAG"),
		"expected the injected `std/option` virtual module to be bundled in \
		 (proving its rewritten specifier resolved), got:\n{}",
		compiled.js
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

/// L3's whole proof obligation for the MAP surface: `get`/`insert`/`remove`/
/// `size`/`keys` — all newly linked — compile, bundle (the stripped `map.ts`
/// intrinsic's own `import { Option } from "../option"` resolves against the
/// injected `std/option` virtual module, exactly like `list.ts`), and RUN
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
	js.push_str(&format!("\nconsole.log({call}());\n"));

	assert_eq!(
		run_node(&js, "string_methods"),
		"HELLO olleH",
		"expected `\"Hello\".to_upper().concat(\" \").concat(\"Hello\".reversed())`"
	);
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
