use std::collections::{BTreeMap, HashMap, HashSet, btree_map::Entry};

use ecow::EcoString;
use nymph_hir::hir::{HirExpr, HirLet, HirMethod, HirModule};
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
	DuplicateRuntimeTypeAttachment {
		object: EcoString,
		name: EcoString,
	},
	MissingOwnerShell {
		owner: DefinitionId,
	},
	Template {
		definition: DefinitionId,
	},
	MissingExecutionBody {
		caller: DefinitionId,
		callee: DefinitionId,
	},
	InitializerCycle {
		cycle: Vec<DefinitionId>,
	},
	UnresolvedInitializerCall {
		initializer: DefinitionId,
		body: DefinitionId,
		call: nymph_sema::UnresolvedRuntimeCall,
	},
}

#[cfg(test)]
fn assemble_runtime_module<'a>(
	target: &ModuleIdentity,
	fragments: impl IntoIterator<Item = (DefinitionId, &'a LoweredRuntimeDefinition)>,
) -> Result<HirModule, RuntimeAssemblyError> {
	let fragments = fragments.into_iter().collect::<Vec<_>>();
	let execution_bodies = fragments.iter().map(|(_, lowered)| *lowered).collect();
	assemble_runtime_module_with_collected(target, fragments, execution_bodies)
}

pub(crate) fn assemble_runtime_module_with_execution<'a>(
	target: &ModuleIdentity,
	fragments: impl IntoIterator<Item = (DefinitionId, &'a LoweredRuntimeDefinition)>,
	execution_bodies: impl IntoIterator<Item = &'a LoweredRuntimeDefinition>,
) -> Result<HirModule, RuntimeAssemblyError> {
	assemble_runtime_module_with_collected(
		target,
		fragments.into_iter().collect(),
		execution_bodies.into_iter().collect(),
	)
}

fn assemble_runtime_module_with_collected(
	target: &ModuleIdentity,
	fragments: Vec<(DefinitionId, &LoweredRuntimeDefinition)>,
	execution_bodies: Vec<&LoweredRuntimeDefinition>,
) -> Result<HirModule, RuntimeAssemblyError> {
	for (supplied, lowered) in &fragments {
		if supplied != lowered.definition() {
			return Err(RuntimeAssemblyError::DefinitionMismatch {
				supplied: supplied.clone(),
				lowered: lowered.definition().clone(),
			});
		}
		validate_fragment_intrinsic(lowered)?;
	}
	let value_order = order_top_level_values(&fragments, &execution_bodies)?;
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
	let mut runtime_type_attachments = Vec::new();
	let mut runtime_type_attachment_selectors = HashSet::new();
	let mut top_level_values = HashMap::new();
	for (_, lowered) in fragments {
		use LoweredHirFragment as Fragment;
		match lowered.fragment() {
			Fragment::TopLevelFunction(value) => {
				validate_module(target, lowered)?;
				hir.funcs.push(value.clone());
			}
			Fragment::TopLevelValue(value) => {
				validate_module(target, lowered)?;
				top_level_values.insert(lowered.definition().clone(), value.clone());
			}
			Fragment::TopLevelExternal { function, .. } => {
				validate_module(target, lowered)?;
				if let Some(function) = function {
					hir.funcs.push(function.clone());
				}
			}
			Fragment::RuntimeTypeAttachment {
				object,
				function,
				method,
			} => {
				validate_module(target, lowered)?;
				let selector = (format!("{object:?}"), method.name.clone());
				if !runtime_type_attachment_selectors.insert(selector.clone()) {
					return Err(RuntimeAssemblyError::DuplicateRuntimeTypeAttachment {
						object: selector.0.into(),
						name: selector.1,
					});
				}
				if let Some(function) = function {
					hir.funcs.push(function.clone());
				}
				let attachment_index = runtime_type_attachments.len();
				runtime_type_attachments.push(HirLet {
					name: format!("$attach${attachment_index}").into(),
					value: HirExpr::RuntimeTypeAttachment {
						object: Box::new(object.clone()),
						method: Box::new(method.clone()),
					},
				});
			}
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
	hir.lets = runtime_type_attachments;
	hir.lets.extend(
		value_order
			.into_iter()
			.map(|definition| {
				top_level_values
					.remove(&definition)
					.expect("ordered top-level value must exist")
			})
			.collect::<Vec<_>>(),
	);
	Ok(hir)
}

fn order_top_level_values(
	fragments: &[(DefinitionId, &LoweredRuntimeDefinition)],
	execution_bodies: &[&LoweredRuntimeDefinition],
) -> Result<Vec<DefinitionId>, RuntimeAssemblyError> {
	let by_definition = execution_bodies
		.iter()
		.map(|lowered| (lowered.definition().clone(), *lowered))
		.collect::<HashMap<_, _>>();
	let values = fragments
		.iter()
		.filter(|(_, lowered)| matches!(lowered.fragment(), LoweredHirFragment::TopLevelValue(_)))
		.map(|(definition, _)| definition.clone())
		.collect::<Vec<_>>();
	let value_set = values.iter().cloned().collect::<HashSet<_>>();
	let mut prerequisites = HashMap::<DefinitionId, HashSet<DefinitionId>>::new();
	for value in &values {
		let Some(root) = by_definition.get(value).copied() else {
			return Err(RuntimeAssemblyError::MissingExecutionBody {
				caller: value.clone(),
				callee: value.clone(),
			});
		};
		let required = prerequisites.entry(value.clone()).or_default();
		let mut bodies = vec![(
			value.clone(),
			root.execution_summary(),
			Vec::<&nymph_sema::RuntimeExecutionSummary>::new(),
		)];
		let mut visited = HashSet::new();
		while let Some((body_definition, execution, mut callables)) = bodies.pop() {
			for read in execution.immediate_reads() {
				if let Some(invocation) = by_definition.get(read).and_then(|body| body.invocation())
					&& !callables
						.iter()
						.any(|known| std::ptr::eq(*known, invocation))
				{
					callables.push(invocation);
				}
			}
			for closure in execution.closures() {
				if !callables.iter().any(|known| std::ptr::eq(*known, closure)) {
					callables.push(closure);
				}
			}
			let callable_key = callables
				.iter()
				.map(|summary| *summary as *const _)
				.collect::<Vec<_>>();
			if !visited.insert((body_definition.clone(), execution as *const _, callable_key)) {
				continue;
			}
			required.extend(
				execution
					.immediate_reads()
					.iter()
					.filter(|read| value_set.contains(*read))
					.cloned(),
			);
			for call in execution.unresolved_calls() {
				match call {
					// Registered externals are host/runtime leaves: they cannot read a
					// Nymph module binding and therefore add no initializer dependency.
					nymph_sema::UnresolvedRuntimeCall::OpaqueExternal(_) => {}
					nymph_sema::UnresolvedRuntimeCall::CallableValue(callee) => {
						let Some(body) = by_definition.get(callee).copied() else {
							return Err(RuntimeAssemblyError::MissingExecutionBody {
								caller: value.clone(),
								callee: callee.clone(),
							});
						};
						let Some(invocation) = body.invocation() else {
							return Err(RuntimeAssemblyError::UnresolvedInitializerCall {
								initializer: value.clone(),
								body: body_definition.clone(),
								call: call.clone(),
							});
						};
						bodies.push((callee.clone(), invocation, callables.clone()));
					}
					nymph_sema::UnresolvedRuntimeCall::DynamicCallee if callables.len() == 1 => {
						bodies.push((body_definition.clone(), callables[0], Vec::new()));
					}
					_ => {
						return Err(RuntimeAssemblyError::UnresolvedInitializerCall {
							initializer: value.clone(),
							body: body_definition.clone(),
							call: call.clone(),
						});
					}
				}
			}
			for callee in execution.immediate_calls() {
				let Some(body) = by_definition.get(callee).copied() else {
					return Err(RuntimeAssemblyError::MissingExecutionBody {
						caller: value.clone(),
						callee: callee.clone(),
					});
				};
				bodies.push((callee.clone(), body.execution_summary(), callables.clone()));
			}
		}
	}

	let mut ordered = Vec::with_capacity(values.len());
	let mut emitted = HashSet::new();
	let mut remaining = values;
	while !remaining.is_empty() {
		let Some(index) = remaining.iter().position(|definition| {
			prerequisites[definition]
				.iter()
				.all(|dependency| emitted.contains(dependency))
		}) else {
			let cycle = initializer_cycle_witness(&remaining, &prerequisites)
				.expect("stalled initializer graph must contain a cycle");
			return Err(RuntimeAssemblyError::InitializerCycle { cycle });
		};
		let definition = remaining.remove(index);
		emitted.insert(definition.clone());
		ordered.push(definition);
	}
	Ok(ordered)
}

fn initializer_cycle_witness(
	remaining: &[DefinitionId],
	prerequisites: &HashMap<DefinitionId, HashSet<DefinitionId>>,
) -> Option<Vec<DefinitionId>> {
	fn visit(
		definition: &DefinitionId,
		remaining: &[DefinitionId],
		prerequisites: &HashMap<DefinitionId, HashSet<DefinitionId>>,
		states: &mut HashMap<DefinitionId, u8>,
		stack: &mut Vec<DefinitionId>,
	) -> Option<Vec<DefinitionId>> {
		states.insert(definition.clone(), 1);
		stack.push(definition.clone());
		for dependency in remaining
			.iter()
			.filter(|candidate| prerequisites[definition].contains(*candidate))
		{
			match states.get(dependency).copied().unwrap_or_default() {
				0 => {
					if let Some(cycle) = visit(dependency, remaining, prerequisites, states, stack) {
						return Some(cycle);
					}
				}
				1 => {
					let start = stack.iter().position(|item| item == dependency).unwrap();
					let mut cycle = stack[start..].to_vec();
					cycle.push(dependency.clone());
					return Some(cycle);
				}
				_ => {}
			}
		}
		stack.pop();
		states.insert(definition.clone(), 2);
		None
	}

	let mut states = HashMap::new();
	let mut stack = Vec::new();
	for definition in remaining {
		if states.get(definition).copied().unwrap_or_default() == 0
			&& let Some(cycle) = visit(
				definition,
				remaining,
				prerequisites,
				&mut states,
				&mut stack,
			) {
			return Some(cycle);
		}
	}
	None
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
		| Fragment::EnumShell(_)
		| Fragment::RuntimeTypeAttachment { .. } => match lowered.placement() {
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
		DeclarationKey::Implementation { header, .. } => match &header.self_type {
			HeaderType::Named { definition, .. } => Some(definition),
			_ => None,
		},
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
	use std::collections::{HashMap, HashSet};

	use nymph_hir::hir::{HirClass, HirExpr, HirMethod};
	use nymph_sema::{
		DeclarationCategory, DeclarationKey, DefinitionId, HeaderType, ImplementationHeader,
		LoweredHirFragment, LoweredRuntimeDefinition, ModuleIdentity, ModuleOrigin,
		RuntimeAssemblyPlacement, StableDemandSet,
	};

	use super::{
		RuntimeAssemblyError, assemble_runtime_module, assemble_runtime_module_with_execution,
		initializer_cycle_witness, insert_exact_virtual_fragment,
	};

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
			defaults: vec![],
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
	fn initializer_cycle_witness_excludes_acyclic_dependents() {
		let target = module("target");
		let a = definition(&target, DeclarationCategory::Let, "a");
		let b = definition(&target, DeclarationCategory::Let, "b");
		let blocked = definition(&target, DeclarationCategory::Let, "blocked");
		let prerequisites = HashMap::from([
			(a.clone(), HashSet::from([b.clone()])),
			(b.clone(), HashSet::from([a.clone()])),
			(blocked.clone(), HashSet::from([a.clone()])),
		]);

		assert_eq!(
			initializer_cycle_witness(&[blocked, a.clone(), b.clone()], &prerequisites),
			Some(vec![a.clone(), b, a])
		);
	}

	#[test]
	fn missing_initializer_execution_body_is_a_typed_error() {
		let target = module("target");
		let value = definition(&target, DeclarationCategory::Let, "value");
		let fragment = lowered(
			value.clone(),
			LoweredHirFragment::TopLevelValue(nymph_hir::hir::HirLet {
				name: "value".into(),
				value: HirExpr::Bool(true),
			}),
			RuntimeAssemblyPlacement::Module(target.clone()),
		);

		assert!(matches!(
			assemble_runtime_module_with_execution(
				&target,
				[(value.clone(), &fragment)],
				std::iter::empty(),
			),
			Err(RuntimeAssemblyError::MissingExecutionBody { caller, callee })
				if caller == value && callee == value
		));
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
				self_type: HeaderType::Named {
					definition: owner.clone(),
					positional: vec![],
					named: vec![],
				},
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
