//! `stdlib/src/option.nym` and `stdlib/src/result.nym` must
//! not mutually `import` each other at the top level.
//!
//! Each needs the other's type for one cross-referencing method:
//! `Option::ok_or_else` builds a `Result`, while `Result::ok`/`err` build an
//! `Option`. Run through the REAL multi-module
//! project driver (`nymph_compiler::check_project`, what the CLI's project
//! build actually calls, over a loader reading the real `stdlib/src` files —
//! using the stable project graph rather than combining module syntax into one
//! namespace and drops `import`s entirely, so it can never see a cycle), that
//! mutual import is rejected outright by the import-graph cycle detector
//! ("import cycle detected: option -> result -> option"), so NO real program
//! could ever resolve `@/option` or `@/result`.
//!
//! The cross-referencing methods (`ok_or`/`ok_or_else`, `ok`/`err`) live in a
//! third module, `stdlib/src/convert.nym`, that imports
//! both `Option` and `Result` one-way — `option.nym` and `result.nym`
//! do not reference each other.
//!
//! A program importing `@/option` cannot fully resolve because
//! an UNRELATED reason — `option.nym` also does `import @/ops with (Unwrap)`,
//! and `@/ops`'s real content lives at `stdlib/src/ops/mod.nym` (a
//! directory + `mod.nym`), while the real project driver's file loader only
//! ever looks up a flat `<key>.nym` (see `crates/nymph-cli/src/
//! project_support.rs`'s `fs_loader`). Other stdlib modules (`math`,
//! `iter`, and `range` use the same directory/`mod.nym` layout and are
//! equally unresolvable). The compiler needs either a directory/
//! `mod.nym` fallback in the loader, or flattening those modules to
//! `<name>.nym` and fixing `nymph_compiler::prelude`'s hardcoded
//! `include_str!` path) — this test only pins that the CYCLE is gone, not
//! that `@/option` fully resolves end-to-end yet.

use std::path::PathBuf;

use nymph_compiler::check_project;

fn stdlib_src_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../stdlib/src")
		.canonicalize()
		.unwrap()
}

/// A loader mirroring the CLI's real `fs_loader`: canonical key `"a/b"` maps
/// to `<stdlib/src>/a/b.nym`. Every synthetic entry key is served from a
/// fixed table instead, so the probe program itself needn't live on disk.
fn stdlib_loader(
	entries: &'static [(&'static str, &'static str)],
) -> impl Fn(&str) -> Option<String> {
	let root = stdlib_src_root();
	move |key: &str| {
		if let Some((_, src)) = entries.iter().find(|(k, _)| *k == key) {
			return Some((*src).to_string());
		}
		std::fs::read_to_string(root.join(format!("{key}.nym"))).ok()
	}
}

#[test]
fn option_and_result_no_longer_form_an_import_cycle() {
	let load = stdlib_loader(&[(
		"probe_main",
		"import @/option with (Option)\nimport @/result with (Result)\n\
		 func main(): void = {}",
	)]);
	let diags = check_project("probe_main", &load);

	assert!(
		!diags.iter().any(|d| d.diag.code.contains("CYCLE")),
		"expected no import-cycle diagnostic between option/result, got: {diags:?}"
	);
}

#[test]
fn option_alone_no_longer_forms_an_import_cycle() {
	// Loading `option.nym` alone must not pull in `@/result` for `ok_or_else`.
	let load = stdlib_loader(&[(
		"probe_main",
		"import @/option with (Option)\nfunc main(): void = {}",
	)]);
	let diags = check_project("probe_main", &load);

	assert!(
		!diags.iter().any(|d| d.diag.code.contains("CYCLE")),
		"expected no import-cycle diagnostic from option alone, got: {diags:?}"
	);
}
