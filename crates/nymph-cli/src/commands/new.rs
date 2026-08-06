use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, bail};

use crate::NymphCommand;
use crate::project_support::ManifestSelection;

const BINARY_SOURCE: &str = "func main(): void = {}\n";
const LIBRARY_SOURCE: &str = "public func hello(): string = \"Hello, world!\"\n";

#[derive(clap::Args)]
pub(crate) struct NewCommand {
	/// Destination for the new package.
	#[arg(value_name = "PATH")]
	path: PathBuf,

	/// Create a library package with src/lib.nym.
	#[arg(long)]
	lib: bool,

	/// Do not initialize a Git repository.
	#[arg(long)]
	no_git: bool,
}

impl NymphCommand for NewCommand {
	fn run(&self, _manifest: &ManifestSelection) -> i32 {
		match create_project(&self.path, self.lib, self.no_git) {
			Ok(name) => {
				let kind = if self.lib { "library" } else { "binary" };
				println!("Created {kind} package `{name}`");
				0
			}
			Err(error) => {
				eprintln!("error: {error:#}");
				1
			}
		}
	}
}

#[derive(Clone, Copy)]
enum DestinationState {
	Absent,
	EmptyDirectory,
}

fn create_project(path: &Path, library: bool, no_git: bool) -> anyhow::Result<String> {
	let name = package_name(path)?;
	let destination = nymph_project::normalize_path(path)?;
	let destination_state = inspect_destination(&destination)?;
	let parent = destination
		.parent()
		.filter(|parent| !parent.as_os_str().is_empty())
		.unwrap_or_else(|| Path::new("."));
	let existing_ancestor = existing_ancestor(parent)?;
	let mut staging = StagingDirectory::new(&existing_ancestor, destination.file_name().unwrap())?;

	let manifest = nymph_project::Manifest::new(name.clone());
	let source_name = if library { "lib.nym" } else { "main.nym" };
	std::fs::create_dir(staging.path().join("src"))?;
	std::fs::write(
		staging.path().join(nymph_project::MANIFEST_FILE),
		manifest.to_toml()?,
	)?;
	std::fs::write(
		staging.path().join("src").join(source_name),
		if library {
			LIBRARY_SOURCE
		} else {
			BINARY_SOURCE
		},
	)?;

	if !no_git {
		initialize_git(staging.path())?;
	}

	publish(
		staging.path(),
		&destination,
		parent,
		existing_ancestor,
		destination_state,
	)?;
	staging.disarm();
	Ok(name)
}

fn package_name(path: &Path) -> anyhow::Result<String> {
	let basename = path
		.file_name()
		.ok_or_else(|| anyhow::anyhow!("destination must end with a package name"))?;
	let name = basename
		.to_str()
		.ok_or_else(|| anyhow::anyhow!("destination basename is not valid Unicode: {basename:?}"))?;
	nymph_project::validate_package_name(name)
		.with_context(|| format!("invalid package name derived from `{name}`"))?;
	Ok(name.to_owned())
}

fn inspect_destination(path: &Path) -> anyhow::Result<DestinationState> {
	match std::fs::symlink_metadata(path) {
		Ok(metadata) if !metadata.file_type().is_dir() => {
			bail!(
				"destination already exists and is not a directory: {}",
				path.display()
			)
		}
		Ok(_) => {
			let mut entries = std::fs::read_dir(path)
				.with_context(|| format!("could not inspect destination {}", path.display()))?;
			if entries.next().transpose()?.is_some() {
				bail!("destination is not empty: {}", path.display());
			}
			Ok(DestinationState::EmptyDirectory)
		}
		Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DestinationState::Absent),
		Err(error) => {
			Err(error).with_context(|| format!("could not inspect destination {}", path.display()))
		}
	}
}

fn existing_ancestor(path: &Path) -> anyhow::Result<PathBuf> {
	let mut candidate = path;
	loop {
		match std::fs::metadata(candidate) {
			Ok(metadata) if metadata.is_dir() => return Ok(candidate.to_path_buf()),
			Ok(_) => bail!("parent path is not a directory: {}", candidate.display()),
			Err(error) if error.kind() == io::ErrorKind::NotFound => {
				candidate = candidate
					.parent()
					.ok_or_else(|| anyhow::anyhow!("destination has no existing directory ancestor"))?;
			}
			Err(error) => {
				return Err(error)
					.with_context(|| format!("could not inspect parent {}", candidate.display()));
			}
		}
	}
}

fn initialize_git(staging: &Path) -> anyhow::Result<()> {
	let result = Command::new("git")
		.args(["init", "--quiet"])
		.arg(staging)
		.output()
		.context("could not run `git init`; install Git or pass --no-git")?;
	if !result.status.success() {
		let stderr = String::from_utf8_lossy(&result.stderr);
		let detail = stderr.trim();
		if detail.is_empty() {
			bail!("`git init` failed with {}", result.status);
		}
		bail!("`git init` failed: {detail}");
	}
	Ok(())
}

fn publish(
	staging: &Path,
	destination: &Path,
	parent: &Path,
	existing_ancestor: PathBuf,
	state: DestinationState,
) -> anyhow::Result<()> {
	match state {
		DestinationState::Absent => {
			let created_parents = create_missing_parents(parent, &existing_ancestor)?;
			if let Err(error) = rename_noreplace(staging, destination) {
				cleanup_created_parents(&created_parents);
				return Err(error)
					.with_context(|| format!("could not publish destination {}", destination.display()));
			}
		}
		DestinationState::EmptyDirectory => {
			std::fs::rename(staging, destination).with_context(|| {
				format!(
					"destination is no longer an empty directory: {}",
					destination.display()
				)
			})?;
		}
	}
	Ok(())
}

fn create_missing_parents(parent: &Path, existing_ancestor: &Path) -> io::Result<Vec<PathBuf>> {
	let mut missing = Vec::new();
	let mut candidate = parent;
	while candidate != existing_ancestor {
		missing.push(candidate.to_path_buf());
		candidate = candidate.parent().ok_or_else(|| {
			io::Error::new(
				io::ErrorKind::InvalidInput,
				"parent escaped existing ancestor",
			)
		})?;
	}

	let mut created = Vec::new();
	for path in missing.iter().rev() {
		match std::fs::create_dir(path) {
			Ok(()) => created.push(path.clone()),
			Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
			Err(error) => {
				cleanup_created_parents(&created);
				return Err(error);
			}
		}
	}
	Ok(created)
}

fn cleanup_created_parents(created: &[PathBuf]) {
	for path in created.iter().rev() {
		let _ = std::fs::remove_dir(path);
	}
}

#[cfg(any(
	target_vendor = "apple",
	target_os = "linux",
	target_os = "android",
	target_os = "redox"
))]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
	rustix::fs::renameat_with(
		rustix::fs::CWD,
		from,
		rustix::fs::CWD,
		to,
		rustix::fs::RenameFlags::NOREPLACE,
	)
	.map_err(Into::into)
}

#[cfg(windows)]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
	// Windows rename fails rather than replacing an existing destination.
	std::fs::rename(from, to)
}

#[cfg(not(any(
	target_vendor = "apple",
	target_os = "linux",
	target_os = "android",
	target_os = "redox",
	windows
)))]
fn rename_noreplace(_from: &Path, _to: &Path) -> io::Result<()> {
	Err(io::Error::new(
		io::ErrorKind::Unsupported,
		"atomic project publication is not supported on this platform",
	))
}

fn unique_sibling(parent: &Path, basename: &OsStr, suffix: &str) -> io::Result<PathBuf> {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	for _ in 0..100 {
		let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
		let path = parent.join(format!(
			".{}.nymph-new-{suffix}-{}-{unique}",
			basename.to_string_lossy(),
			std::process::id()
		));
		if !path.try_exists()? {
			return Ok(path);
		}
	}
	Err(io::Error::new(
		io::ErrorKind::AlreadyExists,
		"could not allocate a temporary project path",
	))
}

struct StagingDirectory {
	path: Option<PathBuf>,
}

impl StagingDirectory {
	fn new(parent: &Path, basename: &OsStr) -> io::Result<Self> {
		for _ in 0..100 {
			let path = unique_sibling(parent, basename, "stage")?;
			match std::fs::create_dir(&path) {
				Ok(()) => return Ok(Self { path: Some(path) }),
				Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
				Err(error) => return Err(error),
			}
		}
		Err(io::Error::new(
			io::ErrorKind::AlreadyExists,
			"could not create a staging directory",
		))
	}

	fn path(&self) -> &Path {
		self.path.as_deref().unwrap()
	}

	fn disarm(&mut self) {
		self.path = None;
	}
}

impl Drop for StagingDirectory {
	fn drop(&mut self) {
		if let Some(path) = &self.path {
			let _ = std::fs::remove_dir_all(path);
		}
	}
}
