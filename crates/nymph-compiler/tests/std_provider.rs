//! Integration tests for the core/std split, Slice B: `import std/…` now
//! resolves through a pluggable `std_provider` closure threaded through
//! `check_project_with_std`/`check_project_library_with_std` and their
//! embedded-provider convenience counterparts, plus
//! `compile_project_with_std`/`compile_project_library_with_std` (the
//! `_with_std`-suffixed siblings of the pre-existing, `std_provider`-free
//! `check_project`/`check_project_library`/`compile_project`/
//! `compile_project_library`, so callers can either supply a provider or use
//! the compiler's shipped std source tree), joining the SAME module graph
//! as ordinary `@/`/`./`/`../` project imports (cycle detection, topological
//! order, `with`-list binding, visibility — all unchanged, all still
//! enforced).
//!
//! The build/test provider here is backed by the real, on-disk `stdlib/src`
//! tree (mirroring `stdlib_option_result_cycle.rs`'s `stdlib_loader`) — the
//! production embedded/installed loader is deferred to a later (print/WASM)
//! slice; `std_provider` is exactly the swap point for that.
//!
//! Today's on-disk stdlib files (`set.nym`, `list.nym`, ...) cross-reference
//! each other and core via `@/…`. Package-root imports stay within the
//! importer's exact package, so those edges remain in the reserved std domain.
//! `stdlib/src/collections/tree.nym` is the import-free real std module used as
//! this file's end-to-end "resolves AND runs" target; the std-to-std link test
//! below uses two small SYNTHETIC std modules (served by a virtual provider,
//! not real files) to prove the linking mechanism itself.

use std::path::PathBuf;

use nymph_compiler::{
	check_project_library_with_embedded_std, check_project_library_with_std,
	check_project_with_embedded_std, check_project_with_std, compile_project_with_std,
};

fn stdlib_src_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../stdlib/src")
		.canonicalize()
		.unwrap()
}

/// The real, on-disk-backed `std_provider`: `path` (already stripped of both
/// the `std/` import root and this driver's `std::` key prefix — see
/// `nymph_compiler::project::resolve::resolve_import_target`'s doc comment)
/// maps to `<stdlib/src>/<path>.nym`.
fn real_stdlib_provider(path: &str) -> Option<String> {
	std::fs::read_to_string(stdlib_src_root().join(format!("{path}.nym")))
		.or_else(|_| std::fs::read_to_string(stdlib_src_root().join(path).join("mod.nym")))
		.ok()
}

/// A `load` closure that only ever serves the entry module itself — every
/// test here reaches every OTHER module exclusively through `std_provider`,
/// so the ordinary project `load` has nothing else to do.
fn only_entry(entry_key: &'static str, entry_src: &'static str) -> impl Fn(&str) -> Option<String> {
	move |key: &str| (key == entry_key).then(|| entry_src.to_string())
}

/// `import std/collections/tree` resolves against the real on-disk provider,
/// and the project compiles + RUNS under Node with the right value —
/// `tree.nym` is import-free, so this is a full, real end-to-end std import,
/// construction, and `match`, with no synthetic scaffolding.
#[test]
fn import_std_collections_tree_resolves_compiles_and_runs() {
	let entry = "import std/collections/tree with (Tree)\n\
		func demo(): int = match (Tree.Leaf(value = 42)) {\n\
		\tTree.Leaf(value) -> value,\n\
		\tTree.Node(...) -> 0,\n\
		}\n\
		func main(): void = {}\n";
	let load = only_entry("main", entry);

	let diags = check_project_with_std("main", &load, &real_stdlib_provider);
	assert!(
		diags.is_empty(),
		"expected `import std/collections/tree` to resolve and check cleanly, got: {diags:?}"
	);

	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("expected the project to compile once `std/collections/tree` resolves");

	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));

	let dir = std::env::temp_dir();
	let path = dir.join(format!(
		"nymph_std_provider_tree_{}.mjs",
		std::process::id()
	));
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
	assert_eq!(
		String::from_utf8_lossy(&output.stdout).trim(),
		"42n",
		"expected `Tree.Leaf(value = 42)` matched back out as `42`"
	);
}

/* Retired tests for unsupported mutable closure and resource semantics.
#[test]
fn managed_resources_close_in_reverse_order_on_normal_and_return_exits() {
	let entry = r#"
import std/io with (println)
struct Resource(id: int) {}
impl Close<!()> for Resource {
  func close(): void = println(this.id)
}
struct Defective(id: int, zero: int) {}
impl Close<!()> for Defective {
  func close(): void = {
    println(this.id)
    let ignored = 1 / this.zero
  }
}
func normal(): int = {
  let use first = Resource(id = 1)
  let alias = first
  let use second = Resource(id = 2)
  7
}
func early(): int = {
  let use resource = Resource(id = 3)
  return 8
}
func fail(): Result<int, string> = Error("stop")
func question(): Result<int, string> = {
  let use resource = Resource(id = 5)
  Ok(fail()?)
}
func tail_target(): int = 9
func tail_exit(): int = {
  let use resource = Resource(id = 6)
  tail_target()
}
func panic_exit(zero: int): float = {
  let use resource = Resource(id = 7)
  1 / zero
}
func cleanup_defects(zero: int): void = {
  let use first = Defective(id = 8, zero = zero)
  let use second = Defective(id = 9, zero = zero)
}
func primary_and_cleanup_defects(zero: int): float = {
  let use first = Defective(id = 10, zero = zero)
  let use second = Defective(id = 11, zero = zero)
  1 / zero
}
async func cancellation_exit(): void = {
  let use resource = Resource(id = 12)
  let waited = async { 1 }.await
}
async func cancellation_cleanup_defect(zero: int): void = {
  let use resource = Defective(id = 13, zero = zero)
  let waited = async { 1 }.await
}
func main(): void = {}
"#;
	let load = |key: &str| (key == "main").then(|| entry.to_string());
	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("managed resource project should compile");
	let normal = compiled.entry_symbol("normal");
	let early = compiled.entry_symbol("early");
	let question = compiled.entry_symbol("question");
	let tail_exit = compiled.entry_symbol("tail_exit");
	let panic_exit = compiled.entry_symbol("panic_exit");
	let cleanup_defects = compiled.entry_symbol("cleanup_defects");
	let primary_and_cleanup_defects = compiled.entry_symbol("primary_and_cleanup_defects");
	let cancellation_exit = compiled.entry_symbol("cancellation_exit");
	let cancellation_cleanup_defect = compiled.entry_symbol("cancellation_cleanup_defect");
	let mut js = compiled.js;
	js.push_str(&format!(
		"\nconsole.log({normal}().v);\nconsole.log({early}().v);\n\
		 {question}(); console.log('question');\n\
		 console.log({tail_exit}().v);\n\
		 try {{ {panic_exit}(new NInt(0n)); }} catch (_) {{ console.log('panic'); }}\n\
		 try {{ {cleanup_defects}(new NInt(0n)); }} catch (error) {{ console.log(`cleanup-${{error.errors.length}}`); }}\n\
		 try {{ {primary_and_cleanup_defects}(new NInt(0n)); }} catch (error) {{ console.log(`primary-${{error.errors.length}}`); }}\n\
		 const cancelled = {cancellation_exit}().spawn(null); cancelled.cancel();\n\
		 console.log((await cancelled.observe()).tag);\n\
		 const cancelledDefect = {cancellation_cleanup_defect}(new NInt(0n)).spawn(null); cancelledDefect.cancel();\n\
		 const cancelledDefectOutcome = await cancelledDefect.observe();\n\
		 console.log(`${{cancelledDefectOutcome.tag}}-${{cancelledDefectOutcome.defect.errors.length}}-${{cancelledDefectOutcome.defect.cancellationContext}}`);\n"
	));
	let path = std::env::temp_dir().join(format!("nymph_managed_{}.mjs", std::process::id()));
	std::fs::write(&path, &js).unwrap();
	let output = std::process::Command::new("node")
		.arg(&path)
		.output()
		.expect("run node");
	let _ = std::fs::remove_file(path);
	assert!(
		output.status.success(),
		"{}\n{js}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(
		String::from_utf8_lossy(&output.stdout).trim(),
		"2\n1\n7n\n3\n8n\n5\nquestion\n6\n9n\n7\npanic\n9\n8\ncleanup-2\n11\n10\nprimary-3\n12\ncancelled\n13\ndefected-2-true"
	);
}

*/
#[test]
fn embedded_std_check_facades_serve_entry_and_library_projects() {
	let entry = "import std/collections/tree with (Tree)\n\
		func main(): void = { let tree = Tree.Leaf(value = 1) }\n";
	let entry_load = only_entry("app", entry);
	let entry_diags = check_project_with_embedded_std("app", &entry_load);
	assert!(
		entry_diags.is_empty(),
		"embedded entry check should resolve std: {entry_diags:?}"
	);

	let library = "import std/collections/tree with (Tree)\n\
		public func leaf(): Tree<int> = Tree.Leaf(value = 1)\n";
	let library_load = only_entry("library", library);
	let library_diags = check_project_library_with_embedded_std("library", &library_load);
	assert!(
		library_diags.is_empty(),
		"embedded library check should resolve std without requiring main: {library_diags:?}"
	);
}

/// A std-to-std link: one std module `import`s ANOTHER std module (not the
/// user's own project). Both are served by a synthetic virtual
/// `std_provider` (no real on-disk file needs to exist for this — it proves
/// the MECHANISM: a `std::`-keyed module's own imports are resolved exactly
/// like any other module's, joining the same graph, topological order, and
/// cycle detection).
#[test]
fn std_to_std_import_resolves_and_links() {
	let provider = |path: &str| -> Option<String> {
		match path {
			"a" => Some("import std/b with (thing)\npublic func use_it(): int = thing() + 1".to_string()),
			"b" => Some("public func thing(): int = 41".to_string()),
			_ => None,
		}
	};
	let entry = "import std/a with (use_it)\nfunc main(): void = {}\nfunc demo(): int = use_it()";
	let load = only_entry("main", entry);

	let diags = check_project_with_std("main", &load, &provider);
	assert!(
		diags.is_empty(),
		"expected a std module importing another std module to resolve and check cleanly, got: {diags:?}"
	);

	let compiled = compile_project_with_std("main", &load, &provider)
		.expect("expected the std-to-std graph to compile");
	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));

	let dir = std::env::temp_dir();
	let path = dir.join(format!(
		"nymph_std_provider_link_{}.mjs",
		std::process::id()
	));
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
	assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42n");
}

/// A package root without a compiler graph alias is unresolved. One-shot
/// source loaders do not invent package identities from import spelling.
#[test]
fn an_unknown_package_import_is_unresolved() {
	let entry = "import some_other_pkg/thing\nfunc main(): void = {}";
	let load = only_entry("main", entry);

	// `std_provider` is reserved for the exact `std` identity domain.
	let diags = check_project_with_std("main", &load, &|_| {
		panic!("std_provider must not be consulted for a non-`std` package root")
	});

	assert!(
		diags.iter().any(|d| d.diag.code == "IMPORT-UNRESOLVED"),
		"expected IMPORT-UNRESOLVED for a package without an exact graph alias, got: {diags:?}"
	);
}

/// A `std/…` path that the provider doesn't recognize is an ordinary
/// `IMPORT-UNRESOLVED` — exactly like a missing `@/…`/`./…` project file —
/// never silently accepted and never a different, std-specific error code.
#[test]
fn an_unresolvable_std_path_is_import_unresolved() {
	let entry = "import std/does_not_exist_anywhere\nfunc main(): void = {}";
	let load = only_entry("main", entry);

	let diags = check_project_library_with_std("main", &load, &|_| None);
	assert!(
		diags.iter().any(|d| d.diag.code == "IMPORT-UNRESOLVED"),
		"expected IMPORT-UNRESOLVED for a std path the provider doesn't serve, got: {diags:?}"
	);
}

#[test]
fn custom_provider_has_no_embedded_std_fallback() {
	let entry = "import std/io with (println)\nfunc main(): void = {}";
	let load = only_entry("main", entry);
	let requested = std::cell::RefCell::new(Vec::new());

	let diags = check_project_with_std("main", &load, &|path| {
		requested.borrow_mut().push(path.to_string());
		None
	});

	assert_eq!(requested.into_inner(), ["io"]);
	assert!(
		diags
			.iter()
			.any(|diag| diag.diag.code == "IMPORT-UNRESOLVED"),
		"an absent custom builtin must not fall back to embedded std: {diags:?}"
	);
}

/// The `std::`-prefixed module key mangles to valid, runnable JS: Slice B's
/// design note is that the mangle scheme (`$m{numeric-tag}$name`) never
/// embeds the module KEY string at all, so a `::` in the key is inert — this
/// exercises it end-to-end (a deep multi-segment std path, imported `with` a
/// specific name, actually called from the entry module and run under Node),
/// rather than just asserting on the JS text.
#[test]
fn a_std_key_mangles_to_valid_runnable_js() {
	let entry = "import std/collections/tree with (Tree)\n\
		func main(): void = {}\n\
		func depth_one(): int = match (Tree.Node(children = #[])) {\n\
		\tTree.Leaf(...) -> -1,\n\
		\tTree.Node(...) -> 0,\n\
		}\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("expected a deep `std/collections/tree` key to compile to valid JS");
	let call = compiled.entry_symbol("depth_one");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));

	let dir = std::env::temp_dir();
	let path = dir.join(format!(
		"nymph_std_provider_mangle_{}.mjs",
		std::process::id()
	));
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
	assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "0n");
}

/// Regression: a `std::`-keyed module's OWN genuine type error, found on its
/// own semantic-analysis turn, must
/// be reported — never silently dropped. `check_project_with_std` must
/// return this diagnostic, not an empty list, exactly as it would for an
/// ordinary `@/…`-keyed project module with the same bug.
#[test]
fn a_genuine_type_error_in_a_std_modules_own_turn_is_reported_not_swallowed() {
	let provider = |path: &str| -> Option<String> {
		(path == "bad").then(|| "func thing(): int = true".to_string())
	};
	let entry = "import std/bad with (thing)\nfunc main(): void = {}\nfunc use_it(): int = thing()";
	let load = only_entry("main", entry);

	let diags = check_project_with_std("main", &load, &provider);
	assert!(
		diags.iter().any(|d| d.module == "std::bad"),
		"expected the std module's own genuine type error to be reported against \
		 `std::bad`, got: {diags:?}"
	);
}

/// Regression sibling of the above, at the `compile_project_with_std` layer:
/// a std module with a genuine own-turn type error must fail the whole
/// compile (`Err`, carrying the diagnostic) — never `Ok` with emitted JS that
/// silently skipped that module's codegen (which would otherwise bundle a
/// dangling `import` from a module specifier nothing ever produced).
#[test]
fn a_genuine_type_error_in_a_std_module_fails_the_whole_compile() {
	let provider = |path: &str| -> Option<String> {
		(path == "bad").then(|| "func thing(): int = true".to_string())
	};
	let entry = "import std/bad with (thing)\nfunc main(): void = {}\nfunc use_it(): int = thing()";
	let load = only_entry("main", entry);

	let result = compile_project_with_std("main", &load, &provider);
	let Err(diags) = result else {
		panic!(
			"expected the std module's genuine type error to fail the compile, but it \
			 reported `Ok` (silently broken JS) instead"
		);
	};
	assert!(
		diags.iter().any(|d| d.module == "std::bad"),
		"expected the compile failure to carry the std module's own diagnostic, got: {diags:?}"
	);
}
