use std::{collections::HashMap, path::Path};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NymphConfig {
	package: Package,
	dependencies: HashMap<String, Dependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Package {
	name: String,
	version: Version,
	description: Option<String>,
	private: Option<bool>,
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
