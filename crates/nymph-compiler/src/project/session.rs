use std::{
	collections::BTreeMap,
	fmt,
	sync::{Arc, Mutex},
};

use nymph_sema::EntryMode;
use salsa::Setter;

use super::{
	ProjectDiagnostic,
	queries::{self, Db},
};

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, salsa::SalsaValue)]
pub struct ProjectId(Arc<str>);

impl ProjectId {
	#[must_use]
	pub fn new(value: impl Into<Arc<str>>) -> Self {
		Self(value.into())
	}
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, salsa::SalsaValue)]
pub struct ModulePath(Arc<str>);

impl ModulePath {
	pub fn new(value: impl AsRef<str>) -> Result<Self, &'static str> {
		let value = value.as_ref();
		if value.is_empty()
			|| value.starts_with('/')
			|| value.ends_with(".nym")
			|| value
				.split('/')
				.any(|segment| segment.is_empty() || segment == "." || segment == "..")
		{
			return Err("module path must be canonical, relative, and extension-less");
		}
		Ok(Self(Arc::from(value)))
	}
	#[must_use]
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl fmt::Display for ModulePath {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.0)
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SourceVersion(pub i64);

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, salsa::SalsaValue)]
pub(crate) struct BuiltinModuleKey(pub(crate) Arc<str>);

#[salsa::input]
#[derive(Debug)]
pub(crate) struct ModuleInput {
	#[returns(clone)]
	pub project: ProjectId,
	#[returns(clone)]
	pub path: ModulePath,
	#[returns(clone)]
	pub source: Option<Arc<str>>,
}

#[salsa::input]
#[derive(Debug)]
pub(crate) struct BuiltinModuleInput {
	#[returns(clone)]
	pub key: BuiltinModuleKey,
	#[returns(clone)]
	pub source: Arc<str>,
}

#[salsa::input]
#[derive(Debug)]
pub(crate) struct BuiltinRegistryInput {
	#[returns(clone)]
	pub modules: Arc<[BuiltinModuleInput]>,
}

#[salsa::input]
#[derive(Debug)]
pub(crate) struct ProjectInput {
	#[returns(clone)]
	pub project: ProjectId,
	#[returns(clone)]
	pub active_modules: Arc<[ModuleInput]>,
}

#[salsa::interned]
pub(crate) struct ProjectKey<'db> {
	#[returns(copy)]
	pub project_input: ProjectInput,
	#[returns(copy)]
	pub builtin_registry: BuiltinRegistryInput,
	#[returns(clone)]
	pub entry: ModulePath,
	#[returns(copy)]
	pub mode: EntryMode,
	#[returns(copy)]
	pub preserve_names: bool,
}

#[salsa::db]
struct Database {
	storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for Database {}
#[salsa::db]
impl Db for Database {}

type RegistryKey = (ProjectId, ModulePath);

struct SourceRecord {
	input: ModuleInput,
	source: Option<Arc<str>>,
	version: SourceVersion,
}

/// Placeholder for the compatibility analysis introduced by the next migration task.
#[allow(dead_code)]
pub(crate) struct ModuleAnalysis;

pub struct CompilerSession {
	db: Database,
	registry: BTreeMap<RegistryKey, SourceRecord>,
	projects: Mutex<BTreeMap<ProjectId, ProjectInput>>,
	builtins: BTreeMap<BuiltinModuleKey, BuiltinModuleInput>,
	builtin_registry: BuiltinRegistryInput,
	tombstones: usize,
	tombstone_threshold: usize,
	event_callback: Arc<dyn Fn(&str) + Send + Sync>,
}

impl Default for CompilerSession {
	fn default() -> Self {
		Self::new()
	}
}

impl CompilerSession {
	#[allow(dead_code)]
	pub(crate) fn module_analysis(
		&self,
		_project: ProjectId,
		_module: ModuleInput,
		_key: ProjectKey<'_>,
	) -> Option<Arc<ModuleAnalysis>> {
		None
	}

	#[must_use]
	pub fn new() -> Self {
		Self::with_event_callback_and_tombstone_threshold(|_| {}, 256)
	}

	#[doc(hidden)]
	pub fn with_event_callback_and_tombstone_threshold(
		callback: impl Fn(&str) + Send + Sync + 'static,
		threshold: usize,
	) -> Self {
		let callback: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(callback);
		let db = Self::database(callback.clone());
		let (builtins, builtin_registry) = Self::create_builtins(&db);
		Self {
			db,
			registry: BTreeMap::new(),
			projects: Mutex::new(BTreeMap::new()),
			builtins,
			builtin_registry,
			tombstones: 0,
			tombstone_threshold: threshold.max(1),
			event_callback: callback,
		}
	}

	fn create_builtins(
		db: &Database,
	) -> (
		BTreeMap<BuiltinModuleKey, BuiltinModuleInput>,
		BuiltinRegistryInput,
	) {
		let builtins: BTreeMap<_, _> = crate::std_source::embedded_std_sources()
			.map(|(path, source)| {
				let key = BuiltinModuleKey(Arc::from(path));
				let input = BuiltinModuleInput::new(db, key.clone(), Arc::from(source));
				(key, input)
			})
			.collect();
		let registry =
			BuiltinRegistryInput::new(db, builtins.values().copied().collect::<Vec<_>>().into());
		(builtins, registry)
	}

	fn database(callback: Arc<dyn Fn(&str) + Send + Sync>) -> Database {
		Database {
			storage: salsa::Storage::new(Some(Box::new(move |event| {
				if let salsa::EventKind::WillExecute { database_key } = event.kind {
					let debug = format!("{database_key:?}");
					let query = debug
						.split_once('(')
						.map_or(debug.as_str(), |(name, _)| name);
					let public_name = match query {
						"parse" | "parse_builtin" => Some("parse"),
						"direct_imports" | "builtin_direct_imports" => Some("direct_imports"),
						"project_graph" => Some("project_graph"),
						_ => None,
					};
					if let Some(name) = public_name {
						callback(name);
					}
				}
			}))),
		}
	}

	pub fn set_source(
		&mut self,
		project: ProjectId,
		module: ModulePath,
		source: String,
		version: SourceVersion,
	) {
		let key = (project.clone(), module.clone());
		let source: Arc<str> = source.into();
		let membership_changed;
		if let Some(record) = self.registry.get_mut(&key) {
			membership_changed = record.source.is_none();
			if membership_changed {
				self.tombstones = self.tombstones.saturating_sub(1);
			}
			if record.source.as_deref() != Some(source.as_ref()) {
				record
					.input
					.set_source(&mut self.db)
					.to(Some(source.clone()));
			}
			record.source = Some(source);
			record.version = version;
		} else {
			let input = ModuleInput::new(&self.db, project.clone(), module, Some(source.clone()));
			self.registry.insert(
				key,
				SourceRecord {
					input,
					source: Some(source),
					version,
				},
			);
			membership_changed = true;
		}
		if membership_changed {
			self.refresh_project(project);
		}
	}

	pub fn remove_source(&mut self, project: ProjectId, module: ModulePath) {
		let key = (project.clone(), module);
		if let Some(record) = self.registry.get_mut(&key)
			&& record.source.take().is_some()
		{
			record.input.set_source(&mut self.db).to(None);
			self.tombstones += 1;
			self.refresh_project(project);
			if self.tombstones >= self.tombstone_threshold {
				self.rebuild_database();
			}
		}
	}

	fn refresh_project(&mut self, project: ProjectId) {
		let active: Arc<[ModuleInput]> = self
			.registry
			.iter()
			.filter(|((owner, _), record)| owner == &project && record.source.is_some())
			.map(|(_, record)| record.input)
			.collect::<Vec<_>>()
			.into();
		let projects = self
			.projects
			.get_mut()
			.unwrap_or_else(|error| error.into_inner());
		if let Some(input) = projects.get(&project) {
			input.set_active_modules(&mut self.db).to(active);
		} else {
			projects.insert(
				project.clone(),
				ProjectInput::new(&self.db, project, active),
			);
		}
	}

	fn rebuild_database(&mut self) {
		self.db = Self::database(self.event_callback.clone());
		(self.builtins, self.builtin_registry) = Self::create_builtins(&self.db);
		self
			.projects
			.get_mut()
			.unwrap_or_else(|error| error.into_inner())
			.clear();
		self.registry.retain(|_, record| record.source.is_some());
		for ((project, path), record) in &mut self.registry {
			record.input = ModuleInput::new(
				&self.db,
				project.clone(),
				path.clone(),
				record.source.clone(),
			);
		}
		let projects: Vec<_> = self
			.registry
			.keys()
			.map(|(project, _)| project.clone())
			.collect();
		for project in projects {
			self.refresh_project(project);
		}
		self.tombstones = 0;
	}

	fn graph(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Arc<queries::ProjectGraph> {
		let mut projects = self
			.projects
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let input = projects.get(&project).copied().unwrap_or_else(|| {
			let input = ProjectInput::new(&self.db, project.clone(), Arc::new([]));
			projects.insert(project.clone(), input);
			input
		});
		queries::project_graph(
			&self.db,
			ProjectKey::new(&self.db, input, self.builtin_registry, entry, mode, false),
		)
		.clone()
	}

	#[must_use]
	pub fn check_project(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Arc<[ProjectDiagnostic]> {
		let graph = self.graph(project.clone(), entry.clone(), mode);
		if !graph.diagnostics.is_empty() {
			return graph.diagnostics.clone();
		}
		let load = |path: &str| {
			self
				.registry
				.get(&(project.clone(), ModulePath::new(path).ok()?))
				.and_then(|record| record.source.as_ref())
				.map(ToString::to_string)
		};
		let diagnostics = match mode {
			EntryMode::Entry => {
				super::check_project_with_std(entry.as_str(), &load, &crate::embedded_std_provider)
			}
			EntryMode::Library => {
				super::check_project_library_with_std(entry.as_str(), &load, &crate::embedded_std_provider)
			}
		};
		diagnostics.into()
	}

	#[doc(hidden)]
	#[must_use]
	pub fn graph_order(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Vec<ModulePath> {
		self
			.graph(project, entry, mode)
			.order
			.iter()
			.map(|module| module.path(&self.db))
			.collect()
	}
	#[doc(hidden)]
	#[must_use]
	pub fn tombstone_count(&self) -> usize {
		self.tombstones
	}
	#[doc(hidden)]
	#[must_use]
	pub fn source_version(&self, project: ProjectId, module: ModulePath) -> Option<SourceVersion> {
		self
			.registry
			.get(&(project, module))
			.map(|record| record.version)
	}
	#[doc(hidden)]
	#[must_use]
	pub fn has_source(&self, project: ProjectId, module: ModulePath) -> bool {
		self
			.registry
			.get(&(project, module))
			.is_some_and(|record| record.source.is_some())
	}
}
