use std::{collections::HashMap, sync::Arc};

use nymph_sema::{
	BodyNodeId, CanonicalModuleSpecifier, DeclarationCategory, DeclarationKey, DefinitionId,
	DefinitionShapeKind, EmittedBindingName, EmittedMemberName, ExportedDefinition, ExternalAbi,
	InterfaceType, ModuleEnvironment, ModuleIdentity, ModuleInterface, ModuleOrigin,
	RuntimeDefinition, RuntimeDefinitionLookup, RuntimeDefinitionLookupError, StableNameLookup,
	StableNameLookupError, StableShapeFact, StableShapeLookup, StableShapeLookupError,
	StableShapeRequest, lower_runtime_definition,
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
	artifacts_and_interface_with_dependencies(source, &[])
}

fn artifacts_and_interface_with_dependencies(
	source: &str,
	dependencies: &[Arc<ModuleEnvironment>],
) -> (Vec<RuntimeDefinition>, nymph_sema::ModuleInterface) {
	let parsed = nymph_syntax::parse_module(source, "fixture.nym");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let module = Arc::new(parsed.tree);
	let identity = ModuleIdentity {
		origin: ModuleOrigin::Project("test".into()),
		project: "test".into(),
		path: "main".into(),
	};
	let environment =
		nymph_sema::SemanticEnvironment::from_modules(identity.clone(), dependencies).unwrap();
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

#[test]
fn implementation_header_generic_body_receiver_has_canonical_type_annotation() {
	let source = "impl<T> #[T] { func first(): T = { let result = this\nresult[0] } }";
	let (artifacts, interface) = artifacts_and_interface(source);
	let implementation = &interface.implementations[0];
	let expected = InterfaceType::List(Box::new(InterfaceType::Generic(
		implementation.binders[0].id.clone(),
	)));
	let first = artifacts
		.into_iter()
		.find(|artifact| source_name(&artifact.definition) == "first")
		.unwrap();
	let nymph_sema::RuntimePayload::NymphBody(body) = first.payload else {
		unreachable!()
	};
	let parsed_body = nymph_syntax::parse_module(
		&format!("func fixture(): void = {}", body.expression),
		"body.nym",
	);
	assert!(
		parsed_body.diagnostics.is_empty(),
		"{:?}",
		parsed_body.diagnostics
	);
	let nymph_ast::decl::Declaration::Func {
		body: parsed_body, ..
	} = &parsed_body.tree.members[0]
	else {
		unreachable!()
	};
	let nymph_ast::expr::ExprKind::Block {
		body: statements, ..
	} = &parsed_body.kind
	else {
		unreachable!()
	};
	let nymph_ast::expr::Statement::Expr(index_access) = &statements.last().unwrap().0 else {
		unreachable!()
	};
	let nymph_ast::expr::ExprKind::IndexAccess {
		parent: receiver, ..
	} = &index_access.kind
	else {
		unreachable!()
	};
	let receiver = fixture_body_node_id(parsed_body, receiver.id);

	assert_eq!(
		body
			.annotations
			.types
			.iter()
			.find(|(node, _)| *node == receiver),
		Some(&(receiver, expected))
	);
}

fn fixture_body_node_id(body: &nymph_ast::expr::Expr, target: nymph_ast::NodeId) -> BodyNodeId {
	fn visit(
		expression: &nymph_ast::expr::Expr,
		target: nymph_ast::NodeId,
		next: &mut u32,
	) -> Option<BodyNodeId> {
		let current = BodyNodeId(*next);
		*next += 1;
		if expression.id == target {
			return Some(current);
		}
		match &expression.kind {
			nymph_ast::expr::ExprKind::Block { body, .. } => body.iter().find_map(|statement| {
				let expression = match &statement.0 {
					nymph_ast::expr::Statement::Expr(expression)
					| nymph_ast::expr::Statement::Let {
						value: expression, ..
					} => expression,
				};
				visit(expression, target, next)
			}),
			nymph_ast::expr::ExprKind::IndexAccess { parent, index, .. } => {
				visit(parent, target, next).or_else(|| visit(index, target, next))
			}
			_ => None,
		}
	}

	visit(body, target, &mut 0).expect("index receiver must have a canonical body node")
}

#[test]
fn imported_external_reference_has_exact_stable_marshal_annotation() {
	let dependency_identity = ModuleIdentity {
		origin: ModuleOrigin::Project("test".into()),
		project: "test".into(),
		path: "dependency".into(),
	};
	let external = DefinitionId::new(
		dependency_identity.clone(),
		DeclarationKey::top_level(DeclarationCategory::Let, "maximum"),
	);
	let dependency = Arc::new(ModuleEnvironment::Complete(ModuleInterface {
		module: dependency_identity,
		exports: vec![ExportedDefinition {
			id: external.clone(),
			name: "maximum".into(),
			visibility: None,
			kind: DefinitionShapeKind::Let,
			binders: vec![],
			constraints: vec![],
			parameters: vec![],
			return_type: None,
			ty: Some(InterfaceType::Float),
			fields: vec![],
			variants: vec![],
			members: vec![],
			super_interfaces: vec![],
			external: Some(ExternalAbi {
				marker: "max_float".into(),
				module: Some("std/math/intrinsics".into()),
				symbol: Some("max_float".into()),
				marshal: Some(nymph_hir::hir::MarshalKind::Float),
			}),
			runtime_owner: None,
		}],
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	}));
	let (artifacts, _) =
		artifacts_and_interface_with_dependencies("func read(): float = maximum", &[dependency]);
	let read = artifacts
		.into_iter()
		.find(|artifact| source_name(&artifact.definition) == "read")
		.unwrap();
	let nymph_sema::RuntimePayload::NymphBody(body) = read.payload else {
		unreachable!()
	};
	let (node, target) = &body.annotations.definition_targets[0];

	assert_eq!(target, &external);
	assert!(
		body
			.annotations
			.external_marshals
			.iter()
			.any(|(candidate, marshal)| {
				candidate == node && *marshal == nymph_hir::hir::MarshalKind::Float
			})
	);
}

fn materialized_fixture(
	source: &str,
) -> (Vec<RuntimeDefinition>, nymph_sema::ModuleInterface, Context) {
	materialized_fixture_in(source, "main")
}

fn materialized_fixture_in(
	source: &str,
	path: &str,
) -> (Vec<RuntimeDefinition>, nymph_sema::ModuleInterface, Context) {
	let parsed = nymph_syntax::parse_module(source, "fixture.nym");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let module = Arc::new(parsed.tree);
	let identity = ModuleIdentity {
		origin: ModuleOrigin::Project("test".into()),
		project: "test".into(),
		path: path.into(),
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
		if let nymph_sema::RuntimePayload::NymphBody(body) = &artifact.payload {
			for (_, variant) in body.annotations.variants.iter() {
				context.members.insert(
					variant.variant_definition.clone(),
					EmittedMemberName::new(variant.variant_name.clone()),
				);
			}
			for (_, variant) in body.annotations.pattern_variants.iter() {
				context.members.insert(
					variant.variant_definition.clone(),
					EmittedMemberName::new(variant.variant_name.clone()),
				);
			}
		}
		if let nymph_sema::RuntimePayload::External(abi) = &artifact.payload {
			context.shapes.insert(
				StableShapeRequest::ExternalAbi(artifact.definition.clone()),
				StableShapeFact::ExternalAbi(abi.clone()),
			);
		}
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
		for member in &definition.members {
			context.members.insert(
				member.id.clone(),
				EmittedMemberName::new(member.name.clone()),
			);
		}
		if let Some(artifact) = artifacts
			.iter()
			.find(|artifact| artifact.definition == definition.id)
		{
			let shell = match &artifact.payload {
				nymph_sema::RuntimePayload::Struct(shell) => {
					Some(nymph_sema::StableTypeShell::Struct(shell.clone()))
				}
				nymph_sema::RuntimePayload::Enum(shell) => {
					Some(nymph_sema::StableTypeShell::Enum(shell.clone()))
				}
				_ => None,
			};
			if let Some(shell) = shell {
				context.shapes.insert(
					StableShapeRequest::TypeShell(definition.id.clone()),
					StableShapeFact::TypeShell(shell),
				);
			}
		}
		if definition.kind == nymph_sema::DefinitionShapeKind::Interface {
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
	let InterfaceType::Named {
		definition: owner, ..
	} = &implementation.self_type
	else {
		panic!("materialized default must target a named owner")
	};
	assert_eq!(lowered.demands(), [owner.clone(), second.member_id.clone()]);
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
	let InterfaceType::Named {
		definition: owner, ..
	} = &implementation.self_type
	else {
		panic!("materialized default must target a named owner")
	};
	assert_eq!(lowered.demands(), [owner.clone(), second.member_id.clone()]);
	assert!(!lowered.demands().contains(&materialized_second));
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
	for (implementation, (_, sibling, materialized)) in interface.implementations.iter().zip(&lowered)
	{
		let InterfaceType::Named {
			definition: owner, ..
		} = &implementation.self_type
		else {
			panic!("materialized default must target a named owner")
		};
		assert_eq!(
			materialized.demands(),
			[owner.clone(), sibling.member_id.clone()]
		);
		assert_eq!(
			materialized.placement(),
			&nymph_sema::RuntimeAssemblyPlacement::Shell(owner.clone())
		);
	}
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
	for item in &lowered {
		assert_eq!(
			item.placement(),
			&nymph_sema::RuntimeAssemblyPlacement::Module(item.definition().module.clone())
		);
	}
}

#[test]
fn namespace_container_members_are_exact_module_runtime_definitions() {
	let (artifacts, _, context) = materialized_fixture("namespace Tools { func answer(): int = 42 }");
	let artifact = artifacts
		.into_iter()
		.find(|artifact| source_name(&artifact.definition) == "answer")
		.expect("namespace member runtime definition");
	let definition = artifact.definition.clone();
	assert!(matches!(definition.key, DeclarationKey::Member { .. }));
	assert_eq!(artifact.placement, nymph_sema::RuntimePlacement::TopLevel);

	let lowered = lower_runtime_definition(&context, Arc::new(artifact)).unwrap();
	assert_eq!(lowered.definition(), &definition);
	assert!(matches!(
		lowered.fragment(),
		nymph_sema::LoweredHirFragment::TopLevelFunction(_)
	));
	assert_eq!(
		lowered.placement(),
		&nymph_sema::RuntimeAssemblyPlacement::Module(definition.module)
	);
}

#[test]
fn namespace_callable_kind_survives_canonical_body_extraction() {
	let (artifacts, interface, context) = materialized_fixture(
		"struct Token(value: int)\nimpl Token { namespace func make(): Token = Token(value = 1) }",
	);
	let implementation = &interface.implementations[0];
	let artifact = artifacts
		.into_iter()
		.find(|artifact| source_name(&artifact.definition) == "make")
		.expect("static implementation member");
	assert!(matches!(
		&artifact.payload,
		nymph_sema::RuntimePayload::NymphBody(body)
			if body.kind == nymph_sema::RuntimeBodyKind::StaticFunction
	));

	let lowered = lower_runtime_definition(&context, Arc::new(artifact)).unwrap();
	let InterfaceType::Named { definition, .. } = &implementation.self_type else {
		panic!("implementation owner must be nominal")
	};
	assert!(matches!(
		lowered.fragment(),
		nymph_sema::LoweredHirFragment::AttachedStatic { owner, .. }
			if owner == &implementation.id
	));
	assert_eq!(
		lowered.placement(),
		&nymph_sema::RuntimeAssemblyPlacement::Shell(definition.clone())
	);
}

#[test]
fn same_named_nominal_statics_keep_distinct_exact_shell_ids() {
	let source =
		"struct Token(value: int)\nimpl Token { namespace func make(): Token = Token(value = 1) }";
	let lowered = ["left", "right"].map(|path| {
		let (artifacts, _, context) = materialized_fixture_in(source, path);
		let artifact = artifacts
			.into_iter()
			.find(|artifact| source_name(&artifact.definition) == "make")
			.unwrap();
		lower_runtime_definition(&context, Arc::new(artifact)).unwrap()
	});
	let [
		nymph_sema::RuntimeAssemblyPlacement::Shell(left),
		nymph_sema::RuntimeAssemblyPlacement::Shell(right),
	] = [lowered[0].placement(), lowered[1].placement()]
	else {
		panic!("both statics must attach to nominal shells")
	};
	assert_eq!(source_name(left), "Token");
	assert_eq!(source_name(right), "Token");
	assert_ne!(left, right);
	assert_ne!(left.module, right.module);
}

#[test]
fn malformed_namespace_shell_owner_is_rejected_without_name_fallback() {
	let (artifacts, interface, context) =
		materialized_fixture("namespace Token { func value(): int = 1 }\nstruct Holder(value: int)");
	let namespace = interface
		.exports
		.iter()
		.find(|shape| shape.name == "Token")
		.unwrap()
		.id
		.clone();
	let mut artifact = artifacts
		.into_iter()
		.find(|artifact| source_name(&artifact.definition) == "value")
		.unwrap();
	artifact.placement = nymph_sema::RuntimePlacement::Attached {
		owner: namespace.clone(),
		name: "value".into(),
	};
	assert!(matches!(
		lower_runtime_definition(&context, Arc::new(artifact)),
		Err(nymph_sema::StableLoweringError::MissingAttachmentShell { owner, .. })
			if owner == namespace
	));
}

#[test]
fn top_level_external_value_lowers_to_exact_marshaled_binding() {
	let (artifacts, _, mut context) = materialized_fixture("external(max_float) let maximum: float");
	let artifact = artifacts.into_iter().next().unwrap();
	context.names.insert(
		artifact.definition.clone(),
		EmittedBindingName::new("canonical$maximum"),
	);
	let lowered = lower_runtime_definition(&context, Arc::new(artifact)).unwrap();
	assert!(matches!(
		lowered.fragment(),
		nymph_sema::LoweredHirFragment::TopLevelValue(nymph_hir::hir::HirLet {
			name,
			mutable: false,
			value: nymph_hir::hir::HirExpr::ExternValue {
				module: "std/math/intrinsics",
				symbol: "max_float",
				marshal: nymph_hir::hir::MarshalKind::Float,
			},
		}) if name == "canonical$maximum"
	));
	assert_eq!(lowered.demands(), []);
}

#[test]
fn same_module_identifier_records_its_exact_definition_demand() {
	let (artifacts, _, context) =
		materialized_fixture("let answer: int = 42\nfunc read(): int = answer");
	let answer = artifacts
		.iter()
		.find(|artifact| source_name(&artifact.definition) == "answer")
		.unwrap()
		.definition
		.clone();
	let read = artifacts
		.into_iter()
		.find(|artifact| source_name(&artifact.definition) == "read")
		.unwrap();

	let lowered = lower_runtime_definition(&context, Arc::new(read)).unwrap();

	assert_eq!(lowered.demands(), [answer]);
}

#[test]
fn direct_external_call_records_its_exact_definition_demand() {
	let (artifacts, _, context) = materialized_fixture(
		"external(compare_number) func compare(first: int, second: int): int\nfunc sign(): int = compare(1, 2)",
	);
	let compare = artifacts
		.iter()
		.find(|artifact| source_name(&artifact.definition) == "compare")
		.unwrap()
		.definition
		.clone();
	let sign = artifacts
		.into_iter()
		.find(|artifact| source_name(&artifact.definition) == "sign")
		.unwrap();

	let lowered = lower_runtime_definition(&context, Arc::new(sign)).unwrap();

	assert_eq!(lowered.demands(), [compare]);
}

#[test]
fn top_level_external_values_preserve_every_stable_marshal_kind() {
	let (artifacts, _, _) = materialized_fixture("external(max_float) let maximum: float");
	let template = artifacts.into_iter().next().unwrap();
	for marshal in [
		nymph_hir::hir::MarshalKind::Int,
		nymph_hir::hir::MarshalKind::UInt,
		nymph_hir::hir::MarshalKind::Float,
		nymph_hir::hir::MarshalKind::Char,
		nymph_hir::hir::MarshalKind::String,
		nymph_hir::hir::MarshalKind::Boolean,
		nymph_hir::hir::MarshalKind::List,
		nymph_hir::hir::MarshalKind::Tuple,
		nymph_hir::hir::MarshalKind::Map,
	] {
		let mut artifact = template.clone();
		let nymph_sema::RuntimePayload::External(abi) = &mut artifact.payload else {
			unreachable!()
		};
		abi.marshal = Some(marshal);
		let mut context = Context::default();
		context.names.insert(
			artifact.definition.clone(),
			EmittedBindingName::new("maximum"),
		);
		context.shapes.insert(
			StableShapeRequest::ExternalAbi(artifact.definition.clone()),
			StableShapeFact::ExternalAbi(abi.clone()),
		);
		let lowered = lower_runtime_definition(&context, Arc::new(artifact)).unwrap();
		assert!(matches!(
			lowered.fragment(),
			nymph_sema::LoweredHirFragment::TopLevelValue(nymph_hir::hir::HirLet {
				value: nymph_hir::hir::HirExpr::ExternValue { marshal: found, .. },
				..
			}) if *found == marshal
		));
	}
}

#[test]
fn malformed_external_artifacts_return_distinct_typed_errors() {
	let (artifacts, _, mut context) = materialized_fixture("external(max_float) let maximum: float");
	let artifact = artifacts.into_iter().next().unwrap();
	let mut missing_module = artifact.clone();
	let nymph_sema::RuntimePayload::External(abi) = &mut missing_module.payload else {
		unreachable!()
	};
	abi.module = None;
	context.shapes.insert(
		StableShapeRequest::ExternalAbi(missing_module.definition.clone()),
		StableShapeFact::ExternalAbi(abi.clone()),
	);
	assert!(matches!(
		lower_runtime_definition(&context, Arc::new(missing_module)),
		Err(nymph_sema::StableLoweringError::MissingExternalModule { .. })
	));

	let mut missing_marshal = artifact.clone();
	let nymph_sema::RuntimePayload::External(abi) = &mut missing_marshal.payload else {
		unreachable!()
	};
	abi.marshal = None;
	context.shapes.insert(
		StableShapeRequest::ExternalAbi(missing_marshal.definition.clone()),
		StableShapeFact::ExternalAbi(abi.clone()),
	);
	assert!(matches!(
		lower_runtime_definition(&context, Arc::new(missing_marshal)),
		Err(nymph_sema::StableLoweringError::MissingExternalMarshal { .. })
	));

	context.shapes.insert(
		StableShapeRequest::ExternalAbi(artifact.definition.clone()),
		StableShapeFact::ExternalAbi(nymph_sema::ExternalAbi {
			marker: "different".into(),
			module: Some("elsewhere".into()),
			symbol: Some("different".into()),
			marshal: Some(nymph_hir::hir::MarshalKind::Float),
		}),
	);
	assert!(matches!(
		lower_runtime_definition(&context, Arc::new(artifact)),
		Err(nymph_sema::StableLoweringError::MismatchedExternalAbi { .. })
	));
}

#[test]
fn external_value_reference_requires_its_body_local_stable_marshal_annotation() {
	let (mut artifacts, _, context) = materialized_fixture(
		"external(max_float) let maximum: float\nfunc pair(): #(float, float) = #(maximum, maximum)",
	);
	let mut pair = artifacts
		.drain(..)
		.find(|artifact| source_name(&artifact.definition) == "pair")
		.unwrap();
	let nymph_sema::RuntimePayload::NymphBody(body) = &mut pair.payload else {
		unreachable!()
	};
	assert_eq!(body.annotations.external_marshals.len(), 2);
	body.annotations.external_marshals = Arc::new([]);
	assert!(matches!(
		lower_runtime_definition(&context, Arc::new(pair)),
		Err(nymph_sema::StableLoweringError::MissingAnnotation { channel, .. })
			if channel == "external marshal"
	));
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
fn inherent_member_dispatch_has_exact_call_fragment_and_member_demand() {
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
	assert_eq!(source_name(&item.demands()[0]), "get");
	assert!(matches!(
		item.demands()[0].key,
		DeclarationKey::Member { .. }
	));
}

#[test]
fn inherent_static_dispatch_uses_exact_owner_binding_and_member_demand() {
	let source = "struct Box(value: int)\nimpl Box { namespace func make(): Box = Box(value = 1) }\nfunc read(): Box = Box.make()";
	let item = lower_named(source, "read");
	let nymph_sema::LoweredHirFragment::TopLevelFunction(function) = item.fragment() else {
		panic!("unexpected fragment: {:?}", item.fragment())
	};
	assert_eq!(
		function.body,
		nymph_hir::hir::HirExpr::Call {
			callee: Box::new(nymph_hir::hir::HirExpr::Field {
				recv: Box::new(nymph_hir::hir::HirExpr::Local("Box".into())),
				name: "make".into(),
			}),
			args: vec![],
		}
	);
	assert_eq!(item.demands().len(), 1);
	assert_eq!(source_name(&item.demands()[0]), "make");
	assert!(matches!(
		item.demands()[0].key,
		DeclarationKey::Member { .. }
	));
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
fn namespaced_call_through_generic_parameter_is_a_typed_unsupported_error() {
	let (artifacts, _, context) = materialized_fixture(
		"interface Default { func default(): self }\nfunc make<T: Default>(): T = T.default()",
	);
	let make = artifacts
		.into_iter()
		.find(|artifact| source_name(&artifact.definition) == "make")
		.unwrap();

	let result = lower_runtime_definition(&context, Arc::new(make));

	assert!(
		matches!(
			&result,
			Err(nymph_sema::StableLoweringError::Unsupported { feature, .. })
				if feature == "namespaced call through a generic type parameter"
		),
		"{result:?}"
	);
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

#[test]
fn lowers_struct_and_enum_construction_and_variant_patterns_from_stable_facts() {
	let source = "struct Point(x: int, y: int)\nenum Choice { Pair(left: int, right: int), None }\nfunc point(): Point = Point(x = 1, y = 2)\nfunc choose(value: Choice): int = match (value) { Choice.Pair(left = left, right = right) if (left < right) -> right, Choice.Pair(left = left, right = _) -> left, Choice.None -> 0 }";
	let point = lower_named(source, "point");
	assert!(matches!(
		point.fragment(),
		nymph_sema::LoweredHirFragment::TopLevelFunction(function)
			if matches!(&function.body, nymph_hir::hir::HirExpr::New { class, fields }
				if class == "Point" && fields.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>() == ["x", "y"])
	));
	let choice = lower_named(source, "choose");
	assert!(matches!(
		choice.fragment(),
		nymph_sema::LoweredHirFragment::TopLevelFunction(function)
			if matches!(&function.body, nymph_hir::hir::HirExpr::Match { arms, .. }
				if matches!(&arms[0].pat, nymph_hir::hir::HirPat::Variant { enum_name, variant, fields }
					if enum_name == "Choice" && variant == "Pair" && fields.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>() == ["left", "right"])
				&& arms[0].guard.is_some())
	));
}

#[test]
fn lowers_native_index_access_and_assignment_without_protocol_fallback() {
	let list = lower_fixture("func update(xs: mut #[int]): int = { xs[0] = 2\nxs[0] }");
	assert!(matches!(
		list.fragment(),
		nymph_sema::LoweredHirFragment::TopLevelFunction(function)
			if matches!(function.body, nymph_hir::hir::HirExpr::Block { tail: Some(ref tail), .. }
				if matches!(**tail, nymph_hir::hir::HirExpr::Index { .. }))
	));
	assert_eq!(list.demands(), []);
	let map = lower_fixture("func lookup(xs: #{string: int}): int = xs[\"one\"]");
	assert!(matches!(
		map.fragment(),
		nymph_sema::LoweredHirFragment::TopLevelFunction(function)
			if matches!(function.body, nymph_hir::hir::HirExpr::MapGet { .. })
	));
}

#[test]
fn lowers_tuple_spread_without_protocol_demands() {
	let tuple = lower_fixture("func copy_tuple(xs: #(int, int)): #(int, int, int) = #(0, ...xs)");
	assert!(
		matches!(tuple.fragment(), nymph_sema::LoweredHirFragment::TopLevelFunction(function) if matches!(function.body, nymph_hir::hir::HirExpr::ArraySpread { .. }))
	);
	assert_eq!(tuple.demands(), []);
}

#[test]
fn lowers_custom_user_index_access_with_exact_method_and_implementation_demand() {
	let source = "interface Index<Key, Output> { func index(key: Key): Output }\nstruct Offset(base: int) { impl Index<Key = int, Output = int> { func index(key: int): int = this.base + key } }\nfunc read(value: Offset): int = value[2]";
	let (items, interface, context) = materialized_fixture(source);
	let item = items
		.into_iter()
		.find(|item| source_name(&item.definition) == "read")
		.unwrap();
	let index = interface.implementations[0].member_slots[0]
		.body_definition_id
		.clone();
	let lowered = lower_runtime_definition(&context, Arc::new(item)).unwrap();
	assert_eq!(
		lowered.fragment(),
		&nymph_sema::LoweredHirFragment::TopLevelFunction(nymph_hir::hir::HirFunc {
			name: "read".into(),
			params: vec!["value".into()],
			body: nymph_hir::hir::HirExpr::Call {
				callee: Box::new(nymph_hir::hir::HirExpr::Field {
					recv: Box::new(nymph_hir::hir::HirExpr::Local("value".into())),
					name: "index".into(),
				}),
				args: vec![nymph_hir::hir::HirExpr::Num(
					2.0,
					nymph_hir::hir::NumKind::Int
				)],
			},
		})
	);
	assert_eq!(lowered.demands(), [index]);
}

#[test]
fn custom_index_assignment_and_compound_assignment_are_rejected_by_the_checker() {
	for assignment in ["value[0] = 1", "value[0] += 1"] {
		let source = format!(
			"interface Index<Key, Output> {{ func index(key: Key): Output }}\nstruct Value {{ impl Index<Key = int, Output = int> {{ func index(key: int): int = key }} }}\nfunc write(value: Value): void = {assignment}"
		);
		let parsed = nymph_syntax::parse_module(&source, "fixture.nym");
		let identity = ModuleIdentity {
			origin: ModuleOrigin::Project("test".into()),
			project: "test".into(),
			path: "main".into(),
		};
		let environment = nymph_sema::SemanticEnvironment::from_modules(identity.clone(), &[]).unwrap();
		let checked = nymph_sema::check_module_with_environment(
			Arc::new(parsed.tree),
			identity,
			&environment,
			nymph_sema::EntryMode::Library,
		);
		assert!(
			checked.diagnostics.iter().any(|diagnostic| diagnostic
				.message
				.contains("cannot assign to `custom index access`")),
			"{assignment}: {:?}",
			checked.diagnostics
		);
	}
}

#[test]
fn lowers_native_map_and_user_iterator_spreads_with_exact_ordered_demands() {
	let native = lower_named(
		"func merge(value: #{int: int}): #{int: int} = #{...value, 2: 3}",
		"merge",
	);
	assert!(
		matches!(native.fragment(), nymph_sema::LoweredHirFragment::TopLevelFunction(function) if matches!(&function.body, nymph_hir::hir::HirExpr::MapSpread(items) if matches!(&items[0], nymph_hir::hir::HirMapElem::Spread(nymph_hir::hir::HirExpr::Local(name)) if name == "value")))
	);
	assert_eq!(native.demands(), []);

	let source = "enum Option<T> { Some(value: T), None }\ninterface Iterator<Item> { mut func next(): Option<Item> }\ninterface Iterable<Item> { func iter(): Iterator<Item> }\nstruct Values\nimpl Iterator<int> for Values { mut func next(): Option<int> = None }\nstruct Pairs\nimpl Iterator<#(int, int)> for Pairs { mut func next(): Option<#(int, int)> = None }\nfunc list(value: mut Values): #[int] = #[...value]\nfunc map(value: mut Pairs): #{int: int} = #{...value}";
	let (items, interface, context) = materialized_fixture(source);
	let expected = interface
		.implementations
		.iter()
		.map(|implementation| implementation.member_slots[0].body_definition_id.clone())
		.collect::<Vec<_>>();
	for (name, map) in [("list", false), ("map", true)] {
		let item = items
			.iter()
			.find(|item| source_name(&item.definition) == name)
			.unwrap()
			.clone();
		let lowered = lower_runtime_definition(&context, Arc::new(item)).unwrap();
		assert!(if map {
			matches!(lowered.fragment(), nymph_sema::LoweredHirFragment::TopLevelFunction(function) if matches!(&function.body, nymph_hir::hir::HirExpr::MapSpread(items) if matches!(&items[0], nymph_hir::hir::HirMapElem::Spread(nymph_hir::hir::HirExpr::Block { .. }))))
		} else {
			matches!(lowered.fragment(), nymph_sema::LoweredHirFragment::TopLevelFunction(function) if matches!(&function.body, nymph_hir::hir::HirExpr::ArraySpread { elems, .. } if matches!(&elems[0], nymph_hir::hir::HirArrayElem::Spread(nymph_hir::hir::HirExpr::Block { .. }))))
		});
		let _resolved_implementation = if map { &expected[1] } else { &expected[0] };
		assert_eq!(lowered.demands(), []);
	}
}

#[test]
fn lowers_for_over_native_list_user_iterator_and_bounded_ranges_with_exact_modes() {
	let source = "enum Option<T> { Some(value: T), None }\ninterface Iterator<Item> { mut func next(): Option<Item> }\ninterface Iterable<Item> { func iter(): Iterator<Item> }\nstruct Values\nimpl Iterator<int> for Values { mut func next(): Option<int> = None }\nfunc each(xs: mut Values): void = for (x in xs) { x }";
	let (items, interface, context) = materialized_fixture(source);
	let item = items
		.into_iter()
		.find(|item| source_name(&item.definition) == "each")
		.unwrap();
	let lowered = lower_runtime_definition(&context, Arc::new(item)).unwrap();
	assert_protocol_for(lowered.fragment(), false);
	let _resolved_implementation = &interface.implementations[0].member_slots[0].body_definition_id;
	assert_eq!(lowered.demands(), []);
}

fn assert_protocol_for(fragment: &nymph_sema::LoweredHirFragment, inclusive: bool) {
	let nymph_sema::LoweredHirFragment::TopLevelFunction(function) = fragment else {
		panic!("expected function")
	};
	let nymph_hir::hir::HirExpr::Block { stmts, .. } = &function.body else {
		panic!("expected protocol block: {:?}", function.body)
	};
	let nymph_hir::hir::HirStmt::Let { name, value, .. } = &stmts[0] else {
		panic!("expected iterator binding")
	};
	assert_eq!(name, "$it");
	if inclusive {
		assert!(matches!(value, nymph_hir::hir::HirExpr::Call { callee, .. }
			if matches!(callee.as_ref(), nymph_hir::hir::HirExpr::Field { name, recv }
				if name == "iter" && matches!(recv.as_ref(), nymph_hir::hir::HirExpr::New { fields, .. }
					if fields[2].1 == nymph_hir::hir::HirExpr::Bool(true)))));
	}
	assert!(
		matches!(&stmts[2], nymph_hir::hir::HirStmt::Expr(nymph_hir::hir::HirExpr::While { body, .. }) if matches!(body.as_ref(), nymph_hir::hir::HirExpr::Match { arms, .. } if matches!(&arms[0].pat, nymph_hir::hir::HirPat::Variant { variant, fields, .. } if variant == "Some" && fields.len() == 1)))
	);
}

#[test]
fn range_for_support_matches_legacy_for_startless_endless_and_unbounded_forms() {
	for range in ["..3", "..=3", "1.."] {
		let (items, _, context) = materialized_fixture(&format!(
			"enum Option<T> {{ Some(value: T), None }}\ninterface Iterator<Item> {{ mut func next(): Option<Item> }}\ninterface Iterable<Item> {{ func iter(): Iterator<Item> }}\nfunc each(): void = for (_ in {range}) {{ 0 }}"
		));
		let error = lower_runtime_definition(
			&context,
			Arc::new(
				items
					.into_iter()
					.find(|item| source_name(&item.definition) == "each")
					.unwrap(),
			),
		)
		.unwrap_err();
		assert!(
			matches!(error, nymph_sema::StableLoweringError::Unsupported { ref feature, .. } if feature == "range/protocol"),
			"{range}: {error:?}"
		);
	}
}

#[test]
fn bounded_range_in_value_position_remains_typed_unsupported() {
	let (items, _, context) = materialized_fixture(
		"enum Option<T> { Some(value: T), None }\ninterface Iterator<Item> { mut func next(): Option<Item> }\ninterface Iterable<Item> { func iter(): Iterator<Item> }\nfunc value(): void = { let range = 1..3 }",
	);
	let error = lower_runtime_definition(
		&context,
		Arc::new(
			items
				.into_iter()
				.find(|item| source_name(&item.definition) == "value")
				.unwrap(),
		),
	)
	.unwrap_err();
	assert!(
		matches!(error, nymph_sema::StableLoweringError::Unsupported { ref feature, .. } if feature == "range/protocol"),
		"{error:?}"
	);
}

#[test]
fn missing_index_dispatch_iteration_mode_and_iteration_resolution_are_distinct_typed_errors() {
	let mut item = artifacts("interface Index<Key, Output> { func index(key: Key): Output }\nstruct Value { impl Index<Key = int, Output = int> { func index(key: int): int = key } }\nfunc read(value: Value): int = value[0]").into_iter().find(|item| source_name(&item.definition) == "read").unwrap();
	let nymph_sema::RuntimePayload::NymphBody(body) = &mut item.payload else {
		unreachable!()
	};
	body.annotations.dispatches = Arc::from([]);
	let mut context = Context::default();
	context
		.names
		.insert(item.definition.clone(), EmittedBindingName::new("read"));
	assert!(
		matches!(lower_runtime_definition(&context, Arc::new(item)), Err(nymph_sema::StableLoweringError::MissingAnnotation { channel, .. }) if channel == "dispatch")
	);

	let (items, _, context) = materialized_fixture(
		"enum Option<T> { Some(value: T), None }\ninterface Iterator<Item> { mut func next(): Option<Item> }\ninterface Iterable<Item> { func iter(): Iterator<Item> }\nstruct Values\nimpl Iterator<int> for Values { mut func next(): Option<int> = None }\nfunc each(xs: mut Values): void = for (x in xs) { x }",
	);
	let mut item = items
		.into_iter()
		.find(|item| source_name(&item.definition) == "each")
		.unwrap();
	let nymph_sema::RuntimePayload::NymphBody(body) = &mut item.payload else {
		unreachable!()
	};
	body.annotations.iterations = Arc::from([]);
	assert!(
		matches!(lower_runtime_definition(&context, Arc::new(item)), Err(nymph_sema::StableLoweringError::MissingAnnotation { channel, .. }) if channel == "iteration")
	);
}

#[test]
fn body_local_pattern_facts_survive_unrelated_earlier_declarations() {
	let body = "enum Choice { Pair(left: int, right: int) }\nfunc choose(value: Choice): int = match (value) { Choice.Pair(left, right) -> left + right }";
	let before = artifacts(body)
		.into_iter()
		.find(|item| source_name(&item.definition) == "choose")
		.unwrap();
	let after = artifacts(&format!("let unrelated = 1\n{body}"))
		.into_iter()
		.find(|item| source_name(&item.definition) == "choose")
		.unwrap();
	let (nymph_sema::RuntimePayload::NymphBody(before), nymph_sema::RuntimePayload::NymphBody(after)) =
		(&before.payload, &after.payload)
	else {
		unreachable!()
	};
	assert_eq!(
		before.annotations.pattern_variants,
		after.annotations.pattern_variants
	);
	assert_eq!(
		before.annotations.positional_fields,
		after.annotations.positional_fields
	);
}
