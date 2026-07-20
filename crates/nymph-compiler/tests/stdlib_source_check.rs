//! Issue 2 (enum/core-std experience): a stdlib SOURCE file must check clean
//! with NO ambient prelude, whereas [`check`] — which always injects
//! [`nymph_compiler::check`]'s ambient `core` prelude — self-duplicates it:
//! opening `stdlib/src/ops/mod.nym` through `check` injects a second copy of
//! `std/ops` right next to the real one, so every declaration collides with
//! its own ambient copy.
//!
//! [`nymph_compiler::check_without_prelude`] is the fix: it runs the same
//! parse+check pipeline as [`check`] but with no injected prelude sources at
//! all, so a self-contained core file (like `ops/mod.nym`) checks clean.
//! [`nymph_compiler::is_stdlib_source_path`] is the principled detection
//! signal a caller (the LSP) uses to pick between the two entry points.

use nymph_compiler::{check, check_without_prelude, is_stdlib_source_path};
use std::path::Path;

fn ops_mod_source() -> &'static str {
	include_str!("../../../stdlib/src/ops/mod.nym")
}

fn ops_mod_path() -> std::path::PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib/src/ops/mod.nym")
}

#[test]
fn ambient_check_self_duplicates_a_stdlib_source_file() {
	// The repro: checking `ops/mod.nym` through the ordinary ambient-prelude
	// `check` injects a second copy of itself, producing a flood of
	// duplicate-declaration errors.
	let src = ops_mod_source();
	let diags = check(src, "stdlib/src/ops/mod.nym");
	let errors: Vec<_> = diags.iter().filter(|d| d.is_error()).collect();
	assert!(
		!errors.is_empty(),
		"expected self-duplication errors from the ambient-prelude check, got none"
	);
}

#[test]
fn prelude_free_check_of_a_stdlib_source_file_has_no_self_duplication() {
	let src = ops_mod_source();
	let diags = check_without_prelude(src, "stdlib/src/ops/mod.nym");
	let errors: Vec<_> = diags.iter().filter(|d| d.is_error()).collect();
	assert!(
		errors.is_empty(),
		"expected a clean prelude-free check of ops/mod.nym, got: {errors:?}"
	);
}

#[test]
fn is_stdlib_source_path_recognizes_the_embedded_stdlib_tree() {
	assert!(is_stdlib_source_path(&ops_mod_path()));
}

#[test]
fn is_stdlib_source_path_rejects_a_scratch_user_path() {
	let scratch = std::env::temp_dir().join("nymph_not_stdlib_scratch.nym");
	std::fs::write(&scratch, "func main() {}").unwrap();
	assert!(!is_stdlib_source_path(&scratch));
	let _ = std::fs::remove_file(&scratch);
}
