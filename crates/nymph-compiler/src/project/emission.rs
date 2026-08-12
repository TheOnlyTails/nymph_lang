//! Stable, identity-driven project emission and bundling.

use std::sync::Arc;

use nymph_ast::{Span, decl::Visibility};
use nymph_diagnostics::Diagnostic;
use rustc_hash::FxHashMap;

use super::queries::Db;
use super::session::{ProjectKey, SemanticModuleDomain, SemanticModuleInput};
use super::{CompiledProject, ProjectDiagnostic, bundle, link_plan, queries};

#[derive(Clone, Debug, PartialEq)]
pub struct StableEmittedProject {
	pub module_sources: FxHashMap<String, String>,
	pub entry_tag: usize,
	pub(crate) compiler_option_binding: String,
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

fn prepend_external_aliases(source: &mut String, aliases: &[link_plan::LinkedExternalAlias]) {
	for alias in aliases.iter().rev() {
		let (module, symbol) = alias
			.abi
			.linked()
			.expect("link plans deliver linked ABIs only");
		source.insert_str(
			0,
			&format!(
				"import {{ {symbol} as {} }} from \"{module}\";\n",
				alias.binding.as_str()
			),
		);
	}
}

struct QueryLinkResolver<'db> {
	db: &'db dyn Db,
	key: ProjectKey<'db>,
}

impl link_plan::LinkNameResolver for QueryLinkResolver<'_> {
	fn binding_name(
		&mut self,
		definition: &nymph_sema::DefinitionId,
	) -> Result<nymph_sema::EmittedBindingName, nymph_sema::StableNameLookupError> {
		queries::binding_name(self.db, self.key, definition.clone())
	}

	fn module_specifier(
		&mut self,
		module: &nymph_sema::ModuleIdentity,
	) -> Result<nymph_sema::CanonicalModuleSpecifier, nymph_sema::StableNameLookupError> {
		queries::module_specifier(self.db, self.key, module.clone())
	}
}

fn link_artifact(fragment: &nymph_sema::LoweredHirFragment) -> link_plan::LinkArtifact<'_> {
	match fragment {
		nymph_sema::LoweredHirFragment::TopLevelFunction(_)
		| nymph_sema::LoweredHirFragment::RuntimeTypeAttachment { .. }
		| nymph_sema::LoweredHirFragment::TopLevelValue(_)
		| nymph_sema::LoweredHirFragment::StructShell(_)
		| nymph_sema::LoweredHirFragment::EnumShell(_) => link_plan::LinkArtifact::TopLevel,
		nymph_sema::LoweredHirFragment::TopLevelExternal { abi, .. } => {
			link_plan::LinkArtifact::External(abi)
		}
		_ => link_plan::LinkArtifact::Attached,
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn emitted_interface_module<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
	transactional: bool,
) -> StableEmissionResult<String> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("emitted_interface_module", module);
	#[cfg(feature = "test-support")]
	let _timing = super::benchmark_support::phase(super::benchmark_support::Phase::Emission);
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
	let environment = queries::interface_module_environment(db, key, module);
	let mut public: std::collections::HashSet<_> = match environment.as_ref() {
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
	if key.mode(db) == nymph_sema::EntryMode::Repl {
		public.extend(
			stable
				.fragments
				.iter()
				.map(|fragment| fragment.definition().clone()),
		);
	}
	public.extend(stable.fragments.iter().filter_map(|fragment| {
		(matches!(
			fragment.fragment(),
			nymph_sema::LoweredHirFragment::RuntimeTypeAttachment { .. }
		) || matches!(
			(&fragment.definition().key, fragment.fragment()),
			(
				nymph_sema::DeclarationKey::MethodBody { name, .. }
					| nymph_sema::DeclarationKey::Member { name, .. },
				nymph_sema::LoweredHirFragment::TopLevelFunction(_)
			) if name == "power"
		))
		.then(|| fragment.definition().clone())
	}));
	let preserve = key.preserve_names(db) && stable.module.path == key.entry(db).as_str();
	let fragments = stable
		.fragments
		.iter()
		.map(|fragment| link_plan::LinkFragment {
			definition: fragment.definition(),
			artifact: link_artifact(fragment.fragment()),
			direct_demands: fragment.direct_demands(),
			routed_demands: fragment.routed_demands(),
		})
		.collect::<Vec<_>>();
	let plan = match link_plan::plan_project_module(
		&stable.module,
		&fragments,
		&public,
		preserve,
		&mut QueryLinkResolver { db, key },
	) {
		Ok(plan) => plan,
		Err(error) => {
			return StableEmissionResult::Diagnostics(internal_diagnostic(
				&module.display_key(db),
				"STABLE-EMISSION-LINK",
				format!("stable module link planning failed: {error:?}"),
			));
		}
	};
	if transactional
		&& let Some(alias) = plan.external_aliases.iter().find(|alias| {
			alias.abi.linked().is_some_and(|(module, symbol)| {
				nymph_hir::linkage::external_effect(module, symbol)
					== nymph_hir::linkage::ExternalEffect::UnauditedStateful
			})
		}) {
		let (module, symbol) = alias.abi.linked().expect("filtered linked external");
		return StableEmissionResult::Diagnostics(internal_diagnostic(
			&stable.module.path,
			"REPL-UNSAFE-EXTERNAL",
			format!("strict REPL mode rejects unaudited stateful external `{module}::{symbol}`"),
		));
	}
	let imports = plan
		.imports
		.iter()
		.map(|import| {
			let name = import.binding.as_str().to_string();
			(
				link_plan::specifier_str(&import.specifier).to_string(),
				name.clone(),
				name,
			)
		})
		.collect::<Vec<_>>();
	let mut source = if transactional {
		let imported_top_level_lets = plan
			.imports
			.iter()
			.filter(|import| {
				matches!(
					import.definition.key,
					nymph_sema::DeclarationKey::TopLevel {
						category: nymph_sema::DeclarationCategory::Let,
						..
					}
				)
			})
			.map(|import| import.binding.as_str().to_string())
			.collect::<Vec<_>>();
		match nymph_codegen::emit_for_transactional_project_module_checked(
			&stable.hir,
			&stable.module.path,
			&imports,
			&imported_top_level_lets,
		) {
			Ok(source) => source,
			Err((module, symbol)) => {
				return StableEmissionResult::Diagnostics(internal_diagnostic(
					&stable.module.path,
					"REPL-UNSAFE-EXTERNAL",
					format!("strict REPL mode rejects unaudited stateful external `{module}::{symbol}`"),
				));
			}
		}
	} else {
		nymph_codegen::emit_for_project_module_with_imports(&stable.hir, &stable.module.path, &imports)
	};
	prepend_external_aliases(&mut source, &plan.external_aliases);
	if !plan.exports.is_empty() {
		source.push_str(&format!(
			"export {{ {} }};\n",
			plan
				.exports
				.iter()
				.map(|export| export.binding.as_str())
				.collect::<Vec<_>>()
				.join(", ")
		));
	}
	StableEmissionResult::Value(Arc::new(source))
}

#[salsa::tracked(returns(clone))]
pub(crate) fn emitted_interface_project<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	transactional: bool,
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
			if let Err(conflict) =
				super::assembly::insert_exact_virtual_fragment(&mut virtual_fragments, fragment.clone())
			{
				return StableEmissionResult::Diagnostics(internal_diagnostic(
					&module.display_key(db),
					"STABLE-VIRTUAL-FRAGMENT-CONFLICT",
					format!(
						"conflicting virtual runtime fragments share exact definition ID `{:?}`",
						conflict.definition
					),
				));
			}
		}
		match emitted_interface_module(db, key, module, transactional) {
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
	let virtual_deliveries = virtual_fragments
		.values()
		.map(|fragment| {
			let delivery = if matches!(
				fragment.fragment.placement(),
				nymph_sema::RuntimeAssemblyPlacement::Module(_)
			) && matches!(
				fragment.fragment.fragment(),
				nymph_sema::LoweredHirFragment::TopLevelFunction(_)
					| nymph_sema::LoweredHirFragment::RuntimeTypeAttachment { .. }
					| nymph_sema::LoweredHirFragment::TopLevelValue(_)
					| nymph_sema::LoweredHirFragment::TopLevelExternal { .. }
					| nymph_sema::LoweredHirFragment::StructShell(_)
					| nymph_sema::LoweredHirFragment::EnumShell(_)
			) {
				link_plan::VirtualDemandDelivery::Binding
			} else {
				link_plan::VirtualDemandDelivery::Attached
			};
			(fragment.definition.clone(), delivery)
		})
		.collect::<std::collections::HashMap<_, _>>();
	let execution_fragments = virtual_fragments.values().cloned().collect::<Vec<_>>();
	for fragment in virtual_fragments.into_values() {
		by_owner
			.entry(fragment.owner.clone())
			.or_default()
			.push(fragment);
	}
	for (owner, fragments) in by_owner {
		match emit_virtual_runtime_module(
			db,
			key,
			&owner,
			&fragments,
			&execution_fragments,
			&virtual_deliveries,
			transactional,
		) {
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
	execution_fragments: &[nymph_sema::VirtualRuntimeFragment],
	virtual_deliveries: &std::collections::HashMap<
		nymph_sema::DefinitionId,
		link_plan::VirtualDemandDelivery,
	>,
	transactional: bool,
) -> Result<String, String> {
	let hir = super::assembly::assemble_runtime_module_with_execution(
		owner,
		fragments
			.iter()
			.map(|fragment| (fragment.definition.clone(), &fragment.fragment)),
		execution_fragments
			.iter()
			.map(|fragment| &fragment.fragment),
	)
	.map_err(|error| format!("runtime assembly failed: {error:?}"))?;
	let current_module = module_specifier(owner);
	let link_fragments = fragments
		.iter()
		.map(|fragment| link_plan::LinkFragment {
			definition: &fragment.definition,
			artifact: link_artifact(fragment.fragment.fragment()),
			direct_demands: fragment.fragment.direct_demands(),
			routed_demands: fragment.fragment.routed_demands(),
		})
		.collect::<Vec<_>>();
	let plan = link_plan::plan_virtual_module(
		owner,
		&link_fragments,
		virtual_deliveries,
		&mut QueryLinkResolver { db, key },
	)
	.map_err(|error| format!("runtime link planning failed: {error:?}"))?;
	if transactional
		&& let Some(alias) = plan.external_aliases.iter().find(|alias| {
			alias.abi.linked().is_some_and(|(module, symbol)| {
				nymph_hir::linkage::external_effect(module, symbol)
					== nymph_hir::linkage::ExternalEffect::UnauditedStateful
			})
		}) {
		let (module, symbol) = alias.abi.linked().expect("filtered linked external");
		return Err(format!(
			"strict REPL mode rejects unaudited stateful external `{module}::{symbol}`"
		));
	}
	let runtime_imports = plan
		.imports
		.iter()
		.map(|import| {
			let name = import.binding.as_str().to_string();
			(
				link_plan::specifier_str(&import.specifier).to_string(),
				name.clone(),
				name,
			)
		})
		.collect::<Vec<_>>();
	let mut source = if transactional {
		let imported_top_level_lets = plan
			.imports
			.iter()
			.filter(|import| {
				matches!(
					import.definition.key,
					nymph_sema::DeclarationKey::TopLevel {
						category: nymph_sema::DeclarationCategory::Let,
						..
					}
				)
			})
			.map(|import| import.binding.as_str().to_string())
			.collect::<Vec<_>>();
		nymph_codegen::emit_for_transactional_project_module_checked(
			&hir,
			&current_module,
			&runtime_imports,
			&imported_top_level_lets,
		)
		.map_err(|(module, symbol)| {
			format!("strict REPL mode rejects unaudited stateful external `{module}::{symbol}`")
		})?
	} else {
		nymph_codegen::emit_for_project_module_with_imports(&hir, &current_module, &runtime_imports)
	};
	prepend_external_aliases(&mut source, &plan.external_aliases);
	if !plan.exports.is_empty() {
		source.push_str(&format!(
			"export {{ {} }};\n",
			plan
				.exports
				.iter()
				.map(|export| export.binding.as_str())
				.collect::<Vec<_>>()
				.join(", ")
		));
	}
	Ok(source)
}

#[salsa::tracked(returns(clone))]
pub(crate) fn compiled_interface_project<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
) -> StableEmissionResult<CompiledProject> {
	let emitted = match emitted_interface_project(db, key, false) {
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
	#[cfg(feature = "test-support")]
	let _timing = super::benchmark_support::phase(super::benchmark_support::Phase::Bundling);
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
