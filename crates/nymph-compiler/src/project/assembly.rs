use std::collections::{BTreeMap, HashMap, HashSet, btree_map::Entry};

use ecow::EcoString;
use nymph_hir::hir::{HirMethod, HirModule};
use nymph_sema::{
	DeclarationCategory, DeclarationKey, DefinitionId, HeaderType, LoweredHirFragment,
	LoweredRuntimeDefinition, ModuleIdentity, RuntimeAssemblyPlacement,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VirtualFragmentConflict {
	pub definition: DefinitionId,
}

pub(crate) fn insert_exact_virtual_fragment(
	fragments: &mut BTreeMap<DefinitionId, nymph_sema::VirtualRuntimeFragment>,
	fragment: nymph_sema::VirtualRuntimeFragment,
) -> Result<(), VirtualFragmentConflict> {
	match fragments.entry(fragment.definition.clone()) {
		Entry::Vacant(entry) => {
			entry.insert(fragment);
			Ok(())
		}
		Entry::Occupied(entry) if entry.get() == &fragment => Ok(()),
		Entry::Occupied(entry) => Err(VirtualFragmentConflict {
			definition: entry.key().clone(),
		}),
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeAssemblyError {
	DefinitionMismatch {
		supplied: DefinitionId,
		lowered: DefinitionId,
	},
	MismatchedModule {
		definition: DefinitionId,
		expected: ModuleIdentity,
		actual: ModuleIdentity,
	},
	MismatchedShell {
		definition: DefinitionId,
		placement_owner: DefinitionId,
	},
	DuplicateShell {
		owner: DefinitionId,
	},
	DuplicateAttachment {
		owner: DefinitionId,
		name: EcoString,
	},
	MissingOwnerShell {
		owner: DefinitionId,
	},
	Template {
		definition: DefinitionId,
	},
}

pub(crate) fn assemble_runtime_module<'a>(
	target: &ModuleIdentity,
	fragments: impl IntoIterator<Item = (DefinitionId, &'a LoweredRuntimeDefinition)>,
) -> Result<HirModule, RuntimeAssemblyError> {
	let fragments = fragments.into_iter().collect::<Vec<_>>();
	for (supplied, lowered) in &fragments {
		if supplied != lowered.definition() {
			return Err(RuntimeAssemblyError::DefinitionMismatch {
				supplied: supplied.clone(),
				lowered: lowered.definition().clone(),
			});
		}
		validate_fragment_intrinsic(lowered)?;
	}
	let mut hir = HirModule {
		lets: vec![],
		funcs: vec![],
		classes: vec![],
		enums: vec![],
	};
	let mut shells = HashMap::new();
	for (_, lowered) in &fragments {
		match lowered.fragment() {
			LoweredHirFragment::StructShell(_) | LoweredHirFragment::EnumShell(_) => {
				let RuntimeAssemblyPlacement::Module(actual) = lowered.placement() else {
					return placement_error(lowered);
				};
				if actual != target {
					return Err(RuntimeAssemblyError::MismatchedModule {
						definition: lowered.definition().clone(),
						expected: target.clone(),
						actual: actual.clone(),
					});
				}
				if shells.contains_key(lowered.definition()) {
					return Err(RuntimeAssemblyError::DuplicateShell {
						owner: lowered.definition().clone(),
					});
				}
				match lowered.fragment() {
					LoweredHirFragment::StructShell(value) => {
						shells.insert(lowered.definition().clone(), (true, hir.classes.len()));
						hir.classes.push(value.clone());
					}
					LoweredHirFragment::EnumShell(value) => {
						shells.insert(lowered.definition().clone(), (false, hir.enums.len()));
						hir.enums.push(value.clone());
					}
					_ => unreachable!(),
				}
			}
			_ => {}
		}
	}
	let mut attachments = HashSet::new();
	for (_, lowered) in fragments {
		use LoweredHirFragment as Fragment;
		match lowered.fragment() {
			Fragment::TopLevelFunction(value) => {
				validate_module(target, lowered)?;
				hir.funcs.push(value.clone());
			}
			Fragment::TopLevelValue(value) => {
				validate_module(target, lowered)?;
				hir.lets.push(value.clone());
			}
			Fragment::TopLevelExternal { .. } => validate_module(target, lowered)?,
			Fragment::AttachedInstance { owner, method }
			| Fragment::AttachedMember { owner, method }
			| Fragment::MaterializedDefault { owner, method, .. } => attach(
				target,
				&mut hir,
				&shells,
				&mut attachments,
				lowered,
				owner,
				method,
				false,
			)?,
			Fragment::AttachedStatic { owner, method } => attach(
				target,
				&mut hir,
				&shells,
				&mut attachments,
				lowered,
				owner,
				method,
				true,
			)?,
			Fragment::StructShell(_) | Fragment::EnumShell(_) => {}
		}
	}
	Ok(hir)
}

fn validate_module(
	target: &ModuleIdentity,
	lowered: &LoweredRuntimeDefinition,
) -> Result<(), RuntimeAssemblyError> {
	match lowered.placement() {
		RuntimeAssemblyPlacement::Module(actual) if actual == target => Ok(()),
		RuntimeAssemblyPlacement::Module(actual) => Err(RuntimeAssemblyError::MismatchedModule {
			definition: lowered.definition().clone(),
			expected: target.clone(),
			actual: actual.clone(),
		}),
		_ => placement_error(lowered),
	}
}

/// Validates identity/placement facts carried by a fragment itself, without
/// assuming which physical module is currently being assembled.
pub(crate) fn validate_fragment_intrinsic(
	lowered: &LoweredRuntimeDefinition,
) -> Result<(), RuntimeAssemblyError> {
	use LoweredHirFragment as Fragment;
	match lowered.fragment() {
		Fragment::TopLevelFunction(_)
		| Fragment::TopLevelValue(_)
		| Fragment::TopLevelExternal { .. }
		| Fragment::StructShell(_)
		| Fragment::EnumShell(_) => match lowered.placement() {
			RuntimeAssemblyPlacement::Module(actual) if actual == &lowered.definition().module => Ok(()),
			_ => placement_error(lowered),
		},
		Fragment::AttachedInstance { owner, .. }
		| Fragment::AttachedStatic { owner, .. }
		| Fragment::AttachedMember { owner, .. } => validate_shell_owner(lowered, owner),
		Fragment::MaterializedDefault {
			owner,
			implementation,
			interface_member,
			..
		} => {
			let DeclarationKey::MaterializedInterfaceMember {
				implementation: key_implementation,
				interface_member: key_member,
			} = &lowered.definition().key
			else {
				return mismatched_shell(lowered);
			};
			if owner != implementation
				|| implementation != key_implementation.as_ref()
				|| interface_member != key_member.as_ref()
			{
				return mismatched_shell(lowered);
			}
			validate_shell_owner(lowered, implementation)
		}
	}
}

fn validate_shell_owner(
	lowered: &LoweredRuntimeDefinition,
	semantic_owner: &DefinitionId,
) -> Result<(), RuntimeAssemblyError> {
	let RuntimeAssemblyPlacement::Shell(placement_owner) = lowered.placement() else {
		return placement_error(lowered);
	};
	if resolve_shell(semantic_owner).is_some_and(|owner| owner == placement_owner) {
		Ok(())
	} else {
		mismatched_shell(lowered)
	}
}

fn resolve_shell(owner: &DefinitionId) -> Option<&DefinitionId> {
	match &owner.key {
		DeclarationKey::TopLevel {
			category: DeclarationCategory::Struct | DeclarationCategory::Enum,
			..
		} => Some(owner),
		DeclarationKey::Implementation { header, .. } => {
			let mut self_type = &header.self_type;
			while let HeaderType::Mutable(inner) = self_type {
				self_type = inner;
			}
			match self_type {
				HeaderType::Named { definition, .. } => Some(definition),
				_ => None,
			}
		}
		_ => None,
	}
}

fn mismatched_shell<T>(lowered: &LoweredRuntimeDefinition) -> Result<T, RuntimeAssemblyError> {
	let placement_owner = match lowered.placement() {
		RuntimeAssemblyPlacement::Shell(owner) => owner.clone(),
		_ => return placement_error(lowered),
	};
	Err(RuntimeAssemblyError::MismatchedShell {
		definition: lowered.definition().clone(),
		placement_owner,
	})
}

fn placement_error<T>(lowered: &LoweredRuntimeDefinition) -> Result<T, RuntimeAssemblyError> {
	match lowered.placement() {
		RuntimeAssemblyPlacement::Template => Err(RuntimeAssemblyError::Template {
			definition: lowered.definition().clone(),
		}),
		RuntimeAssemblyPlacement::Shell(owner) => Err(RuntimeAssemblyError::MismatchedShell {
			definition: lowered.definition().clone(),
			placement_owner: owner.clone(),
		}),
		RuntimeAssemblyPlacement::Module(actual) => Err(RuntimeAssemblyError::MismatchedModule {
			definition: lowered.definition().clone(),
			expected: lowered.definition().module.clone(),
			actual: actual.clone(),
		}),
	}
}

fn attach(
	target: &ModuleIdentity,
	hir: &mut HirModule,
	shells: &HashMap<DefinitionId, (bool, usize)>,
	seen: &mut HashSet<(DefinitionId, DefinitionId, bool)>,
	lowered: &LoweredRuntimeDefinition,
	_fragment_owner: &DefinitionId,
	method: &HirMethod,
	static_: bool,
) -> Result<(), RuntimeAssemblyError> {
	let RuntimeAssemblyPlacement::Shell(placement_owner) = lowered.placement() else {
		return placement_error(lowered);
	};
	if &placement_owner.module != target {
		return Err(RuntimeAssemblyError::MismatchedModule {
			definition: lowered.definition().clone(),
			expected: target.clone(),
			actual: placement_owner.module.clone(),
		});
	}
	let (class, index) = shells.get(placement_owner).copied().ok_or_else(|| {
		RuntimeAssemblyError::MissingOwnerShell {
			owner: placement_owner.clone(),
		}
	})?;
	if !seen.insert((
		placement_owner.clone(),
		lowered.definition().clone(),
		static_,
	)) {
		return Err(RuntimeAssemblyError::DuplicateAttachment {
			owner: placement_owner.clone(),
			name: method.name.clone(),
		});
	}
	let methods = match (class, static_) {
		(true, true) => &mut hir.classes[index].statics,
		(true, false) => &mut hir.classes[index].methods,
		(false, true) => &mut hir.enums[index].statics,
		(false, false) => &mut hir.enums[index].methods,
	};
	methods.push(method.clone());
	Ok(())
}

#[cfg(test)]
mod tests {
	use nymph_hir::hir::{HirClass, HirExpr, HirMethod};
	use nymph_sema::{
		DeclarationCategory, DeclarationKey, DefinitionId, HeaderType, ImplementationHeader,
		LoweredHirFragment, LoweredRuntimeDefinition, ModuleIdentity, ModuleOrigin,
		RuntimeAssemblyPlacement, StableDemandSet,
	};

	use super::{RuntimeAssemblyError, assemble_runtime_module, insert_exact_virtual_fragment};

	fn module(path: &str) -> ModuleIdentity {
		ModuleIdentity {
			origin: ModuleOrigin::Project("assembly-test".into()),
			project: "assembly-test".into(),
			path: path.into(),
		}
	}

	fn definition(
		module: &ModuleIdentity,
		category: DeclarationCategory,
		name: &str,
	) -> DefinitionId {
		DefinitionId::new(module.clone(), DeclarationKey::top_level(category, name))
	}

	fn lowered(
		definition: DefinitionId,
		fragment: LoweredHirFragment,
		placement: RuntimeAssemblyPlacement,
	) -> LoweredRuntimeDefinition {
		LoweredRuntimeDefinition::new(definition, fragment, StableDemandSet::new(), placement)
	}

	fn shell(name: &str) -> HirClass {
		HirClass {
			name: name.into(),
			fields: vec![],
			methods: vec![],
			statics: vec![],
		}
	}

	fn method(name: &str) -> HirMethod {
		HirMethod {
			name: name.into(),
			params: vec![],
			body: HirExpr::Bool(true),
		}
	}

	#[test]
	fn project_and_virtual_inputs_attach_to_the_exact_shell_identically() {
		let target = module("target");
		let owner = definition(&target, DeclarationCategory::Struct, "Box");
		let member = DefinitionId::new(
			target.clone(),
			DeclarationKey::member(owner.clone(), DeclarationCategory::Method, "get"),
		);
		let fragments = vec![
			lowered(
				owner.clone(),
				LoweredHirFragment::StructShell(shell("Box")),
				RuntimeAssemblyPlacement::Module(target.clone()),
			),
			lowered(
				member,
				LoweredHirFragment::AttachedInstance {
					owner: owner.clone(),
					method: method("get"),
				},
				RuntimeAssemblyPlacement::Shell(owner),
			),
		];
		let project = assemble_runtime_module(
			&target,
			fragments
				.iter()
				.map(|item| (item.definition().clone(), item)),
		);
		let virtual_ = assemble_runtime_module(
			&target,
			fragments
				.iter()
				.map(|item| (item.definition().clone(), item)),
		);
		assert_eq!(project.unwrap(), virtual_.unwrap());
	}

	#[test]
	fn implementation_owner_resolves_to_its_nominal_shell() {
		let target = module("target");
		let owner = definition(&target, DeclarationCategory::Struct, "Box");
		let implementation = DefinitionId::new(
			target.clone(),
			DeclarationKey::implementation(ImplementationHeader {
				interface: None,
				interface_arguments: vec![],
				self_type: HeaderType::Mutable(Box::new(HeaderType::Named {
					definition: owner.clone(),
					positional: vec![],
					named: vec![],
				})),
				mutable: true,
				binders: vec![],
				constraints: vec![],
			}),
		);
		let attached = lowered(
			definition(&target, DeclarationCategory::Function, "attached"),
			LoweredHirFragment::AttachedInstance {
				owner: implementation.clone(),
				method: method("get"),
			},
			RuntimeAssemblyPlacement::Shell(owner.clone()),
		);
		let interface = definition(&target, DeclarationCategory::Interface, "Readable");
		let interface_member = DefinitionId::new(
			target.clone(),
			DeclarationKey::member(interface, DeclarationCategory::Method, "default"),
		);
		let materialized_id = DefinitionId::new(
			target.clone(),
			DeclarationKey::materialized_interface_member(
				implementation.clone(),
				interface_member.clone(),
			),
		);
		let materialized = lowered(
			materialized_id,
			LoweredHirFragment::MaterializedDefault {
				owner: implementation.clone(),
				implementation,
				interface_member,
				method: method("default"),
			},
			RuntimeAssemblyPlacement::Shell(owner.clone()),
		);
		let shell = lowered(
			owner.clone(),
			LoweredHirFragment::StructShell(shell("Box")),
			RuntimeAssemblyPlacement::Module(target.clone()),
		);
		assert!(
			assemble_runtime_module(
				&target,
				[
					(owner, &shell),
					(attached.definition().clone(), &attached),
					(materialized.definition().clone(), &materialized),
				]
			)
			.is_ok()
		);
	}

	#[test]
	fn rejects_top_level_definition_module_even_when_placement_matches_target() {
		let target = module("target");
		let other = module("other");
		let id = definition(&other, DeclarationCategory::Function, "foreign");
		let fragment = lowered(
			id.clone(),
			LoweredHirFragment::TopLevelFunction(nymph_hir::hir::HirFunc {
				name: "foreign".into(),
				params: vec![],
				body: HirExpr::Bool(true),
			}),
			RuntimeAssemblyPlacement::Module(target.clone()),
		);
		assert!(matches!(
			assemble_runtime_module(&target, [(id, &fragment)]),
			Err(RuntimeAssemblyError::MismatchedModule { expected, actual, .. })
				if expected == other && actual == target
		));
	}

	#[test]
	fn rejects_wrong_module_duplicate_shell_missing_shell_template_and_unhandled_fragments() {
		let target = module("target");
		let other = module("other");
		let owner = definition(&target, DeclarationCategory::Struct, "Box");
		let missing_owner = definition(&target, DeclarationCategory::Struct, "Missing");
		let wrong_module = lowered(
			owner.clone(),
			LoweredHirFragment::StructShell(shell("Box")),
			RuntimeAssemblyPlacement::Module(other),
		);
		assert!(matches!(
			assemble_runtime_module(&target, [(owner.clone(), &wrong_module)]),
			Err(RuntimeAssemblyError::MismatchedModule { .. })
		));

		let first = lowered(
			owner.clone(),
			LoweredHirFragment::StructShell(shell("Box")),
			RuntimeAssemblyPlacement::Module(target.clone()),
		);
		let duplicate = lowered(
			owner.clone(),
			LoweredHirFragment::StructShell(shell("Again")),
			RuntimeAssemblyPlacement::Module(target.clone()),
		);
		assert!(matches!(
			assemble_runtime_module(
				&target,
				[(owner.clone(), &first), (owner.clone(), &duplicate)]
			),
			Err(RuntimeAssemblyError::DuplicateShell { .. })
		));

		let attachment_id = definition(&target, DeclarationCategory::Function, "attachment");
		let attachment = lowered(
			attachment_id.clone(),
			LoweredHirFragment::AttachedInstance {
				owner: missing_owner.clone(),
				method: method("get"),
			},
			RuntimeAssemblyPlacement::Shell(missing_owner.clone()),
		);
		assert!(
			matches!(assemble_runtime_module(&target, [(attachment_id, &attachment)]), Err(RuntimeAssemblyError::MissingOwnerShell { owner }) if owner == missing_owner)
		);
		let mismatched_owner_id = definition(
			&target,
			DeclarationCategory::Function,
			"mismatched-owner-attachment",
		);
		let mismatched_owner = lowered(
			mismatched_owner_id.clone(),
			LoweredHirFragment::AttachedInstance {
				owner: missing_owner,
				method: method("mismatched"),
			},
			RuntimeAssemblyPlacement::Shell(owner.clone()),
		);
		assert!(matches!(
			assemble_runtime_module(
				&target,
				[
					(owner.clone(), &first),
					(mismatched_owner_id, &mismatched_owner),
				],
			),
			Err(RuntimeAssemblyError::MismatchedShell { .. })
		));

		let template = lowered(
			owner.clone(),
			LoweredHirFragment::StructShell(shell("Box")),
			RuntimeAssemblyPlacement::Template,
		);
		assert!(matches!(
			assemble_runtime_module(&target, [(owner.clone(), &template)]),
			Err(RuntimeAssemblyError::Template { .. })
		));

		let mismatched_definition = definition(&target, DeclarationCategory::Struct, "Different");
		assert!(matches!(
			assemble_runtime_module(&target, [(mismatched_definition, &first)]),
			Err(RuntimeAssemblyError::DefinitionMismatch { .. })
		));
	}

	#[test]
	fn duplicate_attachment_rule_uses_exact_definition_and_distinguishes_staticness() {
		let target = module("target");
		let owner = definition(&target, DeclarationCategory::Struct, "Box");
		let shell = lowered(
			owner.clone(),
			LoweredHirFragment::StructShell(shell("Box")),
			RuntimeAssemblyPlacement::Module(target.clone()),
		);
		let a = definition(&target, DeclarationCategory::Function, "a");
		let b = definition(&target, DeclarationCategory::Function, "b");
		let instance = lowered(
			a.clone(),
			LoweredHirFragment::AttachedInstance {
				owner: owner.clone(),
				method: method("same"),
			},
			RuntimeAssemblyPlacement::Shell(owner.clone()),
		);
		let static_ = lowered(
			b.clone(),
			LoweredHirFragment::AttachedStatic {
				owner: owner.clone(),
				method: method("same"),
			},
			RuntimeAssemblyPlacement::Shell(owner.clone()),
		);
		assert!(
			assemble_runtime_module(
				&target,
				[
					(owner.clone(), &shell),
					(a.clone(), &instance),
					(b.clone(), &static_)
				]
			)
			.is_ok()
		);
		let same_name_distinct_definition = lowered(
			b.clone(),
			LoweredHirFragment::AttachedInstance {
				owner: owner.clone(),
				method: method("same"),
			},
			RuntimeAssemblyPlacement::Shell(owner.clone()),
		);
		assert!(
			assemble_runtime_module(
				&target,
				[
					(owner.clone(), &shell),
					(a.clone(), &instance),
					(b.clone(), &same_name_distinct_definition),
				]
			)
			.is_ok()
		);
		let duplicate = lowered(
			a.clone(),
			LoweredHirFragment::AttachedInstance {
				owner: owner.clone(),
				method: method("same"),
			},
			RuntimeAssemblyPlacement::Shell(owner.clone()),
		);
		assert!(matches!(
			assemble_runtime_module(
				&target,
				[(owner, &shell), (a.clone(), &instance), (a, &duplicate)]
			),
			Err(RuntimeAssemblyError::DuplicateAttachment { .. })
		));
	}

	#[test]
	fn exact_virtual_fragment_dedup_accepts_identical_and_rejects_every_difference() {
		let owner = module("runtime");
		let definition = definition(&owner, DeclarationCategory::Struct, "Box");
		let runtime_definition = lowered(
			definition.clone(),
			LoweredHirFragment::StructShell(shell("Box")),
			RuntimeAssemblyPlacement::Module(owner.clone()),
		);
		let fragment = nymph_sema::VirtualRuntimeFragment {
			owner: owner.clone(),
			definition: definition.clone(),
			fragment: runtime_definition,
		};
		let mut fragments = std::collections::BTreeMap::new();
		assert!(insert_exact_virtual_fragment(&mut fragments, fragment.clone()).is_ok());
		assert!(insert_exact_virtual_fragment(&mut fragments, fragment.clone()).is_ok());
		assert_eq!(fragments.len(), 1);

		let mut differing_owner = fragment.clone();
		differing_owner.owner = module("other-runtime");
		assert!(insert_exact_virtual_fragment(&mut fragments, differing_owner).is_err());

		let mut differing_placement = fragment.clone();
		differing_placement.fragment = lowered(
			definition.clone(),
			LoweredHirFragment::StructShell(shell("Box")),
			RuntimeAssemblyPlacement::Module(module("other-runtime")),
		);
		assert!(insert_exact_virtual_fragment(&mut fragments, differing_placement).is_err());

		let mut differing_payload = fragment;
		differing_payload.fragment = lowered(
			definition,
			LoweredHirFragment::StructShell(shell("Different")),
			RuntimeAssemblyPlacement::Module(owner),
		);
		assert!(insert_exact_virtual_fragment(&mut fragments, differing_payload).is_err());
	}
}
