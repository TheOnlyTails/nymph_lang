use std::{collections::HashMap, sync::Arc};

use nymph_sema::{
	CanonicalModuleSpecifier, DeclarationKey, DefinitionId, EmittedBindingName, EmittedMemberName,
	ModuleIdentity, ModuleOrigin, RuntimeDefinition, RuntimeDefinitionLookup,
	RuntimeDefinitionLookupError, StableNameLookup, StableNameLookupError, StableShapeFact,
	StableShapeLookup, StableShapeLookupError, StableShapeRequest, lower_runtime_definition,
};

#[derive(Default)]
struct Context {
	names: HashMap<DefinitionId, EmittedBindingName>,
	members: HashMap<DefinitionId, EmittedMemberName>,
	runtime: HashMap<DefinitionId, Arc<RuntimeDefinition>>,
	shapes: HashMap<StableShapeRequest, StableShapeFact>,
}

impl RuntimeDefinitionLookup for Context {
	fn runtime_definition(
		&self,
		definition: &DefinitionId,
	) -> Result<Arc<RuntimeDefinition>, RuntimeDefinitionLookupError> {
		self
			.runtime
			.get(definition)
			.cloned()
			.ok_or_else(|| RuntimeDefinitionLookupError::Missing {
				definition: definition.clone(),
			})
	}
}
impl StableShapeLookup for Context {
	fn stable_shape(
		&self,
		request: &StableShapeRequest,
	) -> Result<StableShapeFact, StableShapeLookupError> {
		self
			.shapes
			.get(request)
			.cloned()
			.ok_or_else(|| StableShapeLookupError::Missing {
				request: request.clone(),
			})
	}
}
impl StableNameLookup for Context {
	fn binding_name(
		&self,
		definition: &DefinitionId,
	) -> Result<EmittedBindingName, StableNameLookupError> {
		self
			.names
			.get(definition)
			.cloned()
			.ok_or_else(|| StableNameLookupError::MissingBinding {
				definition: definition.clone(),
			})
	}
	fn member_name(
		&self,
		definition: &DefinitionId,
	) -> Result<EmittedMemberName, StableNameLookupError> {
		self
			.members
			.get(definition)
			.cloned()
			.ok_or_else(|| StableNameLookupError::MissingMember {
				definition: definition.clone(),
			})
	}
	fn module_specifier(
		&self,
		module: &ModuleIdentity,
	) -> Result<CanonicalModuleSpecifier, StableNameLookupError> {
		Ok(CanonicalModuleSpecifier::Project(module.path.clone()))
	}
}

fn lower_fixture(source: &str) -> nymph_sema::LoweredRuntimeDefinition {
	let mut items = artifacts(source);
	let item = items.remove(0);
	let mut context = Context::default();
	context
		.names
		.insert(item.definition.clone(), EmittedBindingName::new("fixture"));
	if let nymph_sema::RuntimePayload::NymphBody(body) = &item.payload {
		for (_, target) in body.annotations.definition_targets.iter() {
			context
				.names
				.insert(target.clone(), EmittedBindingName::new("target"));
			context
				.members
				.insert(target.clone(), EmittedMemberName::new("member"));
		}
		for (_, dispatch) in body.annotations.dispatches.iter() {
			let member = match dispatch {
				nymph_sema::StableDispatch::Builtin { .. } => None,
				nymph_sema::StableDispatch::Direct { member, .. }
				| nymph_sema::StableDispatch::SelectedImplementation { member, .. }
				| nymph_sema::StableDispatch::InterfaceDefault { member, .. }
				| nymph_sema::StableDispatch::GenericBound { member, .. }
				| nymph_sema::StableDispatch::External { member, .. } => Some(member),
			};
			if let Some(member) = member {
				context
					.members
					.insert(member.clone(), EmittedMemberName::new("member"));
			}
		}
	}
	lower_runtime_definition(&context, Arc::new(item)).unwrap()
}

fn source_name(id: &DefinitionId) -> &str {
	match &id.key {
		DeclarationKey::TopLevel { name, .. }
		| DeclarationKey::Member { name, .. }
		| DeclarationKey::MethodBody { name, .. } => name,
		DeclarationKey::MaterializedInterfaceMember {
			interface_member, ..
		} => source_name(interface_member),
		DeclarationKey::Implementation { .. } | DeclarationKey::RecoveredImplementation { .. } => {
			"implementation"
		}
	}
}

fn lower_named(source: &str, wanted: &str) -> nymph_sema::LoweredRuntimeDefinition {
	let (items, _, mut context) = materialized_fixture(source);
	let item = items
		.into_iter()
		.find(|item| source_name(&item.definition) == wanted)
		.unwrap_or_else(|| panic!("missing runtime artifact {wanted}"));
	context.names.insert(
		item.definition.clone(),
		EmittedBindingName::new(source_name(&item.definition)),
	);
	lower_runtime_definition(&context, Arc::new(item)).unwrap()
}

fn artifacts(source: &str) -> Vec<RuntimeDefinition> {
	artifacts_and_interface(source).0
}

fn artifacts_and_interface(source: &str) -> (Vec<RuntimeDefinition>, nymph_sema::ModuleInterface) {
	let parsed = nymph_syntax::parse_module(source, "fixture.nym");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let module = Arc::new(parsed.tree);
	let identity = ModuleIdentity {
		origin: ModuleOrigin::Project("test".into()),
		project: "test".into(),
		path: "main".into(),
	};
	let environment = nymph_sema::SemanticEnvironment::from_modules(identity.clone(), &[]).unwrap();
	let checked = nymph_sema::check_module_with_environment(
		module.clone(),
		identity.clone(),
		&environment,
		nymph_sema::EntryMode::Library,
	);
	assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
	let facts = nymph_sema::Checked {
		diags: vec![],
		facts: checked.analysis.checked.as_ref().clone(),
	};
	let headers = nymph_sema::declared_headers(identity.clone(), &module);
	let interface =
		nymph_sema::extract_module_interface(identity, &module, &facts, &headers).unwrap();
	let artifacts =
		nymph_sema::runtime_definitions(&module, source, &facts.facts, &interface).unwrap();
	(artifacts, interface)
}

fn materialized_fixture(
	source: &str,
) -> (Vec<RuntimeDefinition>, nymph_sema::ModuleInterface, Context) {
	let (artifacts, interface) = artifacts_and_interface(source);
	let mut context = Context::default();
	for artifact in &artifacts {
		context
			.runtime
			.insert(artifact.definition.clone(), Arc::new(artifact.clone()));
		context.names.insert(
			artifact.definition.clone(),
			EmittedBindingName::new(source_name(&artifact.definition)),
		);
		context.members.insert(
			artifact.definition.clone(),
			EmittedMemberName::new(source_name(&artifact.definition)),
		);
	}
	for implementation in &interface.implementations {
		context.shapes.insert(
			StableShapeRequest::Implementation(implementation.id.clone()),
			StableShapeFact::Implementation(implementation.clone()),
		);
		for slot in &implementation.member_slots {
			context.members.insert(
				slot.member_id.clone(),
				EmittedMemberName::new(slot.name.clone()),
			);
		}
	}
	for definition in interface.exports.iter().chain(
		interface
			.support_definitions
			.iter()
			.map(|item| &item.definition),
	) {
		if definition.kind == nymph_sema::DefinitionShapeKind::Interface {
			for member in &definition.members {
				context.members.insert(
					member.id.clone(),
					EmittedMemberName::new(member.name.clone()),
				);
			}
			context.shapes.insert(
				StableShapeRequest::InterfaceShell(definition.id.clone()),
				StableShapeFact::InterfaceShell(definition.clone()),
			);
			context.shapes.insert(
				StableShapeRequest::ImplementationsForInterface(definition.id.clone()),
				StableShapeFact::Implementations(
					interface
						.implementations
						.iter()
						.filter(|implementation| implementation.interface.as_ref() == Some(&definition.id))
						.cloned()
						.collect(),
				),
			);
		}
	}
	(artifacts, interface, context)
}

const DEFAULTS: &str = "interface Pair { func anchor(): int\nfunc first(): int = this.second()\nfunc second(): int = this.anchor() }\nstruct One(value: int)\nimpl Pair for One { func anchor(): int = 1 }";

#[test]
fn materialized_default_is_placed_in_implementation_and_dispatches_to_materialized_sibling() {
	let (artifacts, interface, context) = materialized_fixture(DEFAULTS);
	let implementation = &interface.implementations[0];
	let first = implementation
		.member_slots
		.iter()
		.find(|slot| slot.name == "first")
		.unwrap();
	let second = implementation
		.member_slots
		.iter()
		.find(|slot| slot.name == "second")
		.unwrap();
	let artifact = artifacts
		.iter()
		.find(|artifact| artifact.definition == first.member_id)
		.unwrap();
	assert_eq!(
		artifact.placement,
		nymph_sema::RuntimePlacement::Attached {
			owner: implementation.id.clone(),
			name: "first".into()
		}
	);
	assert!(
		matches!(&artifact.payload, nymph_sema::RuntimePayload::MaterializedInterfaceMember { body_definition, interface_member } if body_definition == &first.body_definition_id && interface_member == &first.interface_member_id)
	);
	let lowered = lower_runtime_definition(&context, Arc::new(artifact.clone())).unwrap();
	assert!(
		matches!(lowered.fragment(), nymph_sema::LoweredHirFragment::MaterializedDefault { owner, implementation: found, interface_member, method } if owner == &implementation.id && found == &implementation.id && interface_member == &first.interface_member_id && matches!(&method.body, nymph_hir::hir::HirExpr::Call { callee, args } if args.is_empty() && matches!(&**callee, nymph_hir::hir::HirExpr::Field { name, .. } if name == "second")))
	);
	assert_eq!(lowered.demands(), [second.member_id.clone()]);
}

#[test]
fn materialized_default_dispatches_to_source_override_sibling() {
	let source = "interface Pair { func anchor(): int\nfunc first(): int = this.second()\nfunc second(): int = this.anchor() }\nstruct One(value: int)\nimpl Pair for One { func anchor(): int = 1\nfunc second(): int = 2 }";
	let (artifacts, interface, context) = materialized_fixture(source);
	let implementation = &interface.implementations[0];
	let first = implementation
		.member_slots
		.iter()
		.find(|slot| slot.name == "first")
		.unwrap();
	let second = implementation
		.member_slots
		.iter()
		.find(|slot| slot.name == "second")
		.unwrap();
	assert_eq!(
		second.source,
		nymph_sema::ImplementationMemberSource::Override
	);
	let materialized_second = DefinitionId::new(
		second.member_id.module.clone(),
		DeclarationKey::materialized_interface_member(
			implementation.id.clone(),
			second.interface_member_id.clone(),
		),
	);
	let artifact = artifacts
		.iter()
		.find(|artifact| artifact.definition == first.member_id)
		.unwrap();
	let lowered = lower_runtime_definition(&context, Arc::new(artifact.clone())).unwrap();
	assert_eq!(lowered.demands(), [second.member_id.clone()]);
	assert_ne!(lowered.demands(), [materialized_second]);
}

#[test]
fn each_implementation_gets_distinct_materialized_defaults_and_its_own_sibling() {
	let source = "interface Pair { func anchor(): int\nfunc first(): int = this.second()\nfunc second(): int = this.anchor() }\nstruct One(value: int)\nstruct Two(value: int)\nimpl Pair for One { func anchor(): int = 1 }\nimpl Pair for Two { func anchor(): int = 2 }";
	let (artifacts, interface, context) = materialized_fixture(source);
	let lowered = interface
		.implementations
		.iter()
		.map(|implementation| {
			let first = implementation
				.member_slots
				.iter()
				.find(|slot| slot.name == "first")
				.unwrap();
			let second = implementation
				.member_slots
				.iter()
				.find(|slot| slot.name == "second")
				.unwrap();
			let artifact = artifacts
				.iter()
				.find(|artifact| artifact.definition == first.member_id)
				.unwrap();
			(
				first,
				second,
				lower_runtime_definition(&context, Arc::new(artifact.clone())).unwrap(),
			)
		})
		.collect::<Vec<_>>();
	assert_ne!(lowered[0].0.member_id, lowered[1].0.member_id);
	assert_ne!(lowered[0].0.placement_owner, lowered[1].0.placement_owner);
	assert_eq!(
		lowered[0].0.body_definition_id,
		lowered[1].0.body_definition_id
	);
	assert_eq!(lowered[0].2.demands(), [lowered[0].1.member_id.clone()]);
	assert_eq!(lowered[1].2.demands(), [lowered[1].1.member_id.clone()]);
}

#[test]
fn malformed_materialized_artifacts_have_distinct_typed_errors() {
	let (artifacts, interface, mut context) = materialized_fixture(DEFAULTS);
	let implementation = &interface.implementations[0];
	let first = implementation
		.member_slots
		.iter()
		.find(|slot| slot.name == "first")
		.unwrap();
	let artifact = artifacts
		.iter()
		.find(|artifact| artifact.definition == first.member_id)
		.unwrap()
		.clone();
	context.runtime.remove(&first.body_definition_id);
	assert!(
		matches!(lower_runtime_definition(&context, Arc::new(artifact.clone())), Err(nymph_sema::StableLoweringError::Runtime(RuntimeDefinitionLookupError::Missing { definition })) if definition == first.body_definition_id)
	);
	context.runtime.insert(
		first.body_definition_id.clone(),
		Arc::new(
			artifacts
				.iter()
				.find(|item| item.definition == first.body_definition_id)
				.unwrap()
				.clone(),
		),
	);
	let mut missing_slot = implementation.clone();
	missing_slot
		.member_slots
		.retain(|slot| slot.member_id != first.member_id);
	context.shapes.insert(
		StableShapeRequest::Implementation(implementation.id.clone()),
		StableShapeFact::Implementation(missing_slot),
	);
	assert!(matches!(
		lower_runtime_definition(&context, Arc::new(artifact.clone())),
		Err(nymph_sema::StableLoweringError::MissingImplementationSlot { .. })
	));
	context.shapes.insert(
		StableShapeRequest::Implementation(implementation.id.clone()),
		StableShapeFact::Implementation(implementation.clone()),
	);
	let mut misplaced = artifact;
	misplaced.placement = nymph_sema::RuntimePlacement::Attached {
		owner: first.interface_member_id.clone(),
		name: "first".into(),
	};
	assert!(matches!(
		lower_runtime_definition(&context, Arc::new(misplaced)),
		Err(nymph_sema::StableLoweringError::MismatchedImplementationPlacement { .. })
	));
}

#[test]
fn lowers_one_canonical_function_and_value_without_a_module() {
	let items = artifacts("func id<T>(x: T): T = x\nlet answer: int = 42");
	let mut context = Context::default();
	for item in &items {
		context.names.insert(
			item.definition.clone(),
			EmittedBindingName::new(format!("stable${}", item.definition.key.duplicate())),
		);
	}
	let lowered = items
		.into_iter()
		.map(|item| lower_runtime_definition(&context, Arc::new(item)).unwrap())
		.collect::<Vec<_>>();
	assert!(
		matches!(lowered[0].fragment(), nymph_sema::LoweredHirFragment::TopLevelFunction(function) if function.params == ["x"] && function.body == nymph_hir::hir::HirExpr::Local("x".into()))
	);
	assert!(
		matches!(lowered[1].fragment(), nymph_sema::LoweredHirFragment::TopLevelValue(value) if value.value == nymph_hir::hir::HirExpr::Num(42.0, nymph_hir::hir::NumKind::Int))
	);
}

#[test]
fn missing_name_is_a_typed_error() {
	let item = artifacts("func answer(): int = 42").remove(0);
	assert!(matches!(
		lower_runtime_definition(&Context::default(), Arc::new(item)),
		Err(nymph_sema::StableLoweringError::Name(
			StableNameLookupError::MissingBinding { .. }
		))
	));
}

#[test]
fn lowers_list_and_map_spreads_to_spread_hir() {
	let list = lower_fixture("func spread(xs: #(int, int)): #(int, int, int) = #(0, ...xs)");
	assert!(
		matches!(list.fragment(), nymph_sema::LoweredHirFragment::TopLevelFunction(f) if matches!(f.body, nymph_hir::hir::HirExpr::ArraySpread { .. }))
	);
}

#[test]
fn lowers_interpolation_compound_assignment_and_closure_shadowing() {
	let interpolation = lower_fixture("func show(x: int): string = \"value: ${x}\"");
	assert!(
		matches!(interpolation.fragment(), nymph_sema::LoweredHirFragment::TopLevelFunction(f) if matches!(f.body, nymph_hir::hir::HirExpr::InterpolatedString(_)))
	);
	let compound = lower_fixture("func bump(mut x: int): void = { x += 1 }");
	assert!(
		matches!(compound.fragment(), nymph_sema::LoweredHirFragment::TopLevelFunction(f) if matches!(f.body, nymph_hir::hir::HirExpr::Block { tail: Some(ref tail), .. } if matches!(**tail, nymph_hir::hir::HirExpr::Assign { .. })))
	);
	let closure = lower_fixture("func nested(x: int): (int) -> int = (x) -> x");
	assert!(
		matches!(closure.fragment(), nymph_sema::LoweredHirFragment::TopLevelFunction(f) if matches!(f.body, nymph_hir::hir::HirExpr::Closure { ref params, ref body } if params == &["x$1"] && **body == nymph_hir::hir::HirExpr::Local("x$1".into())))
	);
}

#[test]
fn inherent_member_dispatch_has_exact_call_fragment_and_implementation_demand() {
	let source = "struct Box(value: int) { func get(): int = this.value }\nfunc read(value: Box): int = value.get()";
	let item = lower_named(source, "read");
	let nymph_sema::LoweredHirFragment::TopLevelFunction(function) = item.fragment() else {
		panic!("unexpected fragment: {:?}", item.fragment())
	};
	assert_eq!(
		function.body,
		nymph_hir::hir::HirExpr::Call {
			callee: Box::new(nymph_hir::hir::HirExpr::Field {
				recv: Box::new(nymph_hir::hir::HirExpr::Local("value".into())),
				name: "get".into(),
			}),
			args: vec![],
		}
	);
	assert_eq!(item.demands().len(), 1);
	assert_eq!(source_name(&item.demands()[0]), "Box");
}

#[test]
fn selected_override_dispatch_has_exact_call_and_placement_demand() {
	let source = "interface Plus<Other, Output> { func plus(other: Other): Output }\nstruct Vec(value: int)\nimpl Plus<Other = Vec, Output = Vec> for Vec { func plus(other: Vec): Vec = other }\nfunc add(a: Vec, b: Vec): Vec = a + b";
	let item = lower_named(source, "add");
	let nymph_sema::LoweredHirFragment::TopLevelFunction(function) = item.fragment() else {
		panic!("unexpected fragment: {:?}", item.fragment())
	};
	assert_eq!(
		function.body,
		nymph_hir::hir::HirExpr::Call {
			callee: Box::new(nymph_hir::hir::HirExpr::Field {
				recv: Box::new(nymph_hir::hir::HirExpr::Local("a".into())),
				name: "plus".into(),
			}),
			args: vec![nymph_hir::hir::HirExpr::Local("b".into())],
		}
	);
	assert_eq!(item.demands().len(), 1);
	assert!(matches!(
		item.demands()[0].key,
		DeclarationKey::Member { .. }
	));
}

#[test]
fn inherited_interface_default_dispatch_has_exact_call_and_materialization_demand() {
	let source = "interface Comparable<Other> { func compare_to(other: Other): int\nfunc less_than(other: Other): boolean = true }\nstruct Vec(value: int)\nimpl Comparable<Other = Vec> for Vec { func compare_to(other: Vec): int = 0 }\nfunc less(a: Vec, b: Vec): boolean = a < b";
	let item = lower_named(source, "less");
	let nymph_sema::LoweredHirFragment::TopLevelFunction(function) = item.fragment() else {
		panic!("unexpected fragment: {:?}", item.fragment())
	};
	assert_eq!(
		function.body,
		nymph_hir::hir::HirExpr::Call {
			callee: Box::new(nymph_hir::hir::HirExpr::Field {
				recv: Box::new(nymph_hir::hir::HirExpr::Local("a".into())),
				name: "less_than".into(),
			}),
			args: vec![nymph_hir::hir::HirExpr::Local("b".into())],
		}
	);
	assert_eq!(item.demands().len(), 1);
	assert!(matches!(
		item.demands()[0].key,
		DeclarationKey::MaterializedInterfaceMember { .. }
	));
}

#[test]
fn generic_bound_dispatch_is_direct_and_has_no_concrete_runtime_demand() {
	let source = "interface Named { func name(): string }\nfunc get_name<T: Named>(value: T): string = value.name()";
	let item = lower_named(source, "get_name");
	let nymph_sema::LoweredHirFragment::TopLevelFunction(function) = item.fragment() else {
		panic!("unexpected fragment: {:?}", item.fragment())
	};
	assert_eq!(
		function.body,
		nymph_hir::hir::HirExpr::Call {
			callee: Box::new(nymph_hir::hir::HirExpr::Field {
				recv: Box::new(nymph_hir::hir::HirExpr::Local("value".into())),
				name: "name".into(),
			}),
			args: vec![],
		}
	);
	assert_eq!(item.demands(), []);
}

#[test]
fn primitive_eager_and_short_circuit_dispatch_stay_native_without_demands() {
	let eager = lower_named("func add(a: int, b: int): int = a + b", "add");
	assert!(matches!(
		eager.fragment(),
		nymph_sema::LoweredHirFragment::TopLevelFunction(function)
			if matches!(function.body, nymph_hir::hir::HirExpr::Binary { op: nymph_hir::hir::BinOp::Add, .. })
	));
	assert_eq!(eager.demands(), []);

	let short = lower_named(
		"func both(a: boolean, b: boolean): boolean = a && b",
		"both",
	);
	assert!(matches!(
		short.fragment(),
		nymph_sema::LoweredHirFragment::TopLevelFunction(function)
			if matches!(function.body, nymph_hir::hir::HirExpr::Binary { op: nymph_hir::hir::BinOp::And, .. })
	));
	assert_eq!(short.demands(), []);
}

#[test]
fn missing_dispatched_member_name_is_a_typed_error() {
	let item = artifacts(
		"struct Box(value: int) { func get(): int = this.value }\nfunc read(value: Box): int = value.get()",
	)
	.into_iter()
	.find(|item| source_name(&item.definition) == "read")
	.unwrap();
	let mut context = Context::default();
	context
		.names
		.insert(item.definition.clone(), EmittedBindingName::new("read"));
	assert!(matches!(
		lower_runtime_definition(&context, Arc::new(item)),
		Err(nymph_sema::StableLoweringError::Name(
			StableNameLookupError::MissingMember { .. }
		))
	));
}

#[test]
fn generic_bound_with_two_primitive_implementations_lowers_to_stable_multi_case_dispatch() {
	let source = "interface Same<Other, Output> { func same(other: Other): Output }\nimpl Same<Other = int, Output = int> for int { func same(other: int): int = other }\nimpl Same<Other = string, Output = string> for string { func same(other: string): string = other }\nfunc choose<T: Same<Other = T, Output = T>>(a: T, b: T): T = a.same(b)";
	let (artifacts, interface, context) = materialized_fixture(source);
	let artifact = artifacts
		.into_iter()
		.find(|item| source_name(&item.definition) == "choose")
		.unwrap();
	let lowered = lower_runtime_definition(&context, Arc::new(artifact)).unwrap();
	let nymph_sema::LoweredHirFragment::TopLevelFunction(function) = lowered.fragment() else {
		panic!("unexpected fragment: {:?}", lowered.fragment())
	};
	let expected_targets = interface
		.implementations
		.iter()
		.map(|implementation| implementation.member_slots[0].body_definition_id.clone())
		.collect::<Vec<_>>();
	assert_eq!(
		function.body,
		nymph_hir::hir::HirExpr::BoundDispatch {
			interface: "Same".into(),
			method: "same".into(),
			receiver: Box::new(nymph_hir::hir::HirExpr::Local("a".into())),
			argument: Box::new(nymph_hir::hir::HirExpr::Local("b".into())),
			cases: vec![
				nymph_hir::hir::HirBoundDispatchCase {
					receiver_tag: "nymph.int".into(),
					argument_tag: "nymph.int".into(),
					target: nymph_hir::hir::HirBoundDispatchTarget::TopLevel {
						module: "main".into(),
						name: source_name(&expected_targets[0]).into(),
					},
				},
				nymph_hir::hir::HirBoundDispatchCase {
					receiver_tag: "nymph.string".into(),
					argument_tag: "nymph.string".into(),
					target: nymph_hir::hir::HirBoundDispatchTarget::TopLevel {
						module: "main".into(),
						name: source_name(&expected_targets[1]).into(),
					},
				},
			],
		}
	);
	assert_eq!(lowered.demands(), expected_targets);
}
