//! Integration tests for the multi-module project driver
//! (`nymph_compiler::project`): resolution, namespace/`with` binding,
//! visibility, cycles, and collisions — over a virtual, filesystem-free
//! project (an `FxHashMap<String, String>` keyed by canonical module path).
use nymph_compiler::{check_project, compile_project};
use rustc_hash::FxHashMap;
/// Build a `load` closure over a virtual project map.
fn loader(files: FxHashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
	move |key: &str| files.get(key).map(|s| (*s).to_string())
}
#[test]
fn resolves_at_import_against_the_source_root() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin)\nfunc main(): void = {}\nfunc used(): int = sin(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn resolves_relative_current_and_parent_imports() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import ./geometry/vec with (make)\nfunc main(): void = {}\nfunc used(): int = make(1)",
		),
		(
			"geometry/vec",
			"import ../helpers with (id)\nfunc make(x: int): int = id(x)",
		),
		("helpers", "func id(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn missing_module_is_an_unresolved_import_diagnostic() {
	let files = FxHashMap::from_iter([("main", "import @/nope with (x)\nfunc main(): void = {}")]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("UNRESOLVED")));
}

#[test]
fn missing_module_diagnostic_is_attributed_to_the_importer_not_the_missing_target() {
	// The diagnostic must point at the module that WROTE the bad `import`
	// (whose source actually exists, and can be rendered), not at the
	// nonexistent target — otherwise the CLI falls back to an empty source
	// and the wrong filename when rendering it.
	let files = FxHashMap::from_iter([("main", "import @/nope with (x)\nfunc main(): void = {}")]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(
		diags.iter().any(|d| d.module == "main"),
		"expected the diagnostic attributed to `main`, got: {diags:?}"
	);
	assert!(
		diags.iter().all(|d| d.module != "nope"),
		"diagnostic must not be attributed to the nonexistent target module: {diags:?}"
	);
}

#[test]
fn parent_import_escaping_the_source_root_is_a_diagnostic() {
	let files = FxHashMap::from_iter([("main", "import ../nope with (x)\nfunc main(): void = {}")]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("ESCAPES-ROOT")));
}
#[test]
fn import_cycle_is_a_clean_diagnostic_not_a_hang() {
	let files = FxHashMap::from_iter([
		("main", "import @/a with (f)\nfunc main(): void = { f() }"),
		("a", "import @/b with (g)\nfunc f(): void = g()"),
		("b", "import @/a with (f)\nfunc g(): void = f()"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("CYCLE")));
}
#[test]
fn namespace_access_resolves_to_the_imported_function() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math\nfunc main(): void = {}\nfunc used(): int = math.sin(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn namespace_alias_binds_under_the_alias_not_the_original_name() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math as m\nfunc main(): void = {}\nfunc used(): int = m.sin(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn with_alias_binds_the_renamed_name_unqualified() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin as sine)\nfunc main(): void = {}\nfunc used(): int = sine(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn private_name_cannot_be_imported_unqualified() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (helper)\nfunc main(): void = { helper() }",
		),
		("math", "private func helper(): void = {}"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("PRIVATE")));
}
#[test]
fn private_name_cannot_be_imported_via_namespace_access() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math\nfunc main(): void = { math.helper() }",
		),
		("math", "private func helper(): void = {}"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("PRIVATE")));
}
#[test]
fn private_helper_is_still_usable_within_its_own_module() {
	// A private decl stays fully usable *inside* its own module — only the
	// cross-module boundary excludes it.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin)\nfunc main(): void = {}\nfunc used(): int = sin(1)",
		),
		(
			"math",
			"private func helper(x: int): int = x\nfunc sin(x: int): int = helper(x)",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn with_name_colliding_with_a_local_declaration_is_diagnosed() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin)\nfunc sin(x: int): int = x\nfunc main(): void = { sin(1) }",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("COLLISION")));
}
#[test]
fn two_import_namespaces_with_the_same_name_are_diagnosed() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math\nimport @/geometry as math\nfunc main(): void = {}",
		),
		("math", "func sin(x: int): int = x"),
		("geometry", "func area(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("COLLISION")));
}
#[test]
fn unresolved_with_name_is_diagnosed() {
	let files = FxHashMap::from_iter([
		("main", "import @/math with (nope)\nfunc main(): void = {}"),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(
		diags
			.iter()
			.any(|d| d.diag.code.contains("UNRESOLVED-NAME"))
	);
}
#[test]
fn compiles_a_multi_module_project() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin)\nfunc main(): void = {}\nfunc used(): int = sin(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let result = compile_project("main", &loader(files));
	let compiled = result.unwrap_or_else(|d| panic!("expected a clean compile, got: {d:?}"));
	assert!(compiled.js.contains("main"));
	assert_eq!(compiled.entry_main, "main");
}
/// Emit the compiled project, append a call to its (mangled) entry `main`,
/// then log the (mangled) entry-module function named `call_fn` invoked with
/// `args` verbatim, run under Node, and return trimmed stdout — mirrors
/// `crates/nymph-codegen/tests/run_node.rs`'s single-module `run` helper,
/// adapted for the fact every top-level name in a project build is mangled.
fn run_project(files: FxHashMap<&'static str, &'static str>, call_fn: &str, args: &str) -> String {
	use std::io::Write;
	use std::process::Command;
	use std::sync::atomic::{AtomicU64, Ordering};
	let result = compile_project("main", &loader(files));
	let compiled = result.unwrap_or_else(|d| panic!("expected a clean compile, got: {d:?}"));
	let call_symbol = compiled.entry_symbol(call_fn);
	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));
	js.push_str(&format!("console.log({call_symbol}({args}));\n"));
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir();
	let path = dir.join(format!(
		"nymph_project_run_{}_{unique}.mjs",
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
		"node failed:\n{}\n--- js ---\n{}",
		String::from_utf8_lossy(&output.stderr),
		js
	);
	String::from_utf8_lossy(&output.stdout).trim().to_string()
}
#[test]
fn three_module_project_runs_under_node() {
	// entry imports a helper module that imports another; the value threads
	// through all three.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/geometry with (area)\nfunc main(): void = {}\nfunc result(): int = area(3, 4)",
		),
		(
			"geometry",
			"import @/math with (mul)\nfunc area(w: int, h: int): int = mul(w, h)",
		),
		("math", "func mul(a: int, b: int): int = a * b"),
	]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "12");
}
#[test]
fn namespace_and_with_together_run_under_node() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math as m with (sin)\nfunc main(): void = {}\nfunc result(): int = m.cos(sin(1))",
		),
		(
			"math",
			"func sin(x: int): int = x + 1\nfunc cos(x: int): int = x * 10",
		),
	]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "20");
}
#[test]
fn imported_struct_construction_lowers_to_new_not_a_plain_call() {
	// Regression pin for the cross-module `struct_names` gap `lower_hir.rs`
	// flagged: an imported struct constructor call must lower to `new`, not
	// a plain function call (which would just crash at runtime).
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/geometry with (Point)\nfunc main(): void = {}\nfunc result(): int = Point(x = 3, y = 4).x",
		),
		("geometry", "struct Point(x: int, y: int)"),
	]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "3");
}

#[test]
fn same_module_struct_pattern_matches_after_own_name_mangling() {
	// A single-file project (no imports at all) still goes through the
	// project driver's own-name mangling (every module gets a `$m{tag}$`
	// rename, entry `main` excepted). A bare struct-constructor pattern
	// matching a struct declared IN THE SAME MODULE must still resolve.
	let files = FxHashMap::from_iter([(
		"main",
		"struct Point(x: int, y: int)\nfunc main(): void = {}\nfunc result(): int = match (Point(x = 1, y = 2)) { Point(x, y) -> x + y }",
	)]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn same_module_struct_pattern_runs_under_node() {
	let files = FxHashMap::from_iter([(
		"main",
		"struct Point(x: int, y: int)\nfunc main(): void = {}\nfunc result(): int = match (Point(x = 1, y = 2)) { Point(x, y) -> x + y }",
	)]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "3");
}

#[test]
fn same_module_qualified_enum_variant_pattern_matches_after_own_name_mangling() {
	// A qualified enum-variant pattern (`Color.Red`) against an enum declared
	// in the SAME module (no imports) must still resolve after the module's
	// own `Color` name gets mangled.
	let files = FxHashMap::from_iter([(
		"main",
		"enum Color { Red, Green }\nfunc main(): void = {}\nfunc result(): int = match (Color.Red) { Color.Red -> 1, Color.Green -> 2 }",
	)]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn same_module_qualified_enum_variant_pattern_runs_under_node() {
	let files = FxHashMap::from_iter([(
		"main",
		"enum Color { Red, Green }\nfunc main(): void = {}\nfunc result(): int = match (Color.Red) { Color.Red -> 1, Color.Green -> 2 }",
	)]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "1");
}

#[test]
fn dependency_with_a_genuine_type_error_is_reported_not_panicked() {
	// `geometry` has a plain, unrelated type error (no name shadowing
	// whatsoever). Checking `main` (which imports and calls it) must report
	// the diagnostic — not panic via the prelude-flattening machinery's
	// internal invariant, which assumed a "prelude" slice entry was always a
	// trusted, bug-free stdlib clone.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/geometry with (bad)\nfunc main(): void = {}\nfunc used(): int = bad()",
		),
		("geometry", "func bad(): int = true"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		!diags.is_empty(),
		"expected the dependency's genuine type error to be reported"
	);
}

#[test]
fn imported_enum_is_emitted_once_and_the_bundle_is_valid_js() {
	// Regression: a cross-module enum used to be BOTH emitted on its own
	// module's turn AND re-materialized into the importing module's chunk
	// (`materialize_referenced_prelude_enums` treated every prelude-slice entry
	// like the ambient stdlib), so the concatenated bundle declared it twice and
	// Node crashed at load with "Identifier ... has already been declared".
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/shapes with (Color)\nfunc pick(): Color = Color.Red\nfunc main(): void = {}",
		),
		("shapes", "public enum Color { Red, Green }"),
	]);
	let js = compile_project("main", &loader(files))
		.expect("project should compile")
		.js;

	// `node --check` catches a duplicate top-level declaration (a SyntaxError)
	// without executing anything.
	let path = std::env::temp_dir().join(format!("nymph_project_enum_{}.mjs", std::process::id()));
	std::fs::write(&path, &js).unwrap();
	let out = std::process::Command::new("node")
		.arg("--check")
		.arg(&path)
		.output()
		.expect("spawn node");
	let _ = std::fs::remove_file(&path);
	assert!(
		out.status.success(),
		"emitted bundle is not valid JS (imported enum likely duplicated):\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
}

#[test]
fn a_namespace_name_colliding_with_a_with_name_is_a_diagnostic() {
	// The spec requires a namespace name and a `with`-bound name sharing an
	// identifier to be a diagnostic, not a silent double-bind. The two checks
	// used to inspect disjoint tables (namespace vs own/namespaces; with vs
	// own/renames), so a namespace/with clash slipped through both.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/a as foo\nimport @/b with (foo)\nfunc main(): void = {}",
		),
		("a", "public func x(): int = 1"),
		("b", "public func foo(): int = 2"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		diags.iter().any(|d| d.diag.code.contains("COLLISION")),
		"expected a namespace/with name-collision diagnostic, got: {diags:?}"
	);
}

#[test]
fn interface_only_dependency_module_bundles_successfully() {
	// A dependency whose only top-level declaration is a (default-visibility)
	// `interface` must not break bundling: `nymph_sema::lower_hir` never emits
	// a JS binding for an `Interface` declaration, so the synthesized
	// `export`/`import` lines must not name it either.
	let files = FxHashMap::from_iter([
		("main", "import @/shapes\nfunc main(): void = {}"),
		("shapes", "interface Shape { func area(): int }"),
	]);
	let result = compile_project("main", &loader(files));
	assert!(
		result.is_ok(),
		"expected a clean compile, got: {:?}",
		result.err()
	);
}

#[test]
fn type_alias_only_dependency_module_bundles_successfully() {
	// Same as above for a `type alias`-only dependency: `TypeAlias` also
	// never lowers to a JS binding.
	let files = FxHashMap::from_iter([
		("main", "import @/aliases\nfunc main(): void = {}"),
		("aliases", "type MyInt = int"),
	]);
	let result = compile_project("main", &loader(files));
	assert!(
		result.is_ok(),
		"expected a clean compile, got: {:?}",
		result.err()
	);
}

#[test]
fn namespace_only_dependency_module_bundles_successfully() {
	// A top-level `namespace` also never lowers to a JS binding today — same
	// hazard as interface/type-alias.
	let files = FxHashMap::from_iter([
		("main", "import @/ns\nfunc main(): void = {}"),
		("ns", "namespace Foo { func bar(): int = 1 }"),
	]);
	let result = compile_project("main", &loader(files));
	assert!(
		result.is_ok(),
		"expected a clean compile, got: {:?}",
		result.err()
	);
}

#[test]
fn a_with_name_colliding_with_a_namespace_name_is_a_diagnostic() {
	// The reverse order (namespace declared after the `with`-name) must also
	// collide — the with-name check now cross-references the namespaces table.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/b with (foo)\nimport @/a as foo\nfunc main(): void = {}",
		),
		("a", "public func x(): int = 1"),
		("b", "public func foo(): int = 2"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		diags.iter().any(|d| d.diag.code.contains("COLLISION")),
		"expected a with/namespace name-collision diagnostic, got: {diags:?}"
	);
}

#[test]
fn cross_module_enum_match_runs_under_node() {
	// Regression (IB2): matching an enum imported from another module used to
	// crash at RUNTIME with `ReferenceError: TAG is not defined` — the shared
	// `TAG` discriminant const is emitted only by the enum's DECLARING module,
	// but across rolldown's per-module ES scopes the MATCHING module referenced
	// it without a binding. `node --check` can't catch this (it's a runtime
	// error), so this actually executes the bundle. `main` recurses forever if
	// the match produced the wrong value, so a clean exit 0 proves both that
	// `TAG` is bound AND the cross-module match resolved correctly.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/shapes with (Color, red)\n\
			 func label(c: Color): int = match (c) { Red -> 1, Green -> 2 }\n\
			 func spin(): void = spin()\n\
			 func main(): void = { if (label(red()) != 1) spin() }",
		),
		(
			"shapes",
			"public enum Color { Red, Green }\npublic func red(): Color = Color.Red",
		),
	]);
	let compiled = compile_project("main", &loader(files))
		.unwrap_or_else(|d| panic!("expected a clean compile, got: {d:?}"));
	let js = format!("{}\n{}();\n", compiled.js, compiled.entry_main);

	let path =
		std::env::temp_dir().join(format!("nymph_project_enum_run_{}.mjs", std::process::id()));
	std::fs::write(&path, &js).unwrap();
	let out = std::process::Command::new("node")
		.arg(&path)
		.output()
		.expect("spawn node");
	let _ = std::fs::remove_file(&path);
	assert!(
		out.status.success(),
		"cross-module enum match crashed under Node:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
}
