//! Filesystem-backed target resolution and multi-module project support for
//! the CLI (`build`, `run`, `check`).
//!
//! A target falls back to loose mode only when discovery reports that no
//! `nymph.toml` exists. A found but unusable manifest is authoritative. The
//! manifest and filesystem policy remain owned by `nymph-project`; this module
//! owns only the commands' shared selection policy.

use std::path::{Path, PathBuf};

/// Select whether target resolution discovers the nearest conventional
/// manifest or loads one explicit path authoritatively.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ManifestSelection {
	#[default]
	Discover,
	Explicit(PathBuf),
}

impl From<Option<PathBuf>> for ManifestSelection {
	fn from(path: Option<PathBuf>) -> Self {
		path.map_or(Self::Discover, Self::Explicit)
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetIntent {
	Entry,
	Library,
}

/// The shared target selected for `run`, `build`, or `check`.
pub(crate) struct ResolvedTarget {
	pub file: PathBuf,
	pub src_root: PathBuf,
	pub entry_key: String,
	pub intent: TargetIntent,
}

/// Resolve an optional explicit file using the selected manifest policy. An
/// explicit manifest is loaded exactly and never falls back to discovery.
/// With a project and no file, the manifest's `build.entry` is selected
/// relative to its source root. Without a discovered project, an explicit file
/// is a loose library; without either, selection fails with a usage hint.
pub(crate) fn resolve(
	file: Option<&Path>,
	manifest: &ManifestSelection,
) -> anyhow::Result<ResolvedTarget> {
	let explicit_file = file.map(nymph_project::normalize_path).transpose()?;
	let current_dir = nymph_project::normalize_path(std::env::current_dir()?)?;
	let start_dir = match &explicit_file {
		Some(file) => file
			.parent()
			.ok_or_else(|| anyhow::anyhow!("source file has no parent: {}", file.display()))?,
		None => &current_dir,
	};

	let project = match manifest {
		ManifestSelection::Discover => match nymph_project::discover(start_dir) {
			Ok(project) => Some(project),
			Err(nymph_project::DiscoverError::NotFound { .. }) => None,
			Err(error) => return Err(error.into()),
		},
		ManifestSelection::Explicit(path) => Some(nymph_project::Project::load(path)?),
	};

	match project {
		Some(project) => {
			let src_root = project.source_root();
			let entry_module = project.entry_module().map_err(|error| {
				anyhow::anyhow!(
					"invalid build entry for manifest {}: {error}",
					project.manifest_path().display()
				)
			})?;
			let (file, module) = match explicit_file {
				Some(file) => {
					let module = project.module_for_file(&file).map_err(|error| {
						anyhow::anyhow!(
							"invalid source path for manifest {}: {error}",
							project.manifest_path().display()
						)
					})?;
					(file, module)
				}
				None => (
					nymph_project::file_for_module(&src_root, &entry_module),
					entry_module.clone(),
				),
			};
			ensure_source_file(&file)?;
			let intent = if module == entry_module {
				TargetIntent::Entry
			} else {
				TargetIntent::Library
			};
			Ok(ResolvedTarget {
				file,
				src_root,
				entry_key: module.as_str().to_string(),
				intent,
			})
		}
		None => {
			let file = explicit_file.ok_or_else(|| {
				anyhow::anyhow!(
					"no nymph.toml found and no source file was provided; pass a .nym file or run from inside a Nymph project"
				)
			})?;
			ensure_source_file(&file)?;
			let src_root = file
				.parent()
				.ok_or_else(|| anyhow::anyhow!("source file has no parent: {}", file.display()))?
				.to_path_buf();
			let entry_key = nymph_project::module_from_file(&src_root, &file)?
				.as_str()
				.to_string();
			Ok(ResolvedTarget {
				file,
				src_root,
				entry_key,
				intent: TargetIntent::Library,
			})
		}
	}
}

fn ensure_source_file(file: &Path) -> anyhow::Result<()> {
	if file.is_file() {
		Ok(())
	} else {
		anyhow::bail!("target source file does not exist: {}", file.display())
	}
}

/// Build the FS-backed `load` closure a `src_root`'s project driver call
/// needs: a canonical key `"a/b"` maps to `<src_root>/a/b.nym`.
pub(crate) fn fs_loader(src_root: PathBuf) -> impl Fn(&str) -> Option<String> {
	nymph_project::fs_loader(src_root)
}

/// Render a batch of project diagnostics, each against its own module's
/// filesystem path and source (re-read through `load`).
pub(crate) fn render_project_diagnostics(
	diags: &[nymph_compiler::ProjectDiagnostic],
	src_root: &Path,
	load: &dyn Fn(&str) -> Option<String>,
) -> String {
	let mut out = String::new();
	for d in diags {
		let source = load(&d.module).unwrap_or_default();
		let filename = nymph_compiler::ModulePath::new(&d.module).map_or_else(
			|_| format!("{}.nym", d.module),
			|module| {
				nymph_project::file_for_module(src_root, &module)
					.display()
					.to_string()
			},
		);
		out.push_str(&nymph_diagnostics::render(
			&filename,
			&source,
			std::slice::from_ref(&d.diag),
		));
	}
	out
}
