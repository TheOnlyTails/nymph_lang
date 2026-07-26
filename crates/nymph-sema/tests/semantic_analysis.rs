use std::sync::Arc;

use nymph_ast::decl::Declaration;
use nymph_ast::expr::ExprKind;
use nymph_sema::{
	DeclarationCategory, DeclarationKey, DefinitionId, DefinitionShapeKind, EntryMode,
	ExportedDefinition, InterfaceType, ModuleAnnotations, ModuleEnvironment, ModuleIdentity,
	ModuleInterface, ModuleOrigin, RecoveredDefinitionReference, RecoveredExportedDefinition,
	RecoveredExportedImpl, RecoveredInterfaceType, RecoveredModuleInterface, SemanticAnalysis,
	SemanticAvailability, SemanticEnvironment, TyKind, check_module, check_module_with_environment,
};
use nymph_syntax::parse_module;

#[test]
fn semantic_analysis_owns_source_and_node_annotations_without_diagnostics() {
	let parsed = parse_module("func value(): string = 1", "analysis.nymph");
	assert!(parsed.diagnostics.is_empty());
	let module = parsed.tree;
	let body_id = match &module.members[0] {
		Declaration::Func { body, .. } => body.id,
		other => panic!("expected function, got {other:?}"),
	};
	let checked = check_module(&module);
	let annotations = Arc::new(ModuleAnnotations::from(checked.annotations.clone()));
	let diagnostics = checked.diags;
	assert!(!diagnostics.is_empty());

	let analysis = SemanticAnalysis {
		module: Arc::new(module.clone()),
		checked: Arc::new(checked.facts),
		annotations,
	};
	let cloned = analysis.clone();

	assert!(Arc::ptr_eq(&analysis.checked, &cloned.checked));
	assert_eq!(analysis.module.as_ref(), &module);
	assert!(analysis.annotations.get(body_id).is_some());
	assert!(format!("{analysis:?}").contains("SemanticAnalysis"));
	assert!(
		!diagnostics.is_empty(),
		"diagnostics remain a separate result"
	);
}

fn identity(path: &str) -> ModuleIdentity {
	ModuleIdentity {
		origin: ModuleOrigin::Project("test".into()),
		project: "test".into(),
		path: path.into(),
	}
}

#[test]
fn environment_check_uses_imported_function_without_mutating_environment() {
	let dependency = identity("dependency.nymph");
	let function_id = DefinitionId::new(
		dependency.clone(),
		DeclarationKey::top_level(DeclarationCategory::Function, "answer"),
	);
	let exported = ExportedDefinition {
		id: function_id.clone(),
		name: "answer".into(),
		visibility: None,
		kind: DefinitionShapeKind::Function,
		binders: vec![],
		constraints: vec![],
		parameters: vec![],
		return_type: Some(InterfaceType::Int),
		ty: None,
		fields: vec![],
		variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: None,
		runtime_owner: None,
	};
	let dependency = Arc::new(ModuleEnvironment::Complete(ModuleInterface {
		module: dependency,
		exports: vec![exported],
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	}));
	let current = identity("consumer.nymph");
	let intermediary = Arc::new(ModuleEnvironment::Complete(ModuleInterface {
		module: identity("intermediary.nymph"),
		exports: vec![],
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	}));
	let environment =
		SemanticEnvironment::from_modules(current.clone(), &[dependency, intermediary]).unwrap();
	let before = format!("{environment:?}");
	let parsed = parse_module("func local(): int = answer()", "consumer.nymph");
	assert!(parsed.diagnostics.is_empty());
	let answer_node = match &parsed.tree.members[0] {
		Declaration::Func { body, .. } => match &body.kind {
			ExprKind::Call { func, .. } => func.id,
			other => panic!("expected call body, got {other:?}"),
		},
		other => panic!("expected function, got {other:?}"),
	};
	let result = check_module_with_environment(
		Arc::new(parsed.tree),
		current.clone(),
		&environment,
		EntryMode::Library,
	);

	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	assert!(result.lowerable);
	assert_eq!(
		result
			.analysis
			.annotations
			.definition_target_of(answer_node),
		Some(&function_id),
	);
	assert_eq!(format!("{environment:?}"), before);
	assert_eq!(result.analysis.checked.semantic.local_definitions.start, 1);
	assert_eq!(result.analysis.checked.semantic.local_definitions.end, 2);
	assert_eq!(
		result
			.analysis
			.checked
			.semantic
			.stable_definition(nymph_sema::DefId(1)),
		Some(&DefinitionId::new(
			current,
			DeclarationKey::top_level(DeclarationCategory::Function, "local"),
		))
	);
}

#[test]
fn local_definition_overlays_imported_name_without_rechecking_imported_body() {
	let dependency = identity("dependency.nymph");
	let imported_id = DefinitionId::new(
		dependency.clone(),
		DeclarationKey::top_level(DeclarationCategory::Function, "answer"),
	);
	let imported = ExportedDefinition {
		id: imported_id,
		name: "answer".into(),
		visibility: None,
		kind: DefinitionShapeKind::Function,
		binders: vec![],
		constraints: vec![],
		parameters: vec![],
		return_type: Some(InterfaceType::String),
		ty: None,
		fields: vec![],
		variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: None,
		runtime_owner: None,
	};
	let dependency = Arc::new(ModuleEnvironment::Complete(ModuleInterface {
		module: dependency,
		exports: vec![imported],
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	}));
	let current = identity("consumer.nymph");
	let environment = SemanticEnvironment::from_modules(current.clone(), &[dependency]).unwrap();
	let source = "func answer(): int = 1\nfunc local(): int = answer()";
	let result = check_module_with_environment(
		Arc::new(parse_module(source, "consumer.nymph").tree),
		current,
		&environment,
		EntryMode::Library,
	);

	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn recovered_environment_result_is_not_lowerable_and_diagnostics_are_separate() {
	let current = identity("consumer.nymph");
	let recovered = Arc::new(ModuleEnvironment::Recovered(
		nymph_sema::RecoveredModuleInterface {
			module: identity("broken.nymph"),
			exports: vec![],
			support_definitions: vec![],
			implementations: vec![],
			fingerprint: 0,
		},
	));
	let environment = SemanticEnvironment::from_modules(current.clone(), &[recovered]).unwrap();
	let module = Arc::new(parse_module("func local(): int = 1", "consumer.nymph").tree);
	let result = check_module_with_environment(module, current, &environment, EntryMode::Library);

	assert!(!result.lowerable);
	assert!(result.diagnostics.is_empty());
}

#[test]
fn recovered_dependency_poison_suppresses_cascades_without_hiding_independent_errors() {
	let dependency = identity("broken.nymph");
	let bad_id = DefinitionId::new(
		dependency.clone(),
		DeclarationKey::top_level(DeclarationCategory::Function, "bad"),
	);
	let good_id = DefinitionId::new(
		dependency.clone(),
		DeclarationKey::top_level(DeclarationCategory::Function, "good"),
	);
	let recovered_function = |id: DefinitionId,
	                          name: &str,
	                          availability: SemanticAvailability,
	                          return_type: RecoveredInterfaceType| {
		RecoveredExportedDefinition {
			id,
			name: name.into(),
			visibility: None,
			kind: DefinitionShapeKind::Function,
			availability,
			binders: vec![],
			constraints: vec![],
			parameters: vec![],
			return_type: Some(return_type),
			ty: None,
			fields: vec![],
			variants: vec![],
			members: vec![],
			super_interfaces: vec![],
			external: None,
			runtime_owner: None,
		}
	};
	let poisoned_impl = RecoveredExportedImpl {
		id: DefinitionId::new(
			dependency.clone(),
			DeclarationKey::top_level(DeclarationCategory::Implementation, "poison"),
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
	let recovered = Arc::new(ModuleEnvironment::Recovered(RecoveredModuleInterface {
		module: dependency.clone(),
		exports: vec![
			recovered_function(
				bad_id.clone(),
				"bad",
				SemanticAvailability::StructureUnavailable,
				RecoveredInterfaceType::Poison,
			),
			recovered_function(
				good_id.clone(),
				"good",
				SemanticAvailability::Available,
				RecoveredInterfaceType::Known(InterfaceType::Int),
			),
		],
		support_definitions: vec![],
		implementations: vec![poisoned_impl],
		fingerprint: 0,
	}));
	let current = identity("consumer.nymph");
	let recovered_environment =
		SemanticEnvironment::from_modules(current.clone(), &[recovered]).unwrap();
	assert!(
		recovered_environment
			.imported
			.implementations
			.impls
			.is_empty()
	);
	assert!(recovered_environment.imported.inherent.impls.is_empty());

	let source =
		"func poisoned(): int = bad(1)\nfunc valid(): int = good()\nfunc independent(): int = missing";
	let parsed = parse_module(source, "consumer.nymph");
	assert!(parsed.diagnostics.is_empty());
	let valid_body = match &parsed.tree.members[1] {
		Declaration::Func { body, .. } => body.id,
		other => panic!("expected function, got {other:?}"),
	};
	let recovered_result = check_module_with_environment(
		Arc::new(parsed.tree.clone()),
		current.clone(),
		&recovered_environment,
		EntryMode::Library,
	);
	assert_eq!(recovered_result.diagnostics.len(), 1);
	assert!(
		recovered_result.diagnostics[0]
			.message
			.contains("cannot find `missing`")
	);
	assert!(matches!(
		recovered_result.analysis.checked.interner.kind(
			recovered_result
				.analysis
				.annotations
				.get(valid_body)
				.unwrap()
				.ty
		),
		TyKind::Int
	));
	assert!(!recovered_result.lowerable);

	let complete_function = |id: DefinitionId, name: &str| ExportedDefinition {
		id,
		name: name.into(),
		visibility: None,
		kind: DefinitionShapeKind::Function,
		binders: vec![],
		constraints: vec![],
		parameters: vec![],
		return_type: Some(InterfaceType::Int),
		ty: None,
		fields: vec![],
		variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: None,
		runtime_owner: None,
	};
	let complete = Arc::new(ModuleEnvironment::Complete(ModuleInterface {
		module: dependency,
		exports: vec![
			complete_function(bad_id, "bad"),
			complete_function(good_id, "good"),
		],
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	}));
	let complete_environment =
		SemanticEnvironment::from_modules(current.clone(), &[complete]).unwrap();
	let complete_module = parse_module("func normal(): int = bad()", "consumer.nymph").tree;
	let normal_body = match &complete_module.members[0] {
		Declaration::Func { body, .. } => body.id,
		other => panic!("expected function, got {other:?}"),
	};
	let complete_result = check_module_with_environment(
		Arc::new(complete_module),
		current,
		&complete_environment,
		EntryMode::Library,
	);
	assert!(complete_result.diagnostics.is_empty());
	assert!(matches!(
		complete_result.analysis.checked.interner.kind(
			complete_result
				.analysis
				.annotations
				.get(normal_body)
				.unwrap()
				.ty
		),
		TyKind::Int
	));
	assert!(complete_result.lowerable);
}
