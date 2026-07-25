use std::{
	collections::HashMap,
	env::current_dir,
	fs::{self, File},
	io::{self, Read},
	path::{Path, PathBuf},
};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::NymphCli;

/// The project's source directory name, relative to the `nymph.toml` root.
/// The default matches the design spec (`docs/superpowers/specs/2026-07-15-nymph-import-binding-design.md`):
/// `@/a/b` resolves to `<root>/<src>/a/b.nym`, and the entry module is
/// `<root>/<src>/main.nym`.
fn default_src() -> String {
	"src".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NymphConfig {
	package: Package,
	dependencies: HashMap<String, Dependency>,
}

impl NymphConfig {
	/// The project's source root: `<root>/<package.src>`.
	pub(crate) fn src_root(&self, root: &Path) -> PathBuf {
		root.join(&self.package.src)
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Package {
	name: String,
	version: Version,
	description: Option<String>,
	private: Option<bool>,
	/// The source directory, relative to this `nymph.toml`'s own directory
	/// (default `"src"`) — `@/a/b` resolves against `<root>/<src>/a/b.nym`,
	/// and the project's entry module is `<root>/<src>/main.nym`.
	#[serde(default = "default_src")]
	src: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Dependency {
	Simple(VersionReq),
	Catalog(Option<String>),
	Workspace,
	Path(Box<Path>),
	Github {
		username: String,
		repo: String,
		path: Option<Vec<String>>,
	},
}

/// Climb from `start` looking for the nearest `nymph.toml`, parsing it if
/// found. Returns both the config and the directory containing it (the
/// project **root** — `<root>/<src>` is the source root every `@/`-import
/// and the entry module resolve against; see [`NymphConfig::src_root`]).
/// Shared by [`get_config_with_root`] (climbing from the CWD) and the CLI's
/// project driver support (climbing from a given file's own directory).
pub(crate) fn find_from(start: &Path) -> anyhow::Result<(NymphConfig, PathBuf)> {
	let mut dir = start.to_path_buf();
	let found = loop {
		match fs::read_dir(&dir)?
			.filter_map(Result::ok)
			.find(|f| f.file_name() == "nymph.toml")
		{
			Some(entry) => break entry,
			None => {
				dir = dir
					.parent()
					.ok_or(io::Error::from(io::ErrorKind::NotFound))?
					.to_path_buf()
			}
		}
	};
	let mut file = File::open(found.path())?;
	let contents = {
		let mut buf = String::new();
		file.read_to_string(&mut buf)?;
		buf
	};
	Ok((toml::from_str(contents.as_str())?, dir))
}

/// Locate and parse the nearest `nymph.toml`, returning both the config and
/// the directory containing it. Honors the CLI's `--config` override
/// (pointing directly at a `nymph.toml` file) if given; otherwise climbs
/// from the current directory (see [`find_from`]).
///
/// Not yet called anywhere: `build`/`run`/`check`'s project support
/// (`crate::project_support::detect`) climbs from the FILE argument's own
/// directory via `find_from` directly, since commands don't have access to
/// `NymphCli`'s `--config` override today. Kept for future commands that need
/// override-aware lookup.
#[allow(dead_code)]
pub(crate) fn get_config_with_root(cli: &NymphCli) -> anyhow::Result<(NymphConfig, PathBuf)> {
	match &cli.config {
		Some(path) => {
			let mut file = File::open(path)?;
			let contents = {
				let mut buf = String::new();
				file.read_to_string(&mut buf)?;
				buf
			};
			let root = path
				.parent()
				.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
			Ok((toml::from_str(contents.as_str())?, root))
		}
		None => find_from(&current_dir()?),
	}
}
