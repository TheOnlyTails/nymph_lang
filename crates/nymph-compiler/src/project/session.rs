use std::{
	collections::BTreeMap,
	fmt,
	sync::{Arc, Mutex},
};

use nymph_sema::EntryMode;
use salsa::Setter;

use super::{
	CompiledProject, ProjectDiagnostic,
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

pub(crate) struct ModuleAnalysis {
	pub module: Arc<nymph_ast::decl::Module>,
	pub checked: Arc<nymph_sema::Checked>,
	pub diagnostics: Arc<[ProjectDiagnostic]>,
}

pub struct CompilerSession {
	db: Database,
	registry: BTreeMap<RegistryKey, SourceRecord>,
	projects: Mutex<BTreeMap<ProjectId, ProjectInput>>,
	builtin_sources: BTreeMap<Arc<str>, Arc<str>>,
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
		project: ProjectId,
		module: ModuleInput,
		key: ProjectKey<'_>,
	) -> Option<Arc<ModuleAnalysis>> {
		(module.project(&self.db) == project
			&& key.project_input(&self.db).project(&self.db) == project
			&& key
				.project_input(&self.db)
				.active_modules(&self.db)
				.contains(&module)
			&& super::compat::compat_project_module_is_reachable(&self.db, key, module)
			&& super::compat::compat_precheck_diagnostics(&self.db, key).is_empty())
		.then(|| {
			super::compat::compat_module_analysis(
				&self.db,
				key,
				super::compat::CompatModuleInput::Project(module),
			)
		})
	}

	#[must_use]
	pub fn new() -> Self {
		Self::with_builtin_sources(
			crate::std_source::embedded_std_sources()
				.map(|(path, source)| (Arc::from(path), Arc::from(source)))
				.collect(),
			Arc::new(|_| {}),
			256,
		)
	}

	pub(crate) fn from_builtin_sources(sources: BTreeMap<String, String>) -> Self {
		Self::with_builtin_sources(
			sources
				.into_iter()
				.map(|(path, source)| (Arc::from(path), Arc::from(source)))
				.collect(),
			Arc::new(|_| {}),
			256,
		)
	}

	#[doc(hidden)]
	pub fn with_event_callback_and_tombstone_threshold(
		callback: impl Fn(&str) + Send + Sync + 'static,
		threshold: usize,
	) -> Self {
		let callback: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(callback);
		let sources = crate::std_source::embedded_std_sources()
			.map(|(path, source)| (Arc::from(path), Arc::from(source)))
			.collect();
		Self::with_builtin_sources(sources, callback, threshold)
	}

	fn with_builtin_sources(
		builtin_sources: BTreeMap<Arc<str>, Arc<str>>,
		callback: Arc<dyn Fn(&str) + Send + Sync>,
		threshold: usize,
	) -> Self {
		let db = Self::database(callback.clone());
		let (builtins, builtin_registry) = Self::create_builtins(&db, &builtin_sources);
		Self {
			db,
			registry: BTreeMap::new(),
			projects: Mutex::new(BTreeMap::new()),
			builtin_sources,
			builtins,
			builtin_registry,
			tombstones: 0,
			tombstone_threshold: threshold.max(1),
			event_callback: callback,
		}
	}

	fn create_builtins(
		db: &Database,
		sources: &BTreeMap<Arc<str>, Arc<str>>,
	) -> (
		BTreeMap<BuiltinModuleKey, BuiltinModuleInput>,
		BuiltinRegistryInput,
	) {
		let builtins: BTreeMap<_, _> = sources
			.iter()
			.map(|(path, source)| {
				let key = BuiltinModuleKey(path.clone());
				let input = BuiltinModuleInput::new(db, key.clone(), source.clone());
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
						"parse" | "compat_parse_builtin" => Some("parse"),
						"direct_imports" | "compat_builtin_direct_imports" => Some("direct_imports"),
						"project_graph" => Some("project_graph"),
						"compat_symbol_map"
						| "compat_rewritten_module"
						| "compat_module_analysis"
						| "compat_lowered_module"
						| "compat_emitted_module"
						| "compat_compiled_project" => Some(query),
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
		(self.builtins, self.builtin_registry) = Self::create_builtins(&self.db, &self.builtin_sources);
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

	fn project_key(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
		preserve_names: bool,
	) -> ProjectKey<'_> {
		let mut projects = self
			.projects
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let input = projects.get(&project).copied().unwrap_or_else(|| {
			let input = ProjectInput::new(&self.db, project.clone(), Arc::new([]));
			projects.insert(project, input);
			input
		});
		ProjectKey::new(
			&self.db,
			input,
			self.builtin_registry,
			entry,
			mode,
			preserve_names,
		)
	}

	#[must_use]
	pub fn check_project(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Arc<[ProjectDiagnostic]> {
		let key = self.project_key(project, entry, mode, false);
		super::compat::compat_checked_project(&self.db, key)
	}

	pub fn compile_project(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Result<Arc<CompiledProject>, Arc<[ProjectDiagnostic]>> {
		self.compile_project_with_options(project, entry, mode, false)
	}

	pub(crate) fn compile_project_with_options(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
		preserve_names: bool,
	) -> Result<Arc<CompiledProject>, Arc<[ProjectDiagnostic]>> {
		let key = self.project_key(project, entry, mode, preserve_names);
		match super::compat::compat_compiled_project(&self.db, key).as_ref() {
			super::compat::CompatCompiledProject::Compiled(compiled) => Ok(compiled.clone()),
			super::compat::CompatCompiledProject::Diagnostics(diagnostics) => Err(diagnostics.clone()),
		}
	}

	/// Returns the exact ES-module graph before bundling, together with the
	/// entry module's compatibility tag.
	#[doc(hidden)]
	pub fn inspect_emitted_project(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Result<(std::collections::HashMap<String, String>, usize), Arc<[ProjectDiagnostic]>> {
		self.inspect_emitted_project_with_options(project, entry, mode, false)
	}

	pub(crate) fn inspect_emitted_project_with_options(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
		preserve_names: bool,
	) -> Result<(std::collections::HashMap<String, String>, usize), Arc<[ProjectDiagnostic]>> {
		let key = self.project_key(project, entry, mode, preserve_names);
		let emitted = super::compat::compat_emitted_module(&self.db, key);
		match &emitted.module_sources {
			Ok(sources) => Ok((sources.clone().into_iter().collect(), emitted.entry_tag)),
			Err(diagnostics) => Err(diagnostics.clone().into()),
		}
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn database_rebuild_preserves_exact_custom_builtins() {
		let mut session = CompilerSession::from_builtin_sources(BTreeMap::from([(
			"custom".to_string(),
			"public func answer(): int = 42".to_string(),
		)]));
		session.tombstone_threshold = 1;
		let project = ProjectId::new("custom-rebuild");
		let main = ModulePath::new("main").unwrap();
		let temporary = ModulePath::new("temporary").unwrap();
		session.set_source(
			project.clone(),
			main.clone(),
			"import std/custom with (answer)\nfunc main(): void = {}\nfunc value(): int = answer()"
				.into(),
			SourceVersion(1),
		);
		session.set_source(
			project.clone(),
			temporary.clone(),
			"let unused = 0".into(),
			SourceVersion(1),
		);
		assert!(
			session
				.check_project(project.clone(), main.clone(), EntryMode::Entry)
				.is_empty()
		);
		session.remove_source(project.clone(), temporary);
		assert!(
			session
				.check_project(project, main, EntryMode::Entry)
				.is_empty()
		);
		assert_eq!(session.builtin_sources.len(), 1);
	}
}
