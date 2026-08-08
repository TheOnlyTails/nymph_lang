//! Filesystem policy shared by Nymph command-line and editor tooling.
//!
//! This crate owns manifest parsing and discovery and converts filesystem
//! paths to the canonical [`nymph_compiler::ModulePath`] owned by the compiler.
//! It deliberately owns no compiler session, module identity, or Salsa state.

use std::{
	collections::BTreeMap,
	io,
	path::{Component, Path, PathBuf},
};

use nymph_compiler::ModulePath;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

pub const MANIFEST_FILE: &str = "nymph.toml";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
	pub package: Package,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub dependencies: BTreeMap<String, Dependency>,
	#[serde(default, skip_serializing_if = "Build::is_default")]
	pub build: Build,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Package {
	pub name: String,
	pub version: Version,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub description: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub private: Option<bool>,
	#[serde(default = "default_src", skip_serializing_if = "is_default_src")]
	pub src: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Dependency {
	Version(VersionReq),
	Detailed(DependencyDetail),
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DependencyDetail {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub version: Option<VersionReq>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub path: Option<PathBuf>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub git: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Build {
	#[serde(default = "default_entry")]
	pub entry: PathBuf,
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub disable_implicit_prelude: bool,
}

impl Default for Build {
	fn default() -> Self {
		Self {
			entry: default_entry(),
			disable_implicit_prelude: false,
		}
	}
}

impl Build {
	fn is_default(&self) -> bool {
		self == &Self::default()
	}
}

fn default_src() -> PathBuf {
	PathBuf::from("src")
}
fn is_default_src(path: &Path) -> bool {
	path == Path::new("src")
}
fn default_entry() -> PathBuf {
	PathBuf::from("main.nym")
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
	"package name must start with a lowercase ASCII letter and contain only lowercase ASCII letters, digits, or hyphens"
)]
pub struct PackageNameError;

pub fn validate_package_name(name: &str) -> Result<(), PackageNameError> {
	let mut chars = name.chars();
	if !chars.next().is_some_and(|ch| ch.is_ascii_lowercase())
		|| chars.any(|ch| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'))
	{
		return Err(PackageNameError);
	}
	Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
	#[error("could not read manifest {path}: {source}")]
	Read {
		path: PathBuf,
		#[source]
		source: io::Error,
	},
	#[error("malformed TOML in manifest {path}: {source}")]
	Parse {
		path: PathBuf,
		#[source]
		source: toml::de::Error,
	},
	#[error("invalid manifest schema in {path}: {source}")]
	Schema {
		path: PathBuf,
		#[source]
		source: toml::de::Error,
	},
	#[error("invalid package name `{name}` in manifest {path}: {source}")]
	InvalidPackageName {
		path: PathBuf,
		name: String,
		#[source]
		source: PackageNameError,
	},
	#[error("invalid `{field}` path in manifest {path}: {value}")]
	InvalidPath {
		path: PathBuf,
		field: &'static str,
		value: PathBuf,
	},
}

impl Manifest {
	#[must_use]
	pub fn new(name: String) -> Self {
		Self {
			package: Package {
				name,
				version: Version::new(0, 1, 0),
				description: None,
				private: None,
				src: default_src(),
			},
			dependencies: BTreeMap::new(),
			build: Build::default(),
		}
	}

	pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
		toml::to_string_pretty(self)
	}

	pub fn read(path: &Path) -> Result<Self, ManifestError> {
		let contents = std::fs::read_to_string(path).map_err(|source| ManifestError::Read {
			path: path.into(),
			source,
		})?;
		let value: toml::Value = toml::from_str(&contents).map_err(|source| ManifestError::Parse {
			path: path.into(),
			source,
		})?;
		let manifest: Self = value.try_into().map_err(|source| ManifestError::Schema {
			path: path.into(),
			source,
		})?;
		manifest.validate_paths(path)?;
		Ok(manifest)
	}

	fn validate_paths(&self, manifest_path: &Path) -> Result<(), ManifestError> {
		validate_package_name(&self.package.name).map_err(|source| {
			ManifestError::InvalidPackageName {
				path: manifest_path.into(),
				name: self.package.name.clone(),
				source,
			}
		})?;
		if !is_contained_relative(&self.package.src) {
			return Err(ManifestError::InvalidPath {
				path: manifest_path.into(),
				field: "package.src",
				value: self.package.src.clone(),
			});
		}
		if !is_contained_relative(&self.build.entry)
			|| self.build.entry.extension().and_then(|ext| ext.to_str()) != Some("nym")
		{
			return Err(ManifestError::InvalidPath {
				path: manifest_path.into(),
				field: "build.entry",
				value: self.build.entry.clone(),
			});
		}
		Ok(())
	}
}

fn is_contained_relative(path: &Path) -> bool {
	!path.as_os_str().is_empty()
		&& !path.is_absolute()
		&& !path.components().any(|component| {
			matches!(
				component,
				Component::ParentDir | Component::RootDir | Component::Prefix(_)
			)
		})
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
	#[error("no {MANIFEST_FILE} found from {start} or its ancestors")]
	NotFound { start: PathBuf },
	#[error(transparent)]
	Manifest(#[from] ManifestError),
}

#[derive(Debug, Clone)]
pub struct Project {
	manifest_path: PathBuf,
	root: PathBuf,
	manifest: Manifest,
}

impl Project {
	/// Load a project from exactly `manifest_path`.
	///
	/// Unlike [`discover`], this never searches ancestors or substitutes the
	/// conventional manifest filename. Relative manifest fields are resolved
	/// from the directory containing the selected file.
	pub fn load(manifest_path: &Path) -> Result<Self, ManifestError> {
		let root = manifest_path
			.parent()
			.filter(|path| !path.as_os_str().is_empty())
			.unwrap_or_else(|| Path::new("."));
		Ok(Self {
			manifest: Manifest::read(manifest_path)?,
			manifest_path: manifest_path.into(),
			root: root.into(),
		})
	}

	#[must_use]
	pub fn manifest_path(&self) -> &Path {
		&self.manifest_path
	}

	#[must_use]
	pub fn root(&self) -> &Path {
		&self.root
	}

	#[must_use]
	pub fn manifest(&self) -> &Manifest {
		&self.manifest
	}

	#[must_use]
	pub fn source_root(&self) -> PathBuf {
		self.root.join(&self.manifest.package.src)
	}

	pub fn entry_module(&self) -> Result<ModulePath, PathError> {
		module_from_relative_file(&self.manifest.build.entry)
	}

	pub fn module_for_file(&self, file: &Path) -> Result<ModulePath, PathError> {
		module_from_file(&self.source_root(), file)
	}
}

pub fn discover(start: &Path) -> Result<Project, DiscoverError> {
	let original = start.to_path_buf();
	let mut dir = start.to_path_buf();
	loop {
		let path = dir.join(MANIFEST_FILE);
		let found = path.try_exists().map_err(|source| {
			DiscoverError::Manifest(ManifestError::Read {
				path: path.clone(),
				source,
			})
		})?;
		if found {
			return Project::load(&path).map_err(DiscoverError::Manifest);
		}
		if !dir.pop() {
			return Err(DiscoverError::NotFound { start: original });
		}
	}
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
	#[error("source file {file} is outside source root {source_root}")]
	OutsideSourceRoot { source_root: PathBuf, file: PathBuf },
	#[error("source path must be a contained relative .nym file: {0}")]
	InvalidSourceFile(PathBuf),
}

/// Make a path absolute and lexically normalize `.` and `..` components.
/// This deliberately does not require the path to exist or resolve symlinks,
/// so editor overlays and prospective source paths remain representable.
pub fn normalize_path(path: impl AsRef<Path>) -> io::Result<PathBuf> {
	let absolute = std::path::absolute(path)?;
	let mut normalized = PathBuf::new();
	for component in absolute.components() {
		match component {
			Component::CurDir => {}
			Component::ParentDir => {
				normalized.pop();
			}
			Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
				normalized.push(component.as_os_str());
			}
		}
	}
	Ok(normalized)
}

pub fn module_from_file(source_root: &Path, file: &Path) -> Result<ModulePath, PathError> {
	let root =
		normalize_path(source_root).map_err(|_| PathError::InvalidSourceFile(source_root.into()))?;
	let file = normalize_path(file).map_err(|_| PathError::InvalidSourceFile(file.into()))?;
	let relative = file
		.strip_prefix(&root)
		.map_err(|_| PathError::OutsideSourceRoot {
			source_root: root,
			file: file.clone(),
		})?;
	module_from_relative_file(relative)
}

fn module_from_relative_file(path: &Path) -> Result<ModulePath, PathError> {
	if path.is_absolute()
		|| path.components().any(|c| {
			matches!(
				c,
				Component::ParentDir | Component::RootDir | Component::Prefix(_)
			)
		}) {
		return Err(PathError::InvalidSourceFile(path.into()));
	}
	ModulePath::from_source_file(path).map_err(|_| PathError::InvalidSourceFile(path.into()))
}

#[must_use]
pub fn file_for_module(source_root: &Path, module: &ModulePath) -> PathBuf {
	module.source_file(source_root)
}

pub fn fs_loader(source_root: PathBuf) -> impl Fn(&str) -> Option<String> {
	move |key| {
		ModulePath::new(key)
			.ok()
			.and_then(|module| std::fs::read_to_string(file_for_module(&source_root, &module)).ok())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn schema_defaults_dependencies_and_build_entry() {
		let value: toml::Value = toml::from_str("[package]\nname='x'\nversion='1.2.3'\n[dependencies]\nfoo='^2'\n[build]\nentry='bin/start.nym'\n").unwrap();
		let manifest: Manifest = value.try_into().unwrap();
		assert_eq!(manifest.package.src, Path::new("src"));
		assert!(manifest.dependencies.contains_key("foo"));
		assert_eq!(manifest.build.entry, Path::new("bin/start.nym"));
		let defaults: Manifest = toml::from_str("[package]\nname='x'\nversion='1.0.0'").unwrap();
		assert!(defaults.dependencies.is_empty());
		assert_eq!(defaults.build.entry, Path::new("main.nym"));
	}

	#[test]
	fn package_names_have_one_shared_ascii_grammar() {
		for valid in ["a", "hello-world", "app2"] {
			assert_eq!(validate_package_name(valid), Ok(()), "{valid}");
		}
		for invalid in ["", "2app", "Hello", "hello_world", "hello--world!", "café"] {
			assert_eq!(
				validate_package_name(invalid),
				Err(PackageNameError),
				"{invalid}"
			);
		}
	}

	#[test]
	fn canonical_manifest_serialization_round_trips() {
		let binary = Manifest::new("hello-world".into());
		assert_eq!(
			binary.to_toml().unwrap(),
			"[package]\nname = \"hello-world\"\nversion = \"0.1.0\"\n"
		);
		let parsed: Manifest = toml::from_str(&binary.to_toml().unwrap()).unwrap();
		assert_eq!(parsed.package.name, "hello-world");
		assert_eq!(parsed.build, Build::default());

		let mut library = Manifest::new("my-lib".into());
		library.build.entry = "lib.nym".into();
		assert_eq!(
			library.to_toml().unwrap(),
			"[package]\nname = \"my-lib\"\nversion = \"0.1.0\"\n\n[build]\nentry = \"lib.nym\"\n"
		);
	}

	#[test]
	fn read_distinguishes_syntax_and_missing_package_schema() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join(MANIFEST_FILE);
		std::fs::write(&path, "not = [toml").unwrap();
		assert!(matches!(
			Manifest::read(&path),
			Err(ManifestError::Parse { .. })
		));
		std::fs::write(&path, "name='x'").unwrap();
		assert!(matches!(
			Manifest::read(&path),
			Err(ManifestError::Schema { .. })
		));
	}

	#[test]
	fn discovery_and_key_file_round_trip() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::create_dir_all(temp.path().join("src/a")).unwrap();
		std::fs::write(
			temp.path().join(MANIFEST_FILE),
			"[package]\nname='x'\nversion='1.0.0'",
		)
		.unwrap();
		let file = temp.path().join("src/a/b.nym");
		let project = discover(file.parent().unwrap()).unwrap();
		let module = project.module_for_file(&file).unwrap();
		assert_eq!(module.as_str(), "a/b");
		assert_eq!(file_for_module(&project.source_root(), &module), file);
		assert!(matches!(
			project.module_for_file(&temp.path().join("other.nym")),
			Err(PathError::OutsideSourceRoot { .. })
		));
	}

	#[test]
	fn source_paths_are_lexically_normalized_before_mapping() {
		let temp = tempfile::tempdir().unwrap();
		let source_root = temp.path().join("src");
		let file = source_root.join("nested/../main.nym");
		assert_eq!(
			module_from_file(&source_root, &file).unwrap().as_str(),
			"main"
		);
	}

	#[test]
	fn discovery_distinguishes_absent_and_found_valid_manifests() {
		let temp = tempfile::tempdir().unwrap();
		let nested = temp.path().join("src/nested");
		std::fs::create_dir_all(&nested).unwrap();
		assert!(matches!(
			discover(&nested),
			Err(DiscoverError::NotFound { .. })
		));

		let manifest_path = temp.path().join(MANIFEST_FILE);
		std::fs::write(&manifest_path, "[package]\nname='x'\nversion='1.0.0'").unwrap();
		assert_eq!(discover(&nested).unwrap().manifest_path(), manifest_path);
	}

	#[test]
	fn explicit_loading_uses_the_exact_path_and_its_directory_as_root() {
		let temp = tempfile::tempdir().unwrap();
		let selected = temp.path().join("selected/project.toml");
		std::fs::create_dir_all(selected.parent().unwrap().join("code/bin")).unwrap();
		std::fs::write(
			&selected,
			"[package]\nname='x'\nversion='1.0.0'\nsrc='code'\n[build]\nentry='bin/start.nym'",
		)
		.unwrap();

		let project = Project::load(&selected).unwrap();
		assert_eq!(project.manifest_path(), selected);
		assert_eq!(project.root(), temp.path().join("selected"));
		assert_eq!(project.source_root(), temp.path().join("selected/code"));
		assert_eq!(
			file_for_module(&project.source_root(), &project.entry_module().unwrap()),
			temp.path().join("selected/code/bin/start.nym")
		);
	}

	#[test]
	fn explicit_loading_does_not_fall_back_to_a_conventional_manifest() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join(MANIFEST_FILE),
			"[package]\nname='x'\nversion='1.0.0'",
		)
		.unwrap();
		let selected = temp.path().join("missing.toml");

		assert!(matches!(
			Project::load(&selected),
			Err(ManifestError::Read { path, .. }) if path == selected
		));
	}

	#[test]
	fn discovery_reports_found_unreadable_content_with_its_path() {
		let temp = tempfile::tempdir().unwrap();
		let manifest_path = temp.path().join(MANIFEST_FILE);
		std::fs::create_dir(&manifest_path).unwrap();
		let error = discover(temp.path()).unwrap_err();
		assert!(matches!(
			&error,
			DiscoverError::Manifest(ManifestError::Read { path, .. }) if path == &manifest_path
		));
		assert!(
			error
				.to_string()
				.contains(&manifest_path.display().to_string())
		);
	}

	#[test]
	fn discovery_reports_invalid_utf8_toml_and_schema_with_their_path() {
		let temp = tempfile::tempdir().unwrap();
		let manifest_path = temp.path().join(MANIFEST_FILE);
		for (contents, expected) in [
			(vec![0xff], "read"),
			(b"not = [toml".to_vec(), "TOML"),
			(b"name='x'".to_vec(), "schema"),
		] {
			std::fs::write(&manifest_path, contents).unwrap();
			let error = discover(temp.path()).unwrap_err();
			assert!(error.to_string().contains(expected), "{error}");
			assert!(
				error
					.to_string()
					.contains(&manifest_path.display().to_string())
			);
		}
	}

	#[test]
	fn entry_cannot_escape_source_root() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join(MANIFEST_FILE);
		for entry in ["../main.nym", "/tmp/main.nym", "main", "main.txt"] {
			std::fs::write(
				&path,
				format!("[package]\nname='x'\nversion='1.0.0'\n[build]\nentry='{entry}'"),
			)
			.unwrap();
			assert!(matches!(
				discover(temp.path()),
				Err(DiscoverError::Manifest(ManifestError::InvalidPath {
					field: "build.entry",
					..
				}))
			));
		}
	}

	#[test]
	fn source_root_must_be_contained_by_the_project() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join(MANIFEST_FILE);
		for src in ["../src", "/tmp/src"] {
			std::fs::write(
				&path,
				format!("[package]\nname='x'\nversion='1.0.0'\nsrc='{src}'"),
			)
			.unwrap();
			assert!(matches!(
				discover(temp.path()),
				Err(DiscoverError::Manifest(ManifestError::InvalidPath {
					field: "package.src",
					..
				}))
			));
		}
	}

	#[test]
	fn tracked_manifests_use_the_canonical_schema() {
		let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
		for relative in [
			"examples/fizzbuzz/nymph.toml",
			"examples/hello-world/nymph.toml",
			"examples/http-server/nymph.toml",
			"examples/shapes/nymph.toml",
			"examples/todo-cli/nymph.toml",
			"examples/word-frequency/nymph.toml",
			"stdlib/nymph.toml",
		] {
			Manifest::read(&workspace.join(relative))
				.unwrap_or_else(|error| panic!("{relative} must parse: {error}"));
		}
	}
}
