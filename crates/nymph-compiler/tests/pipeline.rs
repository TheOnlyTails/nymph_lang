//! Integration tests for the `nymph-compiler` facade: `compile` and `check`,
//! plus their entry-mode counterparts `compile_entry` and `check_entry`.

use nymph_compiler::{check, check_entry, compile, compile_entry};

fn run_node(js: &str, call: &str) -> String {
	use std::io::Write;

	let path = std::env::temp_dir().join(format!(
		"nymph_compile_entry_{}_{}.mjs",
		std::process::id(),
		std::thread::current().name().unwrap_or("test")
	));
	let mut file = std::fs::File::create(&path).expect("create temporary JavaScript module");
	write!(file, "{js}\nconsole.log({call});").expect("write temporary JavaScript module");

	let output = std::process::Command::new("node")
		.arg(&path)
		.output()
		.expect("run emitted JavaScript with Node");
	let _ = std::fs::remove_file(path);
	assert!(
		output.status.success(),
		"node failed:\n{}\n--- js ---\n{js}",
		String::from_utf8_lossy(&output.stderr)
	);
	String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn compiles_a_valid_program() {
	let result = compile("func double(n: int): int = n * 2", "test");
	let js = result.expect("valid program should compile");
	assert!(js.contains("double"));
	assert_eq!(run_node(&js, "double(new NInt(3)).v"), "6");
}

#[test]
fn reports_type_errors() {
	let result = compile("func f(): int = true", "test");
	let diags = result.expect_err("type-mismatched program should not compile");
	assert!(!diags.is_empty());
}

#[test]
fn reports_parse_errors() {
	let result = compile("func f(: int = 1", "test");
	assert!(result.is_err());
}

#[test]
fn check_returns_all_diagnostics() {
	let diags = check("func f(): int = true", "test");
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.is_error()));
}

#[test]
fn check_is_clean_for_a_valid_program() {
	let diags = check("func double(n: int): int = n * 2", "test");
	assert!(!diags.iter().any(|d| d.is_error()));
}

// ── Entry mode (`check_entry` / `compile_entry`) ────────────────────────────
//
// GG1: entry mode is additive — the same source that's clean under library
// mode (`check`/`compile`) can error under entry mode if it has no valid
// top-level `main`, and vice versa is never true (entry mode is strictly more
// demanding than library mode).

#[test]
fn library_mode_is_clean_for_a_source_with_no_main() {
	let diags = check("func double(n: int): int = n * 2", "test");
	assert!(!diags.iter().any(|d| d.is_error()));
}

#[test]
fn entry_mode_errors_on_the_same_source_with_no_main() {
	let diags = check_entry("func double(n: int): int = n * 2", "test");
	assert!(
		diags.iter().any(|d| d.is_error()),
		"expected an entry-mode error for a source with no `main`, got: {diags:?}"
	);
}

#[test]
fn entry_mode_is_clean_for_a_valid_main() {
	let diags = check_entry("func main(): void = {}", "test");
	assert!(!diags.iter().any(|d| d.is_error()), "diags: {diags:?}");
}

#[test]
fn compile_entry_compiles_a_program_with_a_valid_main() {
	let result = compile_entry("func main(): void = {}", "test");
	let js = result.expect("valid entry program should compile");
	assert!(js.contains("main"));
}

#[test]
fn compile_entry_preserves_callable_names_and_canonical_option_identity() {
	let source = r#"
		func intrinsic_option(): Option<char> = "x".char_at(0u)
		func source_option(): Option<char> = Some(value = 'x')
		func main(): void = {}
	"#;
	let js = compile_entry(source, "entry_option_owner").expect("valid main should compile");

	for callable in ["main", "intrinsic_option", "source_option"] {
		assert!(
			js.contains(&format!("function {callable}(")),
			"standalone entry callable `{callable}` must remain unmangled: {js}"
		);
	}
	assert_eq!(
		run_node(
			&js,
			"Object.getPrototypeOf(intrinsic_option()) === Object.getPrototypeOf(source_option())",
		),
		"true"
	);
}

#[test]
fn standalone_diagnostic_paths_never_become_virtual_module_keys() {
	for path in ["std/option", "std/box", "std::anything"] {
		let js = compile("func identity(n: int): int = n", path)
			.unwrap_or_else(|diags| panic!("diagnostic path `{path}` must compile: {diags:?}"));
		assert!(js.contains("function identity("), "path `{path}`: {js}");
	}
}

#[test]
fn compile_entry_reports_parse_diagnostics_before_missing_main() {
	let diags = compile_entry("func broken(: int = 1", "malformed/source.nym")
		.expect_err("malformed entry without main must not compile");
	let codes = diags
		.iter()
		.map(|diag| diag.code.as_str())
		.collect::<Vec<_>>();

	assert!(
		codes.first().is_some_and(|code| code.starts_with('1')),
		"parse diagnostics must come first: {diags:?}"
	);
	assert!(
		codes.iter().skip(1).any(|code| code.starts_with('2')),
		"checker/entry diagnostics must follow parse diagnostics: {diags:?}"
	);
}

#[test]
fn standalone_private_function_remains_callable_by_unmangled_name() {
	let source = "private func secret(): int = 42\nfunc main(): void = {}";
	let js = compile_entry(source, "private_entry").expect("private standalone function is valid");

	assert_eq!(run_node(&js, "secret().v"), "42");
}

#[test]
fn compile_entry_errors_on_a_program_with_no_main() {
	let result = compile_entry("func double(n: int): int = n * 2", "test");
	let diags = result.expect_err("a program with no `main` should not compile in entry mode");
	assert!(!diags.is_empty());
}

#[test]
fn compile_still_succeeds_without_a_main_in_library_mode() {
	// The same source `compile_entry` rejects above compiles fine in plain
	// library mode: `compile`/`check` are unaffected by entry validation.
	let result = compile("func double(n: int): int = n * 2", "test");
	assert!(result.is_ok(), "library mode should not require `main`");
}
