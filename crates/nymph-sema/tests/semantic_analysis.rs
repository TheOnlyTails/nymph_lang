use std::sync::Arc;

use nymph_ast::decl::Declaration;
use nymph_ast::expr::{ExprKind, Statement};
use nymph_sema::{
	BinderScope, DeclarationCategory, DeclarationKey, DefinitionId, DefinitionShapeKind, EntryMode,
	ExportedDefinition, InterfaceType, ModuleAnnotations, ModuleEnvironment, ModuleIdentity,
	ModuleInterface, ModuleOrigin, RecoveredDefinitionReference, RecoveredExportedDefinition,
	RecoveredExportedImpl, RecoveredInterfaceType, RecoveredModuleInterface, SemanticAnalysis,
	SemanticAvailability, SemanticEnvironment, TyKind, check_module, check_module_with_environment,
	check_module_with_owned_environment, declared_headers, extract_module_interface,
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
		declarations: Arc::default(),
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
		declaration_kind: None,
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
	let module = Arc::new(parsed.tree);
	let result = check_module_with_environment(
		module.clone(),
		current.clone(),
		&environment,
		EntryMode::Library,
	);
	assert_eq!(format!("{environment:?}"), before);
	let owned = check_module_with_owned_environment(module, environment, EntryMode::Library);

	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	assert!(result.lowerable);
	assert_eq!(result.diagnostics, owned.diagnostics);
	assert_eq!(result.lowerable, owned.lowerable);
	assert_eq!(
		result.analysis.checked.semantic.local_definitions,
		owned.analysis.checked.semantic.local_definitions
	);
	assert_eq!(
		result.analysis.checked.semantic.local_implementations,
		owned.analysis.checked.semantic.local_implementations
	);
	assert_eq!(
		result.analysis.checked.semantic.local_inherent,
		owned.analysis.checked.semantic.local_inherent
	);
	assert_eq!(
		result.analysis.checked.semantic.compiler_runtime_roles,
		owned.analysis.checked.semantic.compiler_runtime_roles
	);
	assert_eq!(
		result
			.analysis
			.annotations
			.definition_target_of(answer_node),
		Some(&function_id),
	);
	assert_eq!(
		nymph_sema::query::stable_definition_kind(&result.analysis, &function_id),
		Some(nymph_sema::DefKind::Func),
	);
	assert_eq!(
		nymph_sema::query::definition_kind_by_name(&result.analysis, "answer"),
		Some(nymph_sema::DefKind::Func),
	);
	assert_eq!(
		result
			.analysis
			.annotations
			.definition_target_of(answer_node),
		owned.analysis.annotations.definition_target_of(answer_node)
	);
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
fn imported_opaque_method_return_retains_its_exact_interface_bound() {
	let dependency_identity = identity("iterable.nymph");
	let parsed = parse_module(
		"public enum Option<T> { Some(value: T), None }\n\
		 public interface Iterator<Item> { mut func next(): Option<Item> }\n\
		 public interface Iterable<Item> { func iter(): Iterator<Item> }\n\
		 public struct ListIter<Item>(item: Item) {\n\
		   impl Iterator<Item> { mut func next(): Option<Item> = Some(this.item) }\n\
		 }\n\
		 public struct Values<Item>(item: Item) {\n\
		   impl Iterable<Item> { func iter(): Iterator<Item> = ListIter(item = this.item) }\n\
		 }",
		"iterable.nymph",
	);
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let module = Arc::new(parsed.tree);
	let dependency_environment =
		SemanticEnvironment::from_modules(dependency_identity.clone(), &[]).unwrap();
	let checked = check_module_with_environment(
		module.clone(),
		dependency_identity.clone(),
		&dependency_environment,
		EntryMode::Library,
	);
	assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
	let facts = nymph_sema::Checked {
		diags: vec![],
		facts: checked.analysis.checked.as_ref().clone(),
	};
	let headers = declared_headers(dependency_identity.clone(), &module);
	let interface =
		extract_module_interface(dependency_identity.clone(), &module, &facts, &headers).unwrap();
	let iterator_id = interface
		.exports
		.iter()
		.find(|definition| definition.name == "Iterator")
		.unwrap()
		.id
		.clone();
	let implementation = interface
		.implementations
		.iter()
		.find(|implementation| {
			matches!(
				&implementation.self_type,
				InterfaceType::Named { definition, .. }
					if matches!(&definition.key, DeclarationKey::TopLevel { name, .. } if name == "Values")
			)
		})
		.unwrap();
	let iter_implementation_id = implementation.id.clone();
	let iter = implementation
		.members
		.iter()
		.find(|member| member.name == "iter")
		.unwrap();
	let opaque_return = iter
		.binders
		.last()
		.expect("iter has an opaque return binder");
	assert_eq!(opaque_return.id.binder.owner, iter.id);
	assert_eq!(opaque_return.id.binder.scope, BinderScope::Member);
	assert_eq!(
		iter.return_type,
		InterfaceType::Generic(opaque_return.id.clone())
	);
	let producer_bound = &iter.constraints[0];
	assert_eq!(producer_bound.parameter, opaque_return.id);
	assert_eq!(producer_bound.interface, iterator_id);
	assert_eq!(
		producer_bound.named,
		[(
			"Item".into(),
			InterfaceType::Generic(implementation.binders[0].id.clone())
		)]
	);

	let consumer_identity = identity("consumer.nymph");
	let environment = SemanticEnvironment::from_modules(
		consumer_identity.clone(),
		&[Arc::new(ModuleEnvironment::Complete(interface.clone()))],
	)
	.unwrap();
	let iterator_local = environment.imported.defs.by_stable(&iterator_id).unwrap();
	let imported_implementation = environment
		.imported
		.implementations
		.impls
		.iter()
		.find(|implementation| implementation.definition.as_ref() == Some(&iter_implementation_id))
		.unwrap();
	let imported_iterable_member =
		environment.imported.interfaces[&imported_implementation.interface].methods["iter"]
			.definition
			.as_ref()
			.unwrap();
	assert!(
		imported_implementation
			.member_catalog
			.target(imported_iterable_member)
			.is_some(),
		"imported implementation catalog must use the imported interface's exact member ID"
	);
	let imported_iter = &imported_implementation.methods["iter"];
	let TyKind::Param(imported_opaque_return) = environment.interner.kind(imported_iter.ret) else {
		panic!("imported iter return is not its method-owned opaque binder")
	};
	let imported_bound = &imported_iter.bounds[0];
	assert_eq!(
		environment.interner.kind(imported_bound.ty),
		&TyKind::Param(*imported_opaque_return)
	);
	assert_eq!(imported_bound.interface, iterator_local);
	let imported_item_argument = imported_bound
		.args
		.iter()
		.find(|(name, _)| name == "Item")
		.expect("imported Iterator bound has its Item argument")
		.1;
	let TyKind::Param(imported_item) = environment.interner.kind(imported_item_argument) else {
		panic!("imported Iterator.Item does not reference the implementation owner binder")
	};
	let TyKind::Adt(_, owner_args) = environment.interner.kind(imported_implementation.self_ty)
	else {
		panic!("imported implementation self type is not Values<Item>")
	};
	let TyKind::Param(imported_owner_item) = environment.interner.kind(owner_args.positional[0])
	else {
		panic!("imported Values argument is not its owner binder")
	};
	assert_eq!(imported_item, imported_owner_item);

	let consumer = parse_module(
			"func advance(values: Values<int>): Option<int> = { let mut iterator = values.iter() iterator.next() }",
			"consumer.nymph",
		)
		.tree;
	let next_call = match &consumer.members[0] {
		Declaration::Func { body, .. } => match &body.kind {
			ExprKind::Block { body, .. } => match &body.last().unwrap().0 {
				Statement::Expr(expression) => expression.id,
				other => panic!("expected trailing next expression, got {other:?}"),
			},
			other => panic!("expected function block, got {other:?}"),
		},
		other => panic!("expected function, got {other:?}"),
	};
	let result = check_module_with_environment(
		Arc::new(consumer),
		consumer_identity,
		&environment,
		EntryMode::Library,
	);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let next_ty = result.analysis.annotations.get(next_call).unwrap().ty;
	let TyKind::Adt(option, arguments) = result.analysis.checked.interner.kind(next_ty) else {
		panic!("next() did not resolve to Option<int>")
	};
	let option_id = interface
		.exports
		.iter()
		.find(|definition| definition.name == "Option")
		.unwrap()
		.id
		.clone();
	assert_eq!(
		result.analysis.checked.semantic.stable_definition(*option),
		Some(&option_id)
	);
	assert_eq!(
		result
			.analysis
			.checked
			.interner
			.kind(arguments.positional[0]),
		&TyKind::Int
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
		declaration_kind: None,
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
			declaration_kind: None,
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
		member_slots: vec![].into(),
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
		declaration_kind: None,
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
