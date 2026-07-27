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
	let mut grouped: FxHashMap<String, Vec<String>> = FxHashMap::default();
	for fragment in &stable.fragments {
		let definition = fragment.definition();
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
		grouped
			.entry(module_specifier(&definition.module))
			.or_default()
			.push(name);
	}
	let mut imports: Vec<_> = grouped.into_iter().collect();
	imports.sort_unstable_by(|left, right| left.0.cmp(&right.0));
	let mut source = String::new();
	for (specifier, names) in &mut imports {
		names.sort_unstable();
		names.dedup();
		source.push_str(&format!(
			"import {{ {} }} from \"{specifier}\";\n",
			names.join(", ")
		));
	}
	source.push_str(&nymph_codegen::emit_for_project_module(
		&stable.hir,
		&stable.module.path,
	));

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
		if definition.module != stable.module || (!preserve && !public.contains(definition)) {
			continue;
		}
		if matches!(
			fragment.fragment(),
			nymph_sema::LoweredHirFragment::TopLevelFunction(_)
				| nymph_sema::LoweredHirFragment::TopLevelValue(_)
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
	let mut sources = FxHashMap::default();
	let mut virtual_fragments = std::collections::BTreeMap::new();
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
		for fragment in &stable.virtual_runtime {
			virtual_fragments
				.entry(fragment.definition.clone())
				.or_insert_with(|| fragment.fragment.clone());
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
	for fragment in virtual_fragments.into_values() {
		by_owner
			.entry(fragment.definition().module.clone())
			.or_default()
			.push(fragment);
	}
	for (owner, fragments) in by_owner {
		match emit_virtual_runtime_module(db, key, &owner, &fragments) {
			Ok(source) => {
				sources.insert(module_specifier(&owner), source);
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
	}))
}

fn emit_virtual_runtime_module(
	db: &dyn Db,
	key: ProjectKey<'_>,
	owner: &nymph_sema::ModuleIdentity,
	fragments: &[nymph_sema::LoweredRuntimeDefinition],
) -> Result<String, String> {
	let mut hir = nymph_hir::hir::HirModule {
		lets: vec![],
		funcs: vec![],
		classes: vec![],
		enums: vec![],
	};
	let mut shells = FxHashMap::default();
	for fragment in fragments {
		match fragment.fragment() {
			nymph_sema::LoweredHirFragment::StructShell(value) => {
				shells.insert(fragment.definition().clone(), (true, hir.classes.len()));
				hir.classes.push(value.clone());
			}
			nymph_sema::LoweredHirFragment::EnumShell(value) => {
				shells.insert(fragment.definition().clone(), (false, hir.enums.len()));
				hir.enums.push(value.clone());
			}
			_ => {}
		}
	}
	for fragment in fragments {
		use nymph_sema::LoweredHirFragment as Fragment;
		match fragment.fragment() {
			Fragment::TopLevelFunction(value) => hir.funcs.push(value.clone()),
			Fragment::TopLevelValue(value) => hir.lets.push(value.clone()),
			Fragment::AttachedInstance { owner, method }
			| Fragment::AttachedMember { owner, method }
			| Fragment::MaterializedDefault { owner, method, .. } => {
				let shell_owner = attachment_shell_owner(db, key, owner, &shells)?;
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
				let Some((class, index)) = shells.get(owner).copied() else {
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
	let mut source = nymph_codegen::emit_for_project_module(&hir, &owner.path);
	let mut external_imports = fragments
		.iter()
		.filter_map(|fragment| match fragment.fragment() {
			nymph_sema::LoweredHirFragment::TopLevelExternal { name, abi } => Some((
				abi.module.as_ref()?.to_string(),
				abi.symbol.as_ref()?.to_string(),
				name.as_str().to_string(),
			)),
			_ => None,
		})
		.collect::<Vec<_>>();
	external_imports.sort_unstable();
	for (module, symbol, name) in external_imports.into_iter().rev() {
		source.insert_str(
			0,
			&format!("import {{ {symbol} as {name} }} from \"{module}\";\n"),
		);
	}
	let mut exports = fragments
		.iter()
		.filter_map(|fragment| match fragment.fragment() {
			nymph_sema::LoweredHirFragment::TopLevelFunction(_)
			| nymph_sema::LoweredHirFragment::TopLevelValue(_)
			| nymph_sema::LoweredHirFragment::TopLevelExternal { .. }
			| nymph_sema::LoweredHirFragment::StructShell(_)
			| nymph_sema::LoweredHirFragment::EnumShell(_) => {
				queries::binding_name(db, key, fragment.definition().clone())
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

fn attachment_shell_owner(
	db: &dyn Db,
	key: ProjectKey<'_>,
	owner: &nymph_sema::DefinitionId,
	shells: &FxHashMap<nymph_sema::DefinitionId, (bool, usize)>,
) -> Result<nymph_sema::DefinitionId, String> {
	if shells.contains_key(owner) {
		return Ok(owner.clone());
	}
	let request = nymph_sema::StableShapeRequest::Implementation(owner.clone());
	match queries::stable_shape(db, key, request) {
		Ok(nymph_sema::StableShapeFact::Implementation(shape)) => shape
			.runtime_owner
			.filter(|runtime_owner| shells.contains_key(runtime_owner))
			.or_else(|| match &owner.key {
				nymph_sema::DeclarationKey::Implementation { header, .. } => match &header.self_type {
					nymph_sema::HeaderType::Named { definition, .. } => Some(definition.clone()),
					_ => None,
				},
				_ => None,
			})
			.filter(|runtime_owner| shells.contains_key(runtime_owner))
			.ok_or_else(|| {
				format!(
					"missing exact owner shell for {owner:?}; available: {:?}",
					shells.keys()
				)
			}),
		Ok(_) => Err(format!(
			"implementation owner returned the wrong stable shape: {owner:?}"
		)),
		Err(error) => Err(format!(
			"cannot resolve exact attachment owner {owner:?}: {error:?}"
		)),
	}
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
	if key.builtin_registry(db).modules(db).is_empty() {
		let mut intrinsics = crate::intrinsics::intrinsic_module_sources();
		for module in ["std/box", "std/string"] {
			if let Some(source) = intrinsics.remove(module) {
				module_sources.entry(module.to_string()).or_insert(source);
			}
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
