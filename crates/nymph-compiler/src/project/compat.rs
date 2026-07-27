//! Owned compatibility extraction of the legacy bind/rewrite phases.

use std::{cell::RefCell, sync::Arc};

use ecow::EcoString;
use nymph_ast::{
	Span,
	decl::{Declaration, FuncKind, ImplMember, Module, Visibility},
	ty::Type,
};
use nymph_diagnostics::{Diagnostic, Label};
use nymph_hir::hir::{HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirMethod, HirModule};
use nymph_sema::{EntryMode, LoweredHir, RuntimeOwner};
use rustc_hash::{FxHashMap, FxHashSet};

use super::metrics::{CompilerPhase, record_phase};
use super::{
	CompiledProject, ProjectDiagnostic, bundle,
	queries::{self, Db},
	rewrite::{DeclaredName, NsInfo, RewriteCtx, declared_names, rewrite_module},
	session::{BuiltinModuleDomain, ModuleAnalysis, ModuleInput, ProjectKey, SemanticModuleInput},
};

pub(crate) type CompatModuleInput = SemanticModuleInput;

#[derive(Clone)]
pub(crate) struct CompatModuleAnalysis {
	pub(crate) analysis: Arc<ModuleAnalysis>,
	pub(crate) diagnostics: Arc<[ProjectDiagnostic]>,
	pub(crate) own_module: Arc<Module>,
}

#[derive(Clone, Debug)]
struct ModuleSymbols {
	renames: FxHashMap<EcoString, EcoString>,
	namespaces: FxHashMap<EcoString, NsInfo>,
	diagnostics: Arc<[ProjectDiagnostic]>,
}

#[derive(Clone, Debug)]
pub(crate) struct CompatibilitySymbolMap {
	order: Arc<[CompatModuleInput]>,
	#[allow(dead_code)]
	handles: FxHashMap<String, CompatModuleInput>,
	#[allow(dead_code)]
	tags: FxHashMap<String, usize>,
	declared: Arc<FxHashMap<String, Vec<DeclaredName>>>,
	modules: FxHashMap<String, ModuleSymbols>,
}

#[derive(Clone, Debug)]
pub(crate) struct CompatRewrittenModule {
	#[allow(dead_code)]
	pub module: Arc<Module>,
	#[allow(dead_code)]
	pub diagnostics: Arc<[ProjectDiagnostic]>,
}
#[derive(Clone)]
pub(crate) struct CompatLoweredModule {
	#[allow(dead_code)]
	pub module: CompatModuleInput,
	#[allow(dead_code)]
	pub lowered: Arc<LoweredHir>,
}
#[derive(Clone, Debug)]
pub(crate) struct CompatEmittedProject {
	pub(crate) module_sources: Result<FxHashMap<String, String>, Vec<ProjectDiagnostic>>,
	pub(crate) entry_tag: usize,
}

#[derive(Clone)]
pub(crate) enum CompatCompiledProject {
	Compiled(Arc<CompiledProject>),
	Diagnostics(Arc<[ProjectDiagnostic]>),
}

fn handles(db: &dyn Db, key: ProjectKey<'_>) -> FxHashMap<String, CompatModuleInput> {
	let mut out = FxHashMap::default();
	for module in key.project_input(db).active_modules(db).iter().copied() {
		out.insert(
			module.path(db).to_string(),
			CompatModuleInput::Project(module),
		);
	}
	for module in key.builtin_registry(db).modules(db).iter().copied() {
		out.insert(
			format!("std::{}", module.key(db).path),
			CompatModuleInput::Builtin(module),
		);
	}
	out
}

fn visit_order(
	db: &dyn Db,
	module: CompatModuleInput,
	all: &FxHashMap<String, CompatModuleInput>,
	seen: &mut FxHashSet<String>,
	out: &mut Vec<CompatModuleInput>,
) {
	let module_key = module.display_key(db);
	if !seen.insert(module_key) {
		return;
	}
	for import in module.imports(db).iter() {
		if let Ok(target) = &import.target
			&& let Some(child) = all.get(target)
		{
			visit_order(db, *child, all, seen, out);
		}
	}
	out.push(module);
}

#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn compat_symbol_map<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
) -> Arc<CompatibilitySymbolMap> {
	let graph = queries::project_graph(db, key);
	let all = handles(db, key);
	let mut order = Vec::new();
	if graph.diagnostics.is_empty() {
		if let Some(entry) = all.get(key.entry(db).as_str()) {
			visit_order(db, *entry, &all, &mut FxHashSet::default(), &mut order);
		}
	}
	let tags: FxHashMap<_, _> = order
		.iter()
		.enumerate()
		.map(|(tag, module)| (module.display_key(db), tag))
		.collect();
	let declared: Arc<FxHashMap<_, _>> = Arc::new(
		order
			.iter()
			.map(|module| {
				(
					module.display_key(db),
					declared_names(&module.parsed(db).tree),
				)
			})
			.collect(),
	);
	let mut modules = FxHashMap::default();
	let mut attachments: FxHashMap<(String, EcoString, EcoString), Span> = FxHashMap::default();
	for module in &order {
		let module_key = module.display_key(db);
		let tree = &module.parsed(db).tree;
		let own_tag = tags[&module_key];
		let own_names: FxHashSet<_> = declared[&module_key]
			.iter()
			.map(|name| name.name.clone())
			.collect();
		let owned_types: FxHashSet<_> = tree
			.members
			.iter()
			.filter_map(|decl| match decl {
				Declaration::Struct { name, .. } | Declaration::Enum { name, .. } => Some(name.0.clone()),
				_ => None,
			})
			.collect();
		let mut diagnostics = Vec::new();
		for decl in &tree.members {
			let Declaration::Impl { type_, members, .. } = decl else {
				continue;
			};
			let Type::Reference { name, .. } = &type_.0 else {
				continue;
			};
			if !owned_types.contains(&name.0) {
				diagnostics.push(ProjectDiagnostic { module: module_key.clone(), diag: Diagnostic::error("INHERENT-IMPL-OWNER".into(), format!("inherent impl for `{}` must be declared in the module that owns the type; extension attachments are not allowed", name.0), name.1) });
			}
			let owner = owned_types
				.contains(&name.0)
				.then(|| module_key.clone())
				.or_else(|| {
					module.imports(db).iter().find_map(|import| {
						import
							.with_idents
							.iter()
							.any(|(imported, alias)| alias.as_ref().unwrap_or(imported).0 == name.0)
							.then(|| import.target.as_ref().ok().cloned())
							.flatten()
					})
				});
			let Some(owner) = owner else { continue };
			for member in members {
				let ImplMember::Func { meta, .. } = &member.0 else {
					continue;
				};
				if meta.kind != FuncKind::Namespace {
					continue;
				}
				if let Some(previous) = attachments.insert(
					(owner.clone(), name.0.clone(), meta.name.0.clone()),
					meta.name.1,
				) {
					diagnostics.push(ProjectDiagnostic {
						module: module_key.clone(),
						diag: Diagnostic::error(
							"2045".into(),
							format!(
								"`{}` is defined more than once on `{}`",
								meta.name.0, name.0
							),
							meta.name.1,
						)
						.with_label(Label::new(previous, "previously defined here")),
					});
				}
			}
		}
		let is_entry = key.mode(db) == EntryMode::Entry && module_key == key.entry(db).as_str();
		let mut renames: FxHashMap<_, _> = declared[&module_key]
			.iter()
			.filter(|name| {
				!((key.preserve_names(db) && module_key == key.entry(db).as_str())
					|| (is_entry && name.name == "main"))
			})
			.map(|name| {
				(
					name.name.clone(),
					format!("$m{own_tag}${}", name.name).into(),
				)
			})
			.collect();
		let mut namespaces = FxHashMap::default();
		for import in module.imports(db).iter() {
			let Ok(target_key) = &import.target else {
				continue;
			};
			let target_tag = tags[target_key];
			let ns = &import.namespace;
			if own_names.contains(&ns.0) || namespaces.contains_key(&ns.0) || renames.contains_key(&ns.0)
			{
				diagnostics.push(ProjectDiagnostic {
					module: module_key.clone(),
					diag: Diagnostic::error(
						"IMPORT-NAME-COLLISION".into(),
						format!(
							"import namespace `{}` collides with another name in this module",
							ns.0
						),
						ns.1,
					),
				});
			} else {
				namespaces.insert(
					ns.0.clone(),
					NsInfo {
						target_key: target_key.clone(),
						target_tag,
					},
				);
			}
			for (name, alias) in &import.with_idents {
				let effective = alias.clone().unwrap_or_else(|| name.clone());
				match declared[target_key].iter().find(|decl| decl.name == name.0) {
					None => diagnostics.push(ProjectDiagnostic {
						module: module_key.clone(),
						diag: Diagnostic::error(
							"IMPORT-UNRESOLVED-NAME".into(),
							format!("module `{target_key}` has no member `{}`", name.0),
							name.1,
						),
					}),
					Some(decl) if decl.vis == Visibility::Private => diagnostics.push(ProjectDiagnostic {
						module: module_key.clone(),
						diag: Diagnostic::error(
							"IMPORT-PRIVATE-NAME".into(),
							format!(
								"`{}` is private to module `{target_key}` and cannot be imported",
								name.0
							),
							name.1,
						),
					}),
					Some(_)
						if own_names.contains(&effective.0)
							|| renames.contains_key(&effective.0)
							|| namespaces.contains_key(&effective.0) =>
					{
						diagnostics.push(ProjectDiagnostic {
							module: module_key.clone(),
							diag: Diagnostic::error(
								"IMPORT-NAME-COLLISION".into(),
								format!(
									"imported name `{}` collides with another name in this module",
									effective.0
								),
								effective.1,
							),
						})
					}
					Some(_) => {
						renames.insert(
							effective.0.clone(),
							format!("$m{target_tag}${}", name.0).into(),
						);
					}
				}
			}
		}
		modules.insert(
			module_key,
			ModuleSymbols {
				renames,
				namespaces,
				diagnostics: diagnostics.into(),
			},
		);
	}
	Arc::new(CompatibilitySymbolMap {
		order: order.into(),
		handles: all,
		tags,
		declared,
		modules,
	})
}

#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn compat_rewritten_module<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: CompatModuleInput,
) -> Arc<CompatRewrittenModule> {
	let symbols = compat_symbol_map(db, key);
	let module_key = module.display_key(db);
	let binding = &symbols.modules[&module_key];
	let ctx = RewriteCtx {
		renames: binding.renames.clone(),
		namespaces: binding.namespaces.clone(),
		declared: &symbols.declared,
		diags: RefCell::new(Vec::new()),
	};
	let rewritten = rewrite_module(&module.parsed(db).tree, &ctx);
	let mut diagnostics = binding.diagnostics.to_vec();
	diagnostics.extend(
		ctx
			.diags
			.into_inner()
			.into_iter()
			.map(|diag| ProjectDiagnostic {
				module: module_key.clone(),
				diag,
			}),
	);
	Arc::new(CompatRewrittenModule {
		module: Arc::new(rewritten),
		diagnostics: diagnostics.into(),
	})
}

fn collect_transitive_dependencies(
	db: &dyn Db,
	module: CompatModuleInput,
	handles: &FxHashMap<String, CompatModuleInput>,
	seen: &mut FxHashSet<String>,
	out: &mut Vec<CompatModuleInput>,
) {
	for import in module.imports(db).iter() {
		let Ok(target) = &import.target else { continue };
		if seen.insert(target.clone()) {
			let dependency = handles[target];
			out.push(dependency);
			collect_transitive_dependencies(db, dependency, handles, seen, out);
		}
	}
}

fn transitive_dependencies(
	db: &dyn Db,
	key: ProjectKey<'_>,
	module: CompatModuleInput,
) -> Vec<CompatModuleInput> {
	let symbols = compat_symbol_map(db, key);
	let mut dependencies = Vec::new();
	collect_transitive_dependencies(
		db,
		module,
		&symbols.handles,
		&mut FxHashSet::default(),
		&mut dependencies,
	);
	dependencies
}

fn prelude(db: &dyn Db, key: ProjectKey<'_>, dependencies: &[CompatModuleInput]) -> Vec<Module> {
	key
		.ambient_prelude(db)
		.then(crate::prelude::core_prelude)
		.into_iter()
		.flatten()
		.cloned()
		.chain(dependencies.iter().map(|module| {
			compat_rewritten_module(db, key, *module)
				.module
				.as_ref()
				.clone()
		}))
		.collect()
}

pub(crate) fn compat_project_module_is_reachable(
	db: &dyn Db,
	key: ProjectKey<'_>,
	module: ModuleInput,
) -> bool {
	compat_symbol_map(db, key)
		.order
		.contains(&CompatModuleInput::Project(module))
}

#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn compat_precheck_diagnostics<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
) -> Arc<[ProjectDiagnostic]> {
	let graph = queries::project_graph(db, key);
	if !graph.diagnostics.is_empty() {
		return graph.diagnostics.clone();
	}
	let symbols = compat_symbol_map(db, key);
	let mut diagnostics = Vec::new();
	for module in symbols.order.iter() {
		diagnostics.extend(
			compat_rewritten_module(db, key, *module)
				.diagnostics
				.iter()
				.cloned(),
		);
	}
	diagnostics.into()
}

#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn compat_checked_project<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
) -> Arc<[ProjectDiagnostic]> {
	let diagnostics = compat_precheck_diagnostics(db, key);
	if !diagnostics.is_empty() {
		return diagnostics;
	}
	let mut diagnostics = Vec::new();
	for module in compat_symbol_map(db, key).order.iter().copied() {
		diagnostics.extend(compat_module_diagnostics(db, key, module).iter().cloned());
	}
	diagnostics.into()
}

#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn compat_module_analysis<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: CompatModuleInput,
) -> Arc<CompatModuleAnalysis> {
	let rewritten = compat_rewritten_module(db, key, module);
	let dependencies = transitive_dependencies(db, key, module);
	let prelude = prelude(db, key, &dependencies);
	record_phase(CompilerPhase::Check);
	let paired =
		if key.mode(db) == EntryMode::Entry && module.display_key(db) == key.entry(db).as_str() {
			nymph_sema::check_module_entry_with_prelude_and_module(&rewritten.module, &prelude)
		} else {
			nymph_sema::check_module_with_prelude_and_module(&rewritten.module, &prelude)
		};
	let diagnostics: Arc<[ProjectDiagnostic]> = paired
		.checked
		.diags
		.iter()
		.cloned()
		.map(|diag| ProjectDiagnostic {
			module: module.display_key(db),
			diag,
		})
		.collect::<Vec<_>>()
		.into();
	let checked = Arc::new(paired.checked);
	let semantic = Arc::new(nymph_sema::SemanticAnalysis {
		module: Arc::new(paired.module),
		checked: Arc::new(checked.facts.clone()),
		annotations: Arc::new(nymph_sema::ModuleAnnotations::from(
			checked.facts.annotations.clone(),
		)),
	});
	Arc::new(CompatModuleAnalysis {
		analysis: Arc::new(ModuleAnalysis {
			semantic,
			diagnostics: super::session::ProjectDiagnostics(diagnostics.clone()),
		}),
		diagnostics,
		own_module: rewritten.module.clone(),
	})
}

fn compat_module_identity(db: &dyn Db, module: CompatModuleInput) -> nymph_sema::ModuleIdentity {
	match module {
		CompatModuleInput::Project(input) => nymph_sema::ModuleIdentity {
			origin: nymph_sema::ModuleOrigin::Project(input.project(db).as_str().into()),
			project: input.project(db).as_str().into(),
			path: input.path(db).as_str().into(),
		},
		CompatModuleInput::Builtin(input) => nymph_sema::ModuleIdentity {
			origin: match input.key(db).domain {
				BuiltinModuleDomain::ImportableStd => nymph_sema::ModuleOrigin::ImportableStd,
				BuiltinModuleDomain::AmbientCore => nymph_sema::ModuleOrigin::Compiler,
			},
			project: "compiler".into(),
			path: input.key(db).path.as_ref().into(),
		},
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn compat_declared_headers<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: CompatModuleInput,
) -> Arc<nymph_sema::DeclaredHeaders> {
	let symbols = compat_symbol_map(db, key);
	let own =
		nymph_sema::declared_headers(compat_module_identity(db, module), &module.parsed(db).tree);
	let mut checked_definitions = Vec::new();
	if key.ambient_prelude(db) {
		for builtin in crate::prelude::core_prelude() {
			let identity = nymph_sema::ModuleIdentity {
				origin: nymph_sema::ModuleOrigin::Compiler,
				project: "compiler".into(),
				path: builtin.path.clone(),
			};
			checked_definitions.extend(nymph_sema::declared_headers(identity, builtin).definitions);
		}
	}
	for owner in symbols.order.iter().copied() {
		let owner_headers =
			nymph_sema::declared_headers(compat_module_identity(db, owner), &owner.parsed(db).tree);
		let owner_symbols = &symbols.modules[&owner.display_key(db)];
		for (source_name, id) in owner_headers.definitions {
			let checked_name = owner_symbols
				.renames
				.get(&source_name)
				.cloned()
				.unwrap_or(source_name);
			checked_definitions.push((checked_name, id));
		}
	}
	Arc::new(own.with_checked_definitions(checked_definitions))
}

#[salsa::tracked(returns(clone))]
pub(crate) fn compat_module_environment<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: CompatModuleInput,
) -> Arc<nymph_sema::ModuleEnvironment> {
	let analysis = compat_module_analysis(db, key, module);
	let checked = nymph_sema::Checked {
		diags: analysis
			.diagnostics
			.iter()
			.map(|item| item.diag.clone())
			.collect(),
		facts: analysis.analysis.semantic.checked.as_ref().clone(),
	};
	let facts = nymph_sema::ExtractionFactSelection::current_module(&analysis.own_module, &checked);
	Arc::new(nymph_sema::recover_module_environment_with_facts(
		compat_module_identity(db, module),
		&analysis.own_module,
		&checked,
		&compat_declared_headers(db, key, module),
		&facts,
	))
}

#[salsa::tracked(returns(clone))]
pub(crate) fn compat_module_interface<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: CompatModuleInput,
) -> Option<Arc<nymph_sema::ModuleInterface>> {
	match &*compat_module_environment(db, key, module) {
		nymph_sema::ModuleEnvironment::Complete(interface) => Some(Arc::new(interface.clone())),
		nymph_sema::ModuleEnvironment::Recovered(_) => None,
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn compat_module_diagnostics<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: CompatModuleInput,
) -> Arc<[ProjectDiagnostic]> {
	let analysis = compat_module_analysis(db, key, module);
	let mut diagnostics = analysis.diagnostics.to_vec();
	if diagnostics.is_empty() {
		let checked = nymph_sema::Checked {
			diags: Vec::new(),
			facts: analysis.analysis.semantic.checked.as_ref().clone(),
		};
		let headers = compat_declared_headers(db, key, module);
		let facts = nymph_sema::ExtractionFactSelection::current_module(&analysis.own_module, &checked);
		if let Err(error) = nymph_sema::extract_module_interface_with_facts(
			compat_module_identity(db, module),
			&analysis.own_module,
			&checked,
			&headers,
			&facts,
		) {
			diagnostics.push(ProjectDiagnostic {
				module: module.display_key(db),
				diag: Diagnostic::error(
					"INTERNAL-INTERFACE-CONVERSION".into(),
					format!("internal interface conversion failed: {error:?}"),
					Span::new(0, 0),
				),
			});
		}
	}
	diagnostics.into()
}

#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn compat_lowered_module<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: CompatModuleInput,
) -> Arc<CompatLoweredModule> {
	let analysis = compat_module_analysis(db, key, module);
	let dependencies = transitive_dependencies(db, key, module);
	let prelude = prelude(db, key, &dependencies);
	let owners = crate::prelude::core_runtime_module_owners()
		.map(RuntimeOwner::Compiler)
		.chain(
			dependencies
				.iter()
				.map(|dependency| RuntimeOwner::Project(dependency.display_key(db).into())),
		)
		.collect::<Vec<_>>();
	let lowered = nymph_sema::lower_hir_with_prelude_runtime_and_deps_with_owners(
		&analysis.own_module,
		&prelude,
		&owners,
		crate::prelude::core_prelude().len(),
		&nymph_sema::Checked {
			diags: Vec::new(),
			facts: analysis.analysis.semantic.checked.as_ref().clone(),
		},
	);
	record_phase(CompilerPhase::Lower);
	Arc::new(CompatLoweredModule {
		module,
		lowered: Arc::new(lowered),
	})
}
fn merge_canonical_enum(target: &mut HirEnum, incoming: HirEnum) {
	assert_eq!(target.name, incoming.name);
	assert_eq!(target.variants, incoming.variants);
	merge_canonical_methods(&mut target.methods, incoming.methods);
	merge_canonical_methods(&mut target.statics, incoming.statics);
}

fn merge_canonical_class(target: &mut HirClass, incoming: HirClass) {
	assert_eq!(target.name, incoming.name);
	assert_eq!(target.fields, incoming.fields);
	merge_canonical_methods(&mut target.methods, incoming.methods);
	merge_canonical_methods(&mut target.statics, incoming.statics);
}

fn merge_canonical_methods(target: &mut Vec<HirMethod>, incoming: Vec<HirMethod>) {
	for method in incoming {
		if !target.iter().any(|item| item.name == method.name) {
			target.push(method);
		}
	}
}

fn runtime_import_lines(imports: &[(String, Vec<String>)]) -> String {
	let mut out = String::new();
	for (specifier, names) in imports {
		out.push_str(&format!(
			"import {{ {} }} from \"{specifier}\";\n",
			names.join(", ")
		));
	}
	out
}

fn insert_runtime_module(
	sources: &mut FxHashMap<String, String>,
	key: String,
	source: String,
) -> Result<(), Vec<ProjectDiagnostic>> {
	if sources.contains_key(&key) {
		return Err(vec![ProjectDiagnostic {
			module: key.clone(),
			diag: Diagnostic::error(
				"PROJECT-RUNTIME-MODULE-COLLISION".into(),
				format!("project module `{key}` conflicts with a compiler runtime module"),
				Span::new(0, 0),
			),
		}]);
	}
	sources.insert(key, source);
	Ok(())
}

fn wrap_module_js(
	db: &dyn Db,
	project: ProjectKey<'_>,
	symbols: &CompatibilitySymbolMap,
	key: &str,
	body: &str,
	runtime_imports: &[(String, Vec<String>)],
) -> String {
	let own_tag = symbols.tags[key];
	let is_entry = project.mode(db) == EntryMode::Entry && key == project.entry(db).as_str();
	let preserve_names = project.preserve_names(db) && key == project.entry(db).as_str();
	let module = symbols.handles[key];
	let mut seen_deps = FxHashSet::default();
	let mut import_lines = Vec::new();
	for imp in module.imports(db).iter() {
		let Ok(target_key) = &imp.target else {
			continue;
		};
		if !seen_deps.insert(target_key.clone()) {
			continue;
		}
		let dep_tag = symbols.tags[target_key];
		let mut names: Vec<String> = symbols.declared[target_key]
			.iter()
			.filter(|d| d.vis != Visibility::Private && d.has_runtime_binding)
			.map(|d| format!("$m{dep_tag}${}", d.name))
			.collect();
		names.sort_unstable();
		if !names.is_empty() {
			import_lines.push(format!(
				"import {{ {} }} from \"{}\";",
				names.join(", "),
				target_key
			));
		}
	}
	let mut export_names: Vec<String> = symbols.declared[key]
		.iter()
		.filter(|d| d.has_runtime_binding && (preserve_names || d.vis != Visibility::Private))
		.map(|d| {
			if preserve_names || (is_entry && d.name == "main") {
				d.name.to_string()
			} else {
				format!("$m{own_tag}${}", d.name)
			}
		})
		.collect();
	export_names.sort_unstable();
	let mut out = String::new();
	let runtime_imports = runtime_imports
		.iter()
		.filter_map(|(module, names)| {
			let names = names
				.iter()
				.filter(|name| !body.contains(&format!("import {{ {name} }} from \"{module}\";")))
				.cloned()
				.collect::<Vec<_>>();
			(!names.is_empty()).then(|| (module.clone(), names))
		})
		.collect::<Vec<_>>();
	out.push_str(&runtime_import_lines(&runtime_imports));
	for line in &import_lines {
		out.push_str(line);
		out.push('\n');
	}
	if body.contains("[TAG]") && !body.contains("const TAG") {
		out.push_str("const TAG = Symbol.for(\"nymph.tag\");\n");
	}
	out.push_str(body);
	if !export_names.is_empty() {
		if !out.ends_with('\n') {
			out.push('\n');
		}
		out.push_str(&format!("export {{ {} }};\n", export_names.join(", ")));
	}
	out
}

#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn compat_emitted_module<'db>(
	db: &'db dyn Db,
	project: ProjectKey<'db>,
) -> Arc<CompatEmittedProject> {
	let diagnostics = compat_checked_project(db, project);
	if diagnostics
		.iter()
		.any(|diagnostic| diagnostic.diag.is_error())
	{
		return Arc::new(CompatEmittedProject {
			module_sources: Err(diagnostics.iter().cloned().collect()),
			entry_tag: 0,
		});
	}
	let symbols = compat_symbol_map(db, project);
	let mut lowered_modules = Vec::new();
	for module in symbols.order.iter() {
		let lowered = compat_lowered_module(db, project, *module);
		lowered_modules.push((module.display_key(db), lowered.lowered.as_ref().clone()));
	}
	let module_sources = (|| {
		let owners = crate::prelude::core_runtime_type_owners();
		let intrinsic_sources = crate::intrinsics::intrinsic_module_sources();
		let intrinsic_type_demands = crate::intrinsics::runtime_type_imports(intrinsic_sources.keys());
		let declaration_seeds = crate::prelude::core_runtime_declaration_seeds();
		let mut runtime_enums: FxHashMap<String, Vec<HirEnum>> = FxHashMap::default();
		let mut runtime_classes: FxHashMap<String, Vec<HirClass>> = FxHashMap::default();
		let mut runtime_funcs: FxHashMap<String, Vec<HirFunc>> = FxHashMap::default();
		let mut proven_project_runtime_owners = FxHashSet::default();
		for seed in &declaration_seeds.enums {
			if intrinsic_type_demands.contains(&seed.name) {
				runtime_enums
					.entry(owners[&seed.name].to_string())
					.or_default()
					.push(seed.clone());
			}
		}
		for seed in &declaration_seeds.classes {
			if intrinsic_type_demands.contains(&seed.name) {
				runtime_classes
					.entry(owners[&seed.name].to_string())
					.or_default()
					.push(seed.clone());
			}
		}
		for (_, lowered) in &mut lowered_modules {
			lowered
				.prelude_runtime
				.lets
				.append(&mut lowered.module.lets);
			lowered.module.lets = std::mem::take(&mut lowered.prelude_runtime.lets);
			for func in lowered.prelude_runtime.funcs.drain(..) {
				let owner = lowered
					.runtime_func_owners
					.get(&func.name)
					.unwrap_or_else(|| {
						panic!(
							"ambient runtime function `{}` has no canonical owner",
							func.name
						)
					});
				{
					if let nymph_sema::RuntimeOwner::Project(owner) = owner {
						proven_project_runtime_owners.insert(owner.to_string());
					}
					let funcs = runtime_funcs.entry(owner.key().to_string()).or_default();
					if let Some(existing) = funcs.iter().find(|item| item.name == func.name) {
						assert_eq!(
							existing, &func,
							"conflicting ambient runtime function `{}`",
							func.name
						);
					} else {
						funcs.push(func);
					}
				}
			}
			for class in lowered.prelude_runtime.classes.drain(..) {
				let owner = owners
					.get(&class.name)
					.unwrap_or_else(|| panic!("ambient class `{}` has no runtime owner", class.name));
				let classes = runtime_classes.entry((*owner).to_string()).or_default();
				if let Some(canonical) = classes.iter_mut().find(|item| item.name == class.name) {
					merge_canonical_class(canonical, class);
				} else {
					classes.push(class);
				}
			}
			for enum_ in lowered.prelude_runtime.enums.drain(..) {
				let owner = owners
					.get(&enum_.name)
					.unwrap_or_else(|| panic!("ambient enum `{}` has no runtime owner", enum_.name));
				let enums = runtime_enums.entry((*owner).to_string()).or_default();
				if let Some(canonical) = enums.iter_mut().find(|item| item.name == enum_.name) {
					merge_canonical_enum(canonical, enum_);
				} else {
					enums.push(enum_);
				}
			}
		}
		// External host snapshots are owned once by the assembled project, not by
		// whichever consumer happened to demand their ambient declarations.
		let mut identities = std::collections::BTreeSet::new();
		for (_, lowered) in &lowered_modules {
			for let_ in &lowered.module.lets {
				if let HirExpr::ExternValue {
					module,
					symbol,
					marshal,
				} = let_.value
				{
					identities.insert((module, symbol, marshal));
				}
			}
		}
		let canonical_names: std::collections::BTreeMap<_, _> = identities
			.iter()
			.enumerate()
			.map(|(index, identity)| (*identity, format!("$nymph_external_value${index}")))
			.collect();
		let mut external_imports: FxHashMap<String, Vec<String>> = FxHashMap::default();
		for (key, lowered) in &mut lowered_modules {
			for let_ in &mut lowered.module.lets {
				if let HirExpr::ExternValue {
					module,
					symbol,
					marshal,
				} = let_.value
				{
					let canonical = canonical_names[&(module, symbol, marshal)].clone();
					let_.value = HirExpr::Local(canonical.clone().into());
					external_imports
						.entry(key.clone())
						.or_default()
						.push(canonical);
				}
			}
		}
		let runtime_names: FxHashSet<_> = runtime_enums
			.values()
			.flat_map(|enums| enums.iter().map(|enum_| enum_.name.clone()))
			.chain(
				runtime_classes
					.values()
					.flat_map(|classes| classes.iter().map(|class| class.name.clone())),
			)
			.chain(
				runtime_funcs
					.values()
					.flat_map(|funcs| funcs.iter().map(|func| func.name.clone())),
			)
			.collect();
		let mut runtime_symbol_owners: FxHashMap<_, String> = owners
			.iter()
			.map(|(name, owner)| (name.clone(), (*owner).to_string()))
			.collect();
		for (owner, funcs) in &runtime_funcs {
			for func in funcs {
				if let Some(previous) = runtime_symbol_owners.insert(func.name.clone(), owner.clone()) {
					assert_eq!(
						previous,
						owner.as_str(),
						"conflicting canonical owners for `{}`",
						func.name
					);
				}
			}
		}
		let imports_for = |hir: &HirModule, own_owner: Option<&str>| {
			let mut imports: FxHashMap<String, Vec<String>> = FxHashMap::default();
			for name in hir.runtime_type_references() {
				if !runtime_names.contains(&name) {
					continue;
				}
				let owner = &runtime_symbol_owners[&name];
				if own_owner == Some(owner.as_str()) {
					continue;
				}
				imports
					.entry(owner.to_string())
					.or_default()
					.push(name.to_string());
			}
			let mut imports: Vec<_> = imports.into_iter().collect();
			for (_, names) in &mut imports {
				names.sort_unstable();
				names.dedup();
			}
			imports.sort_unstable_by(|a, b| a.0.cmp(&b.0));
			imports
		};

		// Demand-generated declarations whose canonical owner is already a real
		// project/dependency module belong to that HIR module. Emitting a second
		// virtual module under the same key would collide rather than establish
		// ownership.
		let mut merged_runtime_exports: FxHashMap<String, Vec<String>> = FxHashMap::default();
		for (key, lowered) in &mut lowered_modules {
			if !proven_project_runtime_owners.contains(key) {
				continue;
			}
			let funcs = runtime_funcs.remove(key).unwrap_or_default();
			let classes = runtime_classes.remove(key).unwrap_or_default();
			let enums = runtime_enums.remove(key).unwrap_or_default();
			let exports = merged_runtime_exports.entry(key.clone()).or_default();
			exports.extend(funcs.iter().map(|item| item.name.to_string()));
			exports.extend(classes.iter().map(|item| item.name.to_string()));
			exports.extend(enums.iter().map(|item| item.name.to_string()));
			for func in funcs {
				if let Some(existing) = lowered
					.module
					.funcs
					.iter()
					.find(|item| item.name == func.name)
				{
					assert_eq!(
						existing, &func,
						"conflicting canonical function `{}`",
						func.name
					);
				} else {
					lowered.module.funcs.push(func);
				}
			}
			for class in classes {
				if let Some(existing) = lowered
					.module
					.classes
					.iter_mut()
					.find(|item| item.name == class.name)
				{
					merge_canonical_class(existing, class);
				} else {
					lowered.module.classes.push(class);
				}
			}
			for enum_ in enums {
				if let Some(existing) = lowered
					.module
					.enums
					.iter_mut()
					.find(|item| item.name == enum_.name)
				{
					merge_canonical_enum(existing, enum_);
				} else {
					lowered.module.enums.push(enum_);
				}
			}
		}

		let mut module_sources: FxHashMap<String, String> = FxHashMap::default();
		for (key, lowered) in lowered_modules {
			record_phase(CompilerPhase::Emit);
			let mut imports = imports_for(&lowered.module, Some(&key));
			if let Some(mut names) = external_imports.remove(&key) {
				names.sort_unstable();
				names.dedup();
				imports.push(("std/nymph/external-values".to_string(), names));
			}
			let body = nymph_codegen::emit_for_project_module(&lowered.module, &key);
			let mut source = wrap_module_js(db, project, &symbols, &key, &body, &imports);
			if let Some(exports) = merged_runtime_exports.get_mut(&key)
				&& !exports.is_empty()
			{
				exports.sort_unstable();
				exports.dedup();
				source.push_str(&format!("export {{ {} }};\n", exports.join(", ")));
			}
			module_sources.insert(key.clone(), source);
		}
		if !canonical_names.is_empty() {
			let lets = canonical_names
				.iter()
				.map(|(&(module, symbol, marshal), name)| HirLet {
					name: name.clone().into(),
					mutable: false,
					value: HirExpr::ExternValue {
						module,
						symbol,
						marshal,
					},
				})
				.collect();
			let hir = HirModule {
				lets,
				funcs: Vec::new(),
				classes: Vec::new(),
				enums: Vec::new(),
			};
			let mut source = nymph_codegen::emit(&hir);
			source.push_str(&format!(
				"export {{ {} }};\n",
				canonical_names
					.values()
					.cloned()
					.collect::<Vec<_>>()
					.join(", ")
			));
			insert_runtime_module(
				&mut module_sources,
				"std/nymph/external-values".to_string(),
				source,
			)?;
		}
		let runtime_owners: FxHashSet<_> = runtime_enums
			.keys()
			.chain(runtime_classes.keys())
			.chain(runtime_funcs.keys())
			.cloned()
			.collect();
		for owner in runtime_owners.clone() {
			let mut enums = runtime_enums.remove(&owner).unwrap_or_default();
			let mut classes = runtime_classes.remove(&owner).unwrap_or_default();
			let mut funcs = runtime_funcs.remove(&owner).unwrap_or_default();
			enums.sort_unstable_by(|a, b| a.name.cmp(&b.name));
			classes.sort_unstable_by(|a, b| a.name.cmp(&b.name));
			funcs.sort_unstable_by(|a, b| a.name.cmp(&b.name));
			let hir = HirModule {
				lets: Vec::new(),
				funcs,
				classes,
				enums,
			};
			let imports = imports_for(&hir, Some(&owner));
			let mut source = runtime_import_lines(&imports);
			source.push_str(&nymph_codegen::emit_for_project_module(&hir, &owner));
			let names = hir
				.classes
				.iter()
				.map(|item| item.name.as_str())
				.chain(hir.enums.iter().map(|item| item.name.as_str()))
				.chain(hir.funcs.iter().map(|item| item.name.as_str()))
				.collect::<Vec<_>>()
				.join(", ");
			source.push_str(&format!("export {{ {names} }};\n"));
			insert_runtime_module(&mut module_sources, owner, source)?;
		}
		// Gap 3 (L0): inject one virtual intrinsic module per distinct
		// LINKED-external registry module (today, just
		// `"std/collections/list"`, seeded with `length`) — the stripped
		// `.ts` runtime source an emitted `import { length } from
		// "std/collections/list"` (see `nymph-codegen`'s `HirExpr::ExternCall`
		// emit arm) actually resolves against. Unconditional per compile (not
		// gated on whether any emitted module actually references it):
		// `VirtualFsPlugin::load` is only invoked on demand, so an unused
		// intrinsic costs one un-consulted map entry, and rolldown
		// tree-shakes it away regardless.
		for (key, source) in intrinsic_sources {
			assert_ne!(
				key, "std/option",
				"intrinsics must not replace the canonical Option module"
			);
			if module_sources.contains_key(&key)
				&& (runtime_owners.contains(&key) || proven_project_runtime_owners.contains(&key))
			{
				let backing = format!("{key}$intrinsics");
				insert_runtime_module(&mut module_sources, backing.clone(), source)?;
				let public = module_sources
					.get_mut(&key)
					.expect("canonical owner exists");
				public.push_str(&format!("export * from \"{backing}\";\n"));
			} else {
				insert_runtime_module(&mut module_sources, key, source)?;
			}
		}
		Ok(module_sources)
	})();
	Arc::new(CompatEmittedProject {
		module_sources,
		entry_tag: symbols.tags[project.entry(db).as_str()],
	})
}

#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn compat_compiled_project<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
) -> Arc<CompatCompiledProject> {
	let diagnostics = compat_checked_project(db, key);
	if diagnostics
		.iter()
		.any(|diagnostic| diagnostic.diag.is_error())
	{
		return Arc::new(CompatCompiledProject::Diagnostics(diagnostics));
	}
	let emitted = compat_emitted_module(db, key);
	record_phase(CompilerPhase::Bundle);
	let result = emitted
		.module_sources
		.clone()
		.and_then(|module_sources| {
			bundle::bundle(key.entry(db).as_str(), module_sources).map_err(|msg| {
				vec![ProjectDiagnostic {
					module: key.entry(db).to_string(),
					diag: Diagnostic::error(
						"BUNDLE-FAILED".into(),
						format!("bundling the project failed: {msg}"),
						Span::new(0, 0),
					),
				}]
			})
		})
		.map(|js| CompiledProject {
			js,
			entry_main: "main".to_string(),
			entry_tag: emitted.entry_tag,
		});
	Arc::new(match result {
		Ok(compiled) => CompatCompiledProject::Compiled(Arc::new(compiled)),
		Err(diagnostics) => CompatCompiledProject::Diagnostics(diagnostics.into()),
	})
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use super::*;
	use crate::project::Driver;
	use crate::project::session::BuiltinModuleInput;
	use crate::project::session::{
		BuiltinModuleDomain, BuiltinModuleKey, BuiltinRegistryInput, ModulePath, ProjectId,
		ProjectInput,
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

	fn fixture(
		files: &[(&str, &str)],
		builtins: &[(&str, &str)],
		project: &str,
		entry: &str,
		mode: EntryMode,
		preserve_names: bool,
	) -> (TestDb, ProjectKey<'static>) {
		let db = TestDb {
			storage: salsa::Storage::default(),
		};
		let id = ProjectId::new(project);
		let modules: Arc<[ModuleInput]> = files
			.iter()
			.map(|(path, source)| {
				ModuleInput::new(
					&db,
					id.clone(),
					ModulePath::new(path).unwrap(),
					Some(Arc::from(*source)),
				)
			})
			.collect::<Vec<_>>()
			.into();
		let builtin_modules: Arc<[BuiltinModuleInput]> = builtins
			.iter()
			.map(|(path, source)| {
				BuiltinModuleInput::new(
					&db,
					BuiltinModuleKey {
						domain: BuiltinModuleDomain::ImportableStd,
						path: Arc::from(*path),
					},
					Arc::from(*source),
				)
			})
			.collect::<Vec<_>>()
			.into();
		let input = ProjectInput::new(&db, id, modules);
		let registry = BuiltinRegistryInput::new(&db, builtin_modules);
		let ambient = crate::project::session::AmbientCoreRegistryInput::new(&db, Arc::new([]));
		// Test databases live for the duration of each test; Salsa keys do not
		// actually borrow the database despite carrying its invariant lifetime.
		let key = ProjectKey::new(
			&db,
			input,
			registry,
			ambient,
			ModulePath::new(entry).unwrap(),
			mode,
			preserve_names,
			true,
		);
		let key = unsafe { std::mem::transmute::<ProjectKey<'_>, ProjectKey<'static>>(key) };
		(db, key)
	}

	fn compat_diagnostics(db: &TestDb, key: ProjectKey<'_>) -> Vec<ProjectDiagnostic> {
		let symbols = compat_symbol_map(db, key);
		symbols
			.order
			.iter()
			.flat_map(|module| {
				compat_rewritten_module(db, key, *module)
					.diagnostics
					.iter()
					.cloned()
					.collect::<Vec<_>>()
			})
			.collect()
	}

	#[test]
	fn extracted_tags_rewrites_and_diagnostics_match_driver() {
		let cases: &[(&[(&str, &str)], EntryMode, bool)] = &[
			(
				&[
					(
						"main",
						"import @/dep with (answer as value)\nfunc main(): void = {}\nfunc use(): int = value()",
					),
					("dep", "public func answer(): int = 42"),
				],
				EntryMode::Entry,
				false,
			),
			(
				&[
					("main", "import @/dep with (secret)\nfunc main(): void = {}"),
					("dep", "private func secret(): int = 1"),
				],
				EntryMode::Entry,
				false,
			),
			(
				&[
					(
						"main",
						"import @/dep with (answer as value)\nfunc value(): int = 0\nfunc main(): void = {}",
					),
					("dep", "public func answer(): int = 42"),
				],
				EntryMode::Entry,
				false,
			),
			(
				&[("main", "func main(): void = {}\nfunc value(): int = 1")],
				EntryMode::Entry,
				false,
			),
			(
				&[("lib", "func value(): int = 1")],
				EntryMode::Library,
				true,
			),
		];
		for (files, mode, preserve) in cases {
			let entry = files[0].0;
			let map: BTreeMap<_, _> = files.iter().copied().collect();
			let load = |path: &str| map.get(path).map(ToString::to_string);
			let legacy = Driver::resolve_and_bind(
				entry,
				&load,
				&|_| None,
				*mode == EntryMode::Entry,
				*preserve,
			);
			let (db, key) = fixture(files, &[], "one", entry, *mode, *preserve);
			match legacy {
				Ok(driver) => {
					let symbols = compat_symbol_map(&db, key);
					assert_eq!(symbols.tags, driver.tags);
					for module in symbols.order.iter() {
						let module_key = module.display_key(&db);
						assert_eq!(
							compat_rewritten_module(&db, key, *module).module.as_ref(),
							&driver.processed[&module_key]
						);
					}
					assert!(compat_diagnostics(&db, key).is_empty());
				}
				Err(expected) => assert_eq!(compat_diagnostics(&db, key), expected),
			}
		}
	}

	#[test]
	fn keys_modes_roots_and_builtin_order_are_isolated() {
		let files = &[
			(
				"main",
				"import std/tool with (answer)\nfunc main(): void = {}",
			),
			("other", "func value(): int = 1"),
		];
		let builtins = &[("tool", "public func answer(): int = 42")];
		let (db_a, key_a) = fixture(files, builtins, "a", "main", EntryMode::Entry, false);
		let symbols = compat_symbol_map(&db_a, key_a);
		assert_eq!(
			symbols
				.order
				.iter()
				.map(|module| module.display_key(&db_a))
				.collect::<Vec<_>>(),
			["std::tool", "main"]
		);
		assert_eq!(queries::project_graph(&db_a, key_a).order.len(), 1);
		let other_id = ProjectId::new("b");
		let other_modules: Arc<[ModuleInput]> = files
			.iter()
			.map(|(path, source)| {
				ModuleInput::new(
					&db_a,
					other_id.clone(),
					ModulePath::new(path).unwrap(),
					Some(Arc::from(*source)),
				)
			})
			.collect::<Vec<_>>()
			.into();
		let other_input = ProjectInput::new(&db_a, other_id, other_modules);
		let other_key = ProjectKey::new(
			&db_a,
			other_input,
			key_a.builtin_registry(&db_a),
			key_a.ambient_core_registry(&db_a),
			ModulePath::new("main").unwrap(),
			EntryMode::Entry,
			false,
			true,
		);
		assert!(!Arc::ptr_eq(&symbols, &compat_symbol_map(&db_a, other_key)));
		let main = symbols.handles["main"];
		let entry_analysis = compat_module_analysis(&db_a, key_a, main);
		let library_key = ProjectKey::new(
			&db_a,
			key_a.project_input(&db_a),
			key_a.builtin_registry(&db_a),
			key_a.ambient_core_registry(&db_a),
			ModulePath::new("main").unwrap(),
			EntryMode::Library,
			false,
			true,
		);
		assert!(!Arc::ptr_eq(
			&entry_analysis,
			&compat_module_analysis(&db_a, library_key, main),
		));
		let other_main = compat_symbol_map(&db_a, other_key).handles["main"];
		assert!(!Arc::ptr_eq(
			&entry_analysis,
			&compat_module_analysis(&db_a, other_key, other_main),
		));
		let (db_b, key_b) = fixture(files, builtins, "b", "other", EntryMode::Library, true);
		assert_eq!(
			compat_symbol_map(&db_b, key_b)
				.order
				.iter()
				.map(|module| module.display_key(&db_b))
				.collect::<Vec<_>>(),
			["other"]
		);
	}

	#[test]
	fn transitive_dependencies_use_legacy_preorder_for_project_and_builtin_nodes() {
		let files = &[
			("main", "import @/a\nimport std/tool"),
			("a", "import std/tool"),
		];
		let builtins = &[("tool", "public let answer = 42")];
		let (db, key) = fixture(files, builtins, "deps", "main", EntryMode::Library, false);
		let main = compat_symbol_map(&db, key).handles["main"];
		assert_eq!(
			transitive_dependencies(&db, key, main)
				.iter()
				.map(|module| module.display_key(&db))
				.collect::<Vec<_>>(),
			["a", "std::tool"]
		);
	}
}
