use std::sync::Arc;

use nymph_sema::{
	BinderScope, DeclarationCategory, DeclarationKey, DefinitionId, DefinitionShapeKind,
	ExportedDefinition, ExportedImpl, ExternalAbi, FieldShape, GenericConstraint, GenericParameter,
	GenericParameterId, InterfaceType, MemberKind, MemberShape, ModuleEnvironment, ModuleIdentity,
	ModuleInterface, ModuleOrigin, ParameterShape, RecoveredDefinitionReference,
	RecoveredExportedDefinition, RecoveredExportedImpl, RecoveredInterfaceType,
	RecoveredModuleInterface, RecoveredSupportDefinition, SemanticAvailability, SemanticEnvironment,
};
use nymph_sema::{ParamIdx, TyKind};

fn module(path: &str) -> ModuleIdentity {
	ModuleIdentity {
		origin: ModuleOrigin::Project("test".into()),
		project: "test".into(),
		path: path.into(),
	}
}

fn id(owner: &ModuleIdentity, name: &str) -> DefinitionId {
	DefinitionId::new(
		owner.clone(),
		DeclarationKey::top_level(DeclarationCategory::Struct, name),
	)
}

fn exported(id: DefinitionId, name: &str) -> ExportedDefinition {
	ExportedDefinition {
		id,
		name: name.into(),
		visibility: None,
		kind: DefinitionShapeKind::Struct,
		binders: vec![],
		constraints: vec![],
		parameters: vec![],
		return_type: None,
		ty: None,
		fields: vec![],
		variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: None,
		runtime_owner: None,
	}
}

fn complete(owner: ModuleIdentity, exports: Vec<ExportedDefinition>) -> Arc<ModuleEnvironment> {
	Arc::new(ModuleEnvironment::Complete(ModuleInterface {
		module: owner,
		exports,
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	}))
}

fn member(
	owner: &DefinitionId,
	name: &str,
	kind: MemberKind,
	ret: InterfaceType,
) -> MemberShape<InterfaceType> {
	MemberShape {
		id: DefinitionId::new(
			owner.module.clone(),
			DeclarationKey::member(owner.clone(), DeclarationCategory::Method, name),
		),
		name: name.into(),
		visibility: None,
		kind,
		binders: vec![],
		constraints: vec![],
		parameters: vec![],
		return_type: ret,
		external: None,
		runtime_owner: Some(owner.clone()),
		has_default: false,
	}
}

#[test]
fn complete_interfaces_and_impls_populate_owned_dispatch_registries() {
	let owner = module("dispatch");
	let iface_id = id(&owner, "Project");
	let type_id = id(&owner, "Widget");
	let mut iface = exported(iface_id.clone(), "Project");
	iface.kind = DefinitionShapeKind::Interface;
	let mut default_method = member(
		&iface_id,
		"project",
		MemberKind::Function,
		InterfaceType::Int,
	);
	default_method.has_default = true;
	iface.members.push(default_method.clone());
	let mut ty = exported(type_id.clone(), "Widget");
	ty.members.push(member(
		&type_id,
		"touch",
		MemberKind::MutatingFunction,
		InterfaceType::Boolean,
	));
	ty.members.push(member(
		&type_id,
		"make",
		MemberKind::StaticFunction,
		InterfaceType::Named {
			definition: type_id.clone(),
			positional: vec![],
			named: vec![],
		},
	));
	let touch_id = ty.members[0].id.clone();
	let impl_id = DefinitionId::new(
		owner.clone(),
		DeclarationKey::top_level(DeclarationCategory::Implementation, "project-widget"),
	);
	let implementation = ExportedImpl {
		id: impl_id.clone(),
		visibility: None,
		interface: Some(iface_id.clone()),
		interface_arguments: vec![("Output".into(), InterfaceType::String)],
		interface_argument_bindings: vec![],
		self_type: InterfaceType::Named {
			definition: type_id.clone(),
			positional: vec![],
			named: vec![],
		},
		mutable: false,
		binders: vec![],
		constraints: vec![],
		members: vec![],
		member_slots: vec![],
		runtime_owner: Some(impl_id.clone()),
	};
	let input = Arc::new(ModuleEnvironment::Complete(ModuleInterface {
		module: owner,
		exports: vec![iface, ty],
		support_definitions: vec![],
		implementations: vec![implementation],
		fingerprint: 0,
	}));
	let env = SemanticEnvironment::from_modules(module("consumer"), &[input]).unwrap();
	let iface_local = env.imported.defs.by_stable(&iface_id).unwrap();
	let iface_method = &env.imported.interfaces[&iface_local].methods["project"];
	assert_eq!(iface_method.definition.as_ref(), Some(&default_method.id));
	assert!(iface_method.has_default);
	assert_eq!(env.imported.implementations.impls.len(), 1);
	let implementation = &env.imported.implementations.impls[0];
	assert_eq!(implementation.definition.as_ref(), Some(&impl_id));
	assert_eq!(implementation.args[0].0, "Output");
	assert!(matches!(
		env.interner.kind(implementation.args[0].1),
		TyKind::String
	));
	assert!(
		!implementation.methods.contains_key("project"),
		"interface defaults remain interface-owned so solver dispatch cannot mistake them for overrides"
	);
	assert_eq!(env.imported.inherent.impls.len(), 1);
	let inherent = &env.imported.inherent.impls[0];
	assert!(inherent.imported);
	assert_eq!(
		inherent.methods["touch"].definition.as_ref(),
		Some(&touch_id)
	);
	assert!(inherent.methods["touch"].mutating);
	assert!(inherent.methods["make"].namespaced);
}

#[test]
fn duplicate_stable_id_is_reused() {
	let a = module("a");
	let shared = id(&a, "Shared");
	let env = SemanticEnvironment::from_modules(
		module("consumer"),
		&[
			complete(a.clone(), vec![exported(shared.clone(), "First")]),
			complete(module("b"), vec![exported(shared.clone(), "Second")]),
		],
	)
	.unwrap();

	assert_eq!(env.imported.defs.defs.len(), 1);
	assert_eq!(
		env.imported.defs.get("First"),
		env.imported.defs.get("Second")
	);
	assert!(env.imported.defs.by_stable(&shared).is_some());
}

#[test]
fn same_name_with_different_owners_remains_distinct_and_later_wins() {
	let a = module("a");
	let b = module("b");
	let a_id = id(&a, "Thing");
	let b_id = id(&b, "Thing");
	let env = SemanticEnvironment::from_modules(
		module("consumer"),
		&[
			complete(a.clone(), vec![exported(a_id.clone(), "Thing")]),
			complete(b.clone(), vec![exported(b_id.clone(), "Thing")]),
		],
	)
	.unwrap();

	assert_ne!(
		env.imported.defs.by_stable(&a_id),
		env.imported.defs.by_stable(&b_id)
	);
	assert_eq!(
		env.imported.defs.get("Thing"),
		env.imported.defs.by_stable(&b_id)
	);
}

#[test]
fn private_support_is_stable_addressable_but_not_bare_visible() {
	let a = module("a");
	let public = id(&a, "Public");
	let private = id(&a, "Private");
	let mut interface = match complete(a.clone(), vec![exported(public, "Public")]).as_ref() {
		ModuleEnvironment::Complete(interface) => interface.clone(),
		ModuleEnvironment::Recovered(_) => unreachable!(),
	};
	interface
		.support_definitions
		.push(nymph_sema::SupportDefinition {
			definition: exported(private.clone(), "Private"),
		});
	let env = SemanticEnvironment::from_modules(
		module("consumer"),
		&[Arc::new(ModuleEnvironment::Complete(interface))],
	)
	.unwrap();

	assert_eq!(env.imported.defs.get("Private"), None);
	assert!(env.imported.defs.by_stable(&private).is_some());
}

#[test]
fn dependency_order_deterministically_allocates_def_ids() {
	let a = module("a");
	let b = module("b");
	let a_id = id(&a, "A");
	let b_id = id(&b, "B");
	let env = SemanticEnvironment::from_modules(
		module("consumer"),
		&[
			complete(a.clone(), vec![exported(a_id.clone(), "A")]),
			complete(b.clone(), vec![exported(b_id.clone(), "B")]),
		],
	)
	.unwrap();

	assert_eq!(env.imported.defs.by_stable(&a_id).unwrap().0, 0);
	assert_eq!(env.imported.defs.by_stable(&b_id).unwrap().0, 1);
	assert_eq!(env.module_exports[&a].by_name["A"], a_id);
}

#[test]
fn impl_only_referenced_identity_is_allocated_in_pass_a() {
	let a = module("a");
	let hidden = id(&a, "OnlyInImplHeader");
	let implementation = ExportedImpl {
		id: DefinitionId::new(
			a.clone(),
			DeclarationKey::top_level(DeclarationCategory::Implementation, "impl"),
		),
		visibility: None,
		interface: None,
		interface_arguments: vec![],
		interface_argument_bindings: vec![],
		self_type: InterfaceType::Named {
			definition: hidden.clone(),
			positional: vec![],
			named: vec![],
		},
		mutable: false,
		binders: vec![],
		constraints: vec![],
		members: vec![],
		member_slots: vec![],
		runtime_owner: None,
	};
	let input = Arc::new(ModuleEnvironment::Complete(ModuleInterface {
		module: a,
		exports: vec![],
		support_definitions: vec![],
		implementations: vec![implementation],
		fingerprint: 0,
	}));
	let env = SemanticEnvironment::from_modules(module("consumer"), &[input]).unwrap();

	assert!(env.imported.defs.by_stable(&hidden).is_some());
	assert_eq!(env.imported.defs.get("OnlyInImplHeader"), None);
}

#[test]
fn recovery_taints_without_fabricating_poison_ids() {
	let a = module("a");
	let known = id(&a, "Known");
	let recovered = RecoveredExportedDefinition {
		id: known.clone(),
		name: "Known".into(),
		visibility: None,
		kind: DefinitionShapeKind::Struct,
		availability: SemanticAvailability::Available,
		binders: vec![],
		constraints: vec![],
		parameters: vec![],
		return_type: None,
		ty: Some(RecoveredInterfaceType::Poison),
		fields: vec![],
		variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: None,
		runtime_owner: None,
	};
	let poison_impl = RecoveredExportedImpl {
		id: DefinitionId::new(
			a.clone(),
			DeclarationKey::top_level(DeclarationCategory::Implementation, "bad"),
		),
		visibility: None,
		availability: SemanticAvailability::StructureUnavailable,
		interface: Some(RecoveredDefinitionReference::Poison),
		interface_arguments: vec![],
		self_type: RecoveredInterfaceType::Poison,
		mutable: false,
		binders: vec![],
		constraints: vec![],
		members: vec![],
		member_slots: vec![],
		runtime_owner: None,
	};
	let input = Arc::new(ModuleEnvironment::Recovered(RecoveredModuleInterface {
		module: a,
		exports: vec![recovered],
		support_definitions: vec![],
		implementations: vec![poison_impl],
		fingerprint: 0,
	}));
	let env = SemanticEnvironment::from_modules(module("consumer"), &[input]).unwrap();

	assert!(env.contains_recovery);
	assert_eq!(env.imported.defs.defs.len(), 1);
	assert!(env.imported.defs.by_stable(&known).is_some());
	assert!(env.imported.implementations.impls.is_empty());
	assert!(env.imported.inherent.impls.is_empty());
}

#[test]
fn pass_b_instantiates_owned_definition_facts_after_all_ids_exist() {
	let a = module("facts");
	let box_id = id(&a, "Box");
	let generic = GenericParameter {
		id: GenericParameterId::new(box_id.binder(BinderScope::Definition, 0), 0),
		name: "T".into(),
	};
	let iface = id(&a, "Project");
	let mut box_def = exported(box_id, "Box");
	box_def.binders.push(generic.clone());
	box_def.fields.push(FieldShape {
		id: DefinitionId::new(
			a.clone(),
			DeclarationKey::member(box_def.id.clone(), DeclarationCategory::Field, "value"),
		),
		name: "value".into(),
		visibility: None,
		ty: InterfaceType::Generic(generic.id.clone()),
		mutable: false,
		has_default: false,
	});
	box_def.constraints.push(GenericConstraint {
		parameter: generic.id.clone(),
		interface: iface.clone(),
		positional: vec![],
		named: vec![("Output".into(), InterfaceType::Generic(generic.id.clone()))],
	});
	let mut function = exported(
		DefinitionId::new(
			a.clone(),
			DeclarationKey::top_level(DeclarationCategory::Function, "wrap"),
		),
		"wrap",
	);
	function.kind = DefinitionShapeKind::Function;
	function.binders.push(generic.clone());
	function.parameters.push(ParameterShape {
		name: Some("item".into()),
		ty: InterfaceType::Generic(generic.id.clone()),
		mutable: false,
		spread: true,
	});
	function.return_type = Some(InterfaceType::Named {
		definition: box_def.id.clone(),
		positional: vec![InterfaceType::Generic(generic.id.clone())],
		named: vec![],
	});
	let mut namespace = exported(id(&a, "tools"), "tools");
	namespace.kind = DefinitionShapeKind::Namespace;
	namespace.members.push(MemberShape {
		id: DefinitionId::new(
			a.clone(),
			DeclarationKey::member(namespace.id.clone(), DeclarationCategory::Function, "make"),
		),
		name: "make".into(),
		visibility: None,
		kind: MemberKind::Function,
		binders: vec![],
		constraints: vec![],
		parameters: vec![],
		return_type: InterfaceType::Int,
		external: None,
		runtime_owner: None,
		has_default: false,
	});
	let env = SemanticEnvironment::from_modules(
		module("consumer"),
		&[complete(
			a,
			vec![
				box_def.clone(),
				function.clone(),
				namespace.clone(),
				exported(iface.clone(), "Project"),
			],
		)],
	)
	.unwrap();
	let box_local = env.imported.defs.by_stable(&box_def.id).unwrap();
	let sig = &env.imported.signatures.structs[&box_local];
	assert!(matches!(
		env.interner.kind(sig.fields[0].1),
		TyKind::Param(ParamIdx(0))
	));
	assert_eq!(
		sig.bounds[0].interface,
		env.imported.defs.by_stable(&iface).unwrap()
	);
	assert_eq!(sig.bounds[0].args[0].0, "Output");
	let func = &env.imported.signatures.funcs[&env.imported.defs.by_stable(&function.id).unwrap()];
	assert_eq!(func.params[0].label.as_deref(), Some("item"));
	assert!(func.params[0].spread);
	assert!(matches!(
		env.interner.kind(func.params[0].ty),
		TyKind::Param(ParamIdx(0))
	));
	let ns =
		&env.imported.signatures.namespaces[&env.imported.defs.by_stable(&namespace.id).unwrap()];
	assert!(ns.members.contains_key("make"));
}

#[test]
fn recovered_poison_is_error_fact_and_environment_remains_tainted() {
	let a = module("recovered-fact");
	let stable = id(&a, "value");
	let mut recovered = RecoveredExportedDefinition {
		id: stable.clone(),
		name: "value".into(),
		visibility: None,
		kind: DefinitionShapeKind::Let,
		availability: SemanticAvailability::Available,
		binders: vec![],
		constraints: vec![],
		parameters: vec![],
		return_type: None,
		ty: Some(RecoveredInterfaceType::Poison),
		fields: vec![],
		variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: None,
		runtime_owner: None,
	};
	recovered.kind = DefinitionShapeKind::Let;
	let input = Arc::new(ModuleEnvironment::Recovered(RecoveredModuleInterface {
		module: a,
		exports: vec![recovered],
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	}));
	let env = SemanticEnvironment::from_modules(module("consumer"), &[input]).unwrap();
	let ty = env.imported.signatures.lets[&env.imported.defs.by_stable(&stable).unwrap()].ty;
	assert!(matches!(env.interner.kind(ty), TyKind::Error));
	assert!(env.contains_recovery);
}

#[test]
fn recovered_definitions_retain_every_independently_known_owned_fact() {
	let owner = module("recovered-complete-facts");
	let iface = id(&owner, "Project");
	let generic_owner = id(&owner, "Alias");
	let generic = GenericParameter {
		id: GenericParameterId::new(generic_owner.binder(BinderScope::Definition, 0), 0),
		name: "T".into(),
	};
	let mut alias = RecoveredExportedDefinition {
		id: generic_owner.clone(),
		name: "Alias".into(),
		visibility: None,
		kind: DefinitionShapeKind::TypeAlias,
		availability: SemanticAvailability::Available,
		binders: vec![generic.clone()],
		constraints: vec![nymph_sema::RecoveredGenericConstraint {
			parameter: generic.id.clone(),
			interface: RecoveredDefinitionReference::Known(iface.clone()),
			positional: vec![RecoveredInterfaceType::Known(InterfaceType::Generic(
				generic.id.clone(),
			))],
			named: vec![],
		}],
		parameters: vec![],
		return_type: None,
		ty: Some(RecoveredInterfaceType::Known(InterfaceType::Generic(
			generic.id.clone(),
		))),
		fields: vec![],
		variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: None,
		runtime_owner: None,
	};
	let value_id = DefinitionId::new(
		owner.clone(),
		DeclarationKey::top_level(DeclarationCategory::Let, "host_value"),
	);
	alias.visibility = None;
	let value = RecoveredExportedDefinition {
		id: value_id.clone(),
		name: "host_value".into(),
		visibility: None,
		kind: DefinitionShapeKind::Let,
		availability: SemanticAvailability::Available,
		binders: vec![generic.clone()],
		constraints: alias.constraints.clone(),
		parameters: vec![],
		return_type: None,
		ty: Some(RecoveredInterfaceType::Known(InterfaceType::Generic(
			generic.id.clone(),
		))),
		fields: vec![],
		variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: Some(ExternalAbi {
			marker: "host".into(),
			callable: nymph_sema::ExternalCallable::Linked {
				module: "runtime".into(),
				symbol: "value".into(),
			},
			marshal: Some(nymph_hir::hir::MarshalKind::Int),
		}),
		runtime_owner: None,
	};
	let input = Arc::new(ModuleEnvironment::Recovered(RecoveredModuleInterface {
		module: owner,
		exports: vec![alias, value],
		support_definitions: vec![RecoveredSupportDefinition {
			definition: RecoveredExportedDefinition {
				id: iface.clone(),
				name: "Project".into(),
				visibility: None,
				kind: DefinitionShapeKind::Interface,
				availability: SemanticAvailability::Available,
				binders: vec![],
				constraints: vec![],
				parameters: vec![],
				return_type: None,
				ty: None,
				fields: vec![],
				variants: vec![],
				members: vec![],
				super_interfaces: vec![],
				external: None,
				runtime_owner: None,
			},
		}],
		implementations: vec![],
		fingerprint: 0,
	}));
	let env = SemanticEnvironment::from_modules(module("consumer"), &[input]).unwrap();
	let alias_sig =
		&env.imported.signatures.aliases[&env.imported.defs.by_stable(&generic_owner).unwrap()];
	assert_eq!(alias_sig.generics, ["T"]);
	assert_eq!(alias_sig.bounds.len(), 1);
	assert!(matches!(
		env.interner.kind(alias_sig.target),
		TyKind::Param(ParamIdx(0))
	));
	let value_sig = &env.imported.signatures.lets[&env.imported.defs.by_stable(&value_id).unwrap()];
	assert_eq!(value_sig.generics, ["T"]);
	assert_eq!(value_sig.bounds.len(), 1);
	assert!(matches!(
		env.interner.kind(value_sig.ty),
		TyKind::Param(ParamIdx(0))
	));
	assert_eq!(
		env.imported.external_abis[&env.imported.defs.by_stable(&value_id).unwrap()].marshal,
		Some(nymph_hir::hir::MarshalKind::Int)
	);
}
