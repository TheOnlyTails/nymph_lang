//! Pure, typed planning for the names and modules delivered to code generation.

use std::collections::{HashMap, HashSet};

use nymph_sema::{
	CanonicalModuleSpecifier, DefinitionId, EmittedBindingName, ExternalAbi, ModuleIdentity,
	StableNameLookupError,
};

#[derive(Clone, Copy)]
pub(crate) enum LinkArtifact<'a> {
	TopLevel,
	External(&'a ExternalAbi),
	WrappedExternal,
	Attached,
}

#[derive(Clone, Copy)]
pub(crate) struct LinkFragment<'a> {
	pub definition: &'a DefinitionId,
	pub artifact: LinkArtifact<'a>,
	pub direct_demands: &'a [DefinitionId],
	pub routed_demands: &'a [DefinitionId],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VirtualDemandDelivery {
	Binding,
	Attached,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedImport {
	pub definition: DefinitionId,
	pub module: ModuleIdentity,
	pub specifier: CanonicalModuleSpecifier,
	pub binding: EmittedBindingName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkedExternalAlias {
	pub definition: DefinitionId,
	pub binding: EmittedBindingName,
	pub abi: ExternalAbi,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedExport {
	pub definition: DefinitionId,
	pub binding: EmittedBindingName,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModuleLinkPlan {
	pub imports: Vec<PlannedImport>,
	pub exports: Vec<PlannedExport>,
	pub external_aliases: Vec<LinkedExternalAlias>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModuleLinkPlanError {
	Binding {
		definition: DefinitionId,
		source: StableNameLookupError,
	},
	Module {
		module: ModuleIdentity,
		source: StableNameLookupError,
	},
	UnresolvedDemand {
		definition: DefinitionId,
	},
	BindingCollision {
		binding: EmittedBindingName,
		first: DefinitionId,
		second: DefinitionId,
	},
}

pub(crate) trait LinkNameResolver {
	fn binding_name(
		&mut self,
		definition: &DefinitionId,
	) -> Result<EmittedBindingName, StableNameLookupError>;
	fn module_specifier(
		&mut self,
		module: &ModuleIdentity,
	) -> Result<CanonicalModuleSpecifier, StableNameLookupError>;
}

pub(crate) fn plan_project_module(
	module: &ModuleIdentity,
	fragments: &[LinkFragment<'_>],
	public: &HashSet<DefinitionId>,
	preserve_names: bool,
	resolver: &mut impl LinkNameResolver,
) -> Result<ModuleLinkPlan, ModuleLinkPlanError> {
	let routed = fragments
		.iter()
		.flat_map(|fragment| fragment.routed_demands)
		.collect::<HashSet<_>>();
	let direct = fragments
		.iter()
		.flat_map(|fragment| fragment.direct_demands)
		.collect::<HashSet<_>>();
	let mut plan = ModuleLinkPlan::default();
	for fragment in fragments {
		if fragment.definition.module != *module
			&& !matches!(fragment.artifact, LinkArtifact::Attached)
			&& !(routed.contains(fragment.definition) && !direct.contains(fragment.definition))
		{
			plan
				.imports
				.push(resolve_import(fragment.definition, resolver)?);
		}
		if fragment.definition.module == *module {
			plan_external_alias(&mut plan, fragment, resolver)?;
			if !matches!(fragment.artifact, LinkArtifact::Attached)
				&& (matches!(
					fragment.artifact,
					LinkArtifact::External(_) | LinkArtifact::WrappedExternal
				) || preserve_names
					|| public.contains(fragment.definition))
			{
				plan.exports.push(PlannedExport {
					definition: fragment.definition.clone(),
					binding: resolve_binding(fragment.definition, resolver)?,
				});
			}
		}
	}
	finish(&mut plan)?;
	Ok(plan)
}

pub(crate) fn plan_virtual_module(
	owner: &ModuleIdentity,
	fragments: &[LinkFragment<'_>],
	deliveries: &HashMap<DefinitionId, VirtualDemandDelivery>,
	resolver: &mut impl LinkNameResolver,
) -> Result<ModuleLinkPlan, ModuleLinkPlanError> {
	let mut plan = ModuleLinkPlan::default();
	for demand in fragments
		.iter()
		.flat_map(|fragment| fragment.direct_demands)
		.filter(|demand| demand.module != *owner)
	{
		let Some(delivery) = deliveries.get(demand) else {
			return Err(ModuleLinkPlanError::UnresolvedDemand {
				definition: demand.clone(),
			});
		};
		if *delivery == VirtualDemandDelivery::Binding {
			plan.imports.push(resolve_import(demand, resolver)?);
		}
	}
	for fragment in fragments {
		plan_external_alias(&mut plan, fragment, resolver)?;
		if !matches!(fragment.artifact, LinkArtifact::Attached) {
			plan.exports.push(PlannedExport {
				definition: fragment.definition.clone(),
				binding: resolve_binding(fragment.definition, resolver)?,
			});
		}
	}
	finish(&mut plan)?;
	Ok(plan)
}

fn plan_external_alias(
	plan: &mut ModuleLinkPlan,
	fragment: &LinkFragment<'_>,
	resolver: &mut impl LinkNameResolver,
) -> Result<(), ModuleLinkPlanError> {
	if let LinkArtifact::External(abi) = fragment.artifact
		&& abi.linked().is_some()
	{
		plan.external_aliases.push(LinkedExternalAlias {
			definition: fragment.definition.clone(),
			binding: resolve_binding(fragment.definition, resolver)?,
			abi: abi.clone(),
		});
	}
	Ok(())
}

fn resolve_binding(
	definition: &DefinitionId,
	resolver: &mut impl LinkNameResolver,
) -> Result<EmittedBindingName, ModuleLinkPlanError> {
	resolver
		.binding_name(definition)
		.map_err(|source| ModuleLinkPlanError::Binding {
			definition: definition.clone(),
			source,
		})
}

fn resolve_import(
	definition: &DefinitionId,
	resolver: &mut impl LinkNameResolver,
) -> Result<PlannedImport, ModuleLinkPlanError> {
	let binding = resolve_binding(definition, resolver)?;
	let module = definition.module.clone();
	let specifier =
		resolver
			.module_specifier(&module)
			.map_err(|source| ModuleLinkPlanError::Module {
				module: module.clone(),
				source,
			})?;
	Ok(PlannedImport {
		definition: definition.clone(),
		module,
		specifier,
		binding,
	})
}

fn finish(plan: &mut ModuleLinkPlan) -> Result<(), ModuleLinkPlanError> {
	plan.imports.sort_by(|left, right| {
		(specifier_str(&left.specifier), left.binding.as_str())
			.cmp(&(specifier_str(&right.specifier), right.binding.as_str()))
	});
	validate_and_dedup_by_binding(
		&mut plan.imports,
		|item| (&item.definition, &item.binding),
		|left, right| left.specifier == right.specifier && left.binding == right.binding,
	)?;
	plan
		.exports
		.sort_by(|left, right| left.binding.cmp(&right.binding));
	validate_and_dedup_by_binding(
		&mut plan.exports,
		|item| (&item.definition, &item.binding),
		|left, right| left.binding == right.binding,
	)?;
	plan.external_aliases.sort_by(|left, right| {
		let (left_module, left_symbol) = left.abi.linked().expect("plans contain linked ABIs only");
		let (right_module, right_symbol) = right.abi.linked().expect("plans contain linked ABIs only");
		(left_module, left_symbol, left.binding.as_str()).cmp(&(
			right_module,
			right_symbol,
			right.binding.as_str(),
		))
	});
	validate_and_dedup_by_binding(
		&mut plan.external_aliases,
		|item| (&item.definition, &item.binding),
		|left, right| left.abi.linked() == right.abi.linked() && left.binding == right.binding,
	)?;
	Ok(())
}

fn validate_and_dedup_by_binding<T>(
	items: &mut Vec<T>,
	identity: impl Fn(&T) -> (&DefinitionId, &EmittedBindingName),
	same_output: impl Fn(&T, &T) -> bool,
) -> Result<(), ModuleLinkPlanError> {
	let mut index = 1;
	while index < items.len() {
		if !same_output(&items[index - 1], &items[index]) {
			index += 1;
			continue;
		}
		let (left_definition, left_binding) = identity(&items[index - 1]);
		let (right_definition, _) = identity(&items[index]);
		if left_definition != right_definition {
			return Err(ModuleLinkPlanError::BindingCollision {
				binding: left_binding.clone(),
				first: left_definition.clone(),
				second: right_definition.clone(),
			});
		}
		items.remove(index);
	}
	Ok(())
}

pub(crate) fn specifier_str(specifier: &CanonicalModuleSpecifier) -> &str {
	match specifier {
		CanonicalModuleSpecifier::Project(value)
		| CanonicalModuleSpecifier::Importable(value)
		| CanonicalModuleSpecifier::CompilerRuntime(value) => value,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use nymph_sema::{DeclarationCategory, DeclarationKey, ExternalCallable, ModuleOrigin};

	fn module(path: &str) -> ModuleIdentity {
		ModuleIdentity {
			origin: ModuleOrigin::Project("test".into()),
			project: "test".into(),
			path: path.into(),
		}
	}
	fn definition(module: &ModuleIdentity, name: &str) -> DefinitionId {
		DefinitionId::new(
			module.clone(),
			DeclarationKey::top_level(DeclarationCategory::Function, name),
		)
	}
	fn linked(module: &str, symbol: &str) -> ExternalAbi {
		ExternalAbi {
			marker: "js".into(),
			callable: ExternalCallable::Linked {
				adapter: nymph_sema::ExternalAdapterId {
					module: module.into(),
					symbol: symbol.into(),
				},
			},
			effects: nymph_sema::EffectRow::pure(),
			audit: nymph_sema::ExternalAudit::default(),
			call_mode: nymph_sema::ExternalCallMode::Ordinary,
			marshal: nymph_sema::ExternalMarshalPlan::default(),
		}
	}
	fn deferred() -> ExternalAbi {
		ExternalAbi {
			marker: "js".into(),
			callable: ExternalCallable::Deferred,
			effects: nymph_sema::EffectRow::pure(),
			audit: nymph_sema::ExternalAudit::default(),
			call_mode: nymph_sema::ExternalCallMode::Ordinary,
			marshal: nymph_sema::ExternalMarshalPlan::default(),
		}
	}
	struct Resolver {
		missing_binding: Option<DefinitionId>,
		missing_module: Option<ModuleIdentity>,
	}
	impl LinkNameResolver for Resolver {
		fn binding_name(
			&mut self,
			id: &DefinitionId,
		) -> Result<EmittedBindingName, StableNameLookupError> {
			if self.missing_binding.as_ref() == Some(id) {
				Err(StableNameLookupError::MissingBinding {
					definition: id.clone(),
				})
			} else {
				let DeclarationKey::TopLevel { name, .. } = &id.key else {
					unreachable!()
				};
				Ok(EmittedBindingName::new(name.clone()))
			}
		}
		fn module_specifier(
			&mut self,
			module: &ModuleIdentity,
		) -> Result<CanonicalModuleSpecifier, StableNameLookupError> {
			if self.missing_module.as_ref() == Some(module) {
				Err(StableNameLookupError::MissingModule {
					module: module.clone(),
				})
			} else {
				Ok(CanonicalModuleSpecifier::Project(module.path.clone()))
			}
		}
	}
	fn resolver() -> Resolver {
		Resolver {
			missing_binding: None,
			missing_module: None,
		}
	}

	#[test]
	fn project_imports_direct_and_excludes_routed_only() {
		let own = module("main");
		let foreign = module("dep");
		let root = definition(&own, "root");
		let direct = definition(&foreign, "direct");
		let routed = definition(&foreign, "routed");
		let fragments = [
			LinkFragment {
				definition: &root,
				artifact: LinkArtifact::TopLevel,
				direct_demands: std::slice::from_ref(&direct),
				routed_demands: std::slice::from_ref(&routed),
			},
			LinkFragment {
				definition: &direct,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &routed,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
		];
		let plan =
			plan_project_module(&own, &fragments, &HashSet::new(), false, &mut resolver()).unwrap();
		assert_eq!(
			plan
				.imports
				.iter()
				.map(|item| item.definition.clone())
				.collect::<Vec<_>>(),
			vec![direct]
		);
	}

	#[test]
	fn same_target_direct_and_routed_is_imported() {
		let own = module("main");
		let foreign = module("dep");
		let root = definition(&own, "root");
		let target = definition(&foreign, "target");
		let fragments = [
			LinkFragment {
				definition: &root,
				artifact: LinkArtifact::TopLevel,
				direct_demands: std::slice::from_ref(&target),
				routed_demands: std::slice::from_ref(&target),
			},
			LinkFragment {
				definition: &target,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
		];
		assert_eq!(
			plan_project_module(&own, &fragments, &HashSet::new(), false, &mut resolver())
				.unwrap()
				.imports
				.len(),
			1
		);
	}

	#[test]
	fn virtual_imports_reject_unavailable_direct_demands() {
		let own = module("runtime");
		let dep = module("dep");
		let a = definition(&dep, "a");
		let b = definition(&dep, "b");
		let unavailable = definition(&dep, "unavailable");
		let routed = definition(&dep, "routed");
		let root = definition(&own, "root");
		let fragments = [LinkFragment {
			definition: &root,
			artifact: LinkArtifact::TopLevel,
			direct_demands: &[b.clone(), unavailable.clone(), a.clone(), b.clone()],
			routed_demands: std::slice::from_ref(&routed),
		}];
		let deliveries = HashMap::from([
			(a, VirtualDemandDelivery::Binding),
			(b, VirtualDemandDelivery::Binding),
			(routed.clone(), VirtualDemandDelivery::Binding),
		]);
		assert!(matches!(
			plan_virtual_module(&own, &fragments, &deliveries, &mut resolver()),
			Err(ModuleLinkPlanError::UnresolvedDemand { definition }) if definition == unavailable
		));
	}

	#[test]
	fn virtual_imports_validate_but_do_not_import_attached_demands() {
		let own = module("runtime");
		let dep = module("dep");
		let attached = definition(&dep, "attached");
		let root = definition(&own, "root");
		let fragments = [LinkFragment {
			definition: &root,
			artifact: LinkArtifact::TopLevel,
			direct_demands: std::slice::from_ref(&attached),
			routed_demands: &[],
		}];
		let deliveries = HashMap::from([(attached.clone(), VirtualDemandDelivery::Attached)]);
		assert!(
			plan_virtual_module(&own, &fragments, &deliveries, &mut resolver())
				.unwrap()
				.imports
				.is_empty()
		);
	}

	#[test]
	fn distinct_exact_definitions_cannot_collapse_to_one_export() {
		let own = module("runtime");
		let function = definition(&own, "same");
		let value = DefinitionId::new(
			own.clone(),
			DeclarationKey::top_level(DeclarationCategory::Let, "same"),
		);
		let fragments = [
			LinkFragment {
				definition: &function,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &value,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
		];
		assert!(matches!(
			plan_virtual_module(&own, &fragments, &HashMap::new(), &mut resolver()),
			Err(ModuleLinkPlanError::BindingCollision { first, second, .. })
				if first == function && second == value
		));
	}

	#[test]
	fn project_exports_follow_public_preserve_and_external_policy() {
		let own = module("main");
		let dep = module("dep");
		let public = definition(&own, "public");
		let private = definition(&own, "private");
		let external = definition(&own, "external");
		let attached = definition(&own, "attached");
		let foreign = definition(&dep, "foreign");
		let deferred = deferred();
		let fragments = [
			LinkFragment {
				definition: &private,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &external,
				artifact: LinkArtifact::External(&deferred),
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &public,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &public,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &attached,
				artifact: LinkArtifact::Attached,
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &foreign,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
		];
		let public_ids = HashSet::from([public.clone()]);
		let exports = |preserve_names| {
			plan_project_module(
				&own,
				&fragments,
				&public_ids,
				preserve_names,
				&mut resolver(),
			)
			.unwrap()
			.exports
			.into_iter()
			.map(|export| export.binding.as_str().to_string())
			.collect::<Vec<_>>()
		};
		assert_eq!(exports(false), ["external", "public"]);
		assert_eq!(exports(true), ["external", "private", "public"]);
	}

	#[test]
	fn exports_are_sorted_and_deduplicated() {
		let own = module("main");
		let a = definition(&own, "a");
		let b = definition(&own, "b");
		let fragments = [
			LinkFragment {
				definition: &b,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &a,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &a,
				artifact: LinkArtifact::TopLevel,
				direct_demands: &[],
				routed_demands: &[],
			},
		];
		let plan = plan_virtual_module(&own, &fragments, &HashMap::new(), &mut resolver()).unwrap();
		assert_eq!(
			plan
				.exports
				.iter()
				.map(|export| export.binding.as_str())
				.collect::<Vec<_>>(),
			vec!["a", "b"]
		);
	}

	#[test]
	fn reports_exact_missing_binding_and_module() {
		let own = module("main");
		let dep = module("dep");
		let target = definition(&dep, "target");
		let fragment = [LinkFragment {
			definition: &target,
			artifact: LinkArtifact::TopLevel,
			direct_demands: &[],
			routed_demands: &[],
		}];
		let mut missing_binding = Resolver {
			missing_binding: Some(target.clone()),
			missing_module: None,
		};
		assert!(
			matches!(plan_project_module(&own, &fragment, &HashSet::new(), false, &mut missing_binding), Err(ModuleLinkPlanError::Binding { definition, source: StableNameLookupError::MissingBinding { definition: source } }) if definition == target && source == target)
		);
		let mut missing_module = Resolver {
			missing_binding: None,
			missing_module: Some(dep.clone()),
		};
		assert!(
			matches!(plan_project_module(&own, &fragment, &HashSet::new(), false, &mut missing_module), Err(ModuleLinkPlanError::Module { module, source: StableNameLookupError::MissingModule { module: source } }) if module == dep && source == dep)
		);
	}

	#[test]
	fn external_aliases_retain_typed_linked_abis_only() {
		let own = module("main");
		let linked_id = definition(&own, "linked");
		let deferred_id = definition(&own, "deferred");
		let linked_abi = linked("host", "call");
		let deferred_abi = deferred();
		let fragments = [
			LinkFragment {
				definition: &linked_id,
				artifact: LinkArtifact::External(&linked_abi),
				direct_demands: &[],
				routed_demands: &[],
			},
			LinkFragment {
				definition: &deferred_id,
				artifact: LinkArtifact::External(&deferred_abi),
				direct_demands: &[],
				routed_demands: &[],
			},
		];
		let plan =
			plan_project_module(&own, &fragments, &HashSet::new(), false, &mut resolver()).unwrap();
		assert_eq!(plan.external_aliases.len(), 1);
		assert_eq!(plan.external_aliases[0].abi, linked_abi);
	}
}
