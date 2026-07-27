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

#[salsa::tracked(returns(clone), no_eq)]
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
			.entry(definition.module.path.to_string())
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

#[salsa::tracked(returns(clone), no_eq)]
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
	for module in graph
		.semantic_order
		.iter()
		.copied()
		.filter(|module| module.domain(db) != SemanticModuleDomain::AmbientCore)
	{
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

#[salsa::tracked(returns(clone), no_eq)]
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
	match bundle::bundle(key.entry(db).as_str(), emitted.module_sources.clone()) {
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
