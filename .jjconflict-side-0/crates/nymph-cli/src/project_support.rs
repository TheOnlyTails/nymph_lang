//! Filesystem-backed target resolution and multi-module project support for
//! the CLI (`build`, `run`, `check`).
//!
//! A target falls back to loose mode only when discovery reports that no
//! `nymph.toml` exists. A found but unusable manifest is authoritative. The
//! manifest and filesystem policy remain owned by `nymph-project`; this module
//! owns only the commands' shared selection policy.

use std::path::{Path, PathBuf};
use std::{cell::RefCell, collections::BTreeMap, fs::OpenOptions, io::Write as _, rc::Rc};

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
	pub options: nymph_compiler::CompilerOptions,
}

type SourceLoader = dyn Fn(&str) -> Option<String>;

/// Shared target, source loader, compiler dispatch, and diagnostic context for
/// a single `build`, `check`, or `run` operation.
pub(crate) struct ProjectOperation {
	target: ResolvedTarget,
	load: Box<SourceLoader>,
	analyzed_sources: Rc<RefCell<BTreeMap<String, String>>>,
}

impl ProjectOperation {
	/// Resolve the command target and report selection failures consistently.
	pub fn resolve(
		file: Option<&Path>,
		manifest: &ManifestSelection,
		profile: nymph_compiler::BuildProfile,
	) -> Option<Self> {
		let target = resolve(file, manifest, profile)
			.map_err(|error| {
				eprintln!("error: {error}");
			})
			.ok()?;
		let fs_load = nymph_project::fs_loader(target.src_root.clone());
		let analyzed_sources = Rc::new(RefCell::new(BTreeMap::<String, String>::new()));
		let observed = analyzed_sources.clone();
		let load = Box::new(move |module: &str| {
			if let Some(source) = observed.borrow().get(module) {
				return Some(source.clone());
			}
			let source = fs_load(module)?;
			observed
				.borrow_mut()
				.entry(module.to_string())
				.or_insert_with(|| source.clone());
			Some(source)
		});
		Some(Self {
			target,
			load,
			analyzed_sources,
		})
	}

	pub fn target_file(&self) -> &Path {
		&self.target.file
	}

	pub fn check_selected_mode(&self) -> Vec<nymph_compiler::ProjectDiagnostic> {
		self.analyzed_sources.borrow_mut().clear();
		match self.target.intent {
			TargetIntent::Entry => nymph_compiler::check_project_with_embedded_std_and_options(
				&self.target.entry_key,
				&self.load,
				&self.target.options,
			),
			TargetIntent::Library => nymph_compiler::check_project_library_with_embedded_std_and_options(
				&self.target.entry_key,
				&self.load,
				&self.target.options,
			),
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
		let source_root = &self.target.src_root;
		let source_uri = |module: &str| {
			url::Url::from_file_path(
				nymph_compiler::ModulePath::new(module)
					.ok()?
					.source_file(source_root),
			)
			.ok()
			.map(String::from)
		};
		let result = guarded(|| match intent {
			TargetIntent::Entry => {
				nymph_compiler::compile_project_with_embedded_std_options_and_source_uris(
					&self.target.entry_key,
					&self.load,
					&self.target.options,
					&source_uri,
				)
			}
			TargetIntent::Library => {
				nymph_compiler::compile_project_library_with_embedded_std_options_and_source_uris(
					&self.target.entry_key,
					&self.load,
					&self.target.options,
					&source_uri,
				)
			}
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
	profile: nymph_compiler::BuildProfile,
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
			let options = nymph_compiler::CompilerOptions {
				profile,
				lints: project.manifest().lints.clone(),
			};
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
				options,
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
				options: nymph_compiler::CompilerOptions {
					profile,
					lints: Default::default(),
				},
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

pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
	let parent = path.parent().unwrap_or(Path::new("."));
	let permissions = std::fs::metadata(path)?.permissions();
	if permissions.readonly() {
		anyhow::bail!("source file is read-only");
	}
	let name = path
		.file_name()
		.and_then(|name| name.to_str())
		.unwrap_or("source.nym");
	for attempt in 0..1000_u32 {
		let temporary = parent.join(format!(
			".{name}.nymph-write-{}-{attempt}",
			std::process::id()
		));
		match OpenOptions::new()
			.write(true)
			.create_new(true)
			.open(&temporary)
		{
			Ok(mut output) => {
				let result = (|| -> std::io::Result<()> {
					output.set_permissions(permissions.clone())?;
					output.write_all(contents)?;
					output.sync_all()?;
					drop(output);
					replace_file(&temporary, path)
				})();
				if result.is_err() {
					let _ = std::fs::remove_file(&temporary);
				}
				return result.map_err(Into::into);
			}
			Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
			Err(error) => return Err(error.into()),
		}
	}
	anyhow::bail!("could not create an atomic temporary file")
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
	std::fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
	use std::os::windows::ffi::OsStrExt as _;
	use windows_sys::Win32::Storage::FileSystem::{
		MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
	};

	let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
	let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
	if unsafe {
		MoveFileExW(
			from.as_ptr(),
			to.as_ptr(),
			MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
		)
	} == 0
	{
		return Err(std::io::Error::last_os_error());
	}
	Ok(())
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
