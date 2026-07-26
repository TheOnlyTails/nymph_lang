use std::sync::Arc;

use nymph_ast::{
	Ident, Span,
	decl::{Declaration, Module},
};
use nymph_diagnostics::Diagnostic;
use rustc_hash::FxHashMap;

use super::{
	ProjectDiagnostic,
	resolve::resolve_import_target,
	session::{
		AmbientCoreRegistryInput, BuiltinModuleDomain, BuiltinModuleInput, BuiltinModuleKey,
		ModuleInput, ModulePath, ProjectInput, ProjectKey, SemanticModuleDomain, SemanticModuleInput,
	},
};

#[salsa::db]
pub(crate) trait Db: salsa::Database {
	#[cfg(feature = "test-support")]
	fn semantic_query_will_execute(&self, _query: &'static str, _module: SemanticModuleInput) {}
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedModule {
	pub tree: Module,
	pub diagnostics: Arc<[Diagnostic]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectImport {
	pub target: Result<String, Diagnostic>,
	pub span: Span,
	pub namespace: Ident,
	pub has_with_list: bool,
	pub with_idents: Vec<(Ident, Option<Ident>)>,
}

pub type DirectImports = [DirectImport];

impl SemanticModuleInput {
	pub(crate) fn domain(self, db: &dyn Db) -> SemanticModuleDomain {
		match self {
			Self::Project(_) => SemanticModuleDomain::Project,
			Self::Builtin(module) => match module.key(db).domain {
				BuiltinModuleDomain::ImportableStd => SemanticModuleDomain::ImportableStd,
				BuiltinModuleDomain::AmbientCore => SemanticModuleDomain::AmbientCore,
			},
		}
	}

	pub(crate) fn display_key(self, db: &dyn Db) -> String {
		match self {
			Self::Project(module) => module.path(db).to_string(),
			Self::Builtin(module) => format!("std::{}", module.key(db).path),
		}
	}

	pub(crate) fn identity(self, db: &dyn Db) -> nymph_sema::ModuleIdentity {
		match self {
			Self::Project(module) => nymph_sema::ModuleIdentity {
				origin: nymph_sema::ModuleOrigin::Project(module.project(db).as_str().into()),
				project: module.project(db).as_str().into(),
				path: module.path(db).as_str().into(),
			},
			Self::Builtin(module) => nymph_sema::ModuleIdentity {
				origin: nymph_sema::ModuleOrigin::Compiler,
				project: "compiler".into(),
				path: module.key(db).path.as_ref().into(),
			},
		}
	}

	pub(crate) fn parsed(self, db: &dyn Db) -> Arc<ParsedModule> {
		match self {
			Self::Project(module) => parse(db, module).clone(),
			Self::Builtin(module) => compat_parse_builtin(db, module).clone(),
		}
	}

	pub(crate) fn imports(self, db: &dyn Db) -> Arc<DirectImports> {
		match self {
			Self::Project(module) => direct_imports(db, module).clone(),
			Self::Builtin(module) => compat_builtin_direct_imports(db, module).clone(),
		}
	}

	pub(crate) fn is_ambient_core(self, db: &dyn Db) -> bool {
		self.domain(db) == SemanticModuleDomain::AmbientCore
	}
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectGraph {
	pub order: Arc<[ModuleInput]>,
	#[allow(dead_code)]
	pub direct: Arc<[(ModuleInput, Arc<[ModuleInput]>)]>,
	pub(crate) semantic_order: Arc<[SemanticModuleInput]>,
	pub(crate) semantic_direct: Arc<[(SemanticModuleInput, Arc<[SemanticModuleInput]>)]>,
	pub diagnostics: Arc<[ProjectDiagnostic]>,
}

impl ProjectGraph {
	/// Deterministic project symbol tags shared by independent semantic pipelines.
	/// This is graph data, not a compatibility-analysis or symbol-map query.
	pub(crate) fn semantic_module_tags(
		&self,
		db: &dyn Db,
	) -> FxHashMap<nymph_sema::ModuleIdentity, usize> {
		self
			.semantic_order
			.iter()
			.copied()
			.enumerate()
			.map(|(tag, module)| (module.identity(db), tag))
			.collect()
	}

	pub(crate) fn semantic_direct_dependencies(
		&self,
		module: SemanticModuleInput,
	) -> Arc<[SemanticModuleInput]> {
		self
			.semantic_direct
			.iter()
			.find_map(|(owner, dependencies)| (*owner == module).then(|| dependencies.clone()))
			.unwrap_or_else(|| Arc::new([]))
	}

	pub(crate) fn semantic_direct_imports(
		&self,
		db: &dyn Db,
		module: SemanticModuleInput,
	) -> Arc<DirectImports> {
		// A validated graph guarantees each successful import has a matching
		// direct semantic edge. Keep the binding's namespace and `with` aliases
		// in source order for the interface environment query.
		if self
			.semantic_direct
			.iter()
			.any(|(owner, _)| *owner == module)
		{
			module.imports(db)
		} else {
			Arc::new([])
		}
	}

	pub(crate) fn semantic_closure(&self, root: SemanticModuleInput) -> Arc<[SemanticModuleInput]> {
		use std::collections::HashSet;
		fn visit(
			graph: &ProjectGraph,
			module: SemanticModuleInput,
			seen: &mut HashSet<SemanticModuleInput>,
		) {
			for dependency in graph.semantic_direct_dependencies(module).iter().copied() {
				if seen.insert(dependency) {
					visit(graph, dependency, seen);
				}
			}
		}
		let mut seen = HashSet::new();
		visit(self, root, &mut seen);
		self
			.semantic_order
			.iter()
			.copied()
			.filter(|module| seen.contains(module))
			.collect::<Vec<_>>()
			.into()
	}
}

#[salsa::tracked]
pub(crate) fn parse(db: &dyn Db, module: ModuleInput) -> Arc<ParsedModule> {
	parse_source(
		module.source(db).unwrap_or_default(),
		format!("{}.nym", module.path(db)),
	)
}

#[salsa::tracked]
pub(crate) fn compat_parse_builtin(db: &dyn Db, module: BuiltinModuleInput) -> Arc<ParsedModule> {
	let key = module.key(db);
	let prefix = match key.domain {
		BuiltinModuleDomain::ImportableStd => "std",
		BuiltinModuleDomain::AmbientCore => "core",
	};
	parse_source(module.source(db), format!("{prefix}::{}.nym", key.path))
}

fn parse_source(source: Arc<str>, path: String) -> Arc<ParsedModule> {
	let parsed = nymph_syntax::parse_module(&source, path);
	Arc::new(ParsedModule {
		tree: parsed.tree,
		diagnostics: parsed.diagnostics.into(),
	})
}

#[salsa::tracked]
pub(crate) fn direct_imports(db: &dyn Db, module: ModuleInput) -> Arc<DirectImports> {
	collect_imports(&parse(db, module), module.path(db).as_str())
}

#[salsa::tracked]
pub(crate) fn compat_builtin_direct_imports(
	db: &dyn Db,
	module: BuiltinModuleInput,
) -> Arc<DirectImports> {
	collect_imports(
		compat_parse_builtin(db, module),
		&format!("std::{}", module.key(db).path),
	)
}

#[salsa::tracked]
pub(crate) fn ambient_core_direct_imports(
	db: &dyn Db,
	module: BuiltinModuleInput,
) -> Arc<DirectImports> {
	collect_imports(compat_parse_builtin(db, module), &module.key(db).path)
}

#[salsa::tracked(returns(clone))]
fn ambient_core_graph(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	root: BuiltinModuleInput,
) -> Arc<[BuiltinModuleInput]> {
	fn visit(
		db: &dyn Db,
		module: BuiltinModuleInput,
		modules: &std::collections::BTreeMap<Arc<str>, BuiltinModuleInput>,
		seen: &mut std::collections::BTreeSet<Arc<str>>,
		order: &mut Vec<BuiltinModuleInput>,
	) {
		let path = module.key(db).path;
		if !seen.insert(path.clone()) {
			return;
		}
		for import in ambient_core_direct_imports(db, module).iter() {
			if let Ok(target) = &import.target {
				let child = modules.get(target.as_str()).unwrap_or_else(|| {
					panic!("ambient core `{path}` imports missing core sibling `{target}`")
				});
				visit(db, *child, modules, seen, order);
			}
		}
		order.push(module);
	}
	let modules = registry
		.modules(db)
		.iter()
		.map(|module| (module.key(db).path, *module))
		.collect();
	let mut seen = std::collections::BTreeSet::new();
	let mut order = Vec::new();
	visit(db, root, &modules, &mut seen, &mut order);
	order.into()
}

#[salsa::tracked(no_eq)]
pub(crate) fn ambient_core_analysis(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<super::session::ModuleAnalysis> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute(
		"ambient_core_analysis",
		SemanticModuleInput::Builtin(module),
	);
	// This private compatibility bootstrap temporarily flattens only the
	// compiler-core dependency closure. It never enters the public ProjectGraph
	// or user flattened analysis. Production compatibility flattening remains
	// explicitly inside the Task 6 review boundary until Task 7 replaces it with
	// interface-driven dependency checking.
	let dependencies = registry
		.modules(db)
		.iter()
		.filter(|input| **input != module)
		.map(|input| compat_parse_builtin(db, *input).tree.clone())
		.collect::<Vec<_>>();
	let parsed = compat_parse_builtin(db, module);
	let paired = nymph_sema::check_module_with_prelude_and_module(&parsed.tree, &dependencies);
	let checked = Arc::new(paired.checked);
	Arc::new(super::session::ModuleAnalysis {
		semantic: Arc::new(nymph_sema::SemanticAnalysis {
			module: Arc::new(parsed.tree.clone()),
			checked: Arc::new(checked.facts.clone()),
			annotations: Arc::new(nymph_sema::ModuleAnnotations::from(
				checked.facts.annotations.clone(),
			)),
		}),
		diagnostics: super::session::ProjectDiagnostics(Arc::new([])),
	})
}

fn ambient_identity(db: &dyn Db, module: BuiltinModuleInput) -> nymph_sema::ModuleIdentity {
	nymph_sema::ModuleIdentity {
		origin: nymph_sema::ModuleOrigin::Compiler,
		project: "compiler".into(),
		path: module.key(db).path.as_ref().into(),
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_core_headers(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<nymph_sema::DeclaredHeaders> {
	let own = nymph_sema::declared_headers(
		ambient_identity(db, module),
		&compat_parse_builtin(db, module).tree,
	);
	let checked = registry
		.modules(db)
		.iter()
		.flat_map(|input| {
			nymph_sema::declared_headers(
				ambient_identity(db, *input),
				&compat_parse_builtin(db, *input).tree,
			)
			.definitions
		})
		.collect();
	Arc::new(own.with_checked_definitions(checked))
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_core_environment(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<nymph_sema::ModuleEnvironment> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute(
		"ambient_core_environment",
		SemanticModuleInput::Builtin(module),
	);
	let analysis = ambient_core_analysis(db, registry, module);
	let checked = checked_from_analysis(&analysis, []);
	let facts =
		nymph_sema::ExtractionFactSelection::current_module(&analysis.semantic.module, &checked);
	Arc::new(nymph_sema::recover_module_environment_with_facts(
		ambient_identity(db, module),
		&analysis.semantic.module,
		&checked,
		&ambient_core_headers(db, registry, module),
		&facts,
	))
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_core_interface(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<nymph_sema::ModuleInterface> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute(
		"ambient_core_interface",
		SemanticModuleInput::Builtin(module),
	);
	match &*ambient_core_environment(db, registry, module) {
		nymph_sema::ModuleEnvironment::Complete(interface) => Arc::new(interface.clone()),
		nymph_sema::ModuleEnvironment::Recovered(_) => panic!(
			"embedded ambient core `{}` did not produce a complete interface",
			module.key(db).path
		),
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_core_diagnostics(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<[nymph_diagnostics::Diagnostic]> {
	let analysis = ambient_core_analysis(db, registry, module);
	let mut diagnostics = analysis
		.diagnostics
		.0
		.iter()
		.map(|item| item.diag.clone())
		.collect::<Vec<_>>();
	if diagnostics.is_empty() {
		let checked = checked_from_analysis(&analysis, []);
		let facts =
			nymph_sema::ExtractionFactSelection::current_module(&analysis.semantic.module, &checked);
		if let Err(error) = nymph_sema::extract_module_interface_with_facts(
			ambient_identity(db, module),
			&analysis.semantic.module,
			&checked,
			&ambient_core_headers(db, registry, module),
			&facts,
		) {
			diagnostics.push(nymph_diagnostics::Diagnostic::error(
				"INTERNAL-INTERFACE-CONVERSION".into(),
				format!("internal interface conversion failed: {error:?}"),
				Span::new(0, 0),
			));
		}
	}
	diagnostics.into()
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_runtime_owner_artifacts(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
) -> Arc<[super::session::BuiltinRuntimeOwnerArtifact]> {
	use super::session::{
		AmbientCoreModuleKey, BuiltinRuntimeOwnerArtifact, BuiltinRuntimeOwnerShape,
	};
	let mut artifacts = std::collections::BTreeMap::new();
	for module in registry.modules(db).iter().copied() {
		let environment = ambient_core_environment(db, registry, module);
		let nymph_sema::ModuleEnvironment::Complete(interface) = &*environment else {
			continue;
		};
		let key = AmbientCoreModuleKey::new(module.key(db).path.as_ref())
			.expect("embedded core paths are canonical");
		for definition in interface.exports.iter().chain(
			interface
				.support_definitions
				.iter()
				.map(|item| &item.definition),
		) {
			if let Some(owner) = &definition.runtime_owner {
				artifacts.insert(
					owner.clone(),
					BuiltinRuntimeOwnerArtifact {
						definition: owner.clone(),
						module: key.clone(),
						shape: BuiltinRuntimeOwnerShape::Definition(definition.clone()),
					},
				);
			}
		}
		for implementation in &interface.implementations {
			if let Some(owner) = &implementation.runtime_owner {
				artifacts.insert(
					owner.clone(),
					BuiltinRuntimeOwnerArtifact {
						definition: owner.clone(),
						module: key.clone(),
						shape: BuiltinRuntimeOwnerShape::Implementation(implementation.clone()),
					},
				);
			}
		}
	}
	artifacts.into_values().collect::<Vec<_>>().into()
}

fn collect_imports(parsed: &ParsedModule, importer: &str) -> Arc<DirectImports> {
	let mut imports = Vec::new();
	for declaration in &parsed.tree.members {
		if let Declaration::Import {
			root,
			path,
			alias,
			idents,
		} = declaration
		{
			let span = alias
				.as_ref()
				.map(|item| item.1)
				.or_else(|| path.last().map(|item| item.1))
				.unwrap_or(Span::new(0, 0));
			let target = resolve_import_target(root, path, importer, span);
			let namespace = alias
				.clone()
				.or_else(|| path.last().cloned())
				.unwrap_or_else(|| nymph_ast::Spanned("".into(), span));
			imports.push(DirectImport {
				target,
				span,
				namespace,
				has_with_list: idents.is_some(),
				with_idents: idents.clone().unwrap_or_default(),
			});
		}
	}
	imports.into()
}

fn checked_from_analysis(
	analysis: &super::session::ModuleAnalysis,
	diagnostics: impl IntoIterator<Item = Diagnostic>,
) -> nymph_sema::Checked {
	nymph_sema::Checked {
		diags: diagnostics.into_iter().collect(),
		facts: analysis.semantic.checked.as_ref().clone(),
	}
}

/// Check one module exclusively from its own tree and dependency interfaces.
#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn interface_module_analysis<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<super::session::ModuleAnalysis> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("interface_module_analysis", module);
	let graph = project_graph(db, key);
	let dependencies = graph
		.semantic_closure(module)
		.iter()
		.copied()
		.map(|dependency| interface_module_environment(db, key, dependency))
		.collect::<Vec<_>>();
	let mut roots = Vec::new();
	if key.ambient_prelude(db) {
		let registry = key.ambient_core_registry(db);
		roots.extend(
			registry
				.modules(db)
				.iter()
				.copied()
				.map(|root| ambient_core_environment(db, registry, root)),
		);
	}
	roots.extend(dependencies);
	let parsed = module.parsed(db);
	let mut environment = nymph_sema::SemanticEnvironment::from_modules(module.identity(db), &roots)
		.expect("validated interfaces form a deterministic semantic environment");
	environment.set_diagnostic_module_tags(&graph.semantic_module_tags(db));
	let mut bindings = FxHashMap::default();
	if key.ambient_prelude(db) {
		let registry = key.ambient_core_registry(db);
		for root in registry.modules(db).iter().copied() {
			let identity = SemanticModuleInput::Builtin(root).identity(db);
			if let Some(exports) = environment.module_exports.get(&identity) {
				for (name, stable) in &exports.by_name {
					bindings.insert(
						name.clone(),
						nymph_sema::ResolvedImportBinding::Definition(stable.clone()),
					);
				}
			}
		}
	}
	let direct = graph.semantic_direct_dependencies(module);
	for import in graph.semantic_direct_imports(db, module).iter() {
		let Ok(target_key) = &import.target else {
			continue;
		};
		let Some(target) = direct
			.iter()
			.copied()
			.find(|candidate| candidate.display_key(db) == *target_key)
		else {
			continue;
		};
		let identity = target.identity(db);
		bindings.insert(
			import.namespace.0.clone(),
			nymph_sema::ResolvedImportBinding::Namespace(identity.clone()),
		);
		if let Some(exports) = environment.module_exports.get(&identity) {
			let selected = import
				.with_idents
				.iter()
				.map(|(source, alias)| (alias.as_ref().unwrap_or(source).0.clone(), source.0.clone()))
				.collect::<Vec<_>>();
			for (local, source) in selected {
				let binding = exports
					.by_name
					.get(&source)
					.cloned()
					.map(nymph_sema::ResolvedImportBinding::Definition)
					.unwrap_or(nymph_sema::ResolvedImportBinding::Poison);
				bindings.insert(local, binding);
			}
		}
	}
	environment.set_resolved_imports(bindings);
	let result = nymph_sema::check_module_with_environment(
		Arc::new(parsed.tree.clone()),
		module.identity(db),
		&environment,
		if key.mode(db) == nymph_sema::EntryMode::Entry
			&& module.display_key(db) == key.entry(db).as_str()
		{
			nymph_sema::EntryMode::Entry
		} else {
			nymph_sema::EntryMode::Library
		},
	);
	let diagnostics = result
		.diagnostics
		.iter()
		.cloned()
		.map(|diag| ProjectDiagnostic {
			module: module.display_key(db),
			diag,
		})
		.collect::<Vec<_>>()
		.into();
	Arc::new(super::session::ModuleAnalysis {
		semantic: result.analysis,
		diagnostics: super::session::ProjectDiagnostics(diagnostics),
	})
}

#[salsa::tracked(returns(clone))]
pub(crate) fn interface_module_interface<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Result<Arc<nymph_sema::ModuleInterface>, Arc<nymph_sema::InterfaceConversionError>> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("interface_module_interface", module);
	let analysis = interface_module_analysis(db, key, module);
	let checked = checked_from_analysis(&analysis, []);
	let headers = nymph_sema::declared_headers(module.identity(db), &analysis.semantic.module);
	let facts =
		nymph_sema::ExtractionFactSelection::current_module(&analysis.semantic.module, &checked);
	nymph_sema::extract_module_interface_with_facts(
		module.identity(db),
		&analysis.semantic.module,
		&checked,
		&headers,
		&facts,
	)
	.map(Arc::new)
	.map_err(Arc::new)
}

#[salsa::tracked(returns(clone))]
pub(crate) fn interface_module_environment<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<nymph_sema::ModuleEnvironment> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("interface_module_environment", module);
	let analysis = interface_module_analysis(db, key, module);
	let diagnostics = interface_module_diagnostics(db, key, module);
	if diagnostics.is_empty()
		&& let Ok(interface) = interface_module_interface(db, key, module)
	{
		return Arc::new(nymph_sema::ModuleEnvironment::Complete(
			(*interface).clone(),
		));
	}
	let checked = checked_from_analysis(&analysis, diagnostics.iter().map(|item| item.diag.clone()));
	let headers = nymph_sema::declared_headers(module.identity(db), &analysis.semantic.module);
	let facts =
		nymph_sema::ExtractionFactSelection::current_module(&analysis.semantic.module, &checked);
	Arc::new(nymph_sema::recover_module_environment_with_facts(
		module.identity(db),
		&analysis.semantic.module,
		&checked,
		&headers,
		&facts,
	))
}

#[salsa::tracked(returns(clone))]
fn interface_module_diagnostics<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<[ProjectDiagnostic]> {
	let analysis = interface_module_analysis(db, key, module);
	analysis.diagnostics.0.clone()
}

#[salsa::tracked(returns(clone))]
pub(crate) fn interface_project_diagnostics<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
) -> super::session::ProjectDiagnostics {
	let graph = project_graph(db, key);
	if !graph.diagnostics.is_empty() {
		return super::session::ProjectDiagnostics(graph.diagnostics.clone());
	}
	let mut all = Vec::new();
	for module in graph.semantic_order.iter().copied() {
		all.extend(
			interface_module_diagnostics(db, key, module)
				.iter()
				.cloned(),
		);
	}
	let diagnostics = all.into();
	super::session::ProjectDiagnostics(diagnostics)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
	Gray,
	Black,
}

#[salsa::tracked]
pub(crate) fn project_graph<'db>(db: &'db dyn Db, key: ProjectKey<'db>) -> Arc<ProjectGraph> {
	use std::collections::BTreeMap;

	struct Walker<'a> {
		db: &'a dyn Db,
		active: &'a BTreeMap<ModulePath, ModuleInput>,
		builtins: &'a BTreeMap<BuiltinModuleKey, BuiltinModuleInput>,
		colors: BTreeMap<String, Color>,
		stack: Vec<String>,
		order: Vec<ModuleInput>,
		direct: Vec<(ModuleInput, Arc<[ModuleInput]>)>,
		semantic_order: Vec<SemanticModuleInput>,
		semantic_direct: Vec<(SemanticModuleInput, Arc<[SemanticModuleInput]>)>,
		diagnostics: Vec<ProjectDiagnostic>,
	}

	impl Walker<'_> {
		fn diagnostic(&mut self, module: &str, diag: Diagnostic) {
			self.diagnostics.push(ProjectDiagnostic {
				module: module.to_string(),
				diag,
			});
		}

		fn visit(&mut self, path: &str, import_site: Option<(&str, Span)>) -> bool {
			match self.colors.get(path) {
				Some(Color::Black) => return true,
				Some(Color::Gray) => {
					let start = self.stack.iter().position(|item| item == path).unwrap_or(0);
					let mut cycle = self.stack[start..].to_vec();
					cycle.push(path.to_string());
					self.diagnostic(
						path,
						Diagnostic::error(
							"IMPORT-CYCLE".into(),
							format!("import cycle detected: {}", cycle.join(" -> ")),
							Span::new(0, 0),
						),
					);
					return false;
				}
				None => {}
			}
			self.colors.insert(path.to_string(), Color::Gray);
			self.stack.push(path.to_string());

			let builtin = path
				.strip_prefix(super::resolve::STD_KEY_PREFIX)
				.and_then(|stripped| {
					self
						.builtins
						.get(&BuiltinModuleKey {
							domain: BuiltinModuleDomain::ImportableStd,
							path: Arc::from(stripped),
						})
						.copied()
				});
			let project = ModulePath::new(path)
				.ok()
				.and_then(|module_path| self.active.get(&module_path).copied());
			if builtin.is_none() && project.is_none() {
				let (blame, span) = import_site.unwrap_or((path, Span::new(0, 0)));
				self.diagnostic(
					blame,
					Diagnostic::error(
						"IMPORT-UNRESOLVED".into(),
						format!("module `{path}` could not be resolved (no source file found)"),
						span,
					),
				);
				self.colors.insert(path.to_string(), Color::Black);
				self.stack.pop();
				return false;
			}

			let parsed = builtin
				.map(|module| compat_parse_builtin(self.db, module))
				.unwrap_or_else(|| parse(self.db, project.unwrap()));
			let mut ok = true;
			for diag in parsed.diagnostics.iter().filter(|diag| diag.is_error()) {
				self.diagnostic(path, diag.clone());
				ok = false;
			}
			let imports = builtin
				.map(|module| compat_builtin_direct_imports(self.db, module))
				.unwrap_or_else(|| direct_imports(self.db, project.unwrap()));
			let mut handles = Vec::new();
			let mut semantic_handles = Vec::new();
			for import in imports.iter() {
				match &import.target {
					Ok(target) => {
						let semantic_handle = target
							.strip_prefix(super::resolve::STD_KEY_PREFIX)
							.and_then(|path| {
								self
									.builtins
									.get(&BuiltinModuleKey {
										domain: BuiltinModuleDomain::ImportableStd,
										path: Arc::from(path),
									})
									.copied()
							})
							.map(SemanticModuleInput::Builtin)
							.or_else(|| {
								ModulePath::new(target)
									.ok()
									.and_then(|path| self.active.get(&path).copied())
									.map(SemanticModuleInput::Project)
							});
						if let Some(handle) = semantic_handle {
							semantic_handles.push(handle);
						}
						if !target.starts_with(super::resolve::STD_KEY_PREFIX) {
							let target_path =
								ModulePath::new(target).expect("resolved local import is canonical");
							if let Some(handle) = self.active.get(&target_path) {
								handles.push(*handle);
							}
						}
						let child_ok = self.visit(target, Some((path, import.span)));
						ok = ok && child_ok;
					}
					Err(diag) => {
						self.diagnostic(path, diag.clone());
						ok = false;
					}
				}
			}
			if let Some(module) = project {
				self.direct.push((module, handles.into()));
			}
			let semantic = builtin
				.map(SemanticModuleInput::Builtin)
				.unwrap_or_else(|| SemanticModuleInput::Project(project.unwrap()));
			self
				.semantic_direct
				.push((semantic, semantic_handles.into()));
			self.colors.insert(path.to_string(), Color::Black);
			self.stack.pop();
			if ok {
				self.semantic_order.push(semantic);
				if let Some(module) = project {
					self.order.push(module);
				}
			}
			ok
		}
	}

	let project_input: ProjectInput = key.project_input(db);
	let active: BTreeMap<ModulePath, ModuleInput> = project_input
		.active_modules(db)
		.iter()
		.map(|module| (module.path(db).clone(), *module))
		.collect();
	let builtins: BTreeMap<BuiltinModuleKey, BuiltinModuleInput> = key
		.builtin_registry(db)
		.modules(db)
		.iter()
		.map(|module| (module.key(db), *module))
		.collect();
	let mut walker = Walker {
		db,
		active: &active,
		builtins: &builtins,
		colors: BTreeMap::new(),
		stack: Vec::new(),
		order: Vec::new(),
		direct: Vec::new(),
		semantic_order: Vec::new(),
		semantic_direct: Vec::new(),
		diagnostics: Vec::new(),
	};
	walker.visit(key.entry(db).as_str(), None);
	Arc::new(ProjectGraph {
		order: walker.order.into(),
		direct: walker.direct.into(),
		semantic_order: walker.semantic_order.into(),
		semantic_direct: walker.semantic_direct.into(),
		diagnostics: walker.diagnostics.into(),
	})
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use nymph_sema::EntryMode;

	use super::*;
	use crate::project::{
		resolve::GraphBuilder,
		session::{BuiltinRegistryInput, ProjectId},
	};

	#[salsa::db]
	#[derive(Clone)]
	struct TestDb {
		storage: salsa::Storage<Self>,
	}
	#[salsa::db]
	impl salsa::Database for TestDb {}
	#[salsa::db]
	impl Db for TestDb {}

	fn fixture(files: &[(&str, &str)], builtins: &[(&str, &str)]) -> (TestDb, ProjectKey<'static>) {
		let db = TestDb {
			storage: salsa::Storage::default(),
		};
		let project = ProjectId::new("graph-regression");
		let modules: Arc<[ModuleInput]> = files
			.iter()
			.map(|(path, source)| {
				ModuleInput::new(
					&db,
					project.clone(),
					ModulePath::new(path).unwrap(),
					Some(Arc::from(*source)),
				)
			})
			.collect::<Vec<_>>()
			.into();
		let builtin_modules: Arc<[BuiltinModuleInput]> = builtins
			.iter()
			.map(|(key, source)| {
				BuiltinModuleInput::new(
					&db,
					BuiltinModuleKey {
						domain: BuiltinModuleDomain::ImportableStd,
						path: Arc::from(*key),
					},
					Arc::from(*source),
				)
			})
			.collect::<Vec<_>>()
			.into();
		let input = ProjectInput::new(&db, project, modules);
		let registry = BuiltinRegistryInput::new(&db, builtin_modules);
		let ambient = AmbientCoreRegistryInput::new(&db, Arc::new([]));
		let key = ProjectKey::new(
			&db,
			input,
			registry,
			ambient,
			ModulePath::new("main").unwrap(),
			EntryMode::Entry,
			false,
			true,
		);
		// Test databases outlive each key in these tests; Salsa's key does not
		// contain an actual reference despite its invariant database lifetime.
		let key = unsafe { std::mem::transmute::<ProjectKey<'_>, ProjectKey<'static>>(key) };
		(db, key)
	}

	type DiagnosticTuple = (String, String, String, usize, usize);

	fn tuples(diags: &[ProjectDiagnostic]) -> Vec<DiagnosticTuple> {
		diags
			.iter()
			.map(|item| {
				(
					item.module.clone(),
					item.diag.code.to_string(),
					item.diag.message.to_string(),
					item.diag.span.start,
					item.diag.span.end,
				)
			})
			.collect()
	}

	fn assert_graph_matches_legacy(files: &[(&str, &str)]) -> Vec<DiagnosticTuple> {
		let sources: BTreeMap<_, _> = files.iter().copied().collect();
		let load = |key: &str| sources.get(key).map(ToString::to_string);
		let mut legacy = GraphBuilder::new(&load, &|_| None);
		assert!(!legacy.visit("main"));
		let expected = tuples(&legacy.diags);
		let (db, key) = fixture(files, &[]);
		assert_eq!(tuples(&project_graph(&db, key).diagnostics), expected);
		expected
	}

	#[test]
	fn graph_diagnostics_exactly_match_legacy_dfs_order_and_deduplication() {
		let cycle = assert_graph_matches_legacy(&[
			("main", "import @/a"),
			("a", "import @/b"),
			("b", "import @/a"),
		]);
		assert_eq!(cycle[0].0, "a");
		assert_eq!(cycle[0].1, "IMPORT-CYCLE");
		assert_eq!(cycle[0].2, "import cycle detected: a -> b -> a");

		let recovered =
			assert_graph_matches_legacy(&[("main", "import @/missing\nfunc broken(: int = 1")]);
		assert!(recovered.len() >= 2);
		assert_ne!(recovered[0].1, "IMPORT-UNRESOLVED");
		assert_eq!(recovered.last().unwrap().1, "IMPORT-UNRESOLVED");

		let mixed = assert_graph_matches_legacy(&[("main", "import pkg/nope\nimport @/missing")]);
		assert_eq!(
			mixed.iter().map(|item| item.1.as_str()).collect::<Vec<_>>(),
			["IMPORT-PACKAGE-UNSUPPORTED", "IMPORT-UNRESOLVED"]
		);

		let duplicate = assert_graph_matches_legacy(&[("main", "import @/missing\nimport @/missing")]);
		assert_eq!(
			duplicate
				.iter()
				.filter(|item| item.1 == "IMPORT-UNRESOLVED")
				.count(),
			1
		);
	}

	#[test]
	fn graph_ignores_unreachable_errors_and_preserves_clean_public_contracts() {
		let (db, key) = fixture(
			&[
				("main", "import @/a\nimport std/tool"),
				("a", "import @/b"),
				("b", "let value = 1"),
				("unreachable", "import @/missing"),
			],
			&[("tool", "public let answer = 42")],
		);
		let graph = project_graph(&db, key);
		assert!(graph.diagnostics.is_empty());
		assert_eq!(
			graph
				.order
				.iter()
				.map(|module| module.path(&db).as_str().to_string())
				.collect::<Vec<_>>(),
			["b", "a", "main"]
		);
		assert_eq!(graph.direct.len(), 3);
	}

	#[test]
	fn semantic_graph_includes_importable_builtins_dependency_first_without_changing_project_order() {
		let (db, key) = fixture(
			&[
				("main", "import @/a\nimport std/tool"),
				("a", "import std/base"),
			],
			&[
				("tool", "import ./base\npublic let tool = 1"),
				("base", "public let base = 1"),
			],
		);
		let graph = project_graph(&db, key);
		assert!(graph.diagnostics.is_empty());
		assert_eq!(
			graph
				.order
				.iter()
				.map(|module| module.path(&db).as_str().to_string())
				.collect::<Vec<_>>(),
			["a", "main"]
		);
		assert_eq!(
			graph
				.semantic_order
				.iter()
				.map(|module| module.display_key(&db))
				.collect::<Vec<_>>(),
			["std::base", "a", "std::tool", "main"]
		);
		let main = SemanticModuleInput::Project(
			*graph
				.order
				.iter()
				.find(|module| module.path(&db).as_str() == "main")
				.unwrap(),
		);
		assert_eq!(
			graph
				.semantic_closure(main)
				.iter()
				.map(|module| module.display_key(&db))
				.collect::<Vec<_>>(),
			["std::base", "a", "std::tool"]
		);
		assert_eq!(
			graph
				.semantic_direct_dependencies(main)
				.iter()
				.map(|module| module.display_key(&db))
				.collect::<Vec<_>>(),
			["a", "std::tool"]
		);
		assert_eq!(graph.semantic_direct_imports(&db, main).len(), 2);
	}

	#[test]
	fn semantic_module_identity_domains_do_not_collide_or_include_ambient_core() {
		let (db, key) = fixture(
			&[
				("main", "import @/std/tool\nimport std/tool"),
				("std/tool", ""),
			],
			&[("tool", "")],
		);
		let graph = project_graph(&db, key);
		let identities = graph
			.semantic_order
			.iter()
			.map(|module| module.identity(&db))
			.collect::<std::collections::BTreeSet<_>>();
		assert_eq!(identities.len(), 3);
		assert!(graph.semantic_order.iter().any(|module| {
			module.domain(&db) == SemanticModuleDomain::Project && module.display_key(&db) == "std/tool"
		}));
		assert!(graph.semantic_order.iter().any(|module| {
			module.domain(&db) == SemanticModuleDomain::ImportableStd
				&& module.display_key(&db) == "std::tool"
		}));
		assert!(
			graph
				.semantic_order
				.iter()
				.all(|module| !module.is_ambient_core(&db))
		);
		assert_eq!(graph.order.len(), 2);
	}

	#[test]
	fn builtin_parse_uses_the_legacy_module_path_through_the_compat_query() {
		let (db, key) = fixture(&[("main", "")], &[("custom", "public let answer = 42")]);
		let builtin = key.builtin_registry(&db).modules(&db)[0];
		assert_eq!(
			compat_parse_builtin(&db, builtin).tree.path.as_str(),
			"std::custom.nym"
		);
	}
}
