use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};

use anyhow::Context;

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::db::{Db, ProjectConfig as CompilerProjectConfig};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ProjectConfig {
	name: String,
	version: Version,
	nymph_version: VersionReq,
	description: Option<String>,
	license: Option<String>,
	author: Vec<Author>,
	repository: Option<Url>,
	homepage: Option<Url>,
	dependencies: Option<BTreeMap<String, Dependency>>,
	build: Option<Build>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
enum Author {
	Single(String),
	Many(Vec<String>),
}

#[derive(Clone, Serialize, Deserialize, Debug)]
enum Dependency {
	Simple(VersionReq),
	Full {
		version: VersionReq,
		name: Option<String>,
	},
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Build {
	output: Option<String>,
	#[serde(default)]
	disable_implicit_prelude: bool,
}

#[derive(Deserialize)]
struct PreludeConfig {
	build: Option<Build>,
}

impl PreludeConfig {
	fn load(project_root: &Path) -> anyhow::Result<Option<Self>> {
		let path = project_root.join("nymph.toml");
		if !path.exists() {
			return Ok(None);
		}

		let contents =
			fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
		let config =
			toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
		Ok(Some(config))
	}

	fn implicit_prelude_enabled(&self) -> bool {
		!self
			.build
			.as_ref()
			.is_some_and(|build| build.disable_implicit_prelude)
	}
}

pub fn implicit_prelude_enabled(project_root: &Path) -> anyhow::Result<bool> {
	Ok(PreludeConfig::load(project_root)?.is_none_or(|config| config.implicit_prelude_enabled()))
}

pub fn load_compiler_project_config(
	db: &dyn Db,
	project_root: PathBuf,
	output_dir: PathBuf,
) -> anyhow::Result<CompilerProjectConfig> {
	let implicit_prelude = implicit_prelude_enabled(&project_root)?;
	Ok(CompilerProjectConfig::new(
		db,
		project_root,
		output_dir,
		implicit_prelude,
	))
}
