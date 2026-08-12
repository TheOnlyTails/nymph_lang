//! Integration tests for the multi-module project driver
//! (`nymph_compiler::project`): resolution, namespace/`with` binding,
//! visibility, cycles, and collisions — over a virtual, filesystem-free
//! project (an `FxHashMap<String, String>` keyed by canonical module path).
use nymph_compiler::{
	CompiledEntryRoot, CompilerOptions, check_project, check_project_with_embedded_std,
	compile_project, compile_project_library_with_embedded_std_and_options,
	compile_project_with_embedded_std_and_options, project::compile_project_module_sources_with_std,
};
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
fn embedded_std_project_check_resolves_project_and_std_graph() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/helper with (leaf)\nfunc main(): void = { let tree = leaf() }",
		),
		(
			"helper",
			"import std/collections/tree with (Tree)\npublic func leaf(): Tree<int> = Tree.Leaf(value = 1)",
		),
	]);
	let diags = check_project_with_embedded_std("main", &loader(files));

	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn entry_compilation_propagates_all_six_static_root_adapters() {
	let cases = [
		("func main(): void = {}", "void"),
		("func main(): Option<void> = None", "option"),
		(
			"func main(): Result<void, string> = Ok(value = {})",
			"result",
		),
		("async func main(): void = {}", "task-void"),
		("async func main(): Option<void> = None", "task-option"),
		(
			"async func main(): Result<void, string> = Ok(value = {})",
			"task-result",
		),
	];
	for (source, expected) in cases {
		let load = |key: &str| (key == "main").then(|| source.to_string());
		let compiled =
			compile_project_with_embedded_std_and_options("main", &load, &CompilerOptions::default())
				.expect("root project should compile");
		let actual = match compiled.entry_root.as_ref().expect("entry adapter") {
			CompiledEntryRoot::Void => "void",
			CompiledEntryRoot::Option { binding } => {
				assert!(!binding.is_empty());
				"option"
			}
			CompiledEntryRoot::Result { binding } => {
				assert!(!binding.is_empty());
				"result"
			}
			CompiledEntryRoot::TaskVoid => "task-void",
			CompiledEntryRoot::TaskOption { binding } => {
				assert!(!binding.is_empty());
				"task-option"
			}
			CompiledEntryRoot::TaskResult { binding } => {
				assert!(!binding.is_empty());
				"task-result"
			}
		};
		assert_eq!(actual, expected);
	}
}

#[test]
fn ordinary_build_is_an_inert_importable_module_without_node_launcher_policy() {
	let load = |key: &str| (key == "main").then(|| "func main(): Option<void> = None".to_string());
	let compiled = compile_project_library_with_embedded_std_and_options(
		"main",
		&load,
		&CompilerOptions::default(),
	)
	.expect("library project should compile");
	assert_eq!(compiled.entry_root, None);
	for forbidden in [
		"nymphStartRoot",
		"execution cancelled",
		"main returned None",
		"main();",
	] {
		assert!(
			!compiled.js.contains(forbidden),
			"ordinary module contains {forbidden:?}"
		);
	}
	let output = std::process::Command::new("node")
		.args(["--input-type=module", "--eval", &compiled.js])
		.output()
		.expect("Node should import the ordinary module");
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert!(output.stdout.is_empty());
	assert!(output.stderr.is_empty());
}

#[test]
fn project_modules_import_one_shared_box_runtime_instead_of_inlining_it() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/helper with (value)\nfunc main(): void = { let result = value() }",
		),
		("helper", "public func value(): int = 1"),
	]);

	let sources = compile_project_module_sources_with_std("main", &loader(files), &|_| None)
		.expect("project should compile");
	let helper = &sources["helper"];

	assert!(
		helper.lines().any(|line| line.starts_with("import { ")
			&& line.contains("NInt")
			&& line.ends_with("from \"std/box\";")),
		"boxed project values should import the canonical runtime: {helper}"
	);
	assert!(
		!helper.contains("class NBox"),
		"project modules must not inline a private box runtime copy: {helper}"
	);
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
	assert!(
		diags.iter().any(|d| d.diag.code.contains("PRIVATE")),
		"{diags:?}"
	);
}
#[test]
fn private_name_cannot_be_imported_via_namespace_access() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (public_value)\nfunc main(): void = { math.helper() }",
		),
		(
			"math",
			"private func helper(): void = {}\npublic let public_value = 1",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert_eq!(diags.len(), 1, "{diags:?}");
	assert_eq!(diags[0].diag.code, "IMPORT-PRIVATE-NAME", "{diags:?}");
}

#[test]
fn missing_imported_namespace_member_has_a_structured_import_diagnostic() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (public_value)\nfunc read(): int = math.missing()\nfunc main(): void = { let ignored = read() }",
		),
		("math", "public let public_value: int = 1"),
	]);
	let diags = check_project("main", &loader(files));
	assert_eq!(diags.len(), 1, "{diags:?}");
	assert_eq!(diags[0].diag.code, "IMPORT-UNRESOLVED-NAME", "{diags:?}");
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
	js.push_str(&format!("console.log(String({call_symbol}({args}).v));\n"));
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
fn imported_generic_callable_alias_captures_its_hidden_type_object() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/seed with (direct)\nfunc main(): void = {}\nfunc answer(): int = { let alias = direct\n alias(0, 40) }",
		),
		(
			"seed",
			"interface Seed { func seed(value: int): int }\nimpl Seed for int { func seed(value: int): int = value + 1 }\npublic func direct<T: Seed>(marker: T, value: int): int = T.seed(value)",
		),
	]);
	assert_eq!(run_project(files, "answer", ""), "41");
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
fn imported_direct_and_default_interface_methods_run_under_node() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/dep with (Cell)\nfunc main(): void = {}\nfunc result(): int = Cell(value = 2).read() + Cell(value = 3).twice()",
		),
		(
			"dep",
			"public interface Read<Output> { func read(): Output\nfunc twice(): Output = this.read() }\npublic struct Cell(value: int) { impl Read<Output = int> { func read(): int = this.value } }",
		),
	]);

	assert_eq!(run_project(files, "result", ""), "5");
}

#[test]
fn imported_struct_construction_lowers_to_new_not_a_plain_call() {
	// Cross-module values require stable runtime identity because
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
fn imported_struct_clone_preserves_private_fields_without_rerunning_defaults() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/vault with (Vault)\nfunc main(): void = {}\nfunc result(): int = { let source = Vault.make(1, 9)\nlet updated = Vault(...source, shown = 2)\nupdated.shown * 10 + updated.reveal() }",
		),
		(
			"vault",
			"public struct Vault(public shown: int, private secret: int = 7) { public namespace func make(shown: int, secret: int): Vault = Vault(shown = shown, secret = secret)\npublic func reveal(): int = this.secret }",
		),
	]);
	assert_eq!(run_project(files, "result", ""), "29");
}

#[test]
fn imported_struct_hidden_fields_block_fresh_construction_and_require_pattern_omission() {
	let fresh = FxHashMap::from_iter([
		(
			"main",
			"import @/vault with (Vault)\nfunc main(): void = {}\nfunc bad(): Vault = Vault(shown = 1)",
		),
		(
			"vault",
			"public struct Vault(public shown: int, private secret: int = 7)",
		),
	]);
	let diagnostics = check_project("main", &loader(fresh));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic
			.diag
			.message
			.contains("cannot be constructed fresh because it has hidden fields")
	}));

	let pattern = FxHashMap::from_iter([
		(
			"main",
			"import @/vault with (Vault)\nfunc main(): void = {}\nfunc bad(value: Vault): int = match (value) { Vault(shown) -> shown }",
		),
		(
			"vault",
			"public struct Vault(public shown: int, private secret: int)",
		),
	]);
	let diagnostics = check_project("main", &loader(pattern));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic
			.diag
			.message
			.contains("partial struct pattern must end with anonymous `...`")
	}));
}

#[test]
fn same_module_struct_pattern_matches_after_own_name_mangling() {
	// A single-file project (no imports at all) still goes through the
	// project driver's own-name mangling (every module gets a `$m{tag}$`
	// rename, entry `main` excepted). A bare struct-constructor pattern
	// matching a struct declared IN THE SAME MODULE must still resolve.
	let files = FxHashMap::from_iter([(
		"main",
		"struct Point(x: int, y: int)\nfunc main(): void = {}\nfunc result(): int = match (Point(x = 1, y = 2)) { Point(x = x, y = y) -> x + y }",
	)]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn same_module_struct_pattern_runs_under_node() {
	let files = FxHashMap::from_iter([(
		"main",
		"struct Point(x: int, y: int)\nfunc main(): void = {}\nfunc result(): int = match (Point(x = 1, y = 2)) { Point(x = x, y = y) -> x + y }",
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
	// internal invariant, which assumed a prelude entry was always a
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
	// A cross-module enum must be emitted only on its own module's turn.
	// `materialize_referenced_prelude_enums` must not treat every prelude entry
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
	// identifier to be a diagnostic, not a silent double-bind. Both checks must
	// inspect the namespace and `with` tables or this clash slips through.
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
	// `interface` must not break bundling: stable lowering never emits
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
fn imported_type_alias_participates_in_consumer_type_checking() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/aliases with (MyInt)\nfunc main(): void = {}\nfunc identity(value: MyInt): MyInt = value",
		),
		("aliases", "public type MyInt = int"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn conflicting_implementations_from_distinct_dependencies_are_diagnosed() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/first\nimport @/second\nfunc main(): void = {}",
		),
		("protocol", "public interface Show { func show(): string }"),
		("model", "public struct Item(value: int)"),
		(
			"first",
			"import @/protocol with (Show)\nimport @/model with (Item)\nimpl Show for Item { func show(): string = \"first\" }",
		),
		(
			"second",
			"import @/protocol with (Show)\nimport @/model with (Item)\nimpl Show for Item { func show(): string = \"second\" }",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		diags.iter().any(|diagnostic| diagnostic
			.diag
			.message
			.contains("conflicting implementations")),
		"expected an imported coherence diagnostic, got: {diags:?}"
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
	// collide, so the with-name check must cross-reference the namespaces table.
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
fn project_module_named_like_a_removed_runtime_shim_remains_distinct() {
	let files = FxHashMap::from_iter([
		("main", "import @/std/option\nfunc main(): void = {}"),
		("std/option", "public func project_value(): int = 1"),
	]);
	assert!(
		compile_project("main", &loader(files)).is_ok(),
		"the compiler runtime's exact owner must not collide with a project std/option module"
	);
}

#[test]
fn project_module_cannot_be_silently_replaced_by_an_intrinsic_module() {
	let files = FxHashMap::from_iter([
		("main", "import @/std/box\nfunc main(): void = {}"),
		("std/box", "public func local(): int = 1"),
	]);
	let diags = match compile_project("main", &loader(files)) {
		Ok(_) => panic!("an intrinsic runtime module must not overwrite a project dependency"),
		Err(diags) => diags,
	};
	assert!(
		diags
			.iter()
			.any(|d| d.diag.code == "STABLE-INTRINSIC-COLLISION"),
		"expected a runtime-module collision diagnostic, got: {diags:?}"
	);
}

#[test]
fn canonical_runtime_functions_are_emitted_once_for_multiple_consumers() {
	std::thread::Builder::new()
		.stack_size(8 * 1024 * 1024)
		.spawn(canonical_runtime_functions_are_emitted_once)
		.unwrap()
		.join()
		.unwrap();
}

fn canonical_runtime_functions_are_emitted_once() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/left with (left_values)\nimport @/right with (right_values)\nfunc main(): void = {}\nfunc result(): int = left_values()[0] + right_values()[0]",
		),
		("left", "public func left_values(): #[int] = #[2, 1].sort()"),
		(
			"right",
			"public func right_values(): #[int] = #[4, 3].sort()",
		),
	]);
	let sources = compile_project_module_sources_with_std("main", &loader(files.clone()), &|_| None)
		.unwrap_or_else(|diags| panic!("canonical graph should compile: {diags:?}"));
	let list = sources
		.get("@nymph/runtime/collections/list")
		.expect("canonical list owner");
	let sort_declarations = list
		.lines()
		.filter(|line| {
			line.starts_with("let ")
				&& line.contains(" = nymphCallable(function(")
				&& line.contains("$list$i")
				&& line
					.split_once(" =")
					.is_some_and(|(binding, _)| binding.ends_with("$sort"))
		})
		.collect::<Vec<_>>();
	assert_eq!(sort_declarations.len(), 1, "{list}");
	let sort_binding = sort_declarations[0]
		.trim_start_matches("let ")
		.split_once(" =")
		.unwrap()
		.0;
	let ops = &sources["@nymph/runtime/ops"];
	let compare_declarations = ops
		.lines()
		.filter(|line| {
			line.starts_with("let ")
				&& line.contains(" = nymphCallable(function(")
				&& line.contains("$int$i")
				&& line
					.split_once(" =")
					.is_some_and(|(binding, _)| binding.ends_with("$compare_to"))
		})
		.collect::<Vec<_>>();
	assert_eq!(compare_declarations.len(), 1, "{ops}");
	for consumer in ["left", "right"] {
		let list_import = sources[consumer].lines().find(|line| {
			line.ends_with("from \"@nymph/runtime/collections/list\";") && line.contains(sort_binding)
		});
		assert!(
			list_import.is_some(),
			"{consumer} must import exact {sort_binding} from its canonical owner:\n{}",
			sources[consumer]
		);
	}
	assert!(
		list.contains("from \"std/collections/list\""),
		"canonical Nymph owner must link its host-only leaf explicitly:\n{list}"
	);
	let out = run_project(
		FxHashMap::from_iter([
			(
				"main",
				"import @/left with (left_values)\nimport @/right with (right_values)\nfunc main(): void = {}\nfunc result(): int = left_values()[0] + right_values()[0]",
			),
			("left", "public func left_values(): #[int] = #[2, 1].sort()"),
			(
				"right",
				"public func right_values(): #[int] = #[4, 3].sort()",
			),
		]),
		"result",
		"",
	);
	assert_eq!(out, "4");
}

#[test]
fn native_generic_dispatch_members_are_declared_once_by_exact_identity() {
	let files = FxHashMap::from_iter([(
		"main",
		"func add<T: Plus<Other = T, Output = T>>(left: T, right: T): T = if (1.plus(1) == 2) left + right else left\n\
		 func result(): int = add(20, 22)\nfunc main(): void = {}",
	)]);
	let sources = compile_project_module_sources_with_std("main", &loader(files.clone()), &|_| None)
		.unwrap_or_else(|diags| panic!("native generic dispatch should assemble: {diags:?}"));
	let plus_declarations = sources
		.iter()
		.flat_map(|(module, source)| {
			source
				.lines()
				.filter(|line| {
					line.starts_with("let ")
						&& line.contains(" = nymphCallable(function(")
						&& line
							.split_once(" =")
							.is_some_and(|(binding, _)| binding.ends_with("$plus"))
				})
				.map(move |line| (module.as_str(), line))
		})
		.collect::<Vec<_>>();
	let unique = plus_declarations
		.iter()
		.map(|(_, declaration)| *declaration)
		.collect::<std::collections::HashSet<_>>();
	assert!(!plus_declarations.is_empty(), "{sources:#?}");
	assert_eq!(
		plus_declarations.len(),
		unique.len(),
		"{plus_declarations:#?}"
	);
	let main = &sources["main"];
	let plus_imports = main
		.lines()
		.filter(|line| line.starts_with("import ") && line.contains("$plus"))
		.flat_map(|line| {
			line
				.split_once('{')
				.unwrap()
				.1
				.split_once('}')
				.unwrap()
				.0
				.split(',')
				.map(str::trim)
				.filter(|name| name.contains("$plus"))
		})
		.collect::<Vec<_>>();
	let unique_imports = plus_imports
		.iter()
		.copied()
		.collect::<std::collections::HashSet<_>>();
	assert!(!plus_imports.is_empty(), "{main}");
	assert_eq!(plus_imports.len(), unique_imports.len(), "{main}");
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn cross_module_enum_match_runs_under_node() {
	// Matching an enum imported from another module must not
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

#[test]
fn cross_module_enum_views_check_deterministically() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/view with (View, widen)\nimport @/source with (Source, make)\nfunc direct(value: Source): View = value\nfunc use(): View = direct(make())\nfunc main(): void = { let value = widen(make()) }",
		),
		(
			"source",
			"public enum Source { A, B }\npublic func make(): Source = Source.A",
		),
		(
			"view",
			"import @/source with (Source)\npublic enum View { ...Source, C }\npublic func widen(value: Source): View = value",
		),
	]);
	let first = check_project("main", &loader(files.clone()));
	let second = check_project("main", &loader(files));
	assert!(
		first.is_empty(),
		"expected a clean fixed point, got: {first:?}"
	);
	assert_eq!(
		first, second,
		"fixed-point diagnostics must be deterministic"
	);
}

#[test]
fn unrelated_module_cannot_attach_a_static_to_an_imported_type() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/owner with (Token)\nimport @/extension\nfunc main(): void = {}",
		),
		("owner", "public struct Token(value: int)"),
		(
			"extension",
			"import @/owner with (Token)\nimpl Token { namespace func forge(): Token = Token(value = 0) }",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		!diags.is_empty(),
		"an unrelated module must not extend another module's type"
	);
}

#[test]
fn same_bare_type_name_in_different_modules_does_not_receive_another_owners_static() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/left with (left_value)\nimport @/right with (right_value)\nfunc main(): void = {}\nfunc result(): int = left_value() + right_value()",
		),
		(
			"left",
			"public struct Token(value: int)\nimpl Token { namespace func left(): Token = Token(value = 1) }\npublic func left_value(): int = Token.left().value",
		),
		(
			"right",
			"public struct Token(value: int)\npublic func right_value(): int = Token(value = 2).value",
		),
	]);
	assert_eq!(run_project(files, "result", ""), "3");
}

#[test]
fn duplicate_static_attachments_across_source_modules_are_diagnosed() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/owner with (Token)\nimport @/first\nimport @/second\nfunc main(): void = {}",
		),
		("owner", "public struct Token(value: int)"),
		(
			"first",
			"import @/owner with (Token)\nimpl Token { namespace func make(): Token = Token(value = 1) }",
		),
		(
			"second",
			"import @/owner with (Token)\nimpl Token { namespace func make(): Token = Token(value = 2) }",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		diags
			.iter()
			.any(|diag| diag.diag.code == "INHERENT-IMPL-OWNER"),
		"malformed cross-module attachments must report exact owner errors: {diags:?}"
	);
}

#[test]
fn multiple_consumers_share_one_canonical_generic_enum_static_attachment() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/left with (left_value)\nimport @/right with (right_value)\nfunc main(): void = {}\nfunc result(): int = left_value() + right_value()",
		),
		(
			"owner",
			"public enum Boxed<T> { Value(value: T) }\nimpl<T> Boxed<T> { namespace func wrap(value: T): Boxed<T> = Boxed.Value(value = value) }",
		),
		(
			"left",
			"import @/owner with (Boxed)\npublic func left_value(): int = match (Boxed.wrap(2)) { Value(value) -> value }",
		),
		(
			"right",
			"import @/owner with (Boxed)\npublic func right_value(): int = match (Boxed.wrap(3)) { Value(value) -> value }",
		),
	]);
	let sources = compile_project_module_sources_with_std("main", &loader(files.clone()), &|_| None)
		.unwrap_or_else(|diags| panic!("canonical static graph should compile: {diags:?}"));
	assert!(sources.contains_key("owner"));
	assert_eq!(
		sources.keys().filter(|key| key.as_str() == "owner").count(),
		1
	);
	assert_eq!(run_project(files, "result", ""), "5");
}
