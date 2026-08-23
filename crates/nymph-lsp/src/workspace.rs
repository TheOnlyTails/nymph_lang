//! LSP adaptation around the shared `nymph-project` filesystem policy.
//!
//! A project is a `nymph.toml` file plus a source root (`<root>/<src>`,
//! `src` defaulting to `"src"`); a canonical module *key* is that root-
//! relative path with the `.nym` extension stripped and components joined
//! by `/` (`<src_root>/a/b.nym` <-> `"a/b"`) — the same convention
//! `nymph_compiler::project`'s `load` closures use.

use std::path::{Path, PathBuf};

use lsp_types::Uri;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};

#[cfg(test)]
thread_local! {
	static PATH_CONVERSIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
	static PROJECT_DETECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// An open project: its source root directory and the canonical module key
/// of the file that triggered detection (the driver's graph root — see
/// `nymph_compiler::check_project_library`'s doc comment on transitive-
/// closure-only checking).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
	pub src_root: PathBuf,
	pub entry_key: String,
	pub lints: std::collections::BTreeMap<String, nymph_compiler::LintLevel>,
}

/// Filesystem policy for an LSP document URI.
///
/// Only `file:` URIs may carry paths. File-backed documents are further
/// separated by whether they belong to a discovered Nymph project, so close
/// handling can reload project files while clearing loose and non-file
/// documents without touching the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriClass {
	ProjectFile {
		path: PathBuf,
		project: Project,
	},
	LooseFile {
		path: PathBuf,
	},
	/// An editor-owned unsaved document with no filesystem identity.
	Untitled,
	/// Any other non-file scheme. These retain their existing strict tooling
	/// behavior and are not opted into untitled recovery semantics.
	NonFile,
}

/// Classify an LSP URI according to the server's filesystem policy.
///
/// Project discovery errors remain authoritative for `file:` URIs. Non-file
/// URIs never pass through path conversion or project discovery.
pub fn classify_uri(uri: &Uri) -> anyhow::Result<UriClass> {
	if uri
		.scheme()
		.is_some_and(|scheme| scheme.as_str().eq_ignore_ascii_case("untitled"))
	{
		return Ok(UriClass::Untitled);
	}
	let Some(path) = uri_to_path(uri) else {
		return Ok(UriClass::NonFile);
	};
	Ok(match detect(&path)? {
		Some(project) => UriClass::ProjectFile { path, project },
		None => UriClass::LooseFile { path },
	})
}

/// Climb from `file`'s directory looking for the nearest `nymph.toml`; if
/// found, and `file` (canonicalized) lies under that project's resolved
/// source root, return the project and `file`'s own canonical module key.
/// Returns `None` only for a bare, project-less file. Discovery and source-
/// path errors are authoritative and must be surfaced instead of selecting
/// loose-file checking.
pub fn detect(file: &Path) -> anyhow::Result<Option<Project>> {
	#[cfg(test)]
	PROJECT_DETECTIONS.set(PROJECT_DETECTIONS.get() + 1);
	let file_abs = std::path::absolute(file)?;
	let start_dir = file_abs
		.parent()
		.ok_or_else(|| anyhow::anyhow!("source file has no parent: {}", file.display()))?;

	let project = match nymph_project::discover(start_dir) {
		Ok(project) => project,
		Err(nymph_project::DiscoverError::NotFound { .. }) => return Ok(None),
		Err(error) => return Err(error.into()),
	};
	let src_root = project.source_root();
	let entry_key = project
		.module_for_file(&file_abs)
		.map_err(|error| {
			anyhow::anyhow!(
				"invalid source path for manifest {}: {error}",
				project.manifest_path().display()
			)
		})?
		.as_str()
		.to_string();

	Ok(Some(Project {
		src_root,
		entry_key,
		lints: project.manifest().lints.clone(),
	}))
}

/// Build the FS-backed `load` closure a `src_root`'s project driver call
/// needs: a canonical key `"a/b"` maps to `<src_root>/a/b.nym`.
pub fn fs_loader(src_root: PathBuf) -> impl Fn(&str) -> Option<String> {
	nymph_project::fs_loader(src_root)
}

/// The inverse of [`module_key_from_relative_path`]: the file `Uri` a
/// canonical module `key` lives at under `src_root`, for mapping a
/// `ProjectDiagnostic` (keyed by module) back to the file it should be
/// published against.
#[must_use]
pub fn key_to_uri(src_root: &Path, key: &str) -> Option<Uri> {
	path_to_uri(&nymph_project::file_for_module(
		src_root,
		&nymph_compiler::ModulePath::new(key).ok()?,
	))
}

/// Characters that must be percent-encoded in a `file://` URI's path
/// component: anything outside the unreserved/sub-delim set that could
/// otherwise be misparsed (space, quote marks, angle brackets, `%` itself,
/// etc.). `/` is deliberately excluded so path separators survive intact.
const PATH_ENCODE_SET: &AsciiSet = &CONTROLS
	.add(b' ')
	.add(b'"')
	.add(b'#')
	.add(b'%')
	.add(b'<')
	.add(b'>')
	.add(b'?')
	.add(b'[')
	.add(b']')
	.add(b'^')
	.add(b'`')
	.add(b'{')
	.add(b'|')
	.add(b'}');

/// Build a `file://` [`Uri`] from an absolute filesystem path, percent-
/// encoding any character in a path segment that would otherwise make the
/// resulting string an invalid (or, worse, silently different) URI — e.g. a
/// literal space in `/home/user/My Documents/project`.
#[must_use]
pub fn path_to_uri(path: &Path) -> Option<Uri> {
	let s = path.to_str()?;
	let encoded = utf8_percent_encode(s, PATH_ENCODE_SET);
	format!("file://{encoded}").parse().ok()
}

/// The filesystem path a `file://` [`Uri`] refers to, percent-decoding the
/// path component (`lsp_types::Uri::path` returns it still encoded, e.g.
/// `%20` for a space). Non-file URIs have no filesystem path.
#[must_use]
pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
	#[cfg(test)]
	PATH_CONVERSIONS.set(PATH_CONVERSIONS.get() + 1);
	if !uri
		.scheme()
		.is_some_and(|scheme| scheme.as_str().eq_ignore_ascii_case("file"))
	{
		return None;
	}
	let decoded = percent_decode_str(uri.path().as_str()).decode_utf8_lossy();
	Some(PathBuf::from(decoded.into_owned()))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicU64, Ordering};

	static COUNTER: AtomicU64 = AtomicU64::new(0);

	/// A scratch directory under the system temp dir, removed on drop.
	struct TempDir(PathBuf);

	impl TempDir {
		fn new() -> Self {
			let n = COUNTER.fetch_add(1, Ordering::Relaxed);
			let dir = std::env::temp_dir().join(format!(
				"nymph-lsp-test-{}-{n}-{:?}",
				std::process::id(),
				std::time::SystemTime::now()
					.duration_since(std::time::UNIX_EPOCH)
					.unwrap()
					.as_nanos()
			));
			std::fs::create_dir_all(&dir).unwrap();
			Self(dir)
		}

		fn path(&self) -> &Path {
			&self.0
		}
	}

	impl Drop for TempDir {
		fn drop(&mut self) {
			let _ = std::fs::remove_dir_all(&self.0);
		}
	}

	#[test]
	fn detects_a_project_and_computes_the_nested_module_key() {
		let tmp = TempDir::new();
		std::fs::write(
			tmp.path().join("nymph.toml"),
			"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n[lints]\necho-in-release = \"deny\"\n",
		)
		.unwrap();
		std::fs::create_dir_all(tmp.path().join("src/sub")).unwrap();
		std::fs::write(tmp.path().join("src/sub/b.nym"), "func b(): void = {}").unwrap();

		let project = detect(&tmp.path().join("src/sub/b.nym"))
			.unwrap()
			.expect("should detect a project");
		assert_eq!(project.entry_key, "sub/b");
		assert_eq!(
			project.lints.get("echo-in-release"),
			Some(&nymph_compiler::LintLevel::Deny)
		);
		assert_eq!(
			std::path::absolute(project.src_root).unwrap(),
			std::path::absolute(tmp.path().join("src")).unwrap()
		);
	}

	#[test]
	fn a_file_outside_any_project_is_not_detected() {
		let tmp = TempDir::new();
		std::fs::write(tmp.path().join("loose.nym"), "func f(): void = {}").unwrap();
		assert!(detect(&tmp.path().join("loose.nym")).unwrap().is_none());
	}

	#[test]
	fn fs_loader_reads_a_module_by_its_canonical_key() {
		let tmp = TempDir::new();
		std::fs::create_dir_all(tmp.path().join("a")).unwrap();
		std::fs::write(tmp.path().join("a/b.nym"), "func f(): void = {}").unwrap();

		let loader = fs_loader(tmp.path().to_path_buf());
		assert_eq!(loader("a/b"), Some("func f(): void = {}".to_string()));
		assert_eq!(loader("missing"), None);
	}

	#[test]
	fn uri_to_path_percent_decodes_the_path_component() {
		let uri: Uri = "file:///tmp/my%20project/src/a.nym".parse().unwrap();
		assert_eq!(
			uri_to_path(&uri),
			Some(PathBuf::from("/tmp/my project/src/a.nym"))
		);
	}

	#[test]
	fn non_file_uris_never_become_filesystem_paths() {
		let untitled: Uri = "untitled:Untitled-1".parse().unwrap();
		PATH_CONVERSIONS.set(0);
		PROJECT_DETECTIONS.set(0);
		assert_eq!(classify_uri(&untitled).unwrap(), UriClass::Untitled);
		assert_eq!(
			PATH_CONVERSIONS.get(),
			0,
			"untitled entered path conversion"
		);
		assert_eq!(
			PROJECT_DETECTIONS.get(),
			0,
			"untitled entered project detection"
		);
		assert_eq!(uri_to_path(&untitled), None);

		let notebook: Uri = "nymph-notebook:/cell/1".parse().unwrap();
		assert_eq!(uri_to_path(&notebook), None);
		assert_eq!(classify_uri(&notebook).unwrap(), UriClass::NonFile);
	}

	#[test]
	fn path_to_uri_percent_encodes_characters_that_need_escaping() {
		let path = Path::new("/tmp/my project/src/a.nym");
		let uri = path_to_uri(path).expect("a path with a space should still yield a Uri");
		assert!(
			uri.as_str().contains("%20"),
			"expected the space to be percent-encoded, got {}",
			uri.as_str()
		);
		assert!(
			!uri.as_str().contains(' '),
			"a raw space must not appear in the URI string, got {}",
			uri.as_str()
		);
	}

	#[test]
	fn path_to_uri_and_uri_to_path_round_trip_through_a_path_with_a_space() {
		let path = Path::new("/tmp/my project/src/a.nym");
		let uri = path_to_uri(path).expect("a path with a space should still yield a Uri");
		assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
	}

	#[test]
	fn detect_finds_a_project_whose_root_contains_a_space() {
		let tmp = TempDir::new();
		let root = tmp.path().join("my project");
		std::fs::create_dir_all(root.join("src/sub")).unwrap();
		std::fs::write(
			root.join("nymph.toml"),
			"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
		)
		.unwrap();
		std::fs::write(root.join("src/sub/b.nym"), "func b(): void = {}").unwrap();

		let project = detect(&root.join("src/sub/b.nym"))
			.unwrap()
			.expect("should detect a project despite the space");
		assert_eq!(project.entry_key, "sub/b");
	}

	#[test]
	fn key_to_uri_round_trips_through_a_src_root_containing_a_space() {
		let tmp = TempDir::new();
		let src_root = tmp.path().join("my project/src");
		std::fs::create_dir_all(&src_root).unwrap();

		let uri = key_to_uri(&src_root, "a/b").expect("should build a Uri for the module key");
		assert_eq!(uri_to_path(&uri), Some(src_root.join("a/b.nym")));
	}
}
