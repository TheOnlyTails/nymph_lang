//! Stable, identity-driven project emission and bundling.

use std::sync::Arc;

use nymph_ast::{Span, decl::Visibility};
use nymph_diagnostics::Diagnostic;
use rustc_hash::{FxHashMap, FxHashSet};

use super::queries::Db;
use super::session::{ProjectKey, SemanticModuleDomain, SemanticModuleInput};
use super::{CompiledProject, ProjectDiagnostic, bundle, queries};

#[derive(Clone, Debug, PartialEq)]
pub struct StableEmittedProject {
	pub module_sources: FxHashMap<String, String>,
	pub entry_tag: usize,
	compiler_option_binding: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StableEmissionResult<T> {
	Value(Arc<T>),
	Diagnostics(Arc<[ProjectDiagnostic]>),
}

fn internal_diagnostic(key: &str, code: &str, message: String) -> Arc<[ProjectDiagnostic]> {
	vec![ProjectDiagnostic {
		module: key.to_string(),
		diag: Diagnostic::error(code.into(), message, Span::new(0, 0)),
	}]
	.into()
}

fn module_specifier(module: &nymph_sema::ModuleIdentity) -> String {
	if matches!(module.origin, nymph_sema::ModuleOrigin::Compiler) {
		format!("@nymph/runtime/{}", module.path)
	} else {
		module.path.to_string()
	}
}

fn compiler_option_definition<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
) -> Result<nymph_sema::DefinitionId, String> {
	queries::compiler_runtime_roles(db, key.ambient_core_registry(db))
		.option
		.as_ref()
		.map(|role| role.option.clone())
		.ok_or_else(|| "compiler Option definition is unavailable".to_string())
}

fn prepend_external_aliases<'a>(
	source: &mut String,
	externals: impl Iterator<Item = (&'a nymph_sema::ExternalAbi, &'a str)>,
) {
	let mut imports = externals
		.filter_map(|(abi, name)| {
			let (module, symbol) = abi.linked()?;
			Some((module.to_string(), symbol.to_string(), name.to_string()))
		})
		.collect::<Vec<_>>();
	imports.sort_unstable();
	imports.dedup();
	for (module, symbol, name) in imports.into_iter().rev() {
		source.insert_str(
			0,
			&format!("import {{ {symbol} as {name} }} from \"{module}\";\n"),
		);
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn emitted_interface_module<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> StableEmissionResult<String> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("emitted_interface_module", module);
	let stable = match queries::lower_interface_module(db, key, module) {
		Ok(module) => module,
		Err(error) => {
			return StableEmissionResult::Diagnostics(internal_diagnostic(
				&module.display_key(db),
				"STABLE-EMISSION-LOWERING",
				format!("stable module lowering failed: {error:?}"),
			));
		}
	};
	let mut predeclared_imports = Vec::new();
	let routed_demands = stable
		.fragments
		.iter()
		.flat_map(|fragment| fragment.routed_demands().iter().cloned())
		.collect::<FxHashSet<_>>();
	let direct_demands = stable
		.fragments
		.iter()
		.flat_map(|fragment| fragment.direct_demands().iter().cloned())
		.collect::<FxHashSet<_>>();
	for fragment in &stable.fragments {
		let definition = fragment.definition();
		if routed_demands.contains(definition) && !direct_demands.contains(definition) {
			continue;
		}
		if definition.module == stable.module
			|| !matches!(
				fragment.fragment(),
				nymph_sema::LoweredHirFragment::TopLevelFunction(_)
					| nymph_sema::LoweredHirFragment::TopLevelValue(_)
					| nymph_sema::LoweredHirFragment::TopLevelExternal { .. }
					| nymph_sema::LoweredHirFragment::StructShell(_)
					| nymph_sema::LoweredHirFragment::EnumShell(_)
			) {
			continue;
		}
		let name = match queries::binding_name(db, key, definition.clone()) {
			Ok(name) => name.as_str().to_string(),
			Err(error) => {
				return StableEmissionResult::Diagnostics(internal_diagnostic(
					&module.display_key(db),
					"STABLE-EMISSION-LINK",
					format!("stable import naming failed: {error:?}"),
				));
			}
		};
		predeclared_imports.push((module_specifier(&definition.module), name.clone(), name));
	}
	let mut source = nymph_codegen::emit_for_project_module_with_imports(
		&stable.hir,
		&stable.module.path,
		&predeclared_imports,
	);
	prepend_external_aliases(
		&mut source,
		stable.fragments.iter().filter_map(|fragment| {
			if fragment.definition().module != stable.module {
				return None;
			}
			match fragment.fragment() {
				nymph_sema::LoweredHirFragment::TopLevelExternal { name, abi } => {
					Some((abi, name.as_str()))
				}
				_ => None,
			}
		}),
	);

	let environment = queries::interface_module_environment(db, key, module);
	let public: FxHashSet<_> = match environment.as_ref() {
		nymph_sema::ModuleEnvironment::Complete(interface) => interface
			.exports
			.iter()
			.filter(|definition| definition.visibility != Some(Visibility::Private))
			.map(|definition| definition.id.clone())
			.collect(),
		nymph_sema::ModuleEnvironment::Recovered(_) => {
			return StableEmissionResult::Diagnostics(internal_diagnostic(
				&module.display_key(db),
				"STABLE-EMISSION-RECOVERED",
				"stable emission cannot consume a recovered environment".to_string(),
			));
		}
	};
	let preserve = key.preserve_names(db) && stable.module.path == key.entry(db).as_str();
	let mut exports = Vec::new();
	for fragment in &stable.fragments {
		let definition = fragment.definition();
		let external_abi = matches!(
			fragment.fragment(),
			nymph_sema::LoweredHirFragment::TopLevelExternal { .. }
		);
		if definition.module != stable.module
			|| (!external_abi && !preserve && !public.contains(definition))
		{
			continue;
		}
		if matches!(
			fragment.fragment(),
			nymph_sema::LoweredHirFragment::TopLevelFunction(_)
				| nymph_sema::LoweredHirFragment::TopLevelValue(_)
				| nymph_sema::LoweredHirFragment::TopLevelExternal { .. }
				| nymph_sema::LoweredHirFragment::StructShell(_)
				| nymph_sema::LoweredHirFragment::EnumShell(_)
		) {
			if let Ok(name) = queries::binding_name(db, key, definition.clone()) {
				exports.push(name.as_str().to_string());
			}
		}
	}
	exports.sort_unstable();
	exports.dedup();
	if !exports.is_empty() {
		source.push_str(&format!("export {{ {} }};\n", exports.join(", ")));
	}
	StableEmissionResult::Value(Arc::new(source))
}

#[salsa::tracked(returns(clone))]
pub(crate) fn emitted_interface_project<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
) -> StableEmissionResult<StableEmittedProject> {
	let diagnostics = queries::interface_project_diagnostics(db, key);
	if diagnostics
		.0
		.iter()
		.any(|diagnostic| diagnostic.diag.is_error())
	{
		return StableEmissionResult::Diagnostics(diagnostics.0);
	}
	let graph = queries::project_graph(db, key);
	let compiler_option = match compiler_option_definition(db, key) {
		Ok(definition) => definition,
		Err(message) => {
			return StableEmissionResult::Diagnostics(internal_diagnostic(
				key.entry(db).as_str(),
				"STABLE-INTRINSIC-DEPENDENCY",
				message,
			));
		}
	};
	let compiler_option_binding = match queries::binding_name(db, key, compiler_option.clone()) {
		Ok(name) => name.as_str().to_string(),
		Err(error) => {
			return StableEmissionResult::Diagnostics(internal_diagnostic(
				key.entry(db).as_str(),
				"STABLE-INTRINSIC-DEPENDENCY",
				format!("cannot name compiler Option definition: {error:?}"),
			));
		}
	};
	let mut sources = FxHashMap::default();
	let mut virtual_fragments = std::collections::BTreeMap::new();
	let mut option_requested = false;
	let mut option_definition = None;
	let host_runtime = crate::host_runtime::HostRuntimeGraph::compiler_facts();
	let reserved_runtime = crate::host_runtime::HostRuntimeGraph::role_import_specifier(
		crate::host_runtime::CompilerRuntimeRole::Option,
	);
	if graph.semantic_order.iter().copied().any(|module| {
		module.domain(db) != SemanticModuleDomain::AmbientCore
			&& module.identity(db).path.as_str() == reserved_runtime
	}) {
		return StableEmissionResult::Diagnostics(internal_diagnostic(
			reserved_runtime,
			"STABLE-RUNTIME-MODULE-COLLISION",
			format!(
				"canonical runtime module `{reserved_runtime}` collides with project module `{reserved_runtime}`"
			),
		));
	}
	for module in graph
		.semantic_order
		.iter()
		.copied()
		.filter(|module| module.domain(db) != SemanticModuleDomain::AmbientCore)
	{
		let stable = match queries::lower_interface_module(db, key, module) {
			Ok(stable) => stable,
			Err(error) => {
				return StableEmissionResult::Diagnostics(internal_diagnostic(
					&module.display_key(db),
					"STABLE-EMISSION-LINK",
					format!("stable runtime linking failed: {error:?}"),
				));
			}
		};
		option_requested |= stable.fragments.iter().any(|fragment| {
			matches!(fragment.fragment(), nymph_sema::LoweredHirFragment::TopLevelExternal { abi, .. }
				if abi.linked().is_some_and(|(module, _)| host_runtime.semantic_dependencies(module).next().is_some()))
		});
		for fragment in &stable.virtual_runtime {
			if fragment.definition == compiler_option {
				option_definition = Some(fragment.definition.clone());
			}
			virtual_fragments
				.entry(fragment.definition.clone())
				.or_insert_with(|| fragment.clone());
		}
		match emitted_interface_module(db, key, module) {
			StableEmissionResult::Value(source) => {
				sources.insert(
					module.identity(db).path.to_string(),
					source.as_ref().clone(),
				);
			}
			StableEmissionResult::Diagnostics(diagnostics) => {
				return StableEmissionResult::Diagnostics(diagnostics);
			}
		}
	}
	let mut by_owner: std::collections::BTreeMap<_, Vec<_>> = std::collections::BTreeMap::new();
	let module_bindings = virtual_fragments
		.values()
		.filter(|fragment| {
			matches!(
				fragment.fragment.placement(),
				nymph_sema::RuntimeAssemblyPlacement::Module(_)
			)
		})
		.filter(|fragment| {
			matches!(
				fragment.fragment.fragment(),
				nymph_sema::LoweredHirFragment::TopLevelFunction(_)
					| nymph_sema::LoweredHirFragment::TopLevelValue(_)
					| nymph_sema::LoweredHirFragment::TopLevelExternal { .. }
					| nymph_sema::LoweredHirFragment::StructShell(_)
					| nymph_sema::LoweredHirFragment::EnumShell(_)
			)
		})
		.map(|fragment| fragment.definition.clone())
		.collect::<FxHashSet<_>>();
	for fragment in virtual_fragments.into_values() {
		by_owner
			.entry(fragment.owner.clone())
			.or_default()
			.push(fragment);
	}
	for (owner, fragments) in by_owner {
		match emit_virtual_runtime_module(db, key, &owner, &fragments, &module_bindings) {
			Ok(source) => {
				let specifier = module_specifier(&owner);
				if sources.contains_key(&specifier) {
					return StableEmissionResult::Diagnostics(internal_diagnostic(
						&specifier,
						"STABLE-RUNTIME-MODULE-COLLISION",
						format!(
							"runtime module `{specifier}` owned by `{}` collides with project module `{specifier}`",
							owner.path
						),
					));
				}
				sources.insert(specifier, source);
			}
			Err(message) => {
				return StableEmissionResult::Diagnostics(internal_diagnostic(
					owner.path.as_ref(),
					"STABLE-EMISSION-ATTACHMENT",
					message,
				));
			}
		}
	}
	if option_requested {
		let Some(option) = option_definition else {
			return StableEmissionResult::Diagnostics(internal_diagnostic(
				key.entry(db).as_str(),
				"STABLE-INTRINSIC-DEPENDENCY",
				"selected intrinsic requires the compiler Option definition".to_string(),
			));
		};
		if option != compiler_option {
			return StableEmissionResult::Diagnostics(internal_diagnostic(
				key.entry(db).as_str(),
				"STABLE-INTRINSIC-DEPENDENCY",
				"assembled Option definition does not match the compiler Option".to_string(),
			));
		}
		let shim = format!(
			"export {{ {} as Option }} from \"@nymph/runtime/{}\";\n",
			compiler_option_binding, compiler_option.module.path
		);
		if sources.insert(reserved_runtime.to_string(), shim).is_some() {
			return StableEmissionResult::Diagnostics(internal_diagnostic(
				reserved_runtime,
				"STABLE-INTRINSIC-COLLISION",
				"compiler Option shim collides with a project module".to_string(),
			));
		}
	}
	let Some(entry_tag) = graph
		.semantic_order
		.iter()
		.position(|module| module.identity(db).path == key.entry(db).as_str())
	else {
		return StableEmissionResult::Diagnostics(internal_diagnostic(
			key.entry(db).as_str(),
			"STABLE-EMISSION-ENTRY",
			"the stable project graph does not contain its entry module".to_string(),
		));
	};
	StableEmissionResult::Value(Arc::new(StableEmittedProject {
		module_sources: sources,
		entry_tag,
		compiler_option_binding,
	}))
}

fn emit_virtual_runtime_module(
	db: &dyn Db,
	key: ProjectKey<'_>,
	owner: &nymph_sema::ModuleIdentity,
	fragments: &[nymph_sema::VirtualRuntimeFragment],
	module_bindings: &FxHashSet<nymph_sema::DefinitionId>,
) -> Result<String, String> {
	let mut hir = nymph_hir::hir::HirModule {
		lets: vec![],
		funcs: vec![],
		classes: vec![],
		enums: vec![],
	};
	let mut shells = FxHashMap::default();
	for fragment in fragments {
		match fragment.fragment.fragment() {
			nymph_sema::LoweredHirFragment::StructShell(value) => {
				shells.insert(fragment.definition.clone(), (true, hir.classes.len()));
				hir.classes.push(value.clone());
			}
			nymph_sema::LoweredHirFragment::EnumShell(value) => {
				shells.insert(fragment.definition.clone(), (false, hir.enums.len()));
				hir.enums.push(value.clone());
			}
			_ => {}
		}
	}
	for fragment in fragments {
		use nymph_sema::LoweredHirFragment as Fragment;
		match fragment.fragment.fragment() {
			Fragment::TopLevelFunction(value) => hir.funcs.push(value.clone()),
			Fragment::TopLevelValue(value) => hir.lets.push(value.clone()),
			Fragment::AttachedInstance { owner, method }
			| Fragment::AttachedMember { owner, method }
			| Fragment::MaterializedDefault { owner, method, .. } => {
				let nymph_sema::RuntimeAssemblyPlacement::Shell(shell_owner) =
					fragment.fragment.placement()
				else {
					return Err(format!(
						"non-shell attachment placement for {:?}",
						fragment.definition
					));
				};
				let Some((class, index)) = shells.get(&shell_owner).copied() else {
					return Err(format!(
						"missing exact owner shell for {owner:?}; available: {:?}",
						shells.keys()
					));
				};
				if class {
					hir.classes[index].methods.push(method.clone());
				} else {
					hir.enums[index].methods.push(method.clone());
				}
			}
			Fragment::AttachedStatic { owner, method } => {
				let nymph_sema::RuntimeAssemblyPlacement::Shell(shell_owner) =
					fragment.fragment.placement()
				else {
					return Err(format!(
						"non-shell attachment placement for {:?}",
						fragment.definition
					));
				};
				let Some((class, index)) = shells.get(shell_owner).copied() else {
					return Err(format!("missing exact owner shell for {owner:?}"));
				};
				if class {
					hir.classes[index].statics.push(method.clone());
				} else {
					hir.enums[index].statics.push(method.clone());
				}
			}
			Fragment::TopLevelExternal { .. } | Fragment::StructShell(_) | Fragment::EnumShell(_) => {}
		}
	}
	let current_module = module_specifier(owner);
	let mut runtime_imports = fragments
		.iter()
		.flat_map(|fragment| fragment.fragment.direct_demands().iter())
		.filter(|demand| demand.module != *owner)
		.filter(|demand| module_bindings.contains(*demand))
		.filter_map(|demand| {
			queries::binding_name(db, key, demand.clone())
				.ok()
				.map(|name| (module_specifier(&demand.module), name.as_str().to_string()))
		})
		.collect::<Vec<_>>();
	runtime_imports.sort_unstable();
	runtime_imports.dedup();
	let runtime_imports = runtime_imports
		.into_iter()
		.map(|(module, name)| (module, name.clone(), name))
		.collect::<Vec<_>>();
	let mut source =
		nymph_codegen::emit_for_project_module_with_imports(&hir, &current_module, &runtime_imports);
	prepend_external_aliases(
		&mut source,
		fragments
			.iter()
			.filter_map(|fragment| match fragment.fragment.fragment() {
				nymph_sema::LoweredHirFragment::TopLevelExternal { name, abi } => {
					Some((abi, name.as_str()))
				}
				_ => None,
			}),
	);
	let mut exports = fragments
		.iter()
		.filter_map(|fragment| match fragment.fragment.fragment() {
			nymph_sema::LoweredHirFragment::TopLevelFunction(_)
			| nymph_sema::LoweredHirFragment::TopLevelValue(_)
			| nymph_sema::LoweredHirFragment::TopLevelExternal { .. }
			| nymph_sema::LoweredHirFragment::StructShell(_)
			| nymph_sema::LoweredHirFragment::EnumShell(_) => {
				queries::binding_name(db, key, fragment.definition.clone())
					.ok()
					.map(|name| name.as_str().to_string())
			}
			_ => None,
		})
		.collect::<Vec<_>>();
	exports.sort_unstable();
	exports.dedup();
	if !exports.is_empty() {
		source.push_str(&format!("export {{ {} }};\n", exports.join(", ")));
	}
	Ok(source)
}

#[salsa::tracked(returns(clone))]
pub(crate) fn compiled_interface_project<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
) -> StableEmissionResult<CompiledProject> {
	let emitted = match emitted_interface_project(db, key) {
		StableEmissionResult::Value(value) => value,
		StableEmissionResult::Diagnostics(diagnostics) => {
			return StableEmissionResult::Diagnostics(diagnostics);
		}
	};
	let mut module_sources = emitted.module_sources.clone();
	for (module, source) in crate::host_runtime::HostRuntimeGraph::compiler_facts()
		.module_sources(&emitted.compiler_option_binding)
	{
		if module_sources.insert(module.clone(), source).is_some() {
			return StableEmissionResult::Diagnostics(internal_diagnostic(
				&module,
				"STABLE-INTRINSIC-COLLISION",
				format!("intrinsic runtime module `{module}` collides with a project module"),
			));
		}
	}
	match bundle::bundle(key.entry(db).as_str(), module_sources) {
		Ok(js) => StableEmissionResult::Value(Arc::new(CompiledProject {
			js,
			entry_main: "main".to_string(),
			entry_tag: emitted.entry_tag,
		})),
		Err(error) => StableEmissionResult::Diagnostics(internal_diagnostic(
			key.entry(db).as_str(),
			"BUNDLE-FAILED",
			format!("bundling the stable project failed: {error}"),
		)),
	}
}
