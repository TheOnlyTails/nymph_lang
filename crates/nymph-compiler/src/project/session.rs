use std::{
	collections::{BTreeMap, BTreeSet},
	fmt,
	sync::{Arc, Mutex},
};

use nymph_ast::decl::Declaration;
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
	#[must_use]
	pub fn as_str(&self) -> &str {
		&self.0
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
			|| value.contains(['\\', ':'])
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

	pub fn from_source_file(path: &std::path::Path) -> Result<Self, &'static str> {
		if path.extension().and_then(std::ffi::OsStr::to_str) != Some("nym") {
			return Err("source path must have the .nym extension");
		}
		let path_text = path.to_str().ok_or("source path must be valid UTF-8")?;
		if path_text
			.split(std::path::is_separator)
			.any(|segment| segment.is_empty() || segment == "." || segment == "..")
		{
			return Err("source path must be relative and normalized");
		}
		let without_extension = path.with_extension("");
		let mut key = String::new();
		for component in without_extension.components() {
			let std::path::Component::Normal(segment) = component else {
				return Err("source path must be relative and normalized");
			};
			let segment = segment.to_str().ok_or("source path must be valid UTF-8")?;
			if !key.is_empty() {
				key.push('/');
			}
			key.push_str(segment);
		}
		Self::new(key)
	}

	#[must_use]
	pub fn source_file(&self, root: &std::path::Path) -> std::path::PathBuf {
		root.join(format!("{}.nym", self.as_str()))
	}
}

impl fmt::Display for ModulePath {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.0)
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SourceVersion(pub i64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, salsa::SalsaValue)]
pub(crate) enum BuiltinModuleDomain {
	ImportableStd,
	AmbientCore,
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, salsa::SalsaValue)]
pub(crate) struct BuiltinModuleKey {
	pub(crate) domain: BuiltinModuleDomain,
	pub(crate) path: Arc<str>,
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct AmbientCoreModuleKey(Arc<str>);

impl AmbientCoreModuleKey {
	pub fn new(value: impl AsRef<str>) -> Result<Self, &'static str> {
		ModulePath::new(value.as_ref())?;
		Ok(Self(Arc::from(value.as_ref())))
	}
	#[must_use]
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

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

/// A module participating in AST-independent semantic interface queries.
///
/// Importable compiler modules share this handle with project modules, while
/// their distinct identity domains prevent path collisions. Ambient core may
/// be described by the handle but is never inserted into the public project
/// semantic graph; it remains an explicit compiler-owned root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub(crate) enum SemanticModuleInput {
	Project(ModuleInput),
	Builtin(BuiltinModuleInput),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SemanticModuleDomain {
	Project,
	ImportableStd,
	AmbientCore,
}

#[salsa::input]
#[derive(Debug)]
pub(crate) struct BuiltinRegistryInput {
	#[returns(clone)]
	pub modules: Arc<[BuiltinModuleInput]>,
}

#[salsa::input]
#[derive(Debug)]
pub(crate) struct AmbientCoreRegistryInput {
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
	#[returns(copy)]
	pub ambient_core_registry: AmbientCoreRegistryInput,
	#[returns(clone)]
	pub entry: ModulePath,
	#[returns(copy)]
	pub mode: EntryMode,
	#[returns(copy)]
	pub preserve_names: bool,
	#[returns(copy)]
	pub ambient_prelude: bool,
}

#[salsa::db]
struct Database {
	storage: salsa::Storage<Self>,
	#[cfg(feature = "test-support")]
	semantic_test_hook: Arc<Mutex<SemanticTestHook>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Clone for Database {
	fn clone(&self) -> Self {
		Self {
			storage: self.storage.clone(),
			#[cfg(feature = "test-support")]
			semantic_test_hook: self.semantic_test_hook.clone(),
		}
	}
}

#[salsa::db]
impl salsa::Database for Database {}
#[salsa::db]
impl Db for Database {
	#[cfg(not(target_arch = "wasm32"))]
	fn parallel_clone(&self) -> Box<dyn Db> {
		Box::new(self.clone())
	}

	#[cfg(feature = "test-support")]
	fn semantic_query_will_execute(&self, query: &'static str, module: SemanticModuleInput) {
		let hook = self.semantic_test_hook.lock().unwrap();
		let module_name = module.display_key(self);
		if let Some((project, allowed)) = &hook.body_guard
			&& module.identity(self).project == project.as_str()
			&& module_name != allowed.as_str()
			&& query == "interface_module_analysis"
		{
			panic!("forbidden dependency AST/analysis access: {module_name}");
		}
		if let Some(callback) = &hook.callback {
			callback(SemanticQueryEvent {
				query: query.to_string(),
				module: Some(module_name),
				definition: None,
			});
		}
	}
	#[cfg(feature = "test-support")]
	fn runtime_query_will_execute(&self, query: &'static str, definition: &nymph_sema::DefinitionId) {
		if let Some(callback) = &self.semantic_test_hook.lock().unwrap().callback {
			callback(SemanticQueryEvent {
				query: query.to_string(),
				module: Some(definition.module.path.to_string()),
				definition: Some(definition.clone()),
			});
		}
	}
}

#[cfg(feature = "test-support")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticQueryEvent {
	pub query: String,
	pub module: Option<String>,
	pub definition: Option<nymph_sema::DefinitionId>,
}

#[cfg(feature = "test-support")]
#[derive(Default)]
struct SemanticTestHook {
	callback: Option<Arc<dyn Fn(SemanticQueryEvent) + Send + Sync>>,
	body_guard: Option<(ProjectId, ModulePath)>,
}

type RegistryKey = (ProjectId, ModulePath);

struct SourceRecord {
	input: ModuleInput,
	source: Option<Arc<str>>,
	version: SourceVersion,
}

/// Immutable parse/check result for one module in a session project.
///
/// This intentionally exposes compiler values rather than Salsa inputs or a
/// database handle, so tooling can safely retain the result between requests.
pub struct ModuleAnalysis {
	pub semantic: Arc<nymph_sema::SemanticAnalysis>,
	pub diagnostics: ProjectDiagnostics,
}

/// Project diagnostics are deliberately separate from reusable semantic facts.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectDiagnostics(pub Arc<[ProjectDiagnostic]>);

impl std::ops::Deref for ProjectDiagnostics {
	type Target = [ProjectDiagnostic];

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

/// Stable semantic owner information for runtime-bearing compiler definitions.
///
/// This ABI/interface runtime descriptor is an input to Task 8, not HIR and not
/// a checked body. Task 8 owns checked-body/HIR projection and resolves `module`
/// to the ambient semantic analysis before provenance-preserving lowering.
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct BuiltinRuntimeOwnerArtifact {
	pub definition: nymph_sema::DefinitionId,
	pub module: AmbientCoreModuleKey,
	pub shape: BuiltinRuntimeOwnerShape,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum BuiltinRuntimeOwnerShape {
	Definition(nymph_sema::ExportedDefinition),
	Implementation(nymph_sema::ExportedImpl),
}

impl ModuleAnalysis {
	/// Query the checked type at a source offset.
	///
	/// This is the safe tooling seam: the private module has the exact flattened
	/// declaration layout that produced `checked`, while `module` remains the
	/// public source/rewrite AST used by lowering and NodeId/span annotations.
	#[must_use]
	pub fn type_at(&self, offset: usize) -> Option<String> {
		let checked = nymph_sema::Checked {
			diags: Vec::new(),
			facts: self.semantic.checked.as_ref().clone(),
		};
		nymph_sema::query::type_at(&self.semantic.module, &checked, offset)
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum RuntimeDefinitionError {
	Recovered,
	Extraction(nymph_sema::RuntimeExtractionError),
	OwnerNotFound,
	DuplicateOwner,
	DefinitionNotFound,
}

pub struct CompilerSession {
	db: Database,
	registry: BTreeMap<RegistryKey, SourceRecord>,
	projects: Mutex<BTreeMap<ProjectId, ProjectInput>>,
	builtin_sources: BTreeMap<Arc<str>, Arc<str>>,
	builtins: BTreeMap<BuiltinModuleKey, BuiltinModuleInput>,
	builtin_registry: BuiltinRegistryInput,
	ambient_core: BTreeMap<BuiltinModuleKey, BuiltinModuleInput>,
	ambient_core_registry: AmbientCoreRegistryInput,
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
	fn ambient_core_input(&self, key: &AmbientCoreModuleKey) -> Option<BuiltinModuleInput> {
		self
			.ambient_core
			.get(&BuiltinModuleKey {
				domain: BuiltinModuleDomain::AmbientCore,
				path: key.0.clone(),
			})
			.copied()
	}

	/// Enumerate canonical compiler-private ambient-core module keys.
	#[must_use]
	pub fn ambient_core_module_keys(&self) -> Vec<AmbientCoreModuleKey> {
		self
			.ambient_core
			.keys()
			.map(|key| AmbientCoreModuleKey(key.path.clone()))
			.collect()
	}

	#[must_use]
	pub fn ambient_core_module_interface(
		&self,
		key: AmbientCoreModuleKey,
	) -> Option<Arc<nymph_sema::ModuleInterface>> {
		let module = self.ambient_core_input(&key)?;
		Some(queries::ambient_core_interface(
			&self.db,
			self.ambient_core_registry,
			module,
		))
	}

	#[must_use]
	pub fn ambient_core_module_environment(
		&self,
		key: AmbientCoreModuleKey,
	) -> Option<Arc<nymph_sema::ModuleEnvironment>> {
		let module = self.ambient_core_input(&key)?;
		Some(queries::ambient_core_environment(
			&self.db,
			self.ambient_core_registry,
			module,
		))
	}

	#[must_use]
	pub fn ambient_core_module_diagnostics(
		&self,
		key: AmbientCoreModuleKey,
	) -> Option<Arc<[nymph_diagnostics::Diagnostic]>> {
		let module = self.ambient_core_input(&key)?;
		Some(queries::ambient_core_diagnostics(
			&self.db,
			self.ambient_core_registry,
			module,
		))
	}

	#[must_use]
	#[cfg(feature = "test-support")]
	pub fn compiler_runtime_roles_for_test(&self) -> nymph_sema::CompilerRuntimeRoles {
		(*queries::compiler_runtime_roles(&self.db, self.ambient_core_registry)).clone()
	}

	#[cfg(feature = "test-support")]
	pub fn importable_std_module_environment_for_test(
		&self,
		path: &str,
	) -> Option<Arc<nymph_sema::ModuleEnvironment>> {
		let module = self.builtins.iter().find_map(|(key, module)| {
			(key.domain == BuiltinModuleDomain::ImportableStd && key.path.as_ref() == path)
				.then_some(*module)
		})?;
		let project = ProjectId::new("stdlib-interface-test");
		let entry = ModulePath::new(path).ok()?;
		let key = self.project_key(project, entry, EntryMode::Library, true, true);
		Some(queries::interface_module_environment(
			&self.db,
			key,
			SemanticModuleInput::Builtin(module),
		))
	}

	/// Test-only mutation seam for proving ambient query invalidation. Ordinary
	/// compiler clients cannot obtain or mutate ambient Salsa inputs directly.
	#[doc(hidden)]
	#[must_use]
	pub fn ambient_core_source_for_test(&self, key: AmbientCoreModuleKey) -> Option<String> {
		Some(self.ambient_core_input(&key)?.source(&self.db).to_string())
	}

	#[doc(hidden)]
	pub fn set_ambient_core_source_for_test(&mut self, key: AmbientCoreModuleKey, source: String) {
		let input = self
			.ambient_core_input(&key)
			.expect("test mutation must name an embedded ambient module");
		input.set_source(&mut self.db).to(Arc::from(source));
	}

	fn semantic_input(
		&self,
		project: &ProjectId,
		module: &ModulePath,
	) -> Option<SemanticModuleInput> {
		self
			.registry
			.get(&(project.clone(), module.clone()))
			.map(|record| SemanticModuleInput::Project(record.input))
	}

	#[must_use]
	#[cfg(feature = "test-support")]
	pub fn module_interface(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: ModulePath,
		mode: EntryMode,
	) -> Option<Arc<nymph_sema::ModuleInterface>> {
		let input = self.semantic_input(&project, &module)?;
		let key = self.project_key(project, entry, mode, true, true);
		queries::interface_module_interface(&self.db, key, input).ok()
	}

	#[must_use]
	#[cfg(feature = "test-support")]
	pub fn module_environment(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: ModulePath,
		mode: EntryMode,
	) -> Option<Arc<nymph_sema::ModuleEnvironment>> {
		let input = self.semantic_input(&project, &module)?;
		let key = self.project_key(project, entry, mode, true, true);
		Some(queries::interface_module_environment(&self.db, key, input))
	}

	#[cfg(feature = "test-support")]
	pub fn environment_is_lowerable(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: ModulePath,
		mode: EntryMode,
	) -> Result<(), &'static str> {
		match &*self
			.module_environment(project, entry, module, mode)
			.ok_or("module is unavailable")?
		{
			nymph_sema::ModuleEnvironment::Complete(_) => Ok(()),
			nymph_sema::ModuleEnvironment::Recovered(_) => {
				Err("reachable module environment is recovered")
			}
		}
	}

	#[must_use]
	#[cfg(feature = "test-support")]
	pub fn module_diagnostics(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: ModulePath,
		mode: EntryMode,
	) -> Option<Arc<[ProjectDiagnostic]>> {
		let input = self.semantic_input(&project, &module)?;
		let key = self.project_key(project, entry, mode, true, true);
		Some(
			queries::interface_module_analysis(&self.db, key, input)
				.diagnostics
				.0
				.clone(),
		)
	}

	#[must_use]
	pub fn builtin_runtime_owner_artifacts(&self) -> Arc<[BuiltinRuntimeOwnerArtifact]> {
		queries::ambient_runtime_owner_artifacts(&self.db, self.ambient_core_registry)
	}

	/// Resolve a canonical runtime owner without exposing Salsa identities.
	#[must_use]
	pub fn builtin_runtime_owner_artifact(
		&self,
		definition: &nymph_sema::DefinitionId,
	) -> Option<BuiltinRuntimeOwnerArtifact> {
		self
			.builtin_runtime_owner_artifacts()
			.binary_search_by(|artifact| artifact.definition.cmp(definition))
			.ok()
			.map(|index| self.builtin_runtime_owner_artifacts()[index].clone())
	}
	fn module_analysis(
		&self,
		project: ProjectId,
		module: ModuleInput,
		key: ProjectKey<'_>,
	) -> Option<Arc<ModuleAnalysis>> {
		let common = module.project(&self.db) == project
			&& key.project_input(&self.db).project(&self.db) == project
			&& key
				.project_input(&self.db)
				.active_modules(&self.db)
				.contains(&module);
		let graph = queries::project_graph(&self.db, key);
		(common
			&& graph.diagnostics.is_empty()
			&& graph
				.semantic_order
				.contains(&SemanticModuleInput::Project(module)))
		.then(|| {
			queries::interface_module_analysis(&self.db, key, SemanticModuleInput::Project(module))
		})
	}

	/// Return the shared immutable analysis for a reachable project module.
	#[must_use]
	pub fn analyze_module(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: ModulePath,
		mode: EntryMode,
	) -> Option<Arc<ModuleAnalysis>> {
		let input = self.registry.get(&(project.clone(), module))?.input;
		let key = self.project_key(project.clone(), entry, mode, true, true);
		self.module_analysis(project, input, key)
	}

	/// Resolve and read one exact runtime-bearing definition from its source
	/// module. The owner comes from `DefinitionId`, never from placement metadata.
	#[must_use]
	pub fn runtime_definition(
		&self,
		project: ProjectId,
		entry: ModulePath,
		definition: nymph_sema::DefinitionId,
		mode: EntryMode,
	) -> Result<Arc<nymph_sema::RuntimeDefinition>, RuntimeDefinitionError> {
		let key = self.project_key(project, entry, mode, false, true);
		queries::runtime_definition(&self.db, key, definition)
	}

	/// Lower exactly one checked runtime definition through the stable semantic context.
	#[must_use]
	pub fn lower_runtime_definition(
		&self,
		project: ProjectId,
		entry: ModulePath,
		definition: nymph_sema::DefinitionId,
		mode: EntryMode,
	) -> Result<Arc<nymph_sema::LoweredRuntimeDefinition>, nymph_sema::StableLoweringError> {
		let key = self.project_key(project, entry, mode, false, true);
		queries::lower_runtime_definition(&self.db, key, definition)
	}

	/// Assemble one module exclusively from exact stable runtime fragments.
	#[cfg(feature = "test-support")]
	#[doc(hidden)]
	#[must_use]
	pub fn lower_interface_module_for_test(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: ModulePath,
		mode: EntryMode,
	) -> Result<Arc<nymph_sema::StableHirModule>, nymph_sema::StableModuleAssemblyError> {
		let input = self
			.registry
			.get(&(project.clone(), module))
			.map(|record| record.input)
			.ok_or_else(
				|| nymph_sema::StableModuleAssemblyError::RecoveredEnvironment {
					module: nymph_sema::ModuleIdentity {
						origin: nymph_sema::ModuleOrigin::Project(project.as_str().into()),
						project: project.as_str().into(),
						path: "<missing>".into(),
					},
				},
			)?;
		let key = self.project_key(project, entry, mode, false, true);
		queries::lower_interface_module(&self.db, key, SemanticModuleInput::Project(input))
	}

	#[cfg(feature = "test-support")]
	#[doc(hidden)]
	pub fn emit_interface_project_for_test(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Result<Arc<super::emission::StableEmittedProject>, Arc<[ProjectDiagnostic]>> {
		let key = self.project_key(project, entry, mode, false, true);
		let diagnostics = queries::interface_project_diagnostics(&self.db, key);
		if diagnostics
			.0
			.iter()
			.any(|diagnostic| diagnostic.diag.is_error())
		{
			return Err(diagnostics.0);
		}
		match super::emission::emitted_interface_project(&self.db, key) {
			super::emission::StableEmissionResult::Value(value) => Ok(value),
			super::emission::StableEmissionResult::Diagnostics(diagnostics) => Err(diagnostics),
		}
	}

	#[cfg(feature = "test-support")]
	#[doc(hidden)]
	pub fn compile_interface_project_for_test(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Result<Arc<CompiledProject>, Arc<[ProjectDiagnostic]>> {
		let key = self.project_key(project, entry, mode, false, true);
		let diagnostics = queries::interface_project_diagnostics(&self.db, key);
		if diagnostics
			.0
			.iter()
			.any(|diagnostic| diagnostic.diag.is_error())
		{
			return Err(diagnostics.0);
		}
		match super::emission::compiled_interface_project(&self.db, key) {
			super::emission::StableEmissionResult::Value(value) => Ok(value),
			super::emission::StableEmissionResult::Diagnostics(diagnostics) => Err(diagnostics),
		}
	}

	#[cfg(feature = "test-support")]
	#[doc(hidden)]
	#[must_use]
	pub fn runtime_definition_consumer_for_test(
		&self,
		project: ProjectId,
		entry: ModulePath,
		definition: nymph_sema::DefinitionId,
		mode: EntryMode,
	) -> Result<Arc<nymph_sema::RuntimeDefinition>, RuntimeDefinitionError> {
		let key = self.project_key(project.clone(), entry, mode, false, true);
		queries::runtime_definition_consumer(&self.db, key, definition)
	}

	/// Return the authoritative emitted binding for one exact stable definition.
	#[cfg(feature = "test-support")]
	#[doc(hidden)]
	pub fn binding_name_for_test(
		&self,
		project: ProjectId,
		entry: ModulePath,
		definition: nymph_sema::DefinitionId,
		mode: EntryMode,
	) -> Result<nymph_sema::EmittedBindingName, nymph_sema::StableNameLookupError> {
		let key = self.project_key(project, entry, mode, false, true);
		queries::binding_name(&self.db, key, definition)
	}

	/// Return the authoritative emitted member name for one exact stable definition.
	#[cfg(feature = "test-support")]
	#[doc(hidden)]
	pub fn member_name_for_test(
		&self,
		project: ProjectId,
		entry: ModulePath,
		definition: nymph_sema::DefinitionId,
		mode: EntryMode,
	) -> Result<nymph_sema::EmittedMemberName, nymph_sema::StableNameLookupError> {
		let key = self.project_key(project, entry, mode, false, true);
		queries::member_name(&self.db, key, definition)
	}

	/// Inspect all exact runtime artifacts owned by one module in tests.
	#[cfg(feature = "test-support")]
	#[doc(hidden)]
	pub fn runtime_definitions_for_test(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: ModulePath,
		mode: EntryMode,
	) -> Option<Vec<Arc<nymph_sema::RuntimeDefinition>>> {
		let input = self.registry.get(&(project.clone(), module))?.input;
		let key = self.project_key(project, entry, mode, true, true);
		Some(
			queries::runtime_manifest(&self.db, key, SemanticModuleInput::Project(input))
				.ok()?
				.definitions()
				.iter()
				.map(|entity| entity.value(&self.db))
				.collect(),
		)
	}

	#[cfg(feature = "test-support")]
	#[doc(hidden)]
	pub fn builtin_interface_member_ids_for_test(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: &str,
		mode: EntryMode,
	) -> Vec<nymph_sema::DefinitionId> {
		let key = self.project_key(project, entry, mode, true, true);
		self
			.builtins
			.iter()
			.filter(|(builtin, _)| builtin.path.as_ref() == module)
			.flat_map(|(_, input)| {
				let environment = queries::interface_module_environment(
					&self.db,
					key,
					SemanticModuleInput::Builtin(*input),
				);
				let nymph_sema::ModuleEnvironment::Complete(interface) = environment.as_ref() else {
					return Vec::new();
				};
				interface
					.implementations
					.iter()
					.filter(|implementation| implementation.interface.is_some())
					.flat_map(|implementation| {
						implementation
							.members
							.iter()
							.map(|member| member.id.clone())
					})
					.collect()
			})
			.collect()
	}

	/// Return tooling analysis under the same request key used by
	/// [`Self::tooling_diagnostics`].
	#[doc(hidden)]
	#[must_use]
	pub fn tooling_analyze_module(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: ModulePath,
		ambient_prelude: bool,
	) -> Option<Arc<ModuleAnalysis>> {
		let input = self.registry.get(&(project.clone(), module))?.input;
		let key = self.tooling_key(project.clone(), entry, ambient_prelude);
		self.module_analysis(project, input, key)
	}

	/// Check a tooling project with the exact key used by
	/// [`Self::tooling_analyze_module`].
	#[doc(hidden)]
	#[must_use]
	pub fn tooling_diagnostics(
		&self,
		project: ProjectId,
		entry: ModulePath,
		ambient_prelude: bool,
	) -> Arc<[ProjectDiagnostic]> {
		let key = self.tooling_key(project, entry, ambient_prelude);
		queries::interface_project_diagnostics(&self.db, key)
			.0
			.clone()
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

	/// Create a session without the ambient embedded standard library.
	///
	/// Tooling uses this when checking the standard-library sources themselves,
	/// where injecting the same sources as a prelude would duplicate every
	/// declaration.
	#[doc(hidden)]
	#[must_use]
	pub fn without_builtin_sources() -> Self {
		Self::with_builtin_sources(BTreeMap::new(), Arc::new(|_| {}), 256)
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

	pub(crate) fn from_source_loaders(
		project: ProjectId,
		entry: &str,
		load: &dyn Fn(&str) -> Option<String>,
		std_provider: &dyn Fn(&str) -> Option<String>,
	) -> Self {
		let mut project_sources = BTreeMap::new();
		let mut builtin_sources = BTreeMap::new();
		let mut seen = BTreeSet::new();
		let mut pending = vec![entry.to_string()];
		while let Some(key) = pending.pop() {
			if !seen.insert(key.clone()) {
				continue;
			}
			let source = if let Some(path) = key.strip_prefix(super::resolve::STD_KEY_PREFIX) {
				std_provider(path)
			} else {
				load(&key)
			};
			let Some(source) = source else {
				continue;
			};
			let parsed = nymph_syntax::parse_module(&source, &format!("{key}.nym"));
			let mut imports = Vec::new();
			for declaration in &parsed.tree.members {
				let Declaration::Import {
					root, path, alias, ..
				} = declaration
				else {
					continue;
				};
				if path.is_empty() && alias.is_none() {
					continue;
				}
				if let Ok(target) =
					super::resolve::resolve_import_target(root, path, &key, nymph_ast::Span::new(0, 0))
				{
					imports.push(target);
				}
			}
			pending.extend(imports.into_iter().rev());
			if let Some(path) = key.strip_prefix(super::resolve::STD_KEY_PREFIX) {
				builtin_sources.insert(path.to_string(), source);
			} else {
				project_sources.insert(key, source);
			}
		}
		let mut session = Self::from_builtin_sources(builtin_sources);
		for (path, source) in project_sources {
			session.set_source(
				project.clone(),
				ModulePath::new(path).expect("resolved source key is canonical"),
				source,
				SourceVersion(1),
			);
		}
		session
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
		let (ambient_core, ambient_core_registry) = Self::create_ambient_core(&db);
		Self {
			db,
			registry: BTreeMap::new(),
			projects: Mutex::new(BTreeMap::new()),
			builtin_sources,
			builtins,
			builtin_registry,
			ambient_core,
			ambient_core_registry,
			tombstones: 0,
			tombstone_threshold: threshold.max(1),
			event_callback: callback,
		}
	}

	#[cfg(feature = "test-support")]
	#[must_use]
	pub fn with_detailed_event_callback_for_test(
		callback: impl Fn(SemanticQueryEvent) + Send + Sync + 'static,
	) -> Self {
		let callback: Arc<dyn Fn(SemanticQueryEvent) + Send + Sync> = Arc::new(callback);
		let public_callback = callback.clone();
		let sources = crate::std_source::embedded_std_sources()
			.map(|(path, source)| (Arc::from(path), Arc::from(source)))
			.collect();
		let session = Self::with_builtin_sources(
			sources,
			Arc::new(move |query| {
				public_callback(SemanticQueryEvent {
					query: query.to_string(),
					module: None,
					definition: None,
				});
			}),
			256,
		);
		session.db.semantic_test_hook.lock().unwrap().callback = Some(callback);
		session
	}

	/// Warm dependency environments before enabling a structural body-access guard.
	#[cfg(feature = "test-support")]
	pub fn warm_interface_dependency_environments_for_test(
		&self,
		project: ProjectId,
		entry: ModulePath,
		module: ModulePath,
		mode: EntryMode,
	) {
		let key = self.project_key(project.clone(), entry, mode, true, true);
		let Some(input) = self.semantic_input(&project, &module) else {
			return;
		};
		let graph = queries::project_graph(&self.db, key);
		for dependency in graph.semantic_closure(input).iter().copied() {
			queries::interface_module_environment(&self.db, key, dependency);
		}
	}

	/// The closed interface boundary itself is the guard: dependency bodies can
	/// only be reached by warming their own environment, never while consuming it.
	#[cfg(feature = "test-support")]
	pub fn panic_on_dependency_body_access_for_test(
		&mut self,
		project: ProjectId,
		allowed_module: ModulePath,
	) {
		self.db.semantic_test_hook.lock().unwrap().body_guard = Some((project, allowed_module));
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
				let key = BuiltinModuleKey {
					domain: BuiltinModuleDomain::ImportableStd,
					path: path.clone(),
				};
				let input = BuiltinModuleInput::new(db, key.clone(), source.clone());
				(key, input)
			})
			.collect();
		let registry =
			BuiltinRegistryInput::new(db, builtins.values().copied().collect::<Vec<_>>().into());
		(builtins, registry)
	}

	fn create_ambient_core(
		db: &Database,
	) -> (
		BTreeMap<BuiltinModuleKey, BuiltinModuleInput>,
		AmbientCoreRegistryInput,
	) {
		let modules: BTreeMap<_, _> = crate::prelude::core_sources()
			.map(|(path, source)| {
				let key = BuiltinModuleKey {
					domain: BuiltinModuleDomain::AmbientCore,
					path: Arc::from(path),
				};
				let input = BuiltinModuleInput::new(db, key.clone(), Arc::from(source));
				(key, input)
			})
			.collect();
		let registry =
			AmbientCoreRegistryInput::new(db, modules.values().copied().collect::<Vec<_>>().into());
		(modules, registry)
	}

	fn database(callback: Arc<dyn Fn(&str) + Send + Sync>) -> Database {
		Database {
			storage: salsa::Storage::new(Some(Box::new(move |event| {
				if let salsa::EventKind::WillExecute { database_key } = event.kind {
					let debug = format!("{database_key:?}");
					let query = debug
						.split_once('(')
						.map_or(debug.as_str(), |(name, _)| name);
					if query == "parse_builtin" {
						// This private compiler-core bootstrap producer serves both importable
						// builtins (observed as `parse`) and ambient-core registry entries.
						// Preserve the established parse event while also exposing the narrower
						// ambient-core event.
						callback("parse");
						callback("ambient_core_parse");
						return;
					}
					let public_name = match query {
						"parse" => Some("parse"),
						"direct_imports" | "builtin_direct_imports" => Some("direct_imports"),
						"project_graph" => Some("project_graph"),
						"interface_module_analysis"
						| "interface_module_interface"
						| "interface_module_environment"
						| "interface_project_diagnostics"
						| "emitted_interface_module"
						| "emitted_interface_project"
						| "compiled_interface_project" => Some(query),
						"ambient_core_analysis"
						| "ambient_core_headers"
						| "ambient_core_environment"
						| "ambient_core_interface"
						| "ambient_core_diagnostics"
						| "ambient_runtime_owner_artifacts" => Some(query),
						_ => None,
					};
					if let Some(name) = public_name {
						callback(name);
					}
				}
			}))),
			#[cfg(feature = "test-support")]
			semantic_test_hook: Arc::new(Mutex::new(SemanticTestHook::default())),
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
		(self.ambient_core, self.ambient_core_registry) = Self::create_ambient_core(&self.db);
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
			ProjectKey::new(
				&self.db,
				input,
				self.builtin_registry,
				self.ambient_core_registry,
				entry,
				mode,
				false,
				true,
			),
		)
		.clone()
	}

	fn project_key(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
		preserve_names: bool,
		ambient_prelude: bool,
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
			self.ambient_core_registry,
			entry,
			mode,
			preserve_names,
			ambient_prelude,
		)
	}

	fn tooling_key(
		&self,
		project: ProjectId,
		entry: ModulePath,
		ambient_prelude: bool,
	) -> ProjectKey<'_> {
		self.project_key(project, entry, EntryMode::Library, true, ambient_prelude)
	}

	#[must_use]
	pub fn check_project(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Arc<[ProjectDiagnostic]> {
		let key = self.project_key(project, entry, mode, false, true);
		queries::interface_project_diagnostics(&self.db, key)
			.0
			.clone()
	}

	pub(crate) fn check_project_with_options(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
		preserve_names: bool,
	) -> Arc<[ProjectDiagnostic]> {
		let key = self.project_key(project, entry, mode, preserve_names, true);
		queries::interface_project_diagnostics(&self.db, key)
			.0
			.clone()
	}

	#[doc(hidden)]
	#[must_use]
	pub fn check_project_without_prelude(
		&self,
		project: ProjectId,
		entry: ModulePath,
		mode: EntryMode,
	) -> Arc<[ProjectDiagnostic]> {
		let key = self.project_key(project, entry, mode, false, false);
		queries::interface_project_diagnostics(&self.db, key)
			.0
			.clone()
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
		let key = self.project_key(project, entry, mode, preserve_names, true);
		match super::emission::compiled_interface_project(&self.db, key) {
			super::emission::StableEmissionResult::Value(compiled) => Ok(compiled),
			super::emission::StableEmissionResult::Diagnostics(diagnostics) => Err(diagnostics),
		}
	}

	/// Returns the exact ES-module graph before bundling, together with the
	/// entry module's stable module tag.
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
		let key = self.project_key(project, entry, mode, preserve_names, true);
		match super::emission::emitted_interface_project(&self.db, key) {
			super::emission::StableEmissionResult::Value(emitted) => Ok((
				emitted.module_sources.clone().into_iter().collect(),
				emitted.entry_tag,
			)),
			super::emission::StableEmissionResult::Diagnostics(diagnostics) => Err(diagnostics),
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
	use std::{cell::RefCell, collections::HashMap};

	use super::*;

	#[test]
	fn module_path_round_trips_nested_source_files() {
		let module = ModulePath::from_source_file(std::path::Path::new("geometry/vector.nym"))
			.expect("relative source path is canonicalizable");
		assert_eq!(module.as_str(), "geometry/vector");
		assert_eq!(
			module.source_file(std::path::Path::new("src")),
			std::path::Path::new("src/geometry/vector.nym")
		);
		assert!(ModulePath::from_source_file(std::path::Path::new("")).is_err());
		assert!(ModulePath::from_source_file(std::path::Path::new("main")).is_err());
		assert!(ModulePath::from_source_file(std::path::Path::new("main.rs")).is_err());
		assert!(ModulePath::from_source_file(std::path::Path::new("/main.nym")).is_err());
		assert!(ModulePath::from_source_file(std::path::Path::new("nested//main.nym")).is_err());
		assert!(ModulePath::from_source_file(std::path::Path::new("nested/./main.nym")).is_err());
		assert!(ModulePath::new(r"nested\main").is_err());
	}

	#[test]
	fn loader_source_acquisition_preserves_recovered_dfs_and_provider_routing() {
		let calls = RefCell::new(Vec::new());
		let project_sources = HashMap::from([
			(
				"main",
				"import @/a\nimport @/missing\nimport @/missing\nimport std/root\nfunc broken(: int = 1",
			),
			("a", "import @/main"),
			("project", "public let value = 1"),
		]);
		let builtin_sources = HashMap::from([
			("root", "import ./child\nimport @/project"),
			("child", "public let value = 1"),
		]);
		let load = |key: &str| {
			calls.borrow_mut().push(format!("project:{key}"));
			project_sources.get(key).map(ToString::to_string)
		};
		let std_provider = |key: &str| {
			calls.borrow_mut().push(format!("std:{key}"));
			builtin_sources.get(key).map(ToString::to_string)
		};

		let _ = CompilerSession::from_source_loaders(
			ProjectId::new("loader-acquisition"),
			"main",
			&load,
			&std_provider,
		);

		assert_eq!(
			calls.into_inner(),
			[
				"project:main",
				"project:a",
				"project:missing",
				"std:root",
				"std:child",
				"project:project",
			]
		);
	}

	#[test]
	fn malformed_empty_relative_import_does_not_load_a_directory_key() {
		let calls = RefCell::new(Vec::new());
		let load = |key: &str| {
			calls.borrow_mut().push(key.to_string());
			match key {
				"dir/main" => Some("import ./".to_string()),
				"dir" => Some("public let unintended = 1".to_string()),
				_ => None,
			}
		};

		let _ = CompilerSession::from_source_loaders(
			ProjectId::new("malformed-import"),
			"dir/main",
			&load,
			&|_| None,
		);

		assert_eq!(calls.into_inner(), ["dir/main"]);
	}

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
