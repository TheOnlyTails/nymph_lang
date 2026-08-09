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
		if !documentation.diagnostics.is_empty() {
			eprint!(
				"{}",
				render_project_diagnostics(&documentation.diagnostics, &source_root, &load)
			);
		}
		let output = self
			.output
			.clone()
			.unwrap_or_else(|| project.root().join("target/nymph/doc"));
		let published = match publish(&output, &documentation.render_html()) {
			Ok(published) => published,
			Err(error) => {
				eprintln!("error: could not publish {}: {error}", output.display());
				return 1;
			}
		};
		let index = published.join("index.html");
		let display_index = output.join("index.html");
		if self.open {
			let absolute_index = match std::fs::canonicalize(&index) {
				Ok(index) => index,
				Err(error) => {
					eprintln!("error: could not open {}: {error}", display_index.display());
					return 1;
				}
			};
			if let Err(error) = opener.open(&absolute_index) {
				eprintln!("error: could not open {}: {error}", display_index.display());
				return 1;
			}
		}
		println!("generated documentation at {}", display_index.display());
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
) -> std::io::Result<PathBuf> {
	publish_before_commit(output, files, || {})
}

fn publish_before_commit(
	output: &Path,
	files: &std::collections::BTreeMap<String, String>,
	before_commit: impl FnOnce(),
) -> std::io::Result<PathBuf> {
	let parent = output
		.parent()
		.filter(|path| !path.as_os_str().is_empty())
		.unwrap_or_else(|| Path::new("."));
	std::fs::create_dir_all(parent)?;
	let parent = std::fs::canonicalize(parent)?;
	let name = output
		.file_name()
		.ok_or_else(|| {
			std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				"documentation output must name a directory below its parent",
			)
		})?
		.to_os_string();
	#[cfg(any(
		target_os = "linux",
		target_os = "android",
		target_os = "macos",
		target_os = "ios"
	))]
	return publish_pinned(parent, name, files, before_commit);
	#[cfg(not(any(
		target_os = "linux",
		target_os = "android",
		target_os = "macos",
		target_os = "ios"
	)))]
	publish_by_path(parent, name, files, before_commit)
}

#[cfg(not(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
)))]
fn publish_by_path(
	parent: PathBuf,
	name: std::ffi::OsString,
	files: &std::collections::BTreeMap<String, String>,
	before_commit: impl FnOnce(),
) -> std::io::Result<PathBuf> {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let output = parent.join(&name);
	let stage = loop {
		let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
		let candidate = parent.join(format!(".nymph-doc-stage-{}-{unique}", std::process::id()));
		match std::fs::create_dir(&candidate) {
			Ok(()) => break candidate,
			Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
			Err(error) => return Err(error),
		}
	};
	let staged = std::fs::symlink_metadata(&stage)?;
	let write_result = (|| {
		for (relative, contents) in files {
			let relative = Path::new(relative);
			if relative.is_absolute()
				|| relative.components().any(|component| {
					matches!(
						component,
						std::path::Component::Prefix(_)
							| std::path::Component::RootDir
							| std::path::Component::ParentDir
					)
				}) {
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
		Ok(())
	})();
	if let Err(error) = write_result {
		let _ = remove_path_if_same(&stage, &staged);
		return Err(error);
	}
	before_commit();
	commit_stage_by_path(&stage, &output, &staged)?;
	Ok(output)
}

#[cfg(not(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
)))]
fn commit_stage_by_path(
	stage: &Path,
	output: &Path,
	staged: &std::fs::Metadata,
) -> std::io::Result<()> {
	ensure_same_file(stage, staged)?;
	match std::fs::symlink_metadata(output) {
		Ok(_) => {
			let _ = remove_path_if_same(stage, staged);
			Err(std::io::Error::new(
				std::io::ErrorKind::Unsupported,
				"atomic replacement of an existing documentation tree is unsupported on this platform",
			))
		}
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			let result = rename_noreplace(stage, output);
			if result.is_err() {
				let _ = remove_path_if_same(stage, staged);
			}
			result
		}
		Err(error) => {
			let _ = remove_path_if_same(stage, staged);
			Err(error)
		}
	}
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
struct PinnedParent {
	fd: rustix::fd::OwnedFd,
	path: PathBuf,
	stat: rustix::fs::Stat,
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn publish_pinned(
	parent: PathBuf,
	name: std::ffi::OsString,
	files: &std::collections::BTreeMap<String, String>,
	before_commit: impl FnOnce(),
) -> std::io::Result<PathBuf> {
	use rustix::fs::{Mode, OFlags};

	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let fd = rustix::fs::open(
		&parent,
		OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
		Mode::empty(),
	)
	.map_err(std::io::Error::from)?;
	let pinned = PinnedParent {
		stat: rustix::fs::fstat(&fd).map_err(std::io::Error::from)?,
		fd,
		path: parent,
	};
	ensure_pinned_parent(&pinned)?;
	let stage = loop {
		let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
		let candidate = format!(".nymph-doc-stage-{}-{unique}", std::process::id());
		match rustix::fs::mkdirat(
			&pinned.fd,
			candidate.as_str(),
			Mode::RWXU | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH,
		) {
			Ok(()) => break std::ffi::OsString::from(candidate),
			Err(rustix::io::Errno::EXIST) => continue,
			Err(error) => return Err(error.into()),
		}
	};
	let staged = stat_at(&pinned.fd, &stage)?;
	let stage_fd = match rustix::fs::openat(
		&pinned.fd,
		&stage,
		OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
		Mode::empty(),
	) {
		Ok(fd) => fd,
		Err(error) => {
			let _ = remove_at_if_same(&pinned.fd, &stage, &staged);
			return Err(error.into());
		}
	};
	if let Err(error) = ensure_same_stat(
		&rustix::fs::fstat(&stage_fd).map_err(std::io::Error::from)?,
		&staged,
		&stage,
	) {
		let _ = remove_at_if_same(&pinned.fd, &stage, &staged);
		return Err(error);
	}
	let write_result = files
		.iter()
		.try_for_each(|(relative, contents)| write_file_at(&stage_fd, relative, contents));
	if let Err(error) = write_result {
		let _ = remove_at_if_same(&pinned.fd, &stage, &staged);
		return Err(error);
	}
	before_commit();
	if let Err(error) = ensure_pinned_parent(&pinned) {
		let _ = remove_at_if_same(&pinned.fd, &stage, &staged);
		return Err(error);
	}
	commit_stage_at(&pinned.fd, &stage, &name, &staged)?;
	Ok(pinned.path.join(name))
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn write_file_at(
	stage: &rustix::fd::OwnedFd,
	relative: &str,
	contents: &str,
) -> std::io::Result<()> {
	use rustix::fs::{Mode, OFlags};

	let components = Path::new(relative)
		.components()
		.map(|component| match component {
			std::path::Component::Normal(name) => Ok(name.to_os_string()),
			_ => Err(std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				"documentation output path escapes the staged tree",
			)),
		})
		.collect::<std::io::Result<Vec<_>>>()?;
	let Some((file_name, directories)) = components.split_last() else {
		return Err(std::io::Error::new(
			std::io::ErrorKind::InvalidInput,
			"documentation output path must name a file",
		));
	};
	let mut directory = rustix::io::dup(stage).map_err(std::io::Error::from)?;
	for component in directories {
		match rustix::fs::mkdirat(
			&directory,
			component,
			Mode::RWXU | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH,
		) {
			Ok(()) | Err(rustix::io::Errno::EXIST) => {}
			Err(error) => return Err(error.into()),
		}
		directory = rustix::fs::openat(
			&directory,
			component,
			OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
			Mode::empty(),
		)
		.map_err(std::io::Error::from)?;
	}
	let file = rustix::fs::openat(
		&directory,
		file_name,
		OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
		Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
	)
	.map_err(std::io::Error::from)?;
	let mut remaining = contents.as_bytes();
	while !remaining.is_empty() {
		let written = rustix::io::write(&file, remaining).map_err(std::io::Error::from)?;
		if written == 0 {
			return Err(std::io::Error::new(
				std::io::ErrorKind::WriteZero,
				"could not finish writing staged documentation",
			));
		}
		remaining = &remaining[written..];
	}
	Ok(())
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn commit_stage_at(
	parent: &rustix::fd::OwnedFd,
	stage: &std::ffi::OsStr,
	output: &std::ffi::OsStr,
	staged: &rustix::fs::Stat,
) -> std::io::Result<()> {
	ensure_same_at(parent, stage, staged)?;
	let previous = match stat_at(parent, output) {
		Ok(metadata) => metadata,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			let result = rename_noreplace_at(parent, stage, output);
			if result.is_err() {
				let _ = remove_at_if_same(parent, stage, staged);
			}
			return result;
		}
		Err(error) => {
			let _ = remove_at_if_same(parent, stage, staged);
			return Err(error);
		}
	};
	if let Err(error) = rename_exchange_at(parent, stage, output) {
		let _ = remove_at_if_same(parent, stage, staged);
		return Err(error);
	}
	let exchanged = stat_at(parent, stage);
	let destination_changed = match &exchanged {
		Ok(exchanged) => !same_stat(exchanged, &previous),
		Err(_) => true,
	};
	if destination_changed {
		let changed = std::io::Error::new(
			std::io::ErrorKind::WouldBlock,
			"documentation destination changed during publication",
		);
		return match rename_exchange_at(parent, stage, output) {
			Ok(()) => {
				let _ = remove_at_if_same(parent, stage, staged);
				Err(changed)
			}
			Err(rollback) => Err(std::io::Error::other(format!(
				"{changed}; restoring the raced destination also failed ({rollback}); preserved staged tree: {}",
				stage.to_string_lossy()
			))),
		};
	}
	// The new tree is committed atomically; cleanup of the old tree cannot
	// turn successful publication into a false failure.
	let _ = remove_at_if_same(parent, stage, &previous);
	Ok(())
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn rename_exchange_at(
	parent: &rustix::fd::OwnedFd,
	left: &std::ffi::OsStr,
	right: &std::ffi::OsStr,
) -> std::io::Result<()> {
	rustix::fs::renameat_with(
		parent,
		left,
		parent,
		right,
		rustix::fs::RenameFlags::EXCHANGE,
	)
	.map_err(std::io::Error::from)
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn rename_noreplace_at(
	parent: &rustix::fd::OwnedFd,
	from: &std::ffi::OsStr,
	to: &std::ffi::OsStr,
) -> std::io::Result<()> {
	rustix::fs::renameat_with(parent, from, parent, to, rustix::fs::RenameFlags::NOREPLACE)
		.map_err(std::io::Error::from)
}

#[cfg(not(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
)))]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
	#[cfg(windows)]
	{
		use std::os::windows::ffi::OsStrExt;

		let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
		let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
		if unsafe {
			windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
		} == 0
		{
			return Err(std::io::Error::last_os_error());
		}
		return Ok(());
	}
	#[cfg(not(windows))]
	Err(std::io::Error::new(
		std::io::ErrorKind::Unsupported,
		"atomic documentation publication is unsupported on this platform",
	))
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn ensure_pinned_parent(parent: &PinnedParent) -> std::io::Result<()> {
	let actual = rustix::fs::stat(&parent.path).map_err(std::io::Error::from)?;
	ensure_same_stat(&actual, &parent.stat, &parent.path)
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn stat_at(
	parent: &rustix::fd::OwnedFd,
	name: &std::ffi::OsStr,
) -> std::io::Result<rustix::fs::Stat> {
	rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
		.map_err(std::io::Error::from)
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn same_stat(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
	left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn ensure_same_stat(
	actual: &rustix::fs::Stat,
	expected: &rustix::fs::Stat,
	path: impl AsRef<Path>,
) -> std::io::Result<()> {
	if same_stat(actual, expected) {
		Ok(())
	} else {
		Err(std::io::Error::new(
			std::io::ErrorKind::WouldBlock,
			format!(
				"refusing to operate on changed path {}",
				path.as_ref().display()
			),
		))
	}
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn ensure_same_at(
	parent: &rustix::fd::OwnedFd,
	name: &std::ffi::OsStr,
	expected: &rustix::fs::Stat,
) -> std::io::Result<()> {
	ensure_same_stat(&stat_at(parent, name)?, expected, Path::new(name))
}

#[cfg(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
))]
fn remove_at_if_same(
	parent: &rustix::fd::OwnedFd,
	name: &std::ffi::OsStr,
	expected: &rustix::fs::Stat,
) -> std::io::Result<()> {
	use std::os::unix::ffi::OsStrExt;

	use rustix::fs::{AtFlags, FileType, Mode, OFlags};

	ensure_same_at(parent, name, expected)?;
	if FileType::from_raw_mode(expected.st_mode).is_dir() {
		let child = rustix::fs::openat(
			parent,
			name,
			OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
			Mode::empty(),
		)
		.map_err(std::io::Error::from)?;
		ensure_same_stat(
			&rustix::fs::fstat(&child).map_err(std::io::Error::from)?,
			expected,
			Path::new(name),
		)?;
		let directory = rustix::fs::Dir::read_from(&child).map_err(std::io::Error::from)?;
		for entry in directory {
			let entry = entry.map_err(std::io::Error::from)?;
			let entry_name = entry.file_name();
			if entry_name.to_bytes() == b"." || entry_name.to_bytes() == b".." {
				continue;
			}
			let entry_stat = rustix::fs::statat(&child, entry_name, AtFlags::SYMLINK_NOFOLLOW)
				.map_err(std::io::Error::from)?;
			remove_at_if_same(
				&child,
				std::ffi::OsStr::from_bytes(entry_name.to_bytes()),
				&entry_stat,
			)?;
		}
		ensure_same_at(parent, name, expected)?;
		rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(std::io::Error::from)
	} else {
		ensure_same_at(parent, name, expected)?;
		rustix::fs::unlinkat(parent, name, AtFlags::empty()).map_err(std::io::Error::from)
	}
}

#[cfg(not(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
)))]
fn remove_path(path: &Path) -> std::io::Result<()> {
	let metadata = std::fs::symlink_metadata(path)?;
	if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
		std::fs::remove_dir_all(path)
	} else {
		std::fs::remove_file(path)
	}
}

#[cfg(not(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
)))]
fn remove_path_if_same(path: &Path, expected: &std::fs::Metadata) -> std::io::Result<()> {
	ensure_same_file(path, expected)?;
	remove_path(path)
}

#[cfg(not(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
)))]
fn ensure_same_file(path: &Path, expected: &std::fs::Metadata) -> std::io::Result<()> {
	let actual = std::fs::symlink_metadata(path)?;
	if same_file(&actual, expected) {
		Ok(())
	} else {
		Err(std::io::Error::new(
			std::io::ErrorKind::WouldBlock,
			format!("refusing to operate on changed path {}", path.display()),
		))
	}
}

#[cfg(all(
	unix,
	not(any(
		target_os = "linux",
		target_os = "android",
		target_os = "macos",
		target_os = "ios"
	))
))]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
	use std::os::unix::fs::MetadataExt;
	left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
	use std::os::windows::fs::MetadataExt;
	left.volume_serial_number() == right.volume_serial_number()
		&& left.file_index() == right.file_index()
}

#[cfg(not(any(
	unix,
	windows,
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	target_os = "ios"
)))]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
	left.file_type() == right.file_type()
		&& left.len() == right.len()
		&& left.modified().ok() == right.modified().ok()
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

	#[cfg(any(
		target_os = "linux",
		target_os = "android",
		target_os = "macos",
		target_os = "ios"
	))]
	#[test]
	fn publication_replaces_a_symlink_without_removing_its_target() {
		use std::os::unix::fs::symlink;

		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let root = std::env::temp_dir().join(format!(
			"nymph_doc_symlink_{}_{}",
			std::process::id(),
			COUNTER.fetch_add(1, Ordering::Relaxed)
		));
		let target = root.join("target");
		let output = root.join("site");
		std::fs::create_dir_all(&target).unwrap();
		std::fs::write(target.join("preserved.txt"), "outside output").unwrap();
		symlink(&target, &output).unwrap();
		let files = std::collections::BTreeMap::from([(
			"index.html".to_string(),
			"new documentation".to_string(),
		)]);

		publish(&output, &files).unwrap();

		let metadata = std::fs::symlink_metadata(&output).unwrap();
		assert!(metadata.is_dir());
		assert!(!metadata.file_type().is_symlink());
		assert_eq!(
			std::fs::read_to_string(output.join("index.html")).unwrap(),
			"new documentation"
		);
		assert_eq!(
			std::fs::read_to_string(target.join("preserved.txt")).unwrap(),
			"outside output"
		);
		std::fs::remove_dir_all(root).unwrap();
	}

	#[cfg(unix)]
	#[test]
	fn publication_stays_in_the_original_parent_when_a_symlink_is_swapped() {
		use std::os::unix::fs::symlink;

		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let root = std::env::temp_dir().join(format!(
			"nymph_doc_parent_symlink_{}_{}",
			std::process::id(),
			COUNTER.fetch_add(1, Ordering::Relaxed)
		));
		let original = root.join("original");
		let replacement = root.join("replacement");
		let parent = root.join("parent");
		std::fs::create_dir_all(&original).unwrap();
		std::fs::create_dir_all(&replacement).unwrap();
		symlink(&original, &parent).unwrap();
		let files = std::collections::BTreeMap::from([(
			"index.html".to_string(),
			"new documentation".to_string(),
		)]);

		let published = publish_before_commit(&parent.join("site"), &files, || {
			std::fs::remove_file(&parent).unwrap();
			symlink(&replacement, &parent).unwrap();
		})
		.unwrap();

		assert_eq!(published, original.join("site"));
		assert_eq!(
			std::fs::read_to_string(original.join("site/index.html")).unwrap(),
			"new documentation"
		);
		assert!(!replacement.join("site").exists());
		std::fs::remove_dir_all(root).unwrap();
	}

	#[cfg(any(
		target_os = "linux",
		target_os = "android",
		target_os = "macos",
		target_os = "ios"
	))]
	#[test]
	fn publication_refuses_a_replaced_canonical_parent_and_preserves_the_old_tree() {
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let root = std::env::temp_dir().join(format!(
			"nymph_doc_parent_replace_{}_{}",
			std::process::id(),
			COUNTER.fetch_add(1, Ordering::Relaxed)
		));
		let parent = root.join("parent");
		let moved = root.join("moved");
		let replacement = root.join("replacement");
		std::fs::create_dir_all(parent.join("site")).unwrap();
		std::fs::write(parent.join("site/index.html"), "old documentation").unwrap();
		std::fs::create_dir_all(&replacement).unwrap();
		let files = std::collections::BTreeMap::from([(
			"index.html".to_string(),
			"new documentation".to_string(),
		)]);

		let error = publish_before_commit(&parent.join("site"), &files, || {
			std::fs::rename(&parent, &moved).unwrap();
			std::fs::rename(&replacement, &parent).unwrap();
		})
		.unwrap_err();

		assert!(
			matches!(
				error.kind(),
				std::io::ErrorKind::NotFound | std::io::ErrorKind::WouldBlock
			),
			"{error}"
		);
		assert_eq!(
			std::fs::read_to_string(moved.join("site/index.html")).unwrap(),
			"old documentation"
		);
		assert!(!parent.join("site").exists());
		assert!(std::fs::read_dir(&moved).unwrap().all(|entry| {
			!entry
				.unwrap()
				.file_name()
				.to_string_lossy()
				.contains("nymph-doc-stage")
		}));
		std::fs::remove_dir_all(root).unwrap();
	}
}
