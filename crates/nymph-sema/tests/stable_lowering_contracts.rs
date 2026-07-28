use std::{collections::HashMap, sync::Arc};

use ecow::EcoString;
use nymph_hir::hir::{HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirMethod};
use nymph_sema::{
	CanonicalModuleSpecifier, DeclarationCategory, DeclarationKey, DefinitionId, EmittedBindingName,
	EmittedMemberName, ExternalAbi, LoweredHirFragment, LoweredRuntimeDefinition, ModuleIdentity,
	ModuleOrigin, RuntimeAssemblyPlacement, RuntimeDefinition, RuntimeDefinitionLookup,
	RuntimeDefinitionLookupError, RuntimePayload, RuntimePlacement, StableDemandSet,
	StableLoweringContext, StableNameLookup, StableNameLookupError, StableShapeFact,
	StableShapeLookup, StableShapeLookupError, StableShapeRequest,
};

fn module(origin: ModuleOrigin, project: &str, path: &str) -> ModuleIdentity {
	ModuleIdentity {
		origin,
		project: project.into(),
		path: path.into(),
	}
}

fn definition(module: ModuleIdentity, category: DeclarationCategory, name: &str) -> DefinitionId {
	DefinitionId::new(module, DeclarationKey::top_level(category, name))
}

#[derive(Default)]
struct FakeLookup {
	runtime: HashMap<DefinitionId, Result<Arc<RuntimeDefinition>, RuntimeDefinitionLookupError>>,
	shapes: HashMap<StableShapeRequest, Result<StableShapeFact, StableShapeLookupError>>,
}

impl RuntimeDefinitionLookup for FakeLookup {
	fn runtime_definition(
		&self,
		definition: &DefinitionId,
	) -> Result<Arc<RuntimeDefinition>, RuntimeDefinitionLookupError> {
		self.runtime.get(definition).cloned().unwrap_or_else(|| {
			Err(RuntimeDefinitionLookupError::Missing {
				definition: definition.clone(),
			})
		})
	}
}

impl StableShapeLookup for FakeLookup {
	fn stable_shape(
		&self,
		request: &StableShapeRequest,
	) -> Result<StableShapeFact, StableShapeLookupError> {
		self.shapes.get(request).cloned().unwrap_or_else(|| {
			Err(StableShapeLookupError::Missing {
				request: request.clone(),
			})
		})
	}
}

impl StableNameLookup for FakeLookup {
	fn binding_name(
		&self,
		definition: &DefinitionId,
	) -> Result<EmittedBindingName, StableNameLookupError> {
		Ok(EmittedBindingName::new(format!(
			"b${}",
			definition.key.duplicate()
		)))
	}

	fn member_name(
		&self,
		definition: &DefinitionId,
	) -> Result<EmittedMemberName, StableNameLookupError> {
		Ok(EmittedMemberName::new(match &definition.key {
			DeclarationKey::Member { name, .. } => name.clone(),
			_ => "member".into(),
		}))
	}

	fn module_specifier(
		&self,
		module: &ModuleIdentity,
	) -> Result<CanonicalModuleSpecifier, StableNameLookupError> {
		Ok(match &module.origin {
			ModuleOrigin::Compiler => CanonicalModuleSpecifier::CompilerRuntime(module.path.clone()),
			ModuleOrigin::ImportableStd => CanonicalModuleSpecifier::Importable(module.path.clone()),
			ModuleOrigin::Project(project) if project == "app" => {
				CanonicalModuleSpecifier::Project(module.path.clone())
			}
			ModuleOrigin::Project(_) => CanonicalModuleSpecifier::Importable(EcoString::from(format!(
				"{}:{}",
				module.project, module.path
			))),
		})
	}
}

fn accepts_context(_: &impl StableLoweringContext) {}

#[test]
fn context_composes_sema_owned_lookups_for_exact_stable_ids() {
	let mut lookup = FakeLookup::default();
	accepts_context(&lookup);

	let id = definition(
		module(ModuleOrigin::Project("app".into()), "app", "main"),
		DeclarationCategory::Function,
		"run",
	);
	let missing = definition(id.module.clone(), DeclarationCategory::Function, "missing");
	assert_eq!(
		lookup.runtime_definition(&missing),
		Err(RuntimeDefinitionLookupError::Missing {
			definition: missing
		})
	);
	let artifact = Arc::new(RuntimeDefinition {
		definition: id.clone(),
		source_owner: id.module.clone(),
		placement: RuntimePlacement::Attached {
			owner: id.clone(),
			name: "default".into(),
		},
		payload: RuntimePayload::MaterializedInterfaceMember {
			body_definition: id.clone(),
			interface_member: id.clone(),
		},
	});
	lookup.runtime.insert(id.clone(), Ok(artifact.clone()));
	assert!(Arc::ptr_eq(
		&lookup.runtime_definition(&id).unwrap(),
		&artifact
	));
	assert_eq!(
		lookup.stable_shape(&StableShapeRequest::ExternalAbi(id.clone())),
		Err(StableShapeLookupError::Missing {
			request: StableShapeRequest::ExternalAbi(id),
		})
	);
}

#[test]
fn shape_requests_cover_exact_lowering_facts_and_recovery() {
	let id = definition(
		module(ModuleOrigin::Project("dep".into()), "dep", "types"),
		DeclarationCategory::Function,
		"host",
	);
	let request = StableShapeRequest::ExternalAbi(id.clone());
	let abi = ExternalAbi {
		marker: "clock".into(),
		callable: nymph_sema::ExternalCallable::Linked {
			module: "runtime/time".into(),
			symbol: "clock".into(),
		},
		marshal: None,
	};
	let mut lookup = FakeLookup::default();
	lookup.shapes.insert(
		request.clone(),
		Ok(StableShapeFact::ExternalAbi(abi.clone())),
	);
	assert_eq!(
		lookup.stable_shape(&request),
		Ok(StableShapeFact::ExternalAbi(abi))
	);

	let recovered = StableShapeRequest::InterfaceShell(id.clone());
	lookup.shapes.insert(
		recovered.clone(),
		Err(StableShapeLookupError::Recovered {
			definition: id.clone(),
		}),
	);
	assert_eq!(
		lookup.stable_shape(&recovered),
		Err(StableShapeLookupError::Recovered {
			definition: id.clone()
		})
	);

	let requests = [
		StableShapeRequest::TypeShell(id.clone()),
		StableShapeRequest::Member(id.clone()),
		StableShapeRequest::Implementation(id.clone()),
		StableShapeRequest::InterfaceShell(id),
	];
	assert_eq!(requests.len(), 4);
}

#[test]
fn naming_keeps_bindings_members_and_module_kinds_distinct() {
	let lookup = FakeLookup::default();
	let project = module(ModuleOrigin::Project("app".into()), "app", "main");
	let imported = module(ModuleOrigin::Project("lib".into()), "lib", "collections");
	let compiler = module(ModuleOrigin::Compiler, "nymph", "core");
	let owner = definition(project.clone(), DeclarationCategory::Struct, "Item");
	let member = DefinitionId::new(
		project.clone(),
		DeclarationKey::member(owner, DeclarationCategory::Field, "value"),
	);

	assert_eq!(lookup.binding_name(&member).unwrap().as_str(), "b$0");
	assert_eq!(lookup.member_name(&member).unwrap().as_str(), "value");
	assert_eq!(
		lookup.module_specifier(&project).unwrap(),
		CanonicalModuleSpecifier::Project("main".into())
	);
	assert_eq!(
		lookup.module_specifier(&imported).unwrap(),
		CanonicalModuleSpecifier::Importable("lib:collections".into())
	);
	assert_eq!(
		lookup.module_specifier(&compiler).unwrap(),
		CanonicalModuleSpecifier::CompilerRuntime("core".into())
	);
}

#[test]
fn lowered_fragments_encode_placement_and_ordered_deduplicated_demands() {
	let module = module(ModuleOrigin::Project("app".into()), "app", "main");
	let owner = definition(module.clone(), DeclarationCategory::Struct, "Counter");
	let first = definition(module.clone(), DeclarationCategory::Function, "first");
	let second = definition(module, DeclarationCategory::Function, "second");
	let mut demands = StableDemandSet::new();
	demands.insert(first.clone());
	demands.insert(second.clone());
	demands.insert(first.clone());

	let lowered = LoweredRuntimeDefinition::new(
		owner.clone(),
		LoweredHirFragment::TopLevelFunction(HirFunc {
			name: "Counter".into(),
			params: vec![],
			body: HirExpr::Bool(false),
		}),
		demands,
		RuntimeAssemblyPlacement::Module(owner.module.clone()),
	);
	assert_eq!(lowered.definition(), &owner);
	assert_eq!(lowered.demands(), &[first, second]);
	assert_eq!(
		lowered.placement(),
		&RuntimeAssemblyPlacement::Module(owner.module.clone())
	);

	let _value = LoweredHirFragment::TopLevelValue(HirLet {
		name: "answer".into(),
		mutable: false,
		value: HirExpr::Bool(false),
	});
	let method = || HirMethod {
		name: "step".into(),
		params: vec![],
		body: HirExpr::Bool(false),
	};
	let variants = [
		LoweredHirFragment::TopLevelExternal {
			name: EmittedBindingName::new("host"),
			abi: ExternalAbi {
				marker: "host".into(),
				callable: nymph_sema::ExternalCallable::Deferred,
				marshal: None,
			},
		},
		LoweredHirFragment::StructShell(HirClass {
			name: "Counter".into(),
			fields: vec![],
			methods: vec![],
			statics: vec![],
		}),
		LoweredHirFragment::EnumShell(HirEnum {
			name: "State".into(),
			variants: vec![],
			methods: vec![],
			statics: vec![],
		}),
		LoweredHirFragment::AttachedInstance {
			owner: owner.clone(),
			method: method(),
		},
		LoweredHirFragment::AttachedStatic {
			owner: owner.clone(),
			method: method(),
		},
		LoweredHirFragment::AttachedMember {
			owner: owner.clone(),
			method: method(),
		},
		LoweredHirFragment::MaterializedDefault {
			owner: owner.clone(),
			implementation: owner.clone(),
			interface_member: owner,
			method: method(),
		},
	];
	assert_eq!(variants.len(), 7);
}
