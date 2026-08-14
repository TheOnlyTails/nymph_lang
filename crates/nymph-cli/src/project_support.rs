//! Filesystem-backed target resolution and multi-module project support for
//! the CLI (`build`, `run`, `check`).
//!
//! A target falls back to loose mode only when discovery reports that no
//! `nymph.toml` exists. A found but unusable manifest is authoritative. The
//! manifest and filesystem policy remain owned by `nymph-project`; this module
//! owns only the commands' shared selection policy.

use std::path::{Path, PathBuf};

use crate::compile_guard::{guarded, unsupported_feature_message};

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

/// Load the project selected for a project-only command. Discovery is strict:
/// unlike source-target commands, absence never falls back to loose-file mode,
/// and an explicit manifest path is always authoritative.
pub(crate) fn load_project(manifest: &ManifestSelection) -> anyhow::Result<nymph_project::Project> {
	match manifest {
		ManifestSelection::Discover => {
			let current_dir = nymph_project::normalize_path(std::env::current_dir()?)?;
			Ok(nymph_project::discover(&current_dir)?)
		}
		ManifestSelection::Explicit(path) => Ok(nymph_project::Project::load(path)?),
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetIntent {
	Entry,
	Library,
}

/// Filesystem context for a REPL. A missing source root is the deliberate
/// loose fallback and occurs only when discovery finds no manifest.
pub(crate) struct ReplContext {
	pub src_root: Option<PathBuf>,
}

pub(crate) fn resolve_repl(manifest: &ManifestSelection) -> anyhow::Result<ReplContext> {
	let current_dir = nymph_project::normalize_path(std::env::current_dir()?)?;
	let project = match manifest {
		ManifestSelection::Discover => match nymph_project::discover(&current_dir) {
			Ok(project) => Some(project),
			Err(nymph_project::DiscoverError::NotFound { .. }) => None,
			Err(error) => return Err(error.into()),
		},
		ManifestSelection::Explicit(path) => Some(nymph_project::Project::load(path)?),
	};
	Ok(ReplContext {
		src_root: project
			.map(|project| nymph_project::normalize_path(project.source_root()))
			.transpose()?,
	})
}

/// The shared target selected for `run`, `build`, or `check`.
pub(crate) struct ResolvedTarget {
	pub file: PathBuf,
	pub src_root: PathBuf,
	pub entry_key: String,
	pub intent: TargetIntent,
}

type SourceLoader = dyn Fn(&str) -> Option<String>;

/// Shared target, source loader, compiler dispatch, and diagnostic context for
/// a single `build`, `check`, or `run` operation.
pub(crate) struct ProjectOperation {
	target: ResolvedTarget,
	load: Box<SourceLoader>,
}

impl ProjectOperation {
	/// Resolve the command target and report selection failures consistently.
	pub fn resolve(file: Option<&Path>, manifest: &ManifestSelection) -> Option<Self> {
		let target = resolve(file, manifest)
			.map_err(|error| {
				eprintln!("error: {error}");
			})
			.ok()?;
		let load = Box::new(nymph_project::fs_loader(target.src_root.clone()));
		Some(Self { target, load })
	}

	pub fn target_file(&self) -> &Path {
		&self.target.file
	}

	pub fn check_selected_mode(&self) -> Vec<nymph_compiler::ProjectDiagnostic> {
		match self.target.intent {
			TargetIntent::Entry => {
				nymph_compiler::check_project_with_embedded_std(&self.target.entry_key, &self.load)
			}
			TargetIntent::Library => {
				nymph_compiler::check_project_library_with_embedded_std(&self.target.entry_key, &self.load)
			}
		}
	}

	pub fn compile_selected_mode(&self) -> Option<nymph_compiler::CompiledProject> {
		self.compile(self.target.intent)
	}

	pub fn compile_entry(&self) -> Option<nymph_compiler::CompiledProject> {
		self.compile(TargetIntent::Entry)
	}

	pub fn render(&self, diagnostics: &[nymph_compiler::ProjectDiagnostic]) -> String {
		render_project_diagnostics(diagnostics, &self.target.src_root, &self.load)
	}

	fn compile(&self, intent: TargetIntent) -> Option<nymph_compiler::CompiledProject> {
		let result = guarded(|| match intent {
			TargetIntent::Entry => nymph_compiler::compile_project_with_std(
				&self.target.entry_key,
				&self.load,
				&nymph_compiler::embedded_std_provider,
			),
			TargetIntent::Library => nymph_compiler::compile_project_library_with_std(
				&self.target.entry_key,
				&self.load,
				&nymph_compiler::embedded_std_provider,
			),
		});
		match result {
			Ok(Ok(compiled)) => Some(compiled),
			Ok(Err(diagnostics)) => {
				eprint!("{}", self.render(&diagnostics));
				None
			}
			Err(payload) => {
				eprintln!("{}", unsupported_feature_message(&payload));
				None
			}
		}
	}
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
			ensure_source_within_root(&file, &src_root)?;
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

fn ensure_source_within_root(file: &Path, src_root: &Path) -> anyhow::Result<()> {
	let canonical_file = std::fs::canonicalize(file)?;
	let canonical_root = std::fs::canonicalize(src_root)?;
	if canonical_file.starts_with(canonical_root) {
		Ok(())
	} else {
		anyhow::bail!(
			"source file {} is outside source root {}",
			file.display(),
			src_root.display()
		)
	}
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
