//! Integration tests: spawn the real `nymph` binary and assert on its
//! observable behavior (exit code, stdout, stderr) for `check`, `build`,
//! `run`, and command-line parsing.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// A unique path in the system temp dir, isolated across parallel test threads
/// (mirrors the pid + monotonic-counter pattern in
/// `crates/nymph-codegen/tests/run_node.rs`).
fn unique_temp_path(prefix: &str, ext: &str) -> std::path::PathBuf {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	std::env::temp_dir().join(format!("{prefix}_{}_{unique}.{ext}", std::process::id()))
}

/// Write `source` to a fresh temp `.nym` file and return its path.
fn write_source(source: &str) -> std::path::PathBuf {
	let path = unique_temp_path("nymph_cli_src", "nym");
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(source.as_bytes()).unwrap();
	path
}

/// Write `source` to a fresh temp *directory* under a file literally named
/// `main.nym`, and return its path. `write_source`'s stems (`nymph_cli_src_
/// <pid>_<n>`) can never equal `main`, so `check`/`build`'s stem-based entry
/// detection needs a helper that can actually produce that stem — a unique
/// directory keeps concurrent test threads from colliding on the shared
/// `main.nym` name.
fn write_main_source(source: &str) -> std::path::PathBuf {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir().join(format!(
		"nymph_cli_main_dir_{}_{unique}",
		std::process::id()
	));
	std::fs::create_dir_all(&dir).unwrap();
	let path = dir.join("main.nym");
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(source.as_bytes()).unwrap();
	path
}

fn write_manifest_fixture(manifest: &[u8], source: &str) -> std::path::PathBuf {
	let dir = unique_temp_path("nymph_cli_manifest", "dir");
	std::fs::create_dir_all(dir.join("src")).unwrap();
	std::fs::write(dir.join("nymph.toml"), manifest).unwrap();
	let source_path = dir.join("src/main.nym");
	std::fs::write(&source_path, source).unwrap();
	source_path
}

struct Output {
	status: std::process::ExitStatus,
	stdout: String,
	stderr: String,
}

/// Run `nymph` with `args`, colors disabled so assertions on plain text
/// are stable regardless of the shell's ANSI settings.
fn nymph(args: &[&str]) -> Output {
	nymph_in(args, std::env::current_dir().unwrap())
}

fn nymph_in(args: &[&str], current_dir: impl AsRef<std::path::Path>) -> Output {
	let out = Command::new(env!("CARGO_BIN_EXE_nymph"))
		.args(args)
		.current_dir(current_dir)
		.env("NO_COLOR", "1")
		.env_remove("FORCE_COLOR")
		.output()
		.expect("spawn nymph");
	Output {
		status: out.status,
		stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
		stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
	}
}

fn write_project(entry: &str, source: &str) -> std::path::PathBuf {
	let root = unique_temp_path("nymph_cli_project", "dir");
	let entry_path = root.join("src").join(entry);
	std::fs::create_dir_all(entry_path.parent().unwrap()).unwrap();
	std::fs::write(
		root.join("nymph.toml"),
		format!("[package]\nname='fixture'\nversion='1.0.0'\n[build]\nentry='{entry}'\n"),
	)
	.unwrap();
	std::fs::write(&entry_path, source).unwrap();
	root
}

#[test]
fn manifest_is_a_global_flag_before_or_after_the_subcommand() {
	let root = write_project("main.nym", "func main(): void = {}\n");
	let manifest = root.join("nymph.toml");
	let outside = unique_temp_path("nymph_cli_outside", "dir");
	std::fs::create_dir_all(&outside).unwrap();

	for args in [
		vec!["--manifest", manifest.to_str().unwrap(), "check"],
		vec!["check", "--manifest", manifest.to_str().unwrap()],
	] {
		let out = nymph_in(&args, &outside);
		assert!(
			out.status.success(),
			"global manifest placement should work; stdout: {} stderr: {}",
			out.stdout,
			out.stderr
		);
	}

	std::fs::remove_dir_all(root).unwrap();
	std::fs::remove_dir_all(outside).unwrap();
}

#[test]
fn explicit_manifest_has_command_parity_and_bases_fields_on_its_directory() {
	let root = unique_temp_path("nymph_cli_selected_project", "dir");
	let manifest = root.join("metadata/project.toml");
	let source = root.join("metadata/code/bin/start.nym");
	std::fs::create_dir_all(source.parent().unwrap()).unwrap();
	std::fs::write(
		&manifest,
		"[package]\nname='fixture'\nversion='1.0.0'\nsrc='code'\n[build]\nentry='bin/start.nym'\n",
	)
	.unwrap();
	std::fs::write(&source, "func main(): void = {}\n").unwrap();
	let outside = unique_temp_path("nymph_cli_outside", "dir");
	std::fs::create_dir_all(&outside).unwrap();
	let relative_manifest = std::path::Path::new("..")
		.join(root.file_name().unwrap())
		.join("metadata/project.toml");

	for command in ["run", "check", "build"] {
		let out = nymph_in(
			&["--manifest", relative_manifest.to_str().unwrap(), command],
			&outside,
		);
		assert!(
			out.status.success(),
			"{command} should use the selected manifest outside its root; stdout: {} stderr: {}",
			out.stdout,
			out.stderr
		);
	}
	assert!(source.with_extension("mjs").is_file());

	std::fs::remove_dir_all(root).unwrap();
	std::fs::remove_dir_all(outside).unwrap();
}

#[test]
fn explicit_manifest_missing_and_invalid_paths_are_authoritative() {
	let root = write_project("main.nym", "func main(): void = {}\n");
	let missing = root.join("missing.toml");
	let invalid = root.join("invalid.toml");
	std::fs::write(&invalid, "not = [toml").unwrap();

	for command in ["run", "check", "build"] {
		let missing_out = nymph_in(&["--manifest", missing.to_str().unwrap(), command], &root);
		assert_eq!(missing_out.status.code(), Some(1), "{command}");
		assert!(
			missing_out.stderr.contains("could not read manifest")
				&& missing_out.stderr.contains(&missing.display().to_string()),
			"{command} must not fall back to the valid nymph.toml: {}",
			missing_out.stderr
		);

		let invalid_out = nymph_in(&["--manifest", invalid.to_str().unwrap(), command], &root);
		assert_eq!(invalid_out.status.code(), Some(1), "{command}");
		assert!(
			invalid_out.stderr.contains("malformed TOML")
				&& invalid_out.stderr.contains(&invalid.display().to_string()),
			"{command} must report the selected invalid path: {}",
			invalid_out.stderr
		);
	}

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_manifest_never_discovers_from_an_explicit_source() {
	let selected = write_project("main.nym", "func main(): void = {}\n");
	let other = write_project("main.nym", "func main(): void = {}\n");
	let other_source = other.join("src/main.nym");
	let out = nymph_in(
		&[
			"--manifest",
			selected.join("nymph.toml").to_str().unwrap(),
			"check",
			other_source.to_str().unwrap(),
		],
		&other,
	);

	assert_eq!(out.status.code(), Some(1));
	assert!(
		out.stderr.contains("outside source root"),
		"the source's nearby manifest must not replace the selected one: {}",
		out.stderr
	);

	std::fs::remove_dir_all(selected).unwrap();
	std::fs::remove_dir_all(other).unwrap();
}

#[test]
fn target_matrix_project_without_file_uses_manifest_entry_relative_to_src() {
	let root = write_project("bin/start.nym", "func main(): void = {}\n");
	for command in ["run", "check", "build"] {
		let out = nymph_in(&[command], &root);
		assert!(
			out.status.success(),
			"{command} should use build.entry; stdout: {} stderr: {}",
			out.stdout,
			out.stderr
		);
	}
	assert!(root.join("src/bin/start.mjs").is_file());
	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn target_matrix_project_explicit_file_is_resolved_from_the_same_project() {
	let root = write_project("bin/start.nym", "func main(): void = {}\n");
	let explicit = root.join("src/tools/task.nym");
	std::fs::create_dir_all(explicit.parent().unwrap()).unwrap();
	std::fs::write(&explicit, "func main(): void = {}\n").unwrap();
	for command in ["run", "check", "build"] {
		let out = nymph_in(&[command, explicit.to_str().unwrap()], &root);
		assert!(
			out.status.success(),
			"{command} should accept an explicit project file; stdout: {} stderr: {}",
			out.stdout,
			out.stderr
		);
	}
	assert!(explicit.with_extension("mjs").is_file());
	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_relative_target_is_normalized_before_project_discovery() {
	let root = write_project("main.nym", "func main(): void = {}\n");
	let nested = root.join("src/nested");
	std::fs::create_dir_all(&nested).unwrap();
	std::fs::write(nested.join("nymph.toml"), "not = [toml").unwrap();

	let out = nymph_in(&["check", "../main.nym"], &nested);
	assert!(
		out.status.success(),
		"the normalized target belongs to the outer project, not the nested manifest: {}",
		out.stderr
	);

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_entry_replaces_filename_stem_entry_policy() {
	let root = write_project("bin/start.nym", "func helper(): int = 1\n");
	let named_main = root.join("src/main.nym");
	std::fs::write(&named_main, "func helper(): int = 1\n").unwrap();

	for command in ["check", "build"] {
		let entry_out = nymph_in(&[command], &root);
		assert!(
			entry_out.stderr.contains("no `main` function found"),
			"{command} should treat custom build.entry as the entry module: {}",
			entry_out.stderr
		);

		let library_out = nymph_in(&[command, named_main.to_str().unwrap()], &root);
		assert!(
			library_out.status.success(),
			"{command} must not infer entry mode from the main.nym filename: {}",
			library_out.stderr
		);
	}

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn target_matrix_loose_explicit_file_works_for_every_command() {
	let path = write_source("func main(): void = {}\n");
	for command in ["run", "check", "build"] {
		let out = nymph(&[command, path.to_str().unwrap()]);
		assert!(
			out.status.success(),
			"{command} should accept a loose file; stdout: {} stderr: {}",
			out.stdout,
			out.stderr
		);
	}
	let _ = std::fs::remove_file(path.with_extension("mjs"));
	std::fs::remove_file(path).unwrap();
}

#[test]
fn target_matrix_loose_explicit_file_resolves_sibling_imports_for_every_command() {
	let path =
		write_source("import @/helper with (value)\nfunc main(): void = { let result = value() }\n");
	let helper = path.parent().unwrap().join("helper.nym");
	std::fs::write(&helper, "public func value(): int = 1\n").unwrap();
	for command in ["run", "check", "build"] {
		let out = nymph(&[command, path.to_str().unwrap()]);
		assert!(
			out.status.success(),
			"{command} should resolve a loose target's sibling imports; stdout: {} stderr: {}",
			out.stdout,
			out.stderr
		);
	}
	let _ = std::fs::remove_file(path.with_extension("mjs"));
	std::fs::remove_file(helper).unwrap();
	std::fs::remove_file(path).unwrap();
}

#[test]
fn target_matrix_without_project_or_file_errors_actionably() {
	let root = unique_temp_path("nymph_cli_empty", "dir");
	std::fs::create_dir_all(&root).unwrap();
	for command in ["run", "check", "build"] {
		let out = nymph_in(&[command], &root);
		assert_eq!(out.status.code(), Some(1), "{command}: {}", out.stderr);
		assert!(
			out.stderr.contains("no nymph.toml found"),
			"{command}: {}",
			out.stderr
		);
		assert!(
			out.stderr.contains("pass a .nym file"),
			"{command}: {}",
			out.stderr
		);
	}
	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn target_matrix_rejects_missing_and_outside_project_sources() {
	let root = write_project("start.nym", "func main(): void = {}\n");
	let missing = root.join("src/missing.nym");
	let outside = root.join("outside.nym");
	std::fs::write(&outside, "func main(): void = {}\n").unwrap();
	for command in ["run", "check", "build"] {
		let missing_out = nymph_in(&[command, missing.to_str().unwrap()], &root);
		assert!(
			missing_out
				.stderr
				.contains("target source file does not exist"),
			"{command}: {}",
			missing_out.stderr
		);
		let outside_out = nymph_in(&[command, outside.to_str().unwrap()], &root);
		assert!(
			outside_out.stderr.contains("outside source root"),
			"{command}: {}",
			outside_out.stderr
		);
	}
	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn check_reports_ok_for_a_well_typed_program() {
	let path = write_source("func add(a: int, b: int): int = a + b");
	let out = nymph(&["check", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		out.status.success(),
		"expected exit 0, got {:?}\nstdout: {}\nstderr: {}",
		out.status.code(),
		out.stdout,
		out.stderr
	);
	assert!(out.stdout.contains("ok"), "stdout was: {}", out.stdout);
}

#[test]
fn run_build_and_check_reject_a_found_invalid_manifest_without_loose_retry() {
	let source = write_manifest_fixture(b"not = [toml", "func main(): void = {}\n");
	let manifest = source
		.parent()
		.unwrap()
		.parent()
		.unwrap()
		.join("nymph.toml");
	for command in ["run", "build", "check"] {
		let out = nymph(&[command, source.to_str().unwrap()]);
		assert_eq!(out.status.code(), Some(1), "{command}: {}", out.stderr);
		assert!(
			out.stderr.contains("malformed TOML"),
			"{command}: {}",
			out.stderr
		);
		assert!(
			out.stderr.contains(&manifest.display().to_string()),
			"{command}: {}",
			out.stderr
		);
	}
	assert!(!source.with_extension("mjs").exists());
	std::fs::remove_dir_all(source.parent().unwrap().parent().unwrap()).unwrap();
}

#[test]
fn check_reports_a_type_error_with_location_and_message() {
	let path = write_source("func f(): int = true");
	let out = nymph(&["check", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!out.status.success(),
		"expected nonzero exit for a type error"
	);
	let filename = path.to_str().unwrap();
	assert!(
		out.stderr.contains(filename),
		"stderr should mention the file path:\n{}",
		out.stderr
	);
	// ariadne's report includes a `filename:line:col` locator line.
	assert!(
		out.stderr.contains(&format!("{filename}:1:")),
		"stderr should include a file:line:col locator:\n{}",
		out.stderr
	);
}

#[test]
fn build_writes_the_compiled_js_on_success() {
	let path = write_source("func add(a: int, b: int): int = a + b");
	let output_path = path.with_extension("mjs");
	let _ = std::fs::remove_file(&output_path);

	let out = nymph(&["build", path.to_str().unwrap()]);

	assert!(
		out.status.success(),
		"expected exit 0, got {:?}\nstderr: {}",
		out.status.code(),
		out.stderr
	);
	assert!(
		output_path.exists(),
		"expected {} to be written",
		output_path.display()
	);
	let js = std::fs::read_to_string(&output_path).unwrap();
	assert!(js.contains("add"), "emitted JS was: {js}");

	let _ = std::fs::remove_file(&path);
	let _ = std::fs::remove_file(&output_path);
}

#[test]
fn build_writes_nothing_on_a_type_error() {
	let path = write_source("func f(): int = true");
	let output_path = path.with_extension("mjs");
	let _ = std::fs::remove_file(&output_path);

	let out = nymph(&["build", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!out.status.success(),
		"expected nonzero exit for a build failure"
	);
	assert!(
		!output_path.exists(),
		"no output file should be written on failure"
	);
}

#[test]
fn build_leaves_a_previously_built_artifact_intact_when_a_later_build_fails() {
	// Fix 3: a failed rebuild must NEVER delete (or otherwise touch) whatever
	// was already at the output path — including a real artifact from an
	// earlier successful build of the same source.
	let path = write_source("func add(a: int, b: int): int = a + b");
	let output_path = path.with_extension("mjs");
	let _ = std::fs::remove_file(&output_path);

	// First build succeeds and writes real JS to `output_path`.
	let first = nymph(&["build", path.to_str().unwrap()]);
	assert!(first.status.success(), "stderr: {}", first.stderr);
	assert!(output_path.exists());
	let original_js = std::fs::read_to_string(&output_path).unwrap();

	// Overwrite the source with a version that fails to compile, then
	// rebuild to the same output path.
	std::fs::write(&path, "func f(): int = true").unwrap();
	let second = nymph(&["build", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!second.status.success(),
		"expected nonzero exit for a build failure"
	);
	assert!(
		output_path.exists(),
		"the artifact from the earlier successful build must survive a later failed build: {}",
		output_path.display()
	);
	assert_eq!(
		std::fs::read_to_string(&output_path).unwrap(),
		original_js,
		"the surviving artifact's contents must be exactly what the successful build wrote"
	);

	let _ = std::fs::remove_file(&output_path);
}

#[test]
fn build_failure_does_not_touch_an_unrelated_file_at_the_output_path() {
	// Fix 3, the other half: the file at `-o` doesn't have to be a stale
	// nymph-build artifact at all — it could be anything already sitting at
	// that path. A failed build must leave it byte-for-byte alone.
	let path = write_source("func f(): int = true");
	let output_path = unique_temp_path("nymph_cli_unrelated", "mjs");
	std::fs::write(&output_path, "totally unrelated pre-existing content\n").unwrap();

	let out = nymph(&[
		"build",
		path.to_str().unwrap(),
		"-o",
		output_path.to_str().unwrap(),
	]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!out.status.success(),
		"expected nonzero exit for a build failure"
	);
	assert_eq!(
		std::fs::read_to_string(&output_path).unwrap(),
		"totally unrelated pre-existing content\n",
		"a failed build must never modify a file at -o that it didn't create"
	);

	let _ = std::fs::remove_file(&output_path);
}

#[test]
fn build_supports_first_class_range_values() {
	let path = write_source("func main(): void = {\n  let r = 1..5\n}");
	let output_path = unique_temp_path("nymph_cli_range_build", "mjs");
	let _ = std::fs::remove_file(&output_path);

	let out = nymph(&[
		"build",
		path.to_str().unwrap(),
		"-o",
		output_path.to_str().unwrap(),
	]);
	let _ = std::fs::remove_file(&path);

	assert!(
		out.status.success(),
		"first-class ranges should build successfully:\n{}",
		out.stderr
	);
	assert!(
		output_path.exists(),
		"a successful range build must write the requested output"
	);
	let _ = std::fs::remove_file(&output_path);
}

#[test]
fn check_reports_ok_for_a_user_struct_plus_impl_via_the_default_prelude() {
	// The prelude-default-flip payoff: `check` resolves a user struct's own
	// `Plus` impl with no local `interface Plus` declaration at all — the
	// stdlib operator-interface prelude is now flattened ahead of every
	// checked module by default (see `nymph-compiler`'s `check`/`compile`).
	let path = write_source(
		"struct P(v: int)\n\
		impl Plus<Other = P, Output = P> for P { func plus(other: P): P = other }\n\
		func add(a: P, b: P): P = a + b",
	);
	let out = nymph(&["check", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		out.status.success(),
		"expected exit 0, got {:?}\nstdout: {}\nstderr: {}",
		out.status.code(),
		out.stdout,
		out.stderr
	);
	assert!(out.stdout.contains("ok"), "stdout was: {}", out.stdout);
}

#[test]
fn build_and_run_succeed_for_a_user_impl_of_a_stdlib_interface() {
	// The stdlib body materialization slice's payoff (flips the prelude
	// flip's former honest-scope KK4 limitation, pinned pre-this-slice as
	// `build_reports_a_readable_error_for_a_user_impl_of_a_stdlib_interface`):
	// checking a user struct's `impl Plus for P` (with no local `interface
	// Plus` declaration at all) was already clean via the default prelude,
	// but lowering used to panic — "impl references unknown interface
	// `Plus`" — because the interface's own declaration lives in the
	// prelude tree, invisible to a lowering that only ever walked the
	// user's own AST. Feeding the prelude's interfaces into the same
	// lookup fixes this directly: `build` now succeeds and writes real JS,
	// and `run` actually executes it. No I/O exists yet to print the
	// result directly, so — mirroring
	// `run_invokes_main_and_surfaces_a_runtime_error_from_its_body`'s
	// "observable side effect without I/O" trick — `main` deliberately
	// recurses forever if `P`'s `+` produced the wrong value, so a clean
	// exit 0 is only possible if the prelude-resolved `Plus` impl actually
	// ran and computed the right answer.
	let path = write_source(
		"struct P(v: int)\n\
		impl Plus<Other = P, Output = P> for P { func plus(other: P): P = P(v = this.v + other.v) }\n\
		func spin(): void = spin()\n\
		func main(): void = {\n\
		\tlet sum = P(v = 1) + P(v = 2)\n\
		\tif (sum.v != 3) spin()\n\
		}",
	);
	let output_path = unique_temp_path("nymph_cli_prelude_build", "mjs");
	let _ = std::fs::remove_file(&output_path);

	let build_out = nymph(&[
		"build",
		path.to_str().unwrap(),
		"-o",
		output_path.to_str().unwrap(),
	]);
	assert!(
		build_out.status.success(),
		"expected `build` to succeed now that a user impl of a stdlib interface lowers cleanly\nstderr: {}",
		build_out.stderr
	);
	assert!(
		output_path.exists(),
		"a successful build must write the compiled JS to the output path"
	);

	let run_out = nymph(&["run", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);
	let _ = std::fs::remove_file(&output_path);

	assert!(
		run_out.status.success(),
		"expected exit 0 — `P`'s prelude-resolved `+` must have computed the right value, or `main` would spin forever\nstdout: {}\nstderr: {}",
		run_out.stdout,
		run_out.stderr
	);
}

#[test]
fn build_respects_the_output_flag() {
	let path = write_source("func add(a: int, b: int): int = a + b");
	let output_path = unique_temp_path("nymph_cli_out", "mjs");
	let _ = std::fs::remove_file(&output_path);

	let out = nymph(&[
		"build",
		path.to_str().unwrap(),
		"-o",
		output_path.to_str().unwrap(),
	]);

	assert!(out.status.success(), "stderr: {}", out.stderr);
	assert!(output_path.exists());

	let _ = std::fs::remove_file(&path);
	let _ = std::fs::remove_file(&output_path);
}

#[test]
fn run_invokes_main_and_exits_successfully_when_main_is_valid() {
	let path = write_source("func main(): void = {\n  let x = 1 + 1\n}");
	let out = nymph(&["run", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		out.status.success(),
		"expected exit 0, got {:?}\nstdout: {}\nstderr: {}",
		out.status.code(),
		out.stdout,
		out.stderr
	);
}

#[test]
fn run_evaluates_an_inline_expression_and_prints_its_value() {
	// `run -e "<expr>"` wraps the expression in a throwaway nullary function,
	// compiles it as a library module, and prints the result via `console.log`
	// — no `main` needed. The prelude is on, so operators resolve.
	let out = nymph(&["run", "-e", "40 + 2"]);
	assert!(
		out.status.success(),
		"expected exit 0, got {:?}\nstderr: {}",
		out.status.code(),
		out.stderr
	);
	assert_eq!(
		out.stdout.trim(),
		"42",
		"expected the printed value 42\nstdout: {}\nstderr: {}",
		out.stdout,
		out.stderr
	);
}

#[test]
fn run_evaluates_boolean_bitwise_operators_to_booleans_not_numbers() {
	// Regression: boolean `&`/`|`/`^` used to hit infer_binary's same-primitive
	// BuiltinEager fast path and emit native JS bitwise ops, which coerce
	// booleans to numbers (`true & false` → 0). They now dispatch to the stdlib
	// BitAnd/BitOr/BitXor impls (materialized) and produce real booleans.
	let out = nymph(&["run", "-e", "#(true & false, true | false, true ^ true)"]);
	assert!(
		out.status.success(),
		"boolean bitwise expressions should run; stderr: {}",
		out.stderr
	);
	assert_eq!(out.stdout.trim(), "#(false, true, false)");
}

#[test]
fn run_reports_a_type_error_in_an_inline_expression() {
	// A type error in the inline expression is a normal rendered diagnostic +
	// exit 1, not a node invocation.
	let out = nymph(&["run", "-e", "1 + true"]);
	assert_eq!(out.status.code(), Some(1));
	assert!(
		!out.stdout.contains("panicked at"),
		"stdout must not carry a raw panic dump: {}",
		out.stdout
	);
}

#[test]
fn run_invokes_main_and_surfaces_a_runtime_error_from_its_body() {
	// The language has no I/O yet, so a `main` can't print a value to prove
	// it ran. Deliberate unbounded recursion is a side effect that IS
	// observable without I/O: it can only reach the JS engine's call-stack
	// limit if `main()` was genuinely invoked (an unexecuted module never
	// runs `spin` at all), and Node reports it deterministically.
	let path = write_source("func spin(): void = spin()\n\nfunc main(): void = spin()");
	let out = nymph(&["run", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!out.status.success(),
		"expected a nonzero exit from the runtime stack overflow"
	);
	assert!(
		out.stderr.contains("Maximum call stack size exceeded") && out.stderr.contains("spin"),
		"stderr should show the crash originating from `main`'s own call to `spin`:\n{}",
		out.stderr
	);
}

#[test]
fn run_reports_a_type_error_inside_main_instead_of_executing() {
	let path = write_source("func main(): void = {\n  let x: int = true\n}");
	let out = nymph(&["run", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!out.status.success(),
		"expected nonzero exit for a type error"
	);
	assert!(
		out.stderr.contains("mismatched types"),
		"stderr should carry the type-check diagnostic:\n{}",
		out.stderr
	);
}

#[test]
fn run_without_a_top_level_main_errors_and_does_not_invoke_node() {
	let path = write_source("func add(a: int, b: int): int = a + b");
	let out = nymph(&["run", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!out.status.success(),
		"expected nonzero exit when there is no top-level `main`"
	);
	assert!(
		out.stderr.contains("no `main` function found"),
		"stderr should carry the checker's missing-main diagnostic:\n{}",
		out.stderr
	);
}

#[test]
fn run_does_not_mistake_a_struct_method_named_main_for_the_entry_point() {
	let path = write_source("struct Foo(x: int) {\n  func main(): int = this.x\n}");
	let out = nymph(&["run", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!out.status.success(),
		"a method named `main` on a struct is not the program's entry point"
	);
	assert!(
		out.stderr.contains("no `main` function found"),
		"stderr should carry the checker's missing-main diagnostic:\n{}",
		out.stderr
	);
}

#[test]
fn run_with_main_taking_parameters_errors() {
	let path = write_source("func main(x: int): void = {}");
	let out = nymph(&["run", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!out.status.success(),
		"expected nonzero exit when `main` declares parameters"
	);
	assert!(
		out
			.stderr
			.contains("`main` must not declare any parameters"),
		"stderr should explain that `main` must take no parameters:\n{}",
		out.stderr
	);
}

#[test]
fn run_with_main_declaring_a_non_void_return_type_errors() {
	let path = write_source("func main(): int = 0");
	let out = nymph(&["run", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		!out.status.success(),
		"expected nonzero exit when `main` declares a non-`void` return type"
	);
	assert!(
		out
			.stderr
			.contains("`main` must not declare a return type other than `void`"),
		"stderr should explain that `main` must not declare a non-`void` return type:\n{}",
		out.stderr
	);
}

#[test]
fn run_supports_first_class_range_values() {
	let path = write_source("func main(): void = {\n  let r = 1..5\n}");
	let out = nymph(&["run", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		out.status.success(),
		"first-class ranges should run successfully:\n{}",
		out.stderr
	);
}

// ── `check`/`build` entry mode via manifest metadata ────────────────────────

#[test]
fn check_treats_a_loose_main_dot_nym_as_a_library() {
	let path = write_main_source("func add(a: int, b: int): int = a + b");
	let out = nymph(&["check", path.to_str().unwrap()]);
	let dir = path.parent().unwrap().to_path_buf();
	let _ = std::fs::remove_dir_all(&dir);

	assert!(
		out.status.success(),
		"a filename alone must not select entry mode; stderr: {}",
		out.stderr
	);
}

#[test]
fn check_passes_a_valid_main_dot_nym() {
	let path = write_main_source("func main(): void = {}");
	let out = nymph(&["check", path.to_str().unwrap()]);
	let dir = path.parent().unwrap().to_path_buf();
	let _ = std::fs::remove_dir_all(&dir);

	assert!(
		out.status.success(),
		"expected exit 0 for a valid main.nym, stderr: {}",
		out.stderr
	);
	assert!(out.stdout.contains("ok"), "stdout was: {}", out.stdout);
}

#[test]
fn check_does_not_require_main_for_a_loose_file() {
	let path = write_source("func add(a: int, b: int): int = a + b");
	let out = nymph(&["check", path.to_str().unwrap()]);
	let _ = std::fs::remove_file(&path);

	assert!(
		out.status.success(),
		"a source with no `main` should still pass `check` under a non-`main` file stem, stderr: {}",
		out.stderr
	);
}

#[test]
fn build_treats_a_loose_main_dot_nym_as_a_library() {
	let path = write_main_source("func add(a: int, b: int): int = a + b");
	let output_path = path.with_extension("mjs");
	let out = nymph(&["build", path.to_str().unwrap()]);
	let dir = path.parent().unwrap().to_path_buf();
	let stderr = out.stderr.clone();
	let succeeded = out.status.success();
	let output_exists = output_path.exists();
	let _ = std::fs::remove_dir_all(&dir);

	assert!(
		succeeded,
		"a filename alone must not select entry mode; stderr: {stderr}"
	);
	assert!(
		output_exists,
		"a loose library build should write its output"
	);
}

#[test]
fn build_writes_a_valid_main_dot_nym() {
	let path = write_main_source("func main(): void = {}");
	let output_path = path.with_extension("mjs");
	let out = nymph(&["build", path.to_str().unwrap()]);
	let dir = path.parent().unwrap().to_path_buf();
	let stderr = out.stderr.clone();
	let succeeded = out.status.success();
	let js = std::fs::read_to_string(&output_path).ok();
	let _ = std::fs::remove_dir_all(&dir);

	assert!(succeeded, "expected exit 0, stderr: {stderr}");
	let js = js.expect("expected the output .mjs to have been written");
	assert!(js.contains("main"), "emitted JS was: {js}");
}

#[test]
fn check_passes_a_main_dot_nym_using_a_prelude_operator_impl() {
	// The entry-mode counterpart of `check_reports_ok_for_a_user_struct_plus_impl_via_the_default_prelude`
	// above: `check_module_entry_with_prelude` (the entry-mode prelude seam)
	// must resolve the same bare `Plus` impl AND still enforce entry mode's
	// own `main` requirement over the combined (prelude + user) module.
	let path = write_main_source(
		"struct P(v: int)\n\
		impl Plus<Other = P, Output = P> for P { func plus(other: P): P = other }\n\
		func main(): void = {\n  let sum = P(v = 1) + P(v = 2)\n}",
	);
	let out = nymph(&["check", path.to_str().unwrap()]);
	let dir = path.parent().unwrap().to_path_buf();
	let _ = std::fs::remove_dir_all(&dir);

	assert!(
		out.status.success(),
		"expected exit 0, got {:?}\nstdout: {}\nstderr: {}",
		out.status.code(),
		out.stdout,
		out.stderr
	);
	assert!(out.stdout.contains("ok"), "stdout was: {}", out.stdout);
}

#[test]
fn stub_subcommand_exits_nonzero_with_a_message() {
	let out = nymph(&["doc"]);
	assert_eq!(out.status.code(), Some(2));
	assert!(
		out.stderr.contains("not implemented"),
		"stderr was: {}",
		out.stderr
	);
}

#[test]
fn bare_invocation_exits_nonzero() {
	let out = nymph(&[]);
	assert!(!out.status.success());
}

#[test]
fn help_displays_nymph_as_the_program_name() {
	let out = nymph(&["--help"]);

	assert!(out.status.success());
	assert!(
		out.stdout.contains("Usage: nymph"),
		"stdout was: {}",
		out.stdout
	);
	assert!(out.stdout.contains("--manifest <PATH>"));
	assert!(!out.stdout.contains("--config"));
}

#[test]
fn config_flag_is_rejected_without_an_alias() {
	let out = nymph(&["--config", "nymph.toml", "check"]);

	assert_eq!(out.status.code(), Some(2));
	assert!(
		out.stderr.contains("unexpected argument '--config'"),
		"stderr was: {}",
		out.stderr
	);
}

#[test]
fn manifest_flag_requires_a_path() {
	let out = nymph(&["check", "--manifest"]);

	assert_eq!(out.status.code(), Some(2));
	assert!(
		out
			.stderr
			.contains("a value is required for '--manifest <PATH>'"),
		"stderr was: {}",
		out.stderr
	);
}

#[test]
fn help_omits_package_management_commands() {
	let out = nymph(&["--help"]);

	assert!(out.status.success());
	for command in ["add", "install", "remove", "search"] {
		assert!(
			!out
				.stdout
				.lines()
				.any(|line| line.trim_start().starts_with(command)),
			"help unexpectedly advertised `{command}`:\n{}",
			out.stdout
		);
	}
}

#[test]
fn package_management_commands_are_unknown() {
	for command in ["add", "install", "remove", "search"] {
		let out = nymph(&[command]);

		assert_eq!(out.status.code(), Some(2), "command: {command}");
		assert!(
			out.stderr.contains("unrecognized subcommand"),
			"`{command}` should be unknown, stderr was: {}",
			out.stderr
		);
	}
}
