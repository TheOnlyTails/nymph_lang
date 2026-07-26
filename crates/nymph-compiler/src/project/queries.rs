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
	#[cfg(feature = "test-support")]
	fn runtime_query_will_execute(
		&self,
		_query: &'static str,
		_definition: &nymph_sema::DefinitionId,
	) {
	}
}

#[salsa::tracked]
pub(crate) struct RuntimeDefinitionEntity<'db> {
	#[returns(ref)]
	pub definition: nymph_sema::DefinitionId,
	#[tracked]
	#[returns(clone)]
	pub value: Arc<nymph_sema::RuntimeDefinition>,
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedModuleImports {
	pub bindings: FxHashMap<ecow::EcoString, nymph_sema::ResolvedImportBinding>,
	pub namespaces: Vec<(ecow::EcoString, nymph_sema::ModuleIdentity)>,
	pub diagnostics: Arc<[ProjectDiagnostic]>,
}

#[salsa::tracked(returns(clone))]
fn interface_module_lexical_private_definitions<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<[(ecow::EcoString, nymph_sema::DefinitionId)]> {
	let analysis = interface_module_analysis(db, key, module);
	let checked = checked_from_analysis(&analysis, []);
	let headers = nymph_sema::declared_headers(module.identity(db), &analysis.semantic.module);
	nymph_sema::extract_lexical_private_definitions(&analysis.semantic.module, &checked, &headers)
		.unwrap_or_default()
		.into()
}

fn local_declaration_name(declaration: &Declaration) -> Option<Ident> {
	match declaration {
		Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => match &meta.name.0 {
			nymph_ast::expr::Pattern::Binding { name, .. } => Some(name.clone()),
			_ => None,
		},
		Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
			Some(meta.name.clone())
		}
		Declaration::TypeAlias { meta, .. } => Some(meta.name.clone()),
		Declaration::Struct { name, .. }
		| Declaration::Enum { name, .. }
		| Declaration::Namespace { name, .. }
		| Declaration::Interface { name, .. } => Some(name.clone()),
		_ => None,
	}
}

fn insert_import_binding(
	bindings: &mut FxHashMap<ecow::EcoString, nymph_sema::ResolvedImportBinding>,
	locals: &FxHashMap<ecow::EcoString, Span>,
	diagnostics: &mut Vec<ProjectDiagnostic>,
	owner: &str,
	local: ecow::EcoString,
	span: Span,
	binding: nymph_sema::ResolvedImportBinding,
) -> bool {
	if bindings.contains_key(&local) || locals.contains_key(&local) {
		diagnostics.push(ProjectDiagnostic {
			module: owner.to_string(),
			diag: Diagnostic::error(
				"IMPORT-NAME-COLLISION".into(),
				format!("import name `{local}` collides with another lexical binding"),
				span,
			),
		});
		return false;
	}
	bindings.insert(local, binding);
	true
}

#[salsa::tracked(returns(clone))]
pub(crate) fn resolved_module_imports<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<ResolvedModuleImports> {
	let graph = project_graph(db, key);
	let direct = graph.semantic_direct_dependencies(module);
	let locals = module
		.parsed(db)
		.tree
		.members
		.iter()
		.filter_map(local_declaration_name)
		.map(|name| (name.0.clone(), name.1))
		.collect::<FxHashMap<_, _>>();
	let mut bindings = FxHashMap::default();
	let mut namespaces = Vec::new();
	let mut diagnostics = Vec::new();
	let owner = module.display_key(db);
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
		let interface = match interface_module_interface(db, key, target) {
			Ok(interface) => interface,
			Err(_) => continue,
		};
		let namespace = import.namespace.0.clone();
		let namespace_matches_export = interface
			.exports
			.iter()
			.any(|definition| definition.name == namespace)
			&& (!import.has_with_list
				|| import.with_idents.iter().any(|(source, alias)| {
					source.0 == namespace && alias.as_ref().unwrap_or(source).0 == namespace
				}));
		if !namespace_matches_export
			&& insert_import_binding(
				&mut bindings,
				&locals,
				&mut diagnostics,
				&owner,
				namespace.clone(),
				import.namespace.1,
				nymph_sema::ResolvedImportBinding::Namespace(identity.clone()),
			) {
			namespaces.push((namespace, identity));
		}
		let selected = if import.has_with_list {
			import
				.with_idents
				.iter()
				.map(|(source, alias)| {
					(
						source.0.clone(),
						alias.as_ref().unwrap_or(source).0.clone(),
						source.1,
					)
				})
				.collect::<Vec<_>>()
		} else {
			interface
				.exports
				.iter()
				.map(|definition| {
					(
						definition.name.clone(),
						definition.name.clone(),
						import.namespace.1,
					)
				})
				.collect::<Vec<_>>()
		};
		for (source, local, span) in selected {
			if let Some(definition) = interface.exports.iter().find(|item| item.name == source) {
				insert_import_binding(
					&mut bindings,
					&locals,
					&mut diagnostics,
					&owner,
					local,
					span,
					nymph_sema::ResolvedImportBinding::Definition(definition.id.clone()),
				);
			} else {
				let target_private = interface_module_lexical_private_definitions(db, key, target);
				let private = target_private.iter().any(|(name, _)| *name == source);
				diagnostics.push(ProjectDiagnostic {
					module: owner.clone(),
					diag: Diagnostic::error(
						if private {
							"IMPORT-PRIVATE-NAME"
						} else {
							"IMPORT-UNRESOLVED-NAME"
						}
						.into(),
						if private {
							format!("`{source}` is private and cannot be imported")
						} else {
							format!("imported module has no exported name `{source}`")
						},
						span,
					),
				});
			}
		}
	}
	Arc::new(ResolvedModuleImports {
		bindings,
		namespaces,
		diagnostics: diagnostics.into(),
	})
}

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
				origin: match module.key(db).domain {
					BuiltinModuleDomain::ImportableStd => nymph_sema::ModuleOrigin::ImportableStd,
					BuiltinModuleDomain::AmbientCore => nymph_sema::ModuleOrigin::Compiler,
				},
				project: "compiler".into(),
				path: module.key(db).path.as_ref().into(),
			},
		}
	}

	pub(crate) fn parsed(self, db: &dyn Db) -> Arc<ParsedModule> {
		match self {
			Self::Project(module) => parse(db, module).clone(),
			Self::Builtin(module) => parse_builtin(db, module).clone(),
		}
	}

	pub(crate) fn imports(self, db: &dyn Db) -> Arc<DirectImports> {
		match self {
			Self::Project(module) => direct_imports(db, module).clone(),
			Self::Builtin(module) => builtin_direct_imports(db, module).clone(),
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
pub(crate) fn parse_builtin(db: &dyn Db, module: BuiltinModuleInput) -> Arc<ParsedModule> {
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
pub(crate) fn builtin_direct_imports(
	db: &dyn Db,
	module: BuiltinModuleInput,
) -> Arc<DirectImports> {
	collect_imports(
		parse_builtin(db, module),
		&format!("std::{}", module.key(db).path),
	)
}

#[salsa::tracked]
pub(crate) fn ambient_core_direct_imports(
	db: &dyn Db,
	module: BuiltinModuleInput,
) -> Arc<DirectImports> {
	collect_imports(parse_builtin(db, module), &module.key(db).path)
}

#[salsa::tracked(returns(clone))]
fn ambient_core_graph(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	root: BuiltinModuleInput,
) -> Arc<AmbientCoreGraph> {
	#[derive(Clone, Copy, PartialEq, Eq)]
	enum Mark {
		Visiting,
		Visited,
	}
	fn visit(
		db: &dyn Db,
		module: BuiltinModuleInput,
		modules: &std::collections::BTreeMap<Arc<str>, BuiltinModuleInput>,
		marks: &mut std::collections::BTreeMap<Arc<str>, Mark>,
		stack: &mut Vec<Arc<str>>,
		order: &mut Vec<BuiltinModuleInput>,
		diagnostics: &mut Vec<Diagnostic>,
	) -> bool {
		let path = module.key(db).path;
		match marks.get(&path) {
			Some(Mark::Visited) => return true,
			Some(Mark::Visiting) => {
				let start = stack.iter().position(|item| item == &path).unwrap_or(0);
				let mut cycle = stack[start..].to_vec();
				cycle.push(path.clone());
				diagnostics.push(Diagnostic::error(
					"CORE-IMPORT-CYCLE".into(),
					format!("ambient core import cycle: {}", cycle.join(" -> ")),
					Span::new(0, 0),
				));
				return false;
			}
			None => {}
		}
		marks.insert(path.clone(), Mark::Visiting);
		stack.push(path.clone());
		let mut valid = true;
		for import in ambient_core_direct_imports(db, module).iter() {
			if let Ok(target) = &import.target {
				if let Some(child) = modules.get(target.as_str()) {
					valid &= visit(db, *child, modules, marks, stack, order, diagnostics);
				} else {
					diagnostics.push(Diagnostic::error(
						"CORE-IMPORT-UNRESOLVED".into(),
						format!("ambient core `{path}` imports missing module `{target}`"),
						import.span,
					));
					valid = false;
				}
			}
		}
		stack.pop();
		marks.insert(path, Mark::Visited);
		if valid {
			order.push(module);
		}
		valid
	}
	let modules = registry
		.modules(db)
		.iter()
		.map(|module| (module.key(db).path, *module))
		.collect();
	let mut marks = std::collections::BTreeMap::new();
	let mut stack = Vec::new();
	let mut order = Vec::new();
	let mut diagnostics = Vec::new();
	visit(
		db,
		root,
		&modules,
		&mut marks,
		&mut stack,
		&mut order,
		&mut diagnostics,
	);
	Arc::new(AmbientCoreGraph {
		order: order.into(),
		diagnostics: diagnostics.into(),
	})
}

#[derive(Clone, Debug, PartialEq)]
struct AmbientCoreGraph {
	order: Arc<[BuiltinModuleInput]>,
	diagnostics: Arc<[Diagnostic]>,
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
	let graph = ambient_core_graph(db, registry, module);
	let dependencies = graph
		.order
		.iter()
		.copied()
		.filter(|input| *input != module)
		.map(|input| ambient_core_environment(db, registry, input))
		.collect::<Vec<_>>();
	let parsed = parse_builtin(db, module);
	let semantic_module = Arc::new(parsed.tree.clone());
	let result = if graph.diagnostics.is_empty() {
		let mut environment =
			nymph_sema::SemanticEnvironment::from_modules(ambient_identity(db, module), &dependencies)
				.expect("ambient dependency interfaces have deterministic identities");
		let mut bindings = FxHashMap::default();
		for import in ambient_core_direct_imports(db, module).iter() {
			let Ok(target) = &import.target else { continue };
			let Some(dependency) = graph
				.order
				.iter()
				.copied()
				.find(|input| input.key(db).path.as_ref() == target.as_str())
			else {
				continue;
			};
			let identity = ambient_identity(db, dependency);
			bindings.insert(
				import.namespace.0.clone(),
				nymph_sema::ResolvedImportBinding::Namespace(identity.clone()),
			);
			if let Some(exports) = environment.module_exports.get(&identity) {
				for (source, alias) in &import.with_idents {
					let local = alias.as_ref().unwrap_or(source).0.clone();
					let binding = exports
						.by_name
						.get(&source.0)
						.cloned()
						.map(nymph_sema::ResolvedImportBinding::Definition)
						.unwrap_or(nymph_sema::ResolvedImportBinding::Poison);
					bindings.insert(local, binding);
				}
			}
		}
		environment.set_resolved_imports(bindings);
		nymph_sema::check_module_with_environment(
			semantic_module.clone(),
			ambient_identity(db, module),
			&environment,
			nymph_sema::EntryMode::Library,
		)
	} else {
		let environment =
			nymph_sema::SemanticEnvironment::from_modules(ambient_identity(db, module), &[])
				.expect("empty environment is valid");
		let mut result = nymph_sema::check_module_with_environment(
			semantic_module.clone(),
			ambient_identity(db, module),
			&environment,
			nymph_sema::EntryMode::Library,
		);
		let mut diagnostics = graph.diagnostics.to_vec();
		diagnostics.extend(result.diagnostics.iter().cloned());
		result.diagnostics = diagnostics.into();
		result
	};
	Arc::new(super::session::ModuleAnalysis {
		semantic: result.analysis,
		diagnostics: super::session::ProjectDiagnostics(
			result
				.diagnostics
				.iter()
				.cloned()
				.map(|diag| ProjectDiagnostic {
					module: module.key(db).path.to_string(),
					diag,
				})
				.collect::<Vec<_>>()
				.into(),
		),
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
		&parse_builtin(db, module).tree,
	);
	let dependency_definitions = ambient_core_graph(db, registry, module)
		.order
		.iter()
		.copied()
		.filter(|dependency| *dependency != module)
		.flat_map(|dependency| {
			nymph_sema::declared_headers(
				ambient_identity(db, dependency),
				&parse_builtin(db, dependency).tree,
			)
			.definitions
		})
		.collect();
	Arc::new(own.with_checked_definitions(dependency_definitions))
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

#[salsa::tracked(returns(clone))]
fn ambient_runtime_definition_index<'db>(
	db: &'db dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<[RuntimeDefinitionEntity<'db>]> {
	let environment = ambient_core_environment(db, registry, module);
	let nymph_sema::ModuleEnvironment::Complete(interface) = environment.as_ref() else {
		return Arc::new([]);
	};
	let analysis = ambient_core_analysis(db, registry, module);
	let Ok(mut definitions) = nymph_sema::runtime_definitions(
		&analysis.semantic.module,
		&module.source(db),
		&analysis.semantic.checked,
		interface,
	) else {
		return Arc::new([]);
	};
	definitions.sort_by(|a, b| a.definition.cmp(&b.definition));
	definitions
		.into_iter()
		.map(|value| RuntimeDefinitionEntity::new(db, value.definition.clone(), Arc::new(value)))
		.collect::<Vec<_>>()
		.into()
}

fn exact_module_environment<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<nymph_sema::ModuleEnvironment> {
	match module {
		SemanticModuleInput::Builtin(input)
			if input.key(db).domain == BuiltinModuleDomain::AmbientCore =>
		{
			ambient_core_environment(db, key.ambient_core_registry(db), input)
		}
		_ => interface_module_environment(db, key, module),
	}
}

#[salsa::tracked]
pub(crate) fn runtime_definition_index<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<[RuntimeDefinitionEntity<'db>]> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("runtime_definition_index", module);
	if let SemanticModuleInput::Builtin(input) = module
		&& input.key(db).domain == BuiltinModuleDomain::AmbientCore
	{
		return ambient_runtime_definition_index(db, key.ambient_core_registry(db), input);
	}
	let environment = interface_module_environment(db, key, module);
	let nymph_sema::ModuleEnvironment::Complete(interface) = environment.as_ref() else {
		return Arc::new([]);
	};
	let analysis = interface_module_analysis(db, key, module);
	let source = match module {
		SemanticModuleInput::Project(input) => match input.source(db) {
			Some(source) => source,
			None => return Arc::new([]),
		},
		SemanticModuleInput::Builtin(input) => input.source(db),
	};
	let Ok(mut definitions) = nymph_sema::runtime_definitions(
		&analysis.semantic.module,
		&source,
		&analysis.semantic.checked,
		interface,
	) else {
		return Arc::new([]);
	};
	definitions.sort_by(|a, b| a.definition.cmp(&b.definition));
	let mut seen = std::collections::BTreeSet::new();
	definitions
		.into_iter()
		.map(|value| {
			assert!(
				seen.insert(value.definition.clone()),
				"duplicate runtime definition identity: {:?}",
				value.definition
			);
			RuntimeDefinitionEntity::new(db, value.definition.clone(), Arc::new(value))
		})
		.collect::<Vec<_>>()
		.into()
}

/// Runtime-bearing identities owned by `module`, in language output order.
/// This is intentionally distinct from lookup: module assembly depends on the
/// identities, then requests each body through its exact per-definition query.
#[salsa::tracked(returns(clone))]
pub(crate) fn runtime_definition_ids<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Result<Arc<[nymph_sema::DefinitionId]>, super::session::RuntimeDefinitionError> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("runtime_definition_ids", module);
	let environment = exact_module_environment(db, key, module);
	let nymph_sema::ModuleEnvironment::Complete(interface) = environment.as_ref() else {
		return Err(super::session::RuntimeDefinitionError::Recovered);
	};
	let analysis = match module {
		SemanticModuleInput::Builtin(input)
			if input.key(db).domain == BuiltinModuleDomain::AmbientCore =>
		{
			ambient_core_analysis(db, key.ambient_core_registry(db), input).clone()
		}
		_ => interface_module_analysis(db, key, module),
	};
	let source = match module {
		SemanticModuleInput::Project(input) => match input.source(db) {
			Some(source) => source,
			None => return Err(super::session::RuntimeDefinitionError::OwnerNotFound),
		},
		SemanticModuleInput::Builtin(input) => input.source(db),
	};
	Ok(
		nymph_sema::runtime_definitions(
			&analysis.semantic.module,
			&source,
			&analysis.semantic.checked,
			interface,
		)
		.map_err(super::session::RuntimeDefinitionError::Extraction)?
		.into_iter()
		.map(|artifact| artifact.definition)
		.collect::<Vec<_>>()
		.into(),
	)
}

#[salsa::tracked(returns(clone))]
pub(crate) fn runtime_definition<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<Arc<nymph_sema::RuntimeDefinition>, super::session::RuntimeDefinitionError> {
	#[cfg(feature = "test-support")]
	db.runtime_query_will_execute("runtime_definition", &definition);
	let module = runtime_owner(db, key, &definition)?;
	let environment = exact_module_environment(db, key, module);
	if matches!(
		environment.as_ref(),
		nymph_sema::ModuleEnvironment::Recovered(_)
	) {
		return Err(super::session::RuntimeDefinitionError::Recovered);
	}
	let nymph_sema::ModuleEnvironment::Complete(interface) = environment.as_ref() else {
		unreachable!()
	};
	if matches!(
		definition.key,
		nymph_sema::DeclarationKey::MaterializedInterfaceMember { .. }
	) {
		let slot = interface
			.implementations
			.iter()
			.flat_map(|implementation| &implementation.member_slots)
			.find(|slot| slot.member_id == definition)
			.ok_or(super::session::RuntimeDefinitionError::ImplementationMemberMappingNotFound)?;
		if slot.source != nymph_sema::ImplementationMemberSource::InheritedDefault
			|| slot.implementation_id != slot.placement_owner
		{
			return Err(super::session::RuntimeDefinitionError::Extraction(
				nymph_sema::RuntimeExtractionError::CorruptImplementationMemberMapping(definition),
			));
		}
		return Ok(Arc::new(nymph_sema::RuntimeDefinition {
			definition: slot.member_id.clone(),
			source_owner: slot.body_definition_id.module.clone(),
			placement: nymph_sema::RuntimePlacement::Attached {
				owner: slot.placement_owner.clone(),
				name: slot.name.clone(),
			},
			payload: nymph_sema::RuntimePayload::MaterializedInterfaceMember {
				body_definition: slot.body_definition_id.clone(),
				interface_member: slot.interface_member_id.clone(),
			},
		}));
	}
	let analysis = match module {
		SemanticModuleInput::Builtin(input)
			if input.key(db).domain == BuiltinModuleDomain::AmbientCore =>
		{
			ambient_core_analysis(db, key.ambient_core_registry(db), input).clone()
		}
		_ => interface_module_analysis(db, key, module),
	};
	let source = match module {
		SemanticModuleInput::Project(input) => input
			.source(db)
			.ok_or(super::session::RuntimeDefinitionError::OwnerNotFound)?,
		SemanticModuleInput::Builtin(input) => input.source(db),
	};
	nymph_sema::runtime_definitions(
		&analysis.semantic.module,
		&source,
		&analysis.semantic.checked,
		interface,
	)
	.map_err(super::session::RuntimeDefinitionError::Extraction)?;
	let entity = runtime_definition_index(db, key, module)
		.iter()
		.copied()
		.find(|entity| entity.definition(db) == &definition)
		.ok_or(super::session::RuntimeDefinitionError::DefinitionNotFound)?;
	Ok(entity.value(db))
}

#[cfg(feature = "test-support")]
#[salsa::tracked(returns(clone))]
pub(crate) fn runtime_definition_consumer<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<Arc<nymph_sema::RuntimeDefinition>, super::session::RuntimeDefinitionError> {
	db.runtime_query_will_execute("runtime_definition_consumer", &definition);
	runtime_definition(db, key, definition)
}

fn runtime_owner<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	definition: &nymph_sema::DefinitionId,
) -> Result<SemanticModuleInput, super::session::RuntimeDefinitionError> {
	let graph = project_graph(db, key);
	let importable = key.builtin_registry(db).modules(db);
	let ambient = key.ambient_core_registry(db).modules(db);
	let owners = graph
		.semantic_order
		.iter()
		.copied()
		.chain(importable.iter().copied().map(SemanticModuleInput::Builtin))
		.chain(ambient.iter().copied().map(SemanticModuleInput::Builtin))
		.filter(|module| module.identity(db) == definition.module)
		.collect::<std::collections::HashSet<_>>();
	let mut owners = owners.into_iter();
	let Some(owner) = owners.next() else {
		return Err(super::session::RuntimeDefinitionError::OwnerNotFound);
	};
	if owners.next().is_some() {
		return Err(super::session::RuntimeDefinitionError::DuplicateOwner);
	}
	Ok(owner)
}

fn complete_interface<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	definition: &nymph_sema::DefinitionId,
) -> Result<Arc<nymph_sema::ModuleEnvironment>, nymph_sema::StableShapeLookupError> {
	let module = runtime_owner(db, key, definition).map_err(|_| {
		nymph_sema::StableShapeLookupError::Missing {
			request: nymph_sema::StableShapeRequest::InterfaceShell(definition.clone()),
		}
	})?;
	let environment = exact_module_environment(db, key, module);
	if matches!(
		environment.as_ref(),
		nymph_sema::ModuleEnvironment::Recovered(_)
	) {
		return Err(nymph_sema::StableShapeLookupError::Recovered {
			definition: definition.clone(),
		});
	}
	Ok(environment)
}

#[salsa::tracked(returns(clone))]
pub(crate) fn stable_shape<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	request: nymph_sema::StableShapeRequest,
) -> Result<nymph_sema::StableShapeFact, nymph_sema::StableShapeLookupError> {
	#[cfg(feature = "test-support")]
	db.runtime_query_will_execute("stable_shape", request.definition());
	use nymph_sema::{StableShapeFact as Fact, StableShapeRequest as Request};
	if let Request::TypeShell(definition) = &request {
		let artifact = runtime_definition(db, key, definition.clone()).map_err(|_| {
			nymph_sema::StableShapeLookupError::Missing {
				request: request.clone(),
			}
		})?;
		return match &artifact.payload {
			nymph_sema::RuntimePayload::Struct(shell) => Ok(Fact::TypeShell(
				nymph_sema::StableTypeShell::Struct(shell.clone()),
			)),
			nymph_sema::RuntimePayload::Enum(shell) => Ok(Fact::TypeShell(
				nymph_sema::StableTypeShell::Enum(shell.clone()),
			)),
			_ => Err(nymph_sema::StableShapeLookupError::Missing { request }),
		};
	}
	let environment = complete_interface(db, key, request.definition())?;
	let nymph_sema::ModuleEnvironment::Complete(interface) = environment.as_ref() else {
		unreachable!()
	};
	let definitions = interface.exports.iter().chain(
		interface
			.support_definitions
			.iter()
			.map(|item| &item.definition),
	);
	let result = match &request {
		Request::Member(id) => definitions
			.clone()
			.flat_map(|definition| &definition.members)
			.chain(
				interface
					.implementations
					.iter()
					.flat_map(|implementation| &implementation.members),
			)
			.find(|member| member.id == *id)
			.cloned()
			.map(Fact::Member),
		Request::Implementation(id) => interface
			.implementations
			.iter()
			.find(|implementation| implementation.id == *id)
			.cloned()
			.map(Fact::Implementation),
		Request::ImplementationsForInterface(id) => {
			let graph = project_graph(db, key);
			let builtins = key.builtin_registry(db).modules(db);
			let ambient = key.ambient_core_registry(db).modules(db);
			let modules = graph
				.semantic_order
				.iter()
				.copied()
				.chain(builtins.iter().copied().map(SemanticModuleInput::Builtin))
				.chain(ambient.iter().copied().map(SemanticModuleInput::Builtin));
			let mut implementations = Vec::new();
			for module in modules {
				let environment = exact_module_environment(db, key, module);
				let nymph_sema::ModuleEnvironment::Complete(candidate) = environment.as_ref() else {
					continue;
				};
				implementations.extend(
					candidate
						.implementations
						.iter()
						.filter(|implementation| implementation.interface.as_ref() == Some(id))
						.cloned(),
				);
			}
			implementations.sort_by(|left, right| left.id.cmp(&right.id));
			implementations.dedup_by(|left, right| left.id == right.id);
			Some(Fact::Implementations(implementations))
		}
		Request::InterfaceShell(id) => definitions
			.clone()
			.find(|definition| definition.id == *id)
			.cloned()
			.map(Fact::InterfaceShell),
		Request::ExternalAbi(id) => definitions
			.clone()
			.find(|definition| definition.id == *id)
			.and_then(|definition| definition.external.clone())
			.or_else(|| {
				definitions
					.clone()
					.flat_map(|definition| &definition.members)
					.chain(
						interface
							.implementations
							.iter()
							.flat_map(|implementation| &implementation.members),
					)
					.find(|member| member.id == *id)
					.and_then(|member| member.external.clone())
			})
			.map(Fact::ExternalAbi),
		Request::TypeShell(_) => unreachable!(),
	};
	result.ok_or(nymph_sema::StableShapeLookupError::Missing { request })
}

fn declaration_name(definition: &nymph_sema::DefinitionId) -> Option<&str> {
	match &definition.key {
		nymph_sema::DeclarationKey::TopLevel { name, .. }
		| nymph_sema::DeclarationKey::Member { name, .. }
		| nymph_sema::DeclarationKey::MethodBody { name, .. } => Some(name),
		nymph_sema::DeclarationKey::MaterializedInterfaceMember {
			interface_member, ..
		} => declaration_name(interface_member),
		_ => None,
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn binding_name<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<nymph_sema::EmittedBindingName, nymph_sema::StableNameLookupError> {
	let name = declaration_name(&definition).ok_or_else(|| {
		nymph_sema::StableNameLookupError::MissingBinding {
			definition: definition.clone(),
		}
	})?;
	let graph = project_graph(db, key);
	let project_tag = graph
		.semantic_order
		.iter()
		.position(|module| module.identity(db) == definition.module);
	let importable = key.builtin_registry(db).modules(db);
	let importable_tag = importable
		.iter()
		.position(|module| SemanticModuleInput::Builtin(*module).identity(db) == definition.module)
		.map(|index| graph.semantic_order.len() + index);
	let ambient = key.ambient_core_registry(db).modules(db);
	let ambient_tag = ambient
		.iter()
		.position(|module| ambient_identity(db, *module) == definition.module)
		.map(|index| graph.semantic_order.len() + importable.len() + index);
	let tag = project_tag
		.or(importable_tag)
		.or(ambient_tag)
		.ok_or_else(|| nymph_sema::StableNameLookupError::MissingBinding {
			definition: definition.clone(),
		})?;
	let preserve = key.preserve_names(db) && definition.module.path == key.entry(db).as_str();
	let entry_main = key.mode(db) == nymph_sema::EntryMode::Entry
		&& definition.module.path == key.entry(db).as_str()
		&& name == "main";
	let implementation_receiver = |implementation: &nymph_sema::DefinitionId| {
		let nymph_sema::DeclarationKey::Implementation { header, .. } = &implementation.key else {
			return Ok(None);
		};
		let Some(receiver) = primitive_header_tag(&header.self_type) else {
			return Ok(None);
		};
		let environment = complete_interface(db, key, implementation).map_err(|_| {
			nymph_sema::StableNameLookupError::MissingBinding {
				definition: implementation.clone(),
			}
		})?;
		let nymph_sema::ModuleEnvironment::Complete(interface) = environment.as_ref() else {
			unreachable!("complete_interface rejects recovered module environments")
		};
		let local_ordinal = interface
			.implementations
			.iter()
			.position(|shape| shape.id == *implementation)
			.ok_or_else(|| nymph_sema::StableNameLookupError::MissingBinding {
				definition: implementation.clone(),
			})?;
		Ok(Some(format!("{receiver}$i{local_ordinal}")))
	};
	let receiver = match &definition.key {
		nymph_sema::DeclarationKey::Member { owner, .. } => match &owner.key {
			nymph_sema::DeclarationKey::Implementation { .. } => implementation_receiver(owner)?,
			_ => None,
		},
		nymph_sema::DeclarationKey::MaterializedInterfaceMember { implementation, .. } => {
			implementation_receiver(implementation)?
		}
		_ => None,
	};
	Ok(nymph_sema::EmittedBindingName::new(
		if preserve || entry_main {
			name.to_owned()
		} else if let Some(receiver) = receiver {
			format!("$m{tag}${receiver}${name}")
		} else {
			format!("$m{tag}${name}")
		},
	))
}

fn primitive_header_tag(ty: &nymph_sema::HeaderType) -> Option<&'static str> {
	use nymph_sema::HeaderType;
	match ty {
		HeaderType::Int => Some("int"),
		HeaderType::UInt => Some("uint"),
		HeaderType::Float => Some("float"),
		HeaderType::Char => Some("char"),
		HeaderType::String => Some("string"),
		HeaderType::Boolean => Some("bool"),
		HeaderType::Void => Some("void"),
		HeaderType::List(_) | HeaderType::Tuple(_) => Some("list"),
		HeaderType::Map(..) => Some("map"),
		HeaderType::Mutable(inner) => primitive_header_tag(inner),
		_ => None,
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn member_name<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<nymph_sema::EmittedMemberName, nymph_sema::StableNameLookupError> {
	let protocol_slots = nominal_protocol_member_slots(db, key);
	let selected_interface_member = match &definition.key {
		nymph_sema::DeclarationKey::Member { owner, .. }
			if matches!(owner.key, nymph_sema::DeclarationKey::Implementation { .. }) =>
		{
			match stable_shape(
				db,
				key,
				nymph_sema::StableShapeRequest::Implementation((**owner).clone()),
			) {
				Ok(nymph_sema::StableShapeFact::Implementation(shape)) => shape
					.member_slots
					.iter()
					.find(|slot| slot.member_id == definition)
					.map(|slot| slot.interface_member_id.clone()),
				_ => None,
			}
		}
		_ => Some(definition.clone()),
	};
	if selected_interface_member.as_ref() == protocol_slots.display.as_ref() {
		return Ok(nymph_sema::EmittedMemberName::new("$nymph$display"));
	}
	if selected_interface_member.as_ref() == protocol_slots.debug.as_ref() {
		return Ok(nymph_sema::EmittedMemberName::new("$nymph$debug"));
	}
	declaration_name(&definition)
		.map(nymph_sema::EmittedMemberName::new)
		.ok_or(nymph_sema::StableNameLookupError::MissingMember { definition })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NominalProtocolMemberSlots {
	display: Option<nymph_sema::DefinitionId>,
	debug: Option<nymph_sema::DefinitionId>,
}

#[salsa::tracked(returns(clone))]
fn nominal_protocol_member_slots(
	db: &dyn Db,
	key: ProjectKey<'_>,
) -> Arc<NominalProtocolMemberSlots> {
	let mut slots = NominalProtocolMemberSlots::default();
	for module in key.ambient_core_registry(db).modules(db).iter().copied() {
		let nymph_sema::ModuleEnvironment::Complete(interface) =
			&*ambient_core_environment(db, key.ambient_core_registry(db), module)
		else {
			continue;
		};
		for definition in &interface.exports {
			if definition.kind != nymph_sema::DefinitionShapeKind::Interface {
				continue;
			}
			let target = match definition.name.as_str() {
				"Display" => (&mut slots.display, "display"),
				"Debug" => (&mut slots.debug, "debug"),
				_ => continue,
			};
			*target.0 = definition
				.members
				.iter()
				.find(|member| member.name == target.1)
				.map(|member| member.id.clone());
		}
	}
	Arc::new(slots)
}

#[salsa::tracked(returns(clone))]
pub(crate) fn module_specifier<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: nymph_sema::ModuleIdentity,
) -> Result<nymph_sema::CanonicalModuleSpecifier, nymph_sema::StableNameLookupError> {
	let owner = runtime_owner(
		db,
		key,
		&nymph_sema::DefinitionId::new(
			module.clone(),
			nymph_sema::DeclarationKey::top_level(nymph_sema::DeclarationCategory::Namespace, "<module>"),
		),
	)
	.map_err(|_| nymph_sema::StableNameLookupError::MissingModule {
		module: module.clone(),
	})?;
	Ok(match owner.domain(db) {
		SemanticModuleDomain::Project => nymph_sema::CanonicalModuleSpecifier::Project(module.path),
		SemanticModuleDomain::ImportableStd => {
			nymph_sema::CanonicalModuleSpecifier::Importable(module.path)
		}
		SemanticModuleDomain::AmbientCore => nymph_sema::CanonicalModuleSpecifier::CompilerRuntime(
			format!("@nymph/runtime/{}", module.path).into(),
		),
	})
}

struct CompilerStableContext<'db> {
	db: &'db dyn Db,
	key: ProjectKey<'db>,
}

impl nymph_sema::RuntimeDefinitionLookup for CompilerStableContext<'_> {
	fn runtime_definition(
		&self,
		definition: &nymph_sema::DefinitionId,
	) -> Result<Arc<nymph_sema::RuntimeDefinition>, nymph_sema::RuntimeDefinitionLookupError> {
		runtime_definition(self.db, self.key, definition.clone()).map_err(|error| match error {
			super::session::RuntimeDefinitionError::Recovered => {
				nymph_sema::RuntimeDefinitionLookupError::Recovered {
					definition: definition.clone(),
				}
			}
			super::session::RuntimeDefinitionError::DefinitionNotFound
			| super::session::RuntimeDefinitionError::OwnerNotFound => {
				nymph_sema::RuntimeDefinitionLookupError::Missing {
					definition: definition.clone(),
				}
			}
			other => nymph_sema::RuntimeDefinitionLookupError::Unavailable {
				definition: definition.clone(),
				reason: format!("{other:?}").into(),
			},
		})
	}
}

impl nymph_sema::StableShapeLookup for CompilerStableContext<'_> {
	fn stable_shape(
		&self,
		request: &nymph_sema::StableShapeRequest,
	) -> Result<nymph_sema::StableShapeFact, nymph_sema::StableShapeLookupError> {
		stable_shape(self.db, self.key, request.clone())
	}
}

impl nymph_sema::StableNameLookup for CompilerStableContext<'_> {
	fn binding_name(
		&self,
		definition: &nymph_sema::DefinitionId,
	) -> Result<nymph_sema::EmittedBindingName, nymph_sema::StableNameLookupError> {
		binding_name(self.db, self.key, definition.clone())
	}
	fn member_name(
		&self,
		definition: &nymph_sema::DefinitionId,
	) -> Result<nymph_sema::EmittedMemberName, nymph_sema::StableNameLookupError> {
		member_name(self.db, self.key, definition.clone())
	}
	fn module_specifier(
		&self,
		module: &nymph_sema::ModuleIdentity,
	) -> Result<nymph_sema::CanonicalModuleSpecifier, nymph_sema::StableNameLookupError> {
		module_specifier(self.db, self.key, module.clone())
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn lower_runtime_definition<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<Arc<nymph_sema::LoweredRuntimeDefinition>, nymph_sema::StableLoweringError> {
	#[cfg(feature = "test-support")]
	db.runtime_query_will_execute("lower_runtime_definition", &definition);
	let artifact = runtime_definition(db, key, definition.clone()).map_err(|error| match error {
		super::session::RuntimeDefinitionError::Recovered => {
			nymph_sema::RuntimeDefinitionLookupError::Recovered {
				definition: definition.clone(),
			}
		}
		super::session::RuntimeDefinitionError::DefinitionNotFound
		| super::session::RuntimeDefinitionError::OwnerNotFound => {
			nymph_sema::RuntimeDefinitionLookupError::Missing {
				definition: definition.clone(),
			}
		}
		other => nymph_sema::RuntimeDefinitionLookupError::Unavailable {
			definition: definition.clone(),
			reason: format!("{other:?}").into(),
		},
	})?;
	let context = CompilerStableContext { db, key };
	nymph_sema::lower_runtime_definition(&context, artifact).map(Arc::new)
}

#[salsa::tracked(returns(clone))]
pub(crate) fn lower_interface_module<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Result<Arc<nymph_sema::StableHirModule>, nymph_sema::StableModuleAssemblyError> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("lower_interface_module", module);
	let graph = project_graph(db, key);
	let mut environments = vec![module];
	environments.extend(graph.semantic_closure(module).iter().copied());
	for reachable in environments {
		let environment = match reachable {
			SemanticModuleInput::Builtin(input)
				if input.key(db).domain == BuiltinModuleDomain::AmbientCore =>
			{
				ambient_core_environment(db, key.ambient_core_registry(db), input)
			}
			_ => interface_module_environment(db, key, reachable),
		};
		if matches!(
			environment.as_ref(),
			nymph_sema::ModuleEnvironment::Recovered(_)
		) {
			return Err(
				nymph_sema::StableModuleAssemblyError::RecoveredEnvironment {
					module: reachable.identity(db),
				},
			);
		}
	}

	let own = runtime_definition_ids(db, key, module).map_err(|error| match error {
		super::session::RuntimeDefinitionError::Extraction(error) => {
			nymph_sema::StableModuleAssemblyError::RuntimeExtraction(error)
		}
		_ => nymph_sema::StableModuleAssemblyError::RecoveredEnvironment {
			module: module.identity(db),
		},
	})?;
	// Interface default bodies are canonical templates, not independently
	// emitted methods. Only a demanded materialized implementation may lower
	// and attach one to a concrete runtime owner.
	let mut queue = own
		.iter()
		.filter(|definition| {
			!matches!(
				&definition.key,
				nymph_sema::DeclarationKey::Member { owner, .. }
					if matches!(
						owner.key,
						nymph_sema::DeclarationKey::TopLevel {
							category: nymph_sema::DeclarationCategory::Interface,
							..
						}
					)
			)
		})
		.cloned()
		.collect::<std::collections::VecDeque<_>>();
	let mut seen = std::collections::HashSet::new();
	let mut lowered = Vec::new();
	while let Some(definition) = queue.pop_front() {
		if !seen.insert(definition.clone()) {
			continue;
		}
		let fragment =
			lower_runtime_definition(db, key, definition.clone()).map_err(|error| match &error {
				nymph_sema::StableLoweringError::Runtime(
					nymph_sema::RuntimeDefinitionLookupError::Missing { .. },
				) => nymph_sema::StableModuleAssemblyError::UnresolvedDemand {
					definition: definition.clone(),
				},
				nymph_sema::StableLoweringError::Runtime(
					nymph_sema::RuntimeDefinitionLookupError::Recovered { .. },
				) => nymph_sema::StableModuleAssemblyError::RecoveredDemand {
					definition: definition.clone(),
				},
				_ => error.into(),
			})?;
		if let nymph_sema::LoweredHirFragment::TopLevelExternal { abi, .. } = fragment.fragment()
			&& let Some((module, _)) = abi.linked()
			&& let Some(dependency) = crate::intrinsics::runtime_dependency(module)
		{
			let option_module = key
				.ambient_core_registry(db)
				.modules(db)
				.iter()
				.copied()
				.find(|module| module.key(db).path.as_ref() == dependency.ambient_module)
				.ok_or_else(|| nymph_sema::StableModuleAssemblyError::UnresolvedDemand {
					definition: definition.clone(),
				})?;
			let option = runtime_definition_ids(db, key, SemanticModuleInput::Builtin(option_module))
				.map_err(
					|_| nymph_sema::StableModuleAssemblyError::UnresolvedDemand {
						definition: definition.clone(),
					},
				)?
				.iter()
				.find(|candidate| {
					matches!(
						&candidate.key,
						nymph_sema::DeclarationKey::TopLevel {
							category: nymph_sema::DeclarationCategory::Enum,
							name,
							..
						} if name == dependency.enum_name
					)
				})
				.cloned()
				.ok_or_else(|| nymph_sema::StableModuleAssemblyError::UnresolvedDemand {
					definition: definition.clone(),
				})?;
			queue.push_back(option);
		}
		queue.extend(fragment.demands().iter().cloned());
		lowered.push(fragment.as_ref().clone());
	}

	let mut hir = nymph_hir::hir::HirModule {
		lets: vec![],
		funcs: vec![],
		classes: vec![],
		enums: vec![],
	};
	let mut shell_indices = std::collections::HashMap::new();
	for fragment in &lowered {
		if fragment.placement() != &nymph_sema::RuntimeAssemblyPlacement::Module(module.identity(db)) {
			continue;
		}
		match fragment.fragment() {
			nymph_sema::LoweredHirFragment::StructShell(class) => {
				shell_indices.insert(fragment.definition().clone(), (true, hir.classes.len()));
				hir.classes.push(class.clone());
			}
			nymph_sema::LoweredHirFragment::EnumShell(enum_) => {
				shell_indices.insert(fragment.definition().clone(), (false, hir.enums.len()));
				hir.enums.push(enum_.clone());
			}
			_ => {}
		}
	}
	let mut attachments = std::collections::HashSet::new();
	for fragment in &lowered {
		let placement_module = match fragment.placement() {
			nymph_sema::RuntimeAssemblyPlacement::Module(owner) => owner,
			nymph_sema::RuntimeAssemblyPlacement::Shell(owner) => &owner.module,
			nymph_sema::RuntimeAssemblyPlacement::Template => {
				return Err(nymph_sema::StableModuleAssemblyError::MismatchedPlacement {
					definition: fragment.definition().clone(),
					owner: fragment.definition().clone(),
				});
			}
		};
		if placement_module != &module.identity(db) {
			continue;
		}
		use nymph_sema::LoweredHirFragment as Fragment;
		match fragment.fragment() {
			Fragment::TopLevelFunction(func) => hir.funcs.push(func.clone()),
			Fragment::TopLevelValue(value) => hir.lets.push(value.clone()),
			Fragment::AttachedInstance { method, .. }
			| Fragment::AttachedMember { method, .. }
			| Fragment::MaterializedDefault { method, .. } => {
				attach_method(
					&mut hir,
					&shell_indices,
					&mut attachments,
					fragment.definition(),
					fragment.placement(),
					method,
					false,
				)?;
			}
			Fragment::AttachedStatic { method, .. } => {
				attach_method(
					&mut hir,
					&shell_indices,
					&mut attachments,
					fragment.definition(),
					fragment.placement(),
					method,
					true,
				)?;
			}
			Fragment::TopLevelExternal { .. } | Fragment::StructShell(_) | Fragment::EnumShell(_) => {}
		}
	}
	let imports = lowered
		.iter()
		.filter(|item| {
			item.definition().module != module.identity(db)
				&& !matches!(
					item.definition().module.origin,
					nymph_sema::ModuleOrigin::Compiler
				)
		})
		.map(|item| item.definition().clone())
		.collect();
	let virtual_runtime = lowered
		.iter()
		.filter_map(|item| {
			let owner = match item.placement() {
				nymph_sema::RuntimeAssemblyPlacement::Module(owner) => owner.clone(),
				nymph_sema::RuntimeAssemblyPlacement::Shell(shell) => shell.module.clone(),
				nymph_sema::RuntimeAssemblyPlacement::Template => return None,
			};
			matches!(owner.origin, nymph_sema::ModuleOrigin::Compiler).then_some((item, owner))
		})
		.map(|(item, owner)| nymph_sema::VirtualRuntimeFragment {
			owner,
			definition: item.definition().clone(),
			fragment: item.clone(),
		})
		.collect();
	Ok(Arc::new(nymph_sema::StableHirModule {
		module: module.identity(db),
		hir,
		own_definitions: own.to_vec(),
		fragments: lowered,
		imports,
		virtual_runtime,
	}))
}

fn attach_method(
	hir: &mut nymph_hir::hir::HirModule,
	shells: &std::collections::HashMap<nymph_sema::DefinitionId, (bool, usize)>,
	seen: &mut std::collections::HashSet<(nymph_sema::DefinitionId, ecow::EcoString, bool)>,
	definition: &nymph_sema::DefinitionId,
	placement: &nymph_sema::RuntimeAssemblyPlacement,
	method: &nymph_hir::hir::HirMethod,
	static_: bool,
) -> Result<(), nymph_sema::StableModuleAssemblyError> {
	let nymph_sema::RuntimeAssemblyPlacement::Shell(owner) = placement else {
		return Err(nymph_sema::StableModuleAssemblyError::MismatchedPlacement {
			definition: definition.clone(),
			owner: definition.clone(),
		});
	};
	let shell = shells
		.get(owner)
		.copied()
		.map(|shell| (owner.clone(), shell))
		.ok_or_else(
			|| nymph_sema::StableModuleAssemblyError::MissingOwnerShell {
				owner: owner.clone(),
			},
		)?;
	if !seen.insert((shell.0.clone(), method.name.clone(), static_)) {
		return Err(nymph_sema::StableModuleAssemblyError::DuplicateAttachment {
			owner: shell.0,
			name: method.name.clone(),
		});
	}
	let list = if shell.1.0 {
		if static_ {
			&mut hir.classes[shell.1.1].statics
		} else {
			&mut hir.classes[shell.1.1].methods
		}
	} else if static_ {
		&mut hir.enums[shell.1.1].statics
	} else {
		&mut hir.enums[shell.1.1].methods
	};
	let _ = definition;
	list.push(method.clone());
	Ok(())
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
	let resolved = resolved_module_imports(db, key, module);
	bindings.extend(
		resolved
			.bindings
			.iter()
			.map(|(name, binding)| (name.clone(), binding.clone())),
	);
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
	let private_accesses = result
		.analysis
		.annotations
		.unresolved_qualified_accesses()
		.iter()
		.filter_map(|access| {
			let dependency = graph
				.semantic_closure(module)
				.iter()
				.copied()
				.find(|dependency| dependency.identity(db) == access.module)?;
			interface_module_lexical_private_definitions(db, key, dependency)
				.iter()
				.any(|(name, _)| *name == access.member)
				.then_some(access)
		})
		.collect::<Vec<_>>();
	let mut diagnostics = resolved.diagnostics.iter().cloned().collect::<Vec<_>>();
	diagnostics.extend(
		result
			.diagnostics
			.iter()
			.filter(|diag| {
				!private_accesses.iter().any(|access| {
					is_missing_diagnostic_for_qualified_access(diag, access.span, access.member.as_str())
				})
			})
			.cloned()
			.map(|diag| ProjectDiagnostic {
				module: module.display_key(db),
				diag,
			})
			.collect::<Vec<_>>(),
	);
	for access in private_accesses {
		diagnostics.push(ProjectDiagnostic {
			module: module.display_key(db),
			diag: Diagnostic::error(
				"IMPORT-PRIVATE-NAME".into(),
				"private imported namespace member cannot be accessed".to_string(),
				access.span,
			),
		});
	}
	for (target, span) in result.analysis.checked.local_inherent_owners() {
		if target.module != module.identity(db) {
			diagnostics.push(ProjectDiagnostic {
				module: module.display_key(db),
				diag: Diagnostic::error(
					"INHERENT-IMPL-OWNER".into(),
					format!(
						"inherent impl must be declared in its owning module `{}`",
						target.module.path
					),
					span,
				),
			});
		}
	}
	Arc::new(super::session::ModuleAnalysis {
		semantic: result.analysis,
		diagnostics: super::session::ProjectDiagnostics(diagnostics.into()),
	})
}

#[salsa::tracked(returns(clone))]
fn interface_declared_headers<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<nymph_sema::DeclaredHeaders> {
	let own = nymph_sema::declared_headers(module.identity(db), &module.parsed(db).tree);
	let mut checked_definitions = own.checked_definitions.clone();
	checked_definitions.extend(
		resolved_module_imports(db, key, module)
			.bindings
			.iter()
			.filter_map(|(name, binding)| match binding {
				nymph_sema::ResolvedImportBinding::Definition(definition) => {
					Some((name.clone(), definition.clone()))
				}
				_ => None,
			})
			.collect::<Vec<_>>(),
	);
	if key.ambient_prelude(db) {
		let registry = key.ambient_core_registry(db);
		for root in registry.modules(db).iter().copied() {
			checked_definitions.extend(
				ambient_core_interface(db, registry, root)
					.exports
					.iter()
					.map(|definition| (definition.name.clone(), definition.id.clone())),
			);
		}
	}
	Arc::new(own.with_checked_definitions(checked_definitions))
}

fn is_missing_diagnostic_for_qualified_access(
	diagnostic: &Diagnostic,
	span: Span,
	member: &str,
) -> bool {
	diagnostic.code == "2022"
		&& diagnostic.span == span
		&& diagnostic
			.message
			.starts_with(&format!("no field `{member}` on `"))
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
	let headers = interface_declared_headers(db, key, module);
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
	let headers = interface_declared_headers(db, key, module);
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
		let mut diagnostics = graph.diagnostics.iter().cloned().collect::<Vec<_>>();
		// A name-preserving project is the standalone facade. Preserve its
		// long-standing recovery contract by checking the parser's recovered AST
		// after reporting parse diagnostics, rather than aborting at graph build.
		if key.preserve_names(db)
			&& key.project_input(db).project(db).as_str() == super::FACADE_PROJECT
			&& let Some(entry) = key
				.project_input(db)
				.active_modules(db)
				.iter()
				.find(|module| module.path(db) == key.entry(db))
		{
			diagnostics.extend(
				interface_module_diagnostics(db, key, SemanticModuleInput::Project(*entry))
					.iter()
					.cloned(),
			);
		}
		return super::session::ProjectDiagnostics(diagnostics.into());
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
				.map(|module| parse_builtin(self.db, module))
				.unwrap_or_else(|| parse(self.db, project.unwrap()));
			let mut ok = true;
			for diag in parsed.diagnostics.iter().filter(|diag| diag.is_error()) {
				self.diagnostic(path, diag.clone());
				ok = false;
			}
			let imports = builtin
				.map(|module| builtin_direct_imports(self.db, module))
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

	use nymph_ast::Span;
	use nymph_diagnostics::Diagnostic;
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

	#[test]
	fn private_access_conversion_preserves_unrelated_diagnostic_at_same_span() {
		let span = Span::new(10, 16);
		let missing_member = Diagnostic::error("2022".into(), "no field `helper` on `math`", span);
		let unrelated = Diagnostic::error("2999".into(), "unrelated", span);

		assert!(is_missing_diagnostic_for_qualified_access(
			&missing_member,
			span,
			"helper"
		));
		assert!(!is_missing_diagnostic_for_qualified_access(
			&unrelated, span, "helper"
		));
	}

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
	fn builtin_parse_uses_the_legacy_module_path() {
		let (db, key) = fixture(&[("main", "")], &[("custom", "public let answer = 42")]);
		let builtin = key.builtin_registry(&db).modules(&db)[0];
		assert_eq!(
			parse_builtin(&db, builtin).tree.path.as_str(),
			"std::custom.nym"
		);
	}
}
