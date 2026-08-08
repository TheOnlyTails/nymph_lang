use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::NymphCommand;
use crate::project_support::{
	ManifestSelection, fs_loader, load_project, render_project_diagnostics,
};

#[derive(clap::Args)]
pub(crate) struct DocCommand {
	/// Directory for the generated site (defaults to target/nymph/doc in the project root).
	#[arg(short, long, value_name = "DIR")]
	output: Option<PathBuf>,

	/// Open the generated index in the system browser after successful publication.
	#[arg(long)]
	open: bool,

	/// Include private declarations in addition to the public API.
	#[arg(long)]
	document_private_items: bool,
}

impl NymphCommand for DocCommand {
	fn run(&self, manifest: &ManifestSelection) -> i32 {
		self.run_with_opener(manifest, &SystemOpener)
	}
}

impl DocCommand {
	fn run_with_opener(&self, manifest: &ManifestSelection, opener: &dyn Opener) -> i32 {
		let project = match load_project(manifest) {
			Ok(project) => project,
			Err(error) => {
				eprintln!("error: {error}");
				return 1;
			}
		};
		let entry = match project.entry_module() {
			Ok(entry) => entry,
			Err(error) => {
				eprintln!(
					"error: invalid build entry for manifest {}: {error}",
					project.manifest_path().display()
				);
				return 1;
			}
		};
		let source_root = project.source_root();
		let load = fs_loader(source_root.clone());
		let documentation = match nymph_compiler::document_project(
			entry.as_str(),
			&load,
			nymph_compiler::DocOptions {
				document_private_items: self.document_private_items,
			},
		) {
			Ok(documentation) => documentation,
			Err(diagnostics) => {
				eprint!(
					"{}",
					render_project_diagnostics(&diagnostics, &source_root, &load)
				);
				return 1;
			}
		};
		let output = self
			.output
			.clone()
			.unwrap_or_else(|| project.root().join("target/nymph/doc"));
		if let Err(error) = publish(&output, &documentation.render_html()) {
			eprintln!("error: could not publish {}: {error}", output.display());
			return 1;
		}
		let index = output.join("index.html");
		if self.open
			&& let Err(error) = opener.open(&index)
		{
			eprintln!("error: could not open {}: {error}", index.display());
			return 1;
		}
		println!("generated documentation at {}", index.display());
		0
	}
}

trait Opener {
	fn open(&self, path: &Path) -> std::io::Result<()>;
}

struct SystemOpener;

impl Opener for SystemOpener {
	fn open(&self, path: &Path) -> std::io::Result<()> {
		#[cfg(target_os = "windows")]
		let mut command = {
			let mut command = Command::new("explorer");
			command.arg(path);
			command
		};
		#[cfg(target_os = "macos")]
		let mut command = {
			let mut command = Command::new("open");
			command.arg(path);
			command
		};
		#[cfg(all(unix, not(target_os = "macos")))]
		let mut command = {
			let mut command = Command::new("xdg-open");
			command.arg(path);
			command
		};
		#[cfg(not(any(unix, target_os = "windows")))]
		return Err(std::io::Error::new(
			std::io::ErrorKind::Unsupported,
			"opening documentation is unsupported on this platform",
		));
		let status = command.status()?;
		if status.success() {
			Ok(())
		} else {
			Err(std::io::Error::other(format!(
				"opener exited with {status}"
			)))
		}
	}
}

fn publish(
	output: &Path,
	files: &std::collections::BTreeMap<String, String>,
) -> std::io::Result<()> {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let parent = output
		.parent()
		.filter(|path| !path.as_os_str().is_empty())
		.unwrap_or_else(|| Path::new("."));
	std::fs::create_dir_all(parent)?;
	let name = output
		.file_name()
		.map_or_else(|| "doc".into(), |name| name.to_string_lossy());
	let stage = loop {
		let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
		let candidate = parent.join(format!(
			".{name}.nymph-doc-stage-{}-{unique}",
			std::process::id()
		));
		match std::fs::create_dir(&candidate) {
			Ok(()) => break candidate,
			Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
			Err(error) => return Err(error),
		}
	};
	let result = (|| {
		for (relative, contents) in files {
			let relative = Path::new(relative);
			if relative.is_absolute()
				|| relative
					.components()
					.any(|component| matches!(component, std::path::Component::ParentDir))
			{
				return Err(std::io::Error::new(
					std::io::ErrorKind::InvalidInput,
					"documentation output path escapes the staged tree",
				));
			}
			let path = stage.join(relative);
			if let Some(parent) = path.parent() {
				std::fs::create_dir_all(parent)?;
			}
			std::fs::write(path, contents)?;
		}
		commit_stage(&stage, output)?;
		Ok(())
	})();
	if result.is_err() {
		let _ = remove_path(&stage);
	}
	result
}

fn commit_stage(stage: &Path, output: &Path) -> std::io::Result<()> {
	if !output.try_exists()? {
		return std::fs::rename(stage, output);
	}
	#[cfg(any(
		target_os = "linux",
		target_os = "android",
		target_os = "macos",
		target_os = "ios"
	))]
	{
		rustix::fs::renameat_with(
			rustix::fs::CWD,
			stage,
			rustix::fs::CWD,
			output,
			rustix::fs::RenameFlags::EXCHANGE,
		)?;
		// The new tree is committed atomically; cleanup of the old tree cannot
		// turn successful publication into a false failure.
		let _ = remove_path(stage);
		Ok(())
	}
	#[cfg(not(any(
		target_os = "linux",
		target_os = "android",
		target_os = "macos",
		target_os = "ios"
	)))]
	{
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let parent = output.parent().unwrap_or_else(|| Path::new("."));
		let backup = loop {
			let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
			let candidate = parent.join(format!(".nymph-doc-backup-{}-{unique}", std::process::id()));
			if !candidate.try_exists()? {
				break candidate;
			}
		};
		std::fs::rename(output, &backup)?;
		if let Err(error) = std::fs::rename(stage, output) {
			return match std::fs::rename(&backup, output) {
				Ok(()) => Err(error),
				Err(rollback) => Err(std::io::Error::other(format!(
					"publication failed ({error}); rollback from {} also failed ({rollback})",
					backup.display()
				))),
			};
		}
		let _ = remove_path(&backup);
		Ok(())
	}
}

fn remove_path(path: &Path) -> std::io::Result<()> {
	let metadata = std::fs::symlink_metadata(path)?;
	if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
		std::fs::remove_dir_all(path)
	} else {
		std::fs::remove_file(path)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::cell::RefCell;

	struct RecordingOpener(RefCell<Vec<PathBuf>>);

	impl Opener for RecordingOpener {
		fn open(&self, path: &Path) -> std::io::Result<()> {
			self.0.borrow_mut().push(path.to_path_buf());
			Ok(())
		}
	}

	#[test]
	fn injected_opener_receives_only_the_published_index() {
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let root = std::env::temp_dir().join(format!(
			"nymph_doc_opener_{}_{}",
			std::process::id(),
			COUNTER.fetch_add(1, Ordering::Relaxed)
		));
		std::fs::create_dir_all(root.join("src")).unwrap();
		std::fs::write(
			root.join("nymph.toml"),
			"[package]\nname='docs'\nversion='1.0.0'\n",
		)
		.unwrap();
		std::fs::write(root.join("src/main.nym"), "public let answer: int = 42").unwrap();
		let output = root.join("site");
		let command = DocCommand {
			output: Some(output.clone()),
			open: true,
			document_private_items: false,
		};
		let opener = RecordingOpener(RefCell::new(Vec::new()));
		assert_eq!(
			command.run_with_opener(
				&ManifestSelection::Explicit(root.join("nymph.toml")),
				&opener
			),
			0
		);
		assert_eq!(*opener.0.borrow(), vec![output.join("index.html")]);
		std::fs::remove_dir_all(root).unwrap();
	}
}
