use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use nymph_ast::decl::Visibility;
use nymph_hir::hir::MarshalKind;
use nymph_sema::{
	BinderId, BinderScope, CanonicalizationContext, ConstraintShape, DeclarationCategory,
	DeclarationKey, DefinitionId, DefinitionShapeKind, ExportedDefinition, ExportedImpl, ExternalAbi,
	FieldShape, GenericParameter, GenericParameterId, InstantiationContext, InterfaceConversionError,
	InterfaceType, MemberKind, MemberShape, ModuleIdentity, ParameterShape,
	RecoveredExportedDefinition, RecoveredInterfaceType, RecoveredModuleInterface,
	RecoveredSupportDefinition, SemanticAvailability, SuperInterfaceShape, SupportDefinition,
	VariantShape, canonicalize_type, instantiate_interface_type,
};
use nymph_sema::{DefId, GenericArgs, InferVar, Interner, ModuleInterface, ParamIdx};

fn definition(name: &str) -> DefinitionId {
	DefinitionId::new(
		ModuleIdentity {
			origin: nymph_sema::ModuleOrigin::Project("app".into()),
			project: "app".into(),
			path: "types".into(),
		},
		DeclarationKey::top_level(DeclarationCategory::Struct, name),
	)
}

#[test]
fn every_type_shape_round_trips_into_a_fresh_interner() {
	let mut source = Interner::new();
	let def = DefId(7);
	let binder = BinderId::new(definition("Owner"), BinderScope::Definition, 0);
	let parameter = GenericParameterId::new(binder.clone(), 0);
	let param = source.mk_param(ParamIdx(3));
	let list = source.mk_list(param);
	let tuple = source.mk_tuple(vec![source.int(), source.string()]);
	let map = source.mk_map(source.string(), list);
	let function = source.mk_fn(vec![tuple, map], source.boolean());
	let adt = source.mk_adt(
		def,
		GenericArgs::new(vec![function], vec![("Output".into(), param)]),
	);
	let mutable = source.mk_mut(adt);
	let intersection = source.mk_intersection(vec![mutable, source.char()]);
	let context = CanonicalizationContext::new(
		HashMap::from([(def, definition("Box"))]),
		HashMap::from([(ParamIdx(3), parameter.clone())]),
	);
	let canonical = canonicalize_type(&source, intersection, &context).unwrap();
	let mut fresh = Interner::new();
	let instantiated = instantiate_interface_type(
		&mut fresh,
		&canonical,
		&InstantiationContext::new(
			HashMap::from([(definition("Box"), DefId(99))]),
			HashMap::from([(parameter, ParamIdx(8))]),
		),
	);

	let expected_param = fresh.mk_param(ParamIdx(8));
	let expected_list = fresh.mk_list(expected_param);
	let expected_tuple = fresh.mk_tuple(vec![fresh.int(), fresh.string()]);
	let expected_map = fresh.mk_map(fresh.string(), expected_list);
	let expected_fn = fresh.mk_fn(vec![expected_tuple, expected_map], fresh.boolean());
	let expected_adt = fresh.mk_adt(
		DefId(99),
		GenericArgs::new(vec![expected_fn], vec![("Output".into(), expected_param)]),
	);
	let expected_mut = fresh.mk_mut(expected_adt);
	let expected = fresh.mk_intersection(vec![expected_mut, fresh.char()]);
	assert_eq!(fresh.kind(instantiated), fresh.kind(expected));
}

#[test]
fn primitive_and_self_types_are_exact() {
	let source = Interner::new();
	let context = CanonicalizationContext::default();
	let pairs = [
		(source.int(), InterfaceType::Int),
		(source.uint(), InterfaceType::UInt),
		(source.float(), InterfaceType::Float),
		(source.char(), InterfaceType::Char),
		(source.string(), InterfaceType::String),
		(source.boolean(), InterfaceType::Boolean),
		(source.void(), InterfaceType::Void),
		(source.never(), InterfaceType::Never),
		(source.self_ty(), InterfaceType::SelfType),
	];
	for (local, canonical) in pairs {
		assert_eq!(canonicalize_type(&source, local, &context), Ok(canonical));
	}
}

#[test]
fn incomplete_types_return_exact_errors() {
	let mut interner = Interner::new();
	let infer = interner.mk_infer(InferVar(4));
	assert_eq!(
		canonicalize_type(&interner, infer, &CanonicalizationContext::default()),
		Err(InterfaceConversionError::UnsolvedInference(InferVar(4)))
	);
	assert_eq!(
		canonicalize_type(
			&interner,
			interner.error(),
			&CanonicalizationContext::default()
		),
		Err(InterfaceConversionError::ErrorType)
	);
}

#[test]
fn missing_definition_and_binder_mappings_are_errors_not_fabricated_types() {
	let mut interner = Interner::new();
	let adt = interner.mk_adt(DefId(1), GenericArgs::none());
	assert_eq!(
		canonicalize_type(&interner, adt, &CanonicalizationContext::default()),
		Err(InterfaceConversionError::UnknownDefinition(DefId(1)))
	);
	let param = interner.mk_param(ParamIdx(1));
	assert_eq!(
		canonicalize_type(&interner, param, &CanonicalizationContext::default()),
		Err(InterfaceConversionError::UnknownBinder(ParamIdx(1)))
	);
}

#[test]
fn module_interface_equality_and_fingerprint_are_structural_and_deterministic() {
	let interface = ModuleInterface {
		module: ModuleIdentity {
			origin: nymph_sema::ModuleOrigin::Project("app".into()),
			project: "app".into(),
			path: "main".into(),
		},
		exports: vec![],
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	};
	let mut same = interface.clone();
	assert_eq!(interface, same);
	assert_eq!(
		interface.structural_fingerprint(),
		same.structural_fingerprint()
	);
	same.module.path = "other".into();
	assert_ne!(interface, same);
	assert_ne!(
		interface.structural_fingerprint(),
		same.structural_fingerprint()
	);
	let mut metadata_changed = interface.clone();
	metadata_changed.fingerprint = 42;
	assert_eq!(interface, metadata_changed);
	use std::hash::{DefaultHasher, Hash, Hasher};
	let hash = |value: &ModuleInterface| {
		let mut state = DefaultHasher::new();
		value.hash(&mut state);
		state.finish()
	};
	assert_eq!(hash(&interface), hash(&metadata_changed));
	assert_eq!(
		interface.structural_fingerprint(),
		metadata_changed.structural_fingerprint()
	);
}

fn hash<T: Hash>(value: &T) -> u64 {
	let mut state = DefaultHasher::new();
	value.hash(&mut state);
	state.finish()
}

fn complete_fixture() -> ModuleInterface {
	let owner = definition("Owner");
	let parameter_id = GenericParameterId::new(owner.binder(BinderScope::Definition, 0), 0);
	let binder = GenericParameter {
		id: parameter_id.clone(),
		name: "T".into(),
	};
	let constraint = ConstraintShape {
		parameter: parameter_id,
		interface: definition("Bound"),
		positional: vec![InterfaceType::Int],
		named: vec![("Output".into(), InterfaceType::String)],
	};
	let field = FieldShape {
		id: definition("field"),
		name: "field".into(),
		visibility: Some(Visibility::Public),
		ty: InterfaceType::Int,
		mutable: true,
		has_default: true,
	};
	let member = MemberShape {
		id: definition("member"),
		name: "member".into(),
		visibility: Some(Visibility::Internal),
		kind: MemberKind::Function,
		binders: vec![binder.clone()],
		constraints: vec![constraint.clone()],
		parameters: vec![ParameterShape {
			name: Some("arg".into()),
			ty: InterfaceType::String,
			mutable: true,
			spread: true,
		}],
		return_type: InterfaceType::Boolean,
		external: Some(ExternalAbi {
			marker: "js".into(),
			callable: nymph_sema::ExternalCallable::Linked {
				module: "host".into(),
				symbol: "call".into(),
			},
			marshal: Some(MarshalKind::Int),
		}),
		runtime_owner: Some(owner.clone()),
		has_default: true,
	};
	let exported = ExportedDefinition {
		id: owner.clone(),
		name: "Owner".into(),
		visibility: Some(Visibility::Public),
		kind: DefinitionShapeKind::Struct,
		binders: vec![binder.clone()],
		constraints: vec![constraint.clone()],
		parameters: vec![ParameterShape {
			name: Some("value".into()),
			ty: InterfaceType::Int,
			mutable: false,
			spread: false,
		}],
		return_type: Some(InterfaceType::String),
		ty: Some(InterfaceType::Int),
		fields: vec![field.clone()],
		variants: vec![VariantShape {
			id: definition("Variant"),
			name: "Variant".into(),
			fields: vec![field],
		}],
		members: vec![member.clone()],
		super_interfaces: vec![SuperInterfaceShape {
			interface: definition("Bound"),
			positional: vec![InterfaceType::Int],
			named: vec![("Output".into(), InterfaceType::String)],
		}],
		external: member.external.clone(),
		runtime_owner: Some(owner.clone()),
	};
	ModuleInterface {
		module: ModuleIdentity {
			origin: nymph_sema::ModuleOrigin::Project("app".into()),
			project: "app".into(),
			path: "complete".into(),
		},
		exports: vec![exported.clone()],
		support_definitions: vec![SupportDefinition {
			definition: exported,
		}],
		implementations: vec![ExportedImpl {
			id: definition("impl"),
			visibility: Some(Visibility::Private),
			interface: Some(definition("Bound")),
			interface_arguments: vec![("Output".into(), InterfaceType::String)],
			interface_argument_bindings: vec![],
			self_type: InterfaceType::Named {
				definition: owner.clone(),
				positional: vec![InterfaceType::Int],
				named: vec![("T".into(), InterfaceType::String)],
			},
			mutable: true,
			binders: vec![binder],
			constraints: vec![constraint],
			members: vec![member],
			member_slots: vec![],
			runtime_owner: Some(owner),
		}],
		fingerprint: 7,
	}
}

#[test]
fn fully_populated_complete_interface_hashes_every_observable_category() {
	let original = complete_fixture();
	macro_rules! changed {
		($mutation:expr) => {{
			let mut changed = original.clone();
			$mutation(&mut changed);
			assert_ne!(original, changed);
			assert_ne!(hash(&original), hash(&changed));
		}};
	}
	changed!(|x: &mut ModuleInterface| x.exports[0].name = "Renamed".into());
	changed!(|x: &mut ModuleInterface| x.exports[0].visibility = Some(Visibility::Private));
	changed!(|x: &mut ModuleInterface| x.exports[0].kind = DefinitionShapeKind::Enum);
	changed!(|x: &mut ModuleInterface| x.exports[0].binders[0].name = "U".into());
	changed!(
		|x: &mut ModuleInterface| x.exports[0].constraints[0].positional[0] = InterfaceType::UInt
	);
	changed!(|x: &mut ModuleInterface| x.exports[0].parameters[0].spread = true);
	changed!(|x: &mut ModuleInterface| x.exports[0].return_type = Some(InterfaceType::Void));
	changed!(|x: &mut ModuleInterface| x.exports[0].fields[0].mutable = false);
	changed!(|x: &mut ModuleInterface| x.exports[0].variants[0].name = "Other".into());
	changed!(|x: &mut ModuleInterface| x.exports[0].members[0].kind = MemberKind::StaticFunction);
	changed!(|x: &mut ModuleInterface| x.exports[0].super_interfaces.clear());
	changed!(
		|x: &mut ModuleInterface| x.exports[0].external.as_mut().unwrap().marshal =
			Some(MarshalKind::String)
	);
	changed!(|x: &mut ModuleInterface| x.exports[0].runtime_owner = Some(definition("Other")));
	changed!(|x: &mut ModuleInterface| x.implementations[0].mutable = false);
	changed!(|x: &mut ModuleInterface| x.implementations[0].members.clear());
	changed!(|x: &mut ModuleInterface| x.support_definitions.clear());
	let mut metadata = original.clone();
	metadata.fingerprint += 1;
	assert_eq!(original, metadata);
	assert_eq!(hash(&original), hash(&metadata));
}

fn recovered_fixture() -> RecoveredModuleInterface {
	let complete = complete_fixture().exports[0].clone();
	let recovered = RecoveredExportedDefinition {
		id: complete.id,
		name: complete.name,
		visibility: complete.visibility,
		kind: complete.kind,
		availability: SemanticAvailability::Available,
		binders: complete.binders,
		constraints: vec![],
		parameters: vec![ParameterShape {
			name: Some("nested".into()),
			ty: RecoveredInterfaceType::Poison,
			mutable: false,
			spread: false,
		}],
		return_type: Some(RecoveredInterfaceType::Known(InterfaceType::String)),
		ty: Some(RecoveredInterfaceType::Poison),
		fields: vec![],
		variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: complete.external,
		runtime_owner: complete.runtime_owner,
	};
	RecoveredModuleInterface {
		module: ModuleIdentity {
			origin: nymph_sema::ModuleOrigin::Project("app".into()),
			project: "app".into(),
			path: "recovered".into(),
		},
		exports: vec![recovered.clone()],
		support_definitions: vec![RecoveredSupportDefinition {
			definition: recovered,
		}],
		implementations: vec![],
		fingerprint: 9,
	}
}

#[test]
fn recovered_poison_is_structural_even_when_nested_and_availability_and_support_are_observable() {
	let original = recovered_fixture();
	let same = recovered_fixture();
	assert_eq!(original, same);
	assert_eq!(hash(&original), hash(&same));
	let mut known = original.clone();
	known.exports[0].parameters[0].ty = RecoveredInterfaceType::Known(InterfaceType::Void);
	assert_ne!(original, known);
	let mut unavailable = original.clone();
	unavailable.exports[0].availability = SemanticAvailability::StructureUnavailable;
	assert_ne!(original, unavailable);
	let mut support = original.clone();
	support.support_definitions.clear();
	assert_ne!(original, support);
	let mut metadata = original.clone();
	metadata.fingerprint += 1;
	assert_eq!(original, metadata);
	assert_eq!(hash(&original), hash(&metadata));
}
