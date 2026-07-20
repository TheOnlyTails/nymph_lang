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

use crate::config;

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

	let (config, root) = config::find_from(start_dir).ok()?;
	let src_root = config.src_root(&root);
	let rel = file_abs.strip_prefix(&src_root).ok()?;
	let entry_key = module_key_from_relative_path(rel)?;

	Some(Project {
		src_root,
		entry_key,
	})
}

/// Turn a source-root-relative file path into the driver's canonical module
/// key: `/`-separated, `.nym` extension stripped (`geometry/vec.nym` →
/// `"geometry/vec"`).
fn module_key_from_relative_path(rel: &Path) -> Option<String> {
	let without_ext = rel.with_extension("");
	let mut segs = Vec::new();
	for component in without_ext.components() {
		segs.push(component.as_os_str().to_str()?.to_string());
	}
	if segs.is_empty() {
		None
	} else {
		Some(segs.join("/"))
	}
}

/// Build the FS-backed `load` closure a `src_root`'s project driver call
/// needs: a canonical key `"a/b"` maps to `<src_root>/a/b.nym`.
pub(crate) fn fs_loader(src_root: PathBuf) -> impl Fn(&str) -> Option<String> {
	move |key: &str| {
		let path = src_root.join(format!("{key}.nym"));
		std::fs::read_to_string(path).ok()
	}
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
