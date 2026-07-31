//! Filesystem-backed multi-module project support for the CLI (`build`,
//! `run`, `check`): detects the nearest `nymph.toml` project (if any), builds
//! the `nymph_compiler::project` loader closure over real files, and maps a
//! given `.nym` file argument to the canonical module key the driver expects.
//!
//! A file with no discoverable `nymph.toml` project (or one that lies
//! outside its resolved source root) falls back to the existing
//! single-file `compile_guarded` path unchanged — this module only adds
//! cross-module import resolution on top of what's already there.

use std::path::{Path, PathBuf};

/// An open project: its source root directory and a canonical key
/// identifying which file to treat as the driver's graph root.
pub(crate) struct Project {
	pub src_root: PathBuf,
	pub entry_key: String,
}

/// Climb from `file`'s directory looking for the nearest `nymph.toml` (see
/// [`config::find_from`]); if found, and `file` (canonicalized) lies under
/// that project's resolved source root, return the project and `file`'s own
/// canonical module key. Returns `None` for a bare, project-less file (or one
/// outside the resolved source root) — the caller should fall back to
/// single-file compilation in that case.
pub(crate) fn detect(file: &Path) -> Option<Project> {
	let file_abs = std::path::absolute(file).ok()?;
	let start_dir = file_abs.parent()?;

	let project = nymph_project::discover(start_dir).ok()?;
	let src_root = project.source_root();
	let entry_key = project
		.module_for_file(&file_abs)
		.ok()?
		.as_str()
		.to_string();

	Some(Project {
		src_root,
		entry_key,
	})
}

/// Treat a bare, project-less `.nym` file as its own single-file project:
/// rooted at the file's own directory (so `import @/sibling` still resolves
/// against neighbouring files), with the file itself as the graph entry. This
/// is what lets a lone file `import std/…` (resolved via the embedded std
/// provider the driver is threaded with) without a `nymph.toml`.
pub(crate) fn single_file(file: &Path) -> Option<Project> {
	let file_abs = std::path::absolute(file).ok()?;
	let src_root = file_abs.parent()?.to_path_buf();
	let entry_key = nymph_project::module_from_file(&src_root, &file_abs)
		.ok()?
		.as_str()
		.to_string();
	Some(Project {
		src_root,
		entry_key,
	})
}

/// Build the FS-backed `load` closure a `src_root`'s project driver call
/// needs: a canonical key `"a/b"` maps to `<src_root>/a/b.nym`.
pub(crate) fn fs_loader(src_root: PathBuf) -> impl Fn(&str) -> Option<String> {
	nymph_project::fs_loader(src_root)
}

/// Render a batch of project diagnostics, each against its own module's
/// source (re-read through `load` — cheap, and keeps this driver
/// filesystem-agnostic; see `nymph_compiler::project`'s doc comment).
pub(crate) fn render_project_diagnostics(
	diags: &[nymph_compiler::ProjectDiagnostic],
	load: &dyn Fn(&str) -> Option<String>,
) -> String {
	let mut out = String::new();
	for d in diags {
		let source = load(&d.module).unwrap_or_default();
		let filename = format!("{}.nym", d.module);
		out.push_str(&nymph_diagnostics::render(
			&filename,
			&source,
			std::slice::from_ref(&d.diag),
		));
	}
	out
}
