//! Integration tests: spawn the real `nymph` binary and assert on its
//! observable behavior (exit code, stdout, stderr) for `check`, `build`,
//! `run`, and command-line parsing.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const ATOMIC_DIRECTORY_EXCHANGE: bool = cfg!(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
));

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
/// `main.nym`, and return its path. A unique directory keeps concurrent test
/// threads from colliding on the shared filename; tests use this to prove that
/// the filename itself has no effect on entry/library intent.
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
fn doc_generates_default_and_custom_deterministic_sites_with_visibility_and_links() {
	let root = write_project(
		"main.nym",
		"import @/model/token with (Token)\n\
		 public func echo(value: Token): Token = value\n\
		 private func hidden(): int = 42\n",
	);
	let model = root.join("src/model/token.nym");
	std::fs::create_dir_all(model.parent().unwrap()).unwrap();
	std::fs::write(&model, "public struct Token {}\n").unwrap();

	let generated = nymph_in(&["doc"], &root);
	assert!(generated.status.success(), "{}", generated.stderr);
	let default = root.join("target/nymph/doc");
	let index = std::fs::read_to_string(default.join("index.html")).unwrap();
	let main = std::fs::read_to_string(default.join("modules/main.html")).unwrap();
	let model = std::fs::read_to_string(default.join("modules/model/token.html")).unwrap();
	let token_anchor = model
		.split_once("<section id=\"")
		.and_then(|(_, html)| html.split_once('"'))
		.map(|(anchor, _)| anchor)
		.expect("Token documentation should have an item anchor");
	assert!(index.contains("modules/model/token.html"), "{index}");
	assert!(main.contains("echo"), "{main}");
	assert!(!main.contains("hidden"), "{main}");
	assert!(
		main.contains(&format!("modules/model/token.html#{token_anchor}")),
		"cross-module type should link by its semantic target: {main}"
	);
	let first = std::fs::read(default.join("modules/main.html")).unwrap();
	let second = root.join("deterministic-docs");
	let regenerated = if ATOMIC_DIRECTORY_EXCHANGE {
		nymph_in(&["doc"], &root)
	} else {
		nymph_in(&["doc", "--output", second.to_str().unwrap()], &root)
	};
	assert!(regenerated.status.success(), "{}", regenerated.stderr);
	assert_eq!(
		first,
		std::fs::read(
			if ATOMIC_DIRECTORY_EXCHANGE {
				&default
			} else {
				&second
			}
			.join("modules/main.html")
		)
		.unwrap(),
		"same checked project must render byte-for-byte deterministically"
	);

	let custom = root.join("custom-docs");
	let private = nymph_in(
		&[
			"doc",
			"--output",
			custom.to_str().unwrap(),
			"--document-private-items",
		],
		&root,
	);
	assert!(private.status.success(), "{}", private.stderr);
	let custom_main = std::fs::read_to_string(custom.join("modules/main.html")).unwrap();
	assert!(custom_main.contains("hidden"), "{custom_main}");

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn doc_failure_preserves_the_previous_output_tree() {
	let root = write_project("main.nym", "public func broken(: int\n");
	let output = root.join("site");
	std::fs::create_dir_all(&output).unwrap();
	std::fs::write(output.join("preserved.txt"), "old documentation").unwrap();

	let result = nymph_in(&["doc", "--output", output.to_str().unwrap()], &root);
	assert_eq!(result.status.code(), Some(1), "{}", result.stderr);
	assert_eq!(
		std::fs::read_to_string(output.join("preserved.txt")).unwrap(),
		"old documentation"
	);
	assert!(!output.join("index.html").exists());

	std::fs::write(root.join("src/main.nym"), "public let fixed: int = 1\n").unwrap();
	let prior_file = root.join("prior-file");
	std::fs::write(&prior_file, "old file").unwrap();
	let replaced = nymph_in(&["doc", "--output", prior_file.to_str().unwrap()], &root);
	if ATOMIC_DIRECTORY_EXCHANGE {
		assert!(replaced.status.success(), "{}", replaced.stderr);
		assert!(prior_file.join("index.html").is_file());
	} else {
		assert_eq!(replaced.status.code(), Some(1), "{}", replaced.stderr);
		assert_eq!(std::fs::read_to_string(&prior_file).unwrap(), "old file");
	}

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn doc_reports_warnings_without_blocking_publication() {
	let root = write_project("main.nym", "public func large(): int = 9007199254740992\n");
	let result = nymph_in(&["doc"], &root);
	assert!(result.status.success(), "{}", result.stderr);
	assert!(
		result.stderr.to_ascii_lowercase().contains("warning"),
		"{}",
		result.stderr
	);
	assert!(
		result.stderr.contains("9007199254740992"),
		"{}",
		result.stderr
	);
	assert!(root.join("target/nymph/doc/index.html").is_file());

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn doc_requires_strict_discovery_and_honors_an_explicit_manifest_authoritatively() {
	let empty = unique_temp_path("nymph_cli_doc_empty", "dir");
	std::fs::create_dir_all(&empty).unwrap();
	let absent = nymph_in(&["doc"], &empty);
	assert_eq!(absent.status.code(), Some(1));
	assert!(
		absent.stderr.contains("no nymph.toml found"),
		"{}",
		absent.stderr
	);

	let root = write_project("main.nym", "public let answer: int = 42\n");
	let missing = root.join("missing.toml");
	let selected = nymph_in(&["--manifest", missing.to_str().unwrap(), "doc"], &root);
	assert_eq!(selected.status.code(), Some(1));
	assert!(
		selected.stderr.contains("could not read manifest"),
		"{}",
		selected.stderr
	);
	assert!(selected.stderr.contains(&missing.display().to_string()));
	assert!(!root.join("target/nymph/doc/index.html").exists());

	std::fs::remove_dir_all(empty).unwrap();
	std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn doc_open_runs_only_after_publication_and_receives_the_generated_index() {
	use std::os::unix::fs::PermissionsExt;

	let root = write_project("main.nym", "public let answer: int = 42\n");
	let bin = root.join("fake-bin");
	std::fs::create_dir_all(&bin).unwrap();
	let record = root.join("opened.txt");
	let opener = bin.join("xdg-open");
	std::fs::write(
		&opener,
		format!(
			"#!/bin/sh\ntest -f \"$1\" || exit 9\nprintf '%s' \"$1\" > '{}'\n",
			record.display()
		),
	)
	.unwrap();
	let mut permissions = std::fs::metadata(&opener).unwrap().permissions();
	permissions.set_mode(0o755);
	std::fs::set_permissions(&opener, permissions).unwrap();
	let path = std::env::var_os("PATH").unwrap_or_default();
	let path = std::iter::once(bin.clone())
		.chain(std::env::split_paths(&path))
		.collect::<Vec<_>>();
	let output = Command::new(env!("CARGO_BIN_EXE_nymph"))
		.arg("doc")
		.arg("--open")
		.arg("--output=-site")
		.current_dir(&root)
		.env("PATH", std::env::join_paths(path).unwrap())
		.output()
		.unwrap();
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(
		std::fs::read_to_string(record).unwrap(),
		root.join("-site/index.html").display().to_string()
	);

	std::fs::remove_dir_all(root).unwrap();
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

#[cfg(unix)]
#[test]
fn explicit_manifest_rejects_a_source_symlink_that_escapes_its_source_root() {
	let selected = write_project("main.nym", "func main(): void = {}\n");
	let outside = write_source("func main(): void = {}\n");
	let selected_source = selected.join("src/main.nym");
	std::fs::remove_file(&selected_source).unwrap();
	std::os::unix::fs::symlink(&outside, &selected_source).unwrap();

	for command in ["run", "check", "build"] {
		for target in [None, Some(selected_source.to_str().unwrap())] {
			let manifest = selected.join("nymph.toml");
			let mut args = vec!["--manifest", manifest.to_str().unwrap(), command];
			args.extend(target);
			let out = nymph_in(&args, &selected);
			assert_eq!(out.status.code(), Some(1), "{command}: {}", out.stderr);
			assert!(
				out.stderr.contains("outside source root"),
				"{command} must not follow the selected target outside its source root: {}",
				out.stderr
			);
		}
	}

	std::fs::remove_dir_all(selected).unwrap();
	std::fs::remove_file(outside).unwrap();
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
fn check_matches_build_and_run_for_project_imports_and_embedded_std() {
	let root = write_project(
		"bin/start.nym",
		"import @/support with (answer)\n\
		 import std/collections/tree with (Tree)\n\
		 func main(): void = {\n\
		   let value = match (Tree.Leaf(value = answer())) {\n\
		     Tree.Leaf(value) -> value,\n\
		     Tree.Node(...) -> 0,\n\
		   }\n\
		 }\n",
	);
	std::fs::write(
		root.join("src/support.nym"),
		"public func answer(): int = 42\n",
	)
	.unwrap();

	for command in ["check", "build", "run"] {
		let out = nymph_in(&[command], &root);
		assert!(
			out.status.success(),
			"{command} should accept the same project/std graph; stdout: {} stderr: {}",
			out.stdout,
			out.stderr
		);
	}

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn check_attributes_imported_module_diagnostics_to_the_source_path() {
	let root = write_project(
		"main.nym",
		"import @/nested/helper with (answer)\nfunc main(): void = { let value = answer() }\n",
	);
	let helper = root.join("src/nested/helper.nym");
	std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
	std::fs::write(&helper, "public func answer(): int = true\n").unwrap();

	let out = nymph_in(&["check"], &root);
	assert_eq!(out.status.code(), Some(1), "{}", out.stderr);
	assert!(
		out.stderr.contains(&format!("{}:1:", helper.display())),
		"diagnostic should point to the imported module's source path: {}",
		out.stderr
	);

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn check_explicit_project_and_loose_targets_use_std_without_emitting_js() {
	let root = write_project(
		"main.nym",
		"import std/io with (println)\nfunc main(): void = println(\"executed\")\n",
	);
	let entry_output = root.join("src/main.mjs");
	let entry_check = nymph_in(&["check"], &root);
	assert!(
		entry_check.status.success(),
		"project entry should check with embedded std: {}",
		entry_check.stderr
	);
	assert_eq!(
		entry_check.stdout, "ok\n",
		"check must not execute the selected entry"
	);
	assert!(
		!entry_output.exists(),
		"check must not emit {}",
		entry_output.display()
	);

	let project_library = root.join("src/lib/tree.nym");
	std::fs::create_dir_all(project_library.parent().unwrap()).unwrap();
	std::fs::write(
		&project_library,
		"import std/collections/tree with (Tree)\n\
		 public func leaf(): Tree<int> = Tree.Leaf(value = 1)\n",
	)
	.unwrap();
	let project_output = project_library.with_extension("mjs");
	let project_check = nymph_in(&["check", project_library.to_str().unwrap()], &root);
	assert!(
		project_check.status.success(),
		"explicit project library should check with embedded std: {}",
		project_check.stderr
	);
	assert!(
		!project_output.exists(),
		"check must not emit {}",
		project_output.display()
	);

	let loose = write_source(
		"import std/collections/tree with (Tree)\n\
		 func leaf(): Tree<int> = Tree.Leaf(value = 1)\n",
	);
	let loose_output = loose.with_extension("mjs");
	let loose_check = nymph(&["check", loose.to_str().unwrap()]);
	assert!(
		loose_check.status.success(),
		"loose library should check with embedded std: {}",
		loose_check.stderr
	);
	assert_eq!(
		loose_check.stdout, "ok\n",
		"successful loose check should print only its status"
	);
	assert!(
		!loose_output.exists(),
		"check must not emit {}",
		loose_output.display()
	);

	std::fs::remove_dir_all(root).unwrap();
	std::fs::remove_file(loose).unwrap();
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
fn new_creates_exact_binary_and_library_projects_that_check() {
	let root = unique_temp_path("nymph_cli_new", "dir");
	std::fs::create_dir_all(&root).unwrap();
	let binary = root.join("hello-world");
	let library = root.join("hello-lib");

	let binary_out = nymph(&["new", binary.to_str().unwrap(), "--no-git"]);
	assert!(binary_out.status.success(), "{}", binary_out.stderr);
	assert_eq!(binary_out.stdout, "Created binary package `hello-world`\n");
	assert_eq!(
		std::fs::read_to_string(binary.join("nymph.toml")).unwrap(),
		"[package]\nname = \"hello-world\"\nversion = \"0.1.0\"\n"
	);
	assert_eq!(
		std::fs::read_to_string(binary.join("src/main.nym")).unwrap(),
		"func main(): void = {}\n"
	);
	assert_eq!(
		std::fs::read_dir(&binary)
			.unwrap()
			.map(|entry| entry.unwrap().file_name())
			.collect::<std::collections::BTreeSet<_>>(),
		["nymph.toml", "src"].into_iter().map(Into::into).collect()
	);
	let check = nymph_in(&["check"], &binary);
	assert!(check.status.success(), "{}", check.stderr);

	let library_out = nymph(&["new", library.to_str().unwrap(), "--lib", "--no-git"]);
	assert!(library_out.status.success(), "{}", library_out.stderr);
	assert_eq!(library_out.stdout, "Created library package `hello-lib`\n");
	assert_eq!(
		std::fs::read_to_string(library.join("nymph.toml")).unwrap(),
		"[package]\nname = \"hello-lib\"\nversion = \"0.1.0\"\n"
	);
	assert_eq!(
		std::fs::read_to_string(library.join("src/lib.nym")).unwrap(),
		"public func hello(): string = \"Hello, world!\"\n"
	);
	let library_source = library.join("src/lib.nym");
	let check = nymph_in(&["check", library_source.to_str().unwrap()], &library);
	assert!(check.status.success(), "{}", check.stderr);

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_derives_names_after_validating_unicode_and_path_cases_before_writing() {
	let root = unique_temp_path("nymph_cli_new_names", "dir");
	let unicode_parent = root.join("日本語");
	let valid = unicode_parent.join("app2-core");
	let out = nymph(&["new", valid.to_str().unwrap(), "--no-git"]);
	assert!(out.status.success(), "{}", out.stderr);
	assert!(valid.join("nymph.toml").is_file());

	for invalid in ["Uppercase", "2fast", "has_underscore", "café"] {
		let destination = root.join(invalid);
		let out = nymph(&["new", destination.to_str().unwrap(), "--no-git"]);
		assert_eq!(out.status.code(), Some(1), "{invalid}: {}", out.stderr);
		assert!(
			out.stderr.contains("invalid package name"),
			"{}",
			out.stderr
		);
		assert!(!destination.exists(), "invalid destination was touched");
	}

	std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn new_rejects_a_non_unicode_destination_basename_without_writing() {
	use std::os::unix::ffi::OsStringExt;

	let root = unique_temp_path("nymph_cli_new_non_unicode", "dir");
	std::fs::create_dir_all(&root).unwrap();
	let destination = root.join(std::ffi::OsString::from_vec(vec![b'a', 0xff]));
	let out = Command::new(env!("CARGO_BIN_EXE_nymph"))
		.arg("new")
		.arg(&destination)
		.arg("--no-git")
		.output()
		.unwrap();
	assert_eq!(out.status.code(), Some(1));
	assert!(
		String::from_utf8_lossy(&out.stderr).contains("not valid Unicode"),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	assert!(!destination.exists());
	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_accepts_an_existing_empty_directory_and_refuses_other_destinations_untouched() {
	let root = unique_temp_path("nymph_cli_new_existing", "dir");
	std::fs::create_dir_all(&root).unwrap();
	let empty = root.join("empty-app");
	std::fs::create_dir(&empty).unwrap();
	let out = nymph(&["new", empty.to_str().unwrap(), "--no-git"]);
	assert!(out.status.success(), "{}", out.stderr);
	assert!(empty.join("src/main.nym").is_file());

	let file = root.join("existing-file");
	std::fs::write(&file, "keep me").unwrap();
	let out = nymph(&["new", file.to_str().unwrap(), "--no-git"]);
	assert_eq!(out.status.code(), Some(1));
	assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me");

	let nonempty = root.join("nonempty");
	std::fs::create_dir(&nonempty).unwrap();
	std::fs::write(nonempty.join("keep"), "untouched").unwrap();
	let out = nymph(&["new", nonempty.to_str().unwrap(), "--no-git"]);
	assert_eq!(out.status.code(), Some(1));
	assert_eq!(
		std::fs::read_to_string(nonempty.join("keep")).unwrap(),
		"untouched"
	);
	assert_eq!(std::fs::read_dir(&nonempty).unwrap().count(), 1);

	std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn new_preserves_existing_empty_directory_permissions() {
	use std::os::unix::fs::PermissionsExt;

	let root = unique_temp_path("nymph_cli_new_existing_permissions", "dir");
	let destination = root.join("private-app");
	std::fs::create_dir_all(&destination).unwrap();
	let mut permissions = std::fs::metadata(&destination).unwrap().permissions();
	permissions.set_mode(0o700);
	std::fs::set_permissions(&destination, permissions).unwrap();

	let out = nymph(&["new", destination.to_str().unwrap(), "--no-git"]);
	assert!(out.status.success(), "{}", out.stderr);
	assert_eq!(
		std::fs::metadata(&destination)
			.unwrap()
			.permissions()
			.mode()
			& 0o777,
		0o700
	);

	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_creates_nested_parents_and_no_git_is_deterministic() {
	let root = unique_temp_path("nymph_cli_new_nested", "dir");
	std::fs::create_dir_all(&root).unwrap();
	let first = root.join("one/more/repeatable");
	let second = root.join("two/more/repeatable");
	for destination in [&first, &second] {
		let out = nymph(&["new", destination.to_str().unwrap(), "--no-git"]);
		assert!(out.status.success(), "{}", out.stderr);
		assert!(!destination.join(".git").exists());
	}
	assert_eq!(
		std::fs::read(first.join("nymph.toml")).unwrap(),
		std::fs::read(second.join("nymph.toml")).unwrap()
	);
	assert_eq!(
		std::fs::read(first.join("src/main.nym")).unwrap(),
		std::fs::read(second.join("src/main.nym")).unwrap()
	);
	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_creates_nested_parents_from_a_relative_destination() {
	let root = unique_temp_path("nymph_cli_new_relative", "dir");
	std::fs::create_dir_all(&root).unwrap();
	let out = nymph_in(&["new", "one/more/relative-app", "--no-git"], &root);
	assert!(out.status.success(), "{}", out.stderr);
	assert!(root.join("one/more/relative-app/src/main.nym").is_file());
	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_initializes_git_by_default_without_a_commit() {
	let root = unique_temp_path("nymph_cli_new_git", "dir");
	std::fs::create_dir_all(&root).unwrap();
	let destination = root.join("git-app");
	let out = nymph(&["new", destination.to_str().unwrap()]);
	assert!(out.status.success(), "{}", out.stderr);
	assert!(destination.join(".git").is_dir());
	let head = Command::new("git")
		.args([
			"-C",
			destination.to_str().unwrap(),
			"rev-parse",
			"--verify",
			"HEAD",
		])
		.output()
		.unwrap();
	assert!(
		!head.status.success(),
		"new must not create an initial commit"
	);
	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_missing_git_leaves_no_destination_or_nested_parents() {
	let root = unique_temp_path("nymph_cli_new_missing_git", "dir");
	std::fs::create_dir_all(&root).unwrap();
	let destination = root.join("missing/parents/git-app");
	let out = Command::new(env!("CARGO_BIN_EXE_nymph"))
		.args(["new"])
		.arg(&destination)
		.env("PATH", "")
		.output()
		.unwrap();
	assert_eq!(out.status.code(), Some(1));
	assert!(String::from_utf8_lossy(&out.stderr).contains("install Git or pass --no-git"));
	assert!(!root.join("missing").exists());
	assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
	std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn new_failing_git_preserves_an_existing_empty_destination_and_cleans_staging() {
	use std::os::unix::fs::PermissionsExt;

	let root = unique_temp_path("nymph_cli_new_failing_git", "dir");
	let bin = root.join("bin");
	let destination = root.join("git-app");
	std::fs::create_dir_all(&bin).unwrap();
	std::fs::create_dir(&destination).unwrap();
	let git = bin.join("git");
	std::fs::write(&git, "#!/bin/sh\necho deliberate failure >&2\nexit 42\n").unwrap();
	let mut permissions = std::fs::metadata(&git).unwrap().permissions();
	permissions.set_mode(0o755);
	std::fs::set_permissions(&git, permissions).unwrap();

	let out = Command::new(env!("CARGO_BIN_EXE_nymph"))
		.args(["new"])
		.arg(&destination)
		.env("PATH", &bin)
		.output()
		.unwrap();
	assert_eq!(out.status.code(), Some(1));
	assert!(String::from_utf8_lossy(&out.stderr).contains("deliberate failure"));
	assert!(destination.is_dir());
	assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
	assert_eq!(
		std::fs::read_dir(&root)
			.unwrap()
			.map(|entry| entry.unwrap().file_name())
			.collect::<std::collections::BTreeSet<_>>(),
		["bin", "git-app"].into_iter().map(Into::into).collect()
	);
	std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn new_rejects_a_symlink_ancestor_changed_by_git_without_touching_either_target() {
	use std::os::unix::fs::{PermissionsExt, symlink};

	let root = unique_temp_path("nymph_cli_new_symlink_race", "dir");
	let original = root.join("original");
	let replacement = root.join("replacement");
	let bin = root.join("bin");
	let link = root.join("link");
	std::fs::create_dir_all(&original).unwrap();
	std::fs::create_dir_all(&replacement).unwrap();
	std::fs::create_dir_all(&bin).unwrap();
	symlink(&original, &link).unwrap();
	let git = bin.join("git");
	std::fs::write(
		&git,
		"#!/bin/sh\nPATH=/usr/bin:/bin\nrm \"$ATTACK_ROOT/link\"\nln -s \"$ATTACK_ROOT/replacement\" \"$ATTACK_ROOT/link\"\nexit 0\n",
	)
	.unwrap();
	let mut permissions = std::fs::metadata(&git).unwrap().permissions();
	permissions.set_mode(0o755);
	std::fs::set_permissions(&git, permissions).unwrap();
	let destination = link.join("nested/app");

	let out = Command::new(env!("CARGO_BIN_EXE_nymph"))
		.arg("new")
		.arg(&destination)
		.env("PATH", &bin)
		.env("ATTACK_ROOT", &root)
		.output()
		.unwrap();
	assert_eq!(out.status.code(), Some(1));
	assert!(
		String::from_utf8_lossy(&out.stderr).contains("parent ancestor changed"),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	assert_eq!(std::fs::read_dir(&original).unwrap().count(), 0);
	assert_eq!(std::fs::read_dir(&replacement).unwrap().count(), 0);

	std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn new_rejects_a_symlink_created_in_a_missing_parent_without_publishing() {
	use std::os::unix::fs::PermissionsExt;

	let root = unique_temp_path("nymph_cli_new_missing_parent_race", "dir");
	let original = root.join("original");
	let replacement = root.join("replacement");
	let bin = root.join("bin");
	std::fs::create_dir_all(&original).unwrap();
	std::fs::create_dir_all(&replacement).unwrap();
	std::fs::create_dir_all(&bin).unwrap();
	let git = bin.join("git");
	std::fs::write(
		&git,
		"#!/bin/sh\nPATH=/usr/bin:/bin\nln -s \"$ATTACK_ROOT/replacement\" \"$ATTACK_ROOT/original/nested\"\nexit 0\n",
	)
	.unwrap();
	let mut permissions = std::fs::metadata(&git).unwrap().permissions();
	permissions.set_mode(0o755);
	std::fs::set_permissions(&git, permissions).unwrap();
	let destination = original.join("nested/app");

	let out = Command::new(env!("CARGO_BIN_EXE_nymph"))
		.arg("new")
		.arg(&destination)
		.env("PATH", &bin)
		.env("ATTACK_ROOT", &root)
		.output()
		.unwrap();
	assert_eq!(out.status.code(), Some(1));
	assert!(!replacement.join("app").exists());
	assert_eq!(
		std::fs::read_dir(&original)
			.unwrap()
			.map(|entry| entry.unwrap().file_name())
			.collect::<Vec<_>>(),
		["nested"]
	);
	assert!(
		std::fs::symlink_metadata(original.join("nested"))
			.unwrap()
			.file_type()
			.is_symlink()
	);

	std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn new_cleans_staging_after_preserving_read_only_destination_permissions() {
	use std::os::unix::fs::{PermissionsExt, symlink};

	let root = unique_temp_path("nymph_cli_new_read_only_cleanup", "dir");
	let original = root.join("original");
	let replacement = root.join("replacement");
	let bin = root.join("bin");
	let link = root.join("link");
	let destination = original.join("private-app");
	std::fs::create_dir_all(&destination).unwrap();
	std::fs::create_dir_all(&replacement).unwrap();
	std::fs::create_dir_all(&bin).unwrap();
	symlink(&original, &link).unwrap();
	let mut permissions = std::fs::metadata(&destination).unwrap().permissions();
	permissions.set_mode(0o555);
	std::fs::set_permissions(&destination, permissions).unwrap();
	let git = bin.join("git");
	std::fs::write(
		&git,
		"#!/bin/sh\nPATH=/usr/bin:/bin\nrm \"$ATTACK_ROOT/link\"\nln -s \"$ATTACK_ROOT/replacement\" \"$ATTACK_ROOT/link\"\nexit 0\n",
	)
	.unwrap();
	let mut permissions = std::fs::metadata(&git).unwrap().permissions();
	permissions.set_mode(0o755);
	std::fs::set_permissions(&git, permissions).unwrap();

	let out = Command::new(env!("CARGO_BIN_EXE_nymph"))
		.arg("new")
		.arg(link.join("private-app"))
		.env("PATH", &bin)
		.env("ATTACK_ROOT", &root)
		.output()
		.unwrap();
	assert_eq!(out.status.code(), Some(1));
	assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
	assert_eq!(std::fs::read_dir(&replacement).unwrap().count(), 0);
	assert_eq!(
		std::fs::read_dir(&original)
			.unwrap()
			.map(|entry| entry.unwrap().file_name())
			.collect::<Vec<_>>(),
		["private-app"]
	);

	let mut permissions = std::fs::metadata(&destination).unwrap().permissions();
	permissions.set_mode(0o755);
	std::fs::set_permissions(&destination, permissions).unwrap();
	std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_help_documents_the_path_and_supported_flags() {
	let out = nymph(&["new", "--help"]);
	assert!(out.status.success(), "{}", out.stderr);
	assert!(out.stdout.contains("Usage: nymph new [OPTIONS] <PATH>"));
	assert!(out.stdout.contains("--lib"));
	assert!(out.stdout.contains("--no-git"));
	assert!(!out.stdout.contains("--name"));
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
