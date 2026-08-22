use crate::NymphCommand;
use crate::project_support::{ManifestSelection, atomic_write};
use anyhow::Context as _;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub(crate) struct FormatCommand {
	/// Report files that need formatting without changing them.
	#[arg(long)]
	check: bool,

	/// Source files to format. With none, format every source in the project.
	#[arg(value_name = "FILES")]
	files: Vec<PathBuf>,
}

impl NymphCommand for FormatCommand {
	fn run(&self, manifest: &ManifestSelection) -> i32 {
		let files = match select_files(&self.files, manifest) {
			Ok(files) => files,
			Err(error) => {
				eprintln!("error: {error}");
				return 2;
			}
		};
		let mut changed = 0;
		let mut failed = 0;
		for file in &files {
			match format_file(file, self.check) {
				Ok(false) => {}
				Ok(true) => {
					changed += 1;
					if self.check {
						eprintln!("would format {}", file.display());
					} else {
						eprintln!("formatted {}", file.display());
					}
				}
				Err(error) => {
					failed += 1;
					eprintln!("error: {error}");
				}
			}
		}
		if files.len() > 1 {
			if self.check && changed + failed > 0 {
				eprintln!("{} files require attention", changed + failed);
			} else if !self.check && changed > 0 {
				eprintln!("formatted {changed} files");
			}
		}
		if failed != 0 {
			2
		} else {
			i32::from(self.check && changed != 0)
		}
	}
}

fn select_files(files: &[PathBuf], manifest: &ManifestSelection) -> anyhow::Result<Vec<PathBuf>> {
	let project = match manifest {
		ManifestSelection::Explicit(path) => Some(nymph_project::Project::load(path)?),
		ManifestSelection::Discover if files.is_empty() => {
			let cwd = std::env::current_dir()?;
			Some(nymph_project::discover(&cwd).map_err(|error| {
				anyhow::anyhow!("no project source root to format: {error}; pass one or more .nym files")
			})?)
		}
		ManifestSelection::Discover => None,
	};
	let authoritative_root = project.as_ref().map(nymph_project::Project::source_root);
	let mut selected = BTreeSet::new();
	if files.is_empty() {
		collect_sources(
			authoritative_root.as_ref().expect("project selected"),
			&mut selected,
		)?;
	} else {
		for file in files {
			let file = nymph_project::normalize_path(file)?;
			if file.extension().and_then(|value| value.to_str()) != Some("nym") || !file.is_file() {
				anyhow::bail!(
					"source file does not exist or is not a .nym file: {}",
					file.display()
				);
			}
			if let Some(root) = &authoritative_root {
				ensure_within(&file, root)?;
			} else {
				match nymph_project::discover(file.parent().unwrap_or(Path::new("."))) {
					Ok(discovered) => ensure_within(&file, &discovered.source_root())?,
					Err(nymph_project::DiscoverError::NotFound { .. }) => {}
					Err(error) => return Err(error.into()),
				}
			}
			// Resolve aliases only after enforcing the source boundary implied by
			// the spelling the user supplied. This deduplicates symlinks and makes
			// the atomic replacement update the source rather than the link itself.
			selected.insert(fs::canonicalize(file)?);
		}
	}
	if selected.is_empty() {
		anyhow::bail!("no .nym source files selected");
	}
	Ok(selected.into_iter().collect())
}

fn ensure_within(file: &Path, root: &Path) -> anyhow::Result<()> {
	let file = fs::canonicalize(file)?;
	let root = fs::canonicalize(root)?;
	if !file.starts_with(&root) {
		anyhow::bail!(
			"source file {} is outside source root {}",
			file.display(),
			root.display()
		);
	}
	Ok(())
}

fn collect_sources(dir: &Path, selected: &mut BTreeSet<PathBuf>) -> anyhow::Result<()> {
	for entry in fs::read_dir(dir)? {
		let entry = entry?;
		let ty = entry.file_type()?;
		if ty.is_symlink() {
			continue;
		}
		let path = entry.path();
		if ty.is_dir() {
			if !matches!(entry.file_name().to_str(), Some("target" | "dependencies")) {
				collect_sources(&path, selected)?;
			}
		} else if ty.is_file() && path.extension().and_then(|value| value.to_str()) == Some("nym") {
			selected.insert(nymph_project::normalize_path(path)?);
		}
	}
	Ok(())
}

fn format_file(path: &Path, check: bool) -> anyhow::Result<bool> {
	let source =
		fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
	let formatted = nymph_format::format(&source, &path.display().to_string()).map_err(|error| {
		anyhow::anyhow!(
			"{}",
			nymph_diagnostics::render(&path.display().to_string(), &source, &error.diagnostics,)
		)
	})?;
	if formatted == source {
		return Ok(false);
	}
	if !check {
		atomic_write(path, formatted.as_bytes())
			.with_context(|| format!("could not write {}", path.display()))?;
	}
	Ok(true)
}
