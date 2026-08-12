//! End-to-end proof of the FREE-FUNCTION extension to external linkage (the
//! print/io slice): unlike Gap 3 (L0/L1/L2)'s method-call `HirExpr::ExternCall`
//! (always `$_this`-first, dispatched off a `MemberAccess` callee resolved
//! through the interface solver), `print`/`println` are bare, receiver-less
//! calls (`ExprKind::Call { func: Identifier(..), .. }`) to a TOP-LEVEL
//! `external` func — a callee shape the pre-existing dispatch in
//! stable runtime lowering never recognized at all, falling through to
//! a plain `HirExpr::Call` to a name with no JS binding (silent-wrong-JS, a
//! runtime `ReferenceError`).
//!
//! `stdlib/src/io.nym` is import-free (only needs the ambient `string`/`void`
//! builtins), so — unlike `std_linkage.rs`'s synthetic `list` provider — this
//! drives the REAL on-disk `io.nym`/`io.ts` through the real-stdlib provider
//! pattern (`std_provider.rs`'s `real_stdlib_provider`), a strict improvement:
//! a full, non-synthetic e2e proof of both the mechanism and the module.

use std::path::PathBuf;

use nymph_compiler::compile_project_with_std;

fn stdlib_src_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../stdlib/src")
		.canonicalize()
		.unwrap()
}

/// The real, on-disk-backed `std_provider` — mirrors `std_provider.rs`'s
/// `real_stdlib_provider` exactly.
fn real_stdlib_provider(path: &str) -> Option<String> {
	std::fs::read_to_string(stdlib_src_root().join(format!("{path}.nym"))).ok()
}

fn only_entry(entry_key: &'static str, entry_src: &'static str) -> impl Fn(&str) -> Option<String> {
	move |key: &str| (key == entry_key).then(|| entry_src.to_string())
}

fn run_node(js: &str, tag: &str) -> String {
	let dir = std::env::temp_dir();
	let path = dir.join(format!("nymph_std_io_{tag}_{}.mjs", std::process::id()));
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
	String::from_utf8_lossy(&output.stdout).to_string()
}

/// `func main(): void = println("hello nymph")` — the brief's own headline
/// proof: a bare top-level call to a linked free-function external compiles,
/// bundles (the injected, stripped `io.ts` intrinsic resolves and inlines
/// into the graph, no surviving `import` statement), and runs under Node.
#[test]
fn println_hello_nymph_compiles_bundles_and_runs() {
	let entry = "import std/io with (println)\n\
		func main(): void = println(\"hello nymph\")\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("expected `import std/io` + a bare `println(..)` call to compile");

	assert!(
		compiled.js.contains("println("),
		"expected the bundle to contain a `println(` call, got:\n{}",
		compiled.js
	);
	assert!(
		compiled.js.contains("console.log"),
		"expected the injected, stripped `io.ts` intrinsic body (`console.log`) \
		 to be inlined into the bundle, got:\n{}",
		compiled.js
	);
	assert!(
		!compiled.js.contains("from \"std/io\""),
		"expected rolldown to fully inline the linked `std/io` intrinsic — no \
		 surviving `import ... from \"std/io\"` — got:\n{}",
		compiled.js
	);

	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));

	assert_eq!(run_node(&js, "hello").trim(), "hello nymph");
}

/// `print` (no trailing newline) vs `println` (adds one) — the exact
/// distinction the runtime `io.ts` implementations must preserve:
/// `process.stdout.write` for `print`, `console.log` for `println`.
#[test]
fn print_has_no_newline_and_println_does() {
	let entry = "import std/io with (print, println)\n\
		func main(): void = {\n\
		\tprint(\"a\")\n\
		\tprintln(\"b\")\n\
		}\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("expected `print`/`println` to both compile as linked free-function externals");

	assert!(
		compiled.js.contains("process.stdout.write"),
		"expected the injected `io.ts` intrinsic's `print` body \
		 (`process.stdout.write`) to be inlined into the bundle, got:\n{}",
		compiled.js
	);

	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));

	let output = run_node(&js, "print_println");
	assert_eq!(
		output, "ab\n",
		"expected `print(\"a\")` with no newline immediately followed by \
		 `println(\"b\")` with one, got: {output:?}"
	);
}

#[test]
fn display_and_debug_render_primitives_and_nested_collections() {
	let entry = r#"import std/io with (println)
func main(): void = {
	println(1)
	println(1.0)
	println(true)
	println('x')
	println("hi")
	println(#["a", "b"].debug())
}"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("Display/Debug values should compile through std/io");
	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));
	assert_eq!(
		run_node(&js, "display_debug"),
		"1\n1.0\ntrue\nx\nhi\n#[\"a\", \"b\"]\n"
	);
}

#[test]
fn composite_display_and_debug_use_nested_debug_overrides() {
	let entry = r#"import std/io with (println)
struct Marker(v: int) {
	impl Debug {
		func debug(): string = "<marker>"
	}
}
struct Holder(value: Marker, v: int)
func main(): void = {
	println(#[Marker(v = 1)].debug())
	println(Holder(v = 2, value = Marker(v = 1)).debug())
	println(Holder(v = 2, value = Marker(v = 1)))
}"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("nested Debug overrides should compile through composite Display/Debug");
	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));
	assert_eq!(
		run_node(&js, "nested_debug"),
		"#[<marker>]\nHolder(value: <marker>, v: 2)\nHolder(value: <marker>, v: 2)\n"
	);
}

#[test]
fn debug_escapes_control_characters() {
	let entry = r#"import std/io with (println)
func main(): void = {
	println('\n'.debug())
	println('\r'.debug())
	println('\t'.debug())
}"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("Debug should compile for escaped characters");
	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));
	assert_eq!(run_node(&js, "debug_chars"), "'\\n'\n'\\r'\n'\\t'\n");
}

#[test]
fn interpolation_uses_overridden_display_and_boxes_once() {
	let entry = r#"import std/io with (println)
struct Label(value: int) {
	impl Display {
		func display(): string = "label=${this.value}"
	}
}
func rendered(): string = "value=${Label(value = 7)}!"
func main(): void = println(rendered())"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("interpolation should resolve Display and compile");
	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));
	assert_eq!(run_node(&js, "interpolation_display"), "value=label=7!\n");
}

#[test]
fn into_string_delegates_to_display() {
	let entry = r#"import std/io with (println)
struct Label(value: int) {
	impl Display {
		func display(): string = "label=${this.value}"
	}
}
func main(): void = {
	println(1 as string)
	println(Label(value = 7) as string)
}"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("Into<string> should be available through the blanket Display implementation");
	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));
	assert_eq!(run_node(&js, "display_into_string"), "1\nlabel=7\n");
}

#[test]
fn unrelated_inherent_methods_do_not_override_display_protocols() {
	let entry = r#"import std/io with (println)
struct Coin() {
	func display(): int = 7
	func debug(): int = 8
}
func main(): void = {
	println(Coin())
	println(#[Coin()].debug())
}"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("unrelated inherent method names should not affect blanket Display/Debug");
	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));
	assert_eq!(run_node(&js, "nominal_display"), "Coin\n#[Coin]\n");
}

#[test]
fn println_accepts_void_through_blanket_display() {
	let entry = r#"import std/io with (println)
func noop(): void = {}
func main(): void = println(noop())"#;
	let load = only_entry("main", entry);
	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("void should satisfy the blanket Display implementation");
	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));
	assert_eq!(run_node(&js, "display_void"), "void\n");
}

/// A program that does NOT `import std/io` is completely unaffected — the
/// free-function dispatch only ever consults `self.prelude_modules` (core
/// plus whatever the entry module actually imports), so with no io import
/// there is no matching top-level `external` func and an ordinary bare call
/// keeps taking the ordinary `HirExpr::Call` path, exactly as before this
/// slice.
#[test]
fn a_program_without_importing_io_is_unaffected() {
	let entry = "func add(a: int, b: int): int = a + b\n\
		func main(): void = {}\n\
		func demo(): int = add(2, 3)\n";
	let load = only_entry("main", entry);

	let compiled = compile_project_with_std("main", &load, &real_stdlib_provider)
		.expect("expected an io-free project to keep compiling exactly as before this slice");

	assert!(
		!compiled.js.contains("std/io"),
		"expected the bundle of an io-free project to carry no trace of the \
		 `std/io` intrinsic at all, got:\n{}",
		compiled.js
	);

	let call = compiled.entry_symbol("demo");
	let mut js = compiled.js;
	js.push_str(&format!("\nconsole.log({call}().v);\n"));

	assert_eq!(run_node(&js, "no_io").trim(), "5");
}

/// `import std/io` alone (through the real on-disk `stdlib/src/io.nym`, no
/// synthetic scaffolding) type-checks with zero diagnostics.
#[test]
fn import_std_io_typechecks_cleanly() {
	use nymph_compiler::check_project_with_std;

	let entry = "import std/io with (print, println)\n\
		func main(): void = {\n\
		\tprint(\"a\")\n\
		\tprintln(\"b\")\n\
		}\n";
	let load = only_entry("main", entry);

	let diags = check_project_with_std("main", &load, &real_stdlib_provider);
	assert!(
		diags.is_empty(),
		"expected `import std/io` + `print`/`println` calls to check cleanly, got: {diags:?}"
	);
}
