use std::sync::Arc;

use nymph_hir::{
	hir::{BinOp, BuiltinResult, MarshalKind},
	linkage::NativeExternal,
};
use nymph_sema::{
	DeclarationCategory, DeclarationKey, DefinitionId, DefinitionShapeKind, EntryMode,
	ExportedDefinition, InterfaceType, ModuleEnvironment, ModuleIdentity, ModuleInterface,
	ModuleOrigin, RecoveredDefinitionReference, RecoveredInterfaceType, SemanticAvailability,
	SemanticEnvironment, check_module_with_environment, declared_headers, extract_module_interface,
	extract_module_interface_from_facts_with_selection, extract_module_interface_with_facts,
	recover_module_environment,
};

#[test]
fn checked_wrapper_and_borrowed_facts_extraction_have_exact_parity() {
	let module = Arc::new(parse("public func answer(): int = 42"));
	let identity = identity();
	let environment = SemanticEnvironment::from_modules(identity.clone(), &[]).unwrap();
	let result = check_module_with_environment(
		module.clone(),
		identity.clone(),
		&environment,
		EntryMode::Library,
	);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let checked = nymph_sema::Checked {
		diags: Vec::new(),
		facts: result.analysis.checked.as_ref().clone(),
	};
	let headers = declared_headers(identity.clone(), &module);
	let selected = nymph_sema::ExtractionFactSelection::current_module(&module, &checked);
	let selected_from_facts =
		nymph_sema::ExtractionFactSelection::current_module_from_facts(&module, &checked.facts);
	assert_eq!(
		extract_module_interface_with_facts(identity.clone(), &module, &checked, &headers, &selected,),
		extract_module_interface_from_facts_with_selection(
			identity.clone(),
			&module,
			&checked.facts,
			&headers,
			&selected_from_facts,
		),
	);

	let erroneous_module = Arc::new(parse("public func broken(): Missing = missing"));
	let erroneous_result = check_module_with_environment(
		erroneous_module.clone(),
		identity.clone(),
		&environment,
		EntryMode::Library,
	);
	assert!(!erroneous_result.diagnostics.is_empty());
	let erroneous_checked = nymph_sema::Checked {
		diags: erroneous_result.diagnostics.to_vec(),
		facts: erroneous_result.analysis.checked.as_ref().clone(),
	};
	let erroneous_headers = declared_headers(identity.clone(), &erroneous_module);
	let selected =
		nymph_sema::ExtractionFactSelection::current_module(&erroneous_module, &erroneous_checked);
	let selected_from_facts = nymph_sema::ExtractionFactSelection::current_module_from_facts(
		&erroneous_module,
		&erroneous_checked.facts,
	);
	assert_eq!(
		extract_module_interface_with_facts(
			identity.clone(),
			&erroneous_module,
			&erroneous_checked,
			&erroneous_headers,
			&selected,
		),
		extract_module_interface_from_facts_with_selection(
			identity,
			&erroneous_module,
			&erroneous_checked.facts,
			&erroneous_headers,
			&selected_from_facts,
		),
	);
}

#[test]
fn implementation_member_slots_materialize_defaults_structurally() {
	let source = r#"
interface Pair {
	func left(): int = this.right()
	func right(): int = 2
	let seed: int = 1
}
struct A
struct B
impl Pair for A { func right(): int = 3
	let seed: int = 4 }
impl Pair for B {}
"#;
	let parsed = nymph_syntax::parse_module(source, "slots.nym");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let module = Arc::new(parsed.tree);
	let identity = ModuleIdentity {
		origin: ModuleOrigin::Project("slots".into()),
		project: "slots".into(),
		path: "main".into(),
	};
	let environment = nymph_sema::SemanticEnvironment::from_modules(identity.clone(), &[]).unwrap();
	let result = nymph_sema::check_module_with_environment(
		module.clone(),
		identity.clone(),
		&environment,
		nymph_sema::EntryMode::Library,
	);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let checked = nymph_sema::Checked {
		diags: vec![],
		facts: result.analysis.checked.as_ref().clone(),
	};
	let headers = nymph_sema::declared_headers(identity.clone(), &module);
	let interface =
		nymph_sema::extract_module_interface(identity, &module, &checked, &headers).unwrap();
	let pair = interface
		.exports
		.iter()
		.find(|item| item.name == "Pair")
		.unwrap();
	let left = pair
		.members
		.iter()
		.find(|member| member.name == "left")
		.unwrap();
	let right = pair
		.members
		.iter()
		.find(|member| member.name == "right")
		.unwrap();
	let seed = pair
		.members
		.iter()
		.find(|member| member.name == "seed")
		.unwrap();
	let a = &interface.implementations[0];
	let b = &interface.implementations[1];
	let a_left = a
		.member_slots
		.iter()
		.find(|slot| slot.interface_member_id == left.id)
		.unwrap();
	let a_right = a
		.member_slots
		.iter()
		.find(|slot| slot.interface_member_id == right.id)
		.unwrap();
	let b_left = b
		.member_slots
		.iter()
		.find(|slot| slot.interface_member_id == left.id)
		.unwrap();
	let a_seed = a.member_slots.target(&seed.id).unwrap();
	let b_seed = b.member_slots.target(&seed.id).unwrap();
	assert_eq!(
		a.member_slots.target(&left.id),
		Some(a_left),
		"the interface member ID must be the sole dispatch key"
	);
	assert_eq!(a_right.member_id, a_right.body_definition_id);
	assert_eq!(a_seed.member_id, a_seed.body_definition_id);
	assert_eq!(b_seed.body_definition_id, seed.id);
	assert_eq!(
		a_seed.source,
		nymph_sema::ImplementationMemberSource::Override
	);
	assert_eq!(
		b_seed.source,
		nymph_sema::ImplementationMemberSource::InheritedDefault
	);
	assert_eq!(
		a_right.source,
		nymph_sema::ImplementationMemberSource::Override
	);
	assert_eq!(a_left.body_definition_id, left.id);
	assert_eq!(b_left.body_definition_id, left.id);
	assert_ne!(a_left.member_id, b_left.member_id);
	assert_eq!(a_left.implementation_id, a.id);
	assert_eq!(a_left.placement_owner, a.id);
}

#[test]
fn recovered_known_implementation_projects_the_complete_exact_catalog() {
	let valid = r#"
interface Limit {
	let mut maximum: float
	func render(): string
	func fallback(): int = 1
}
struct Box {}
impl Limit for Box {
	external(max_float) let maximum: float
	external(display) func render(): string
}
"#;
	let recovered = format!("{valid}\nfunc broken(value: Missing): Missing = value\n");
	let project = |source: &str| {
		let module = Arc::new(parse(source));
		let module_identity = identity();
		let environment = SemanticEnvironment::from_modules(module_identity.clone(), &[]).unwrap();
		let result = check_module_with_environment(
			module.clone(),
			module_identity.clone(),
			&environment,
			EntryMode::Library,
		);
		let checked = nymph_sema::Checked {
			diags: result.diagnostics.to_vec(),
			facts: result.analysis.checked.as_ref().clone(),
		};
		let headers = declared_headers(module_identity.clone(), &module);
		(module_identity, module, checked, headers)
	};
	let (complete_identity, complete_module, complete_checked, complete_headers) = project(valid);
	assert!(
		complete_checked.diags.is_empty(),
		"{:?}",
		complete_checked.diags
	);
	let complete = extract_module_interface(
		complete_identity,
		&complete_module,
		&complete_checked,
		&complete_headers,
	)
	.unwrap();
	let (_, recovered_module, recovered_checked, recovered_headers) = project(&recovered);
	assert!(
		recovered_checked
			.diags
			.iter()
			.any(nymph_diagnostics::Diagnostic::is_error)
	);
	let ModuleEnvironment::Recovered(recovered) = recover_module_environment(
		identity(),
		&recovered_module,
		&recovered_checked,
		&recovered_headers,
	) else {
		panic!("expected recovered environment")
	};
	let complete = &complete.implementations[0];
	let recovered = recovered
		.implementations
		.iter()
		.find(|implementation| implementation.id == complete.id)
		.expect("known recovered implementation retains its exact ID");
	assert_eq!(recovered.availability, SemanticAvailability::Available);
	assert_eq!(recovered.member_slots, complete.member_slots);
	assert_eq!(recovered.members.len(), complete.members.len());
	for (recovered, complete) in recovered.members.iter().zip(&complete.members) {
		assert_eq!(recovered.id, complete.id);
		assert_eq!(recovered.external, complete.external);
	}
	let maximum = complete
		.member_slots
		.iter()
		.find(|slot| slot.name == "maximum")
		.unwrap();
	let fallback = complete
		.member_slots
		.iter()
		.find(|slot| slot.name == "fallback")
		.unwrap();
	assert!(maximum.external);
	assert_eq!(maximum.member_id, maximum.body_definition_id);
	assert_eq!(maximum.placement_owner, complete.id);
	assert_eq!(
		fallback.source,
		nymph_sema::ImplementationMemberSource::InheritedDefault
	);
	assert_eq!(fallback.body_definition_id, fallback.interface_member_id);
	assert_eq!(fallback.placement_owner, complete.id);
}

#[test]
fn recovered_known_nested_empty_catalog_is_available_with_exact_identity() {
	let valid = "interface Marker {}\npublic struct Box { impl Marker {} }\n";
	let project = |source: &str| {
		let module = Arc::new(parse(source));
		let module_identity = identity();
		let environment = SemanticEnvironment::from_modules(module_identity.clone(), &[]).unwrap();
		let result = check_module_with_environment(
			module.clone(),
			module_identity.clone(),
			&environment,
			EntryMode::Library,
		);
		let checked = nymph_sema::Checked {
			diags: result.diagnostics.to_vec(),
			facts: result.analysis.checked.as_ref().clone(),
		};
		let headers = declared_headers(module_identity.clone(), &module);
		(module, checked, headers)
	};
	let (complete_module, complete_checked, complete_headers) = project(valid);
	let complete = extract_module_interface(
		identity(),
		&complete_module,
		&complete_checked,
		&complete_headers,
	)
	.unwrap();
	let complete = &complete.implementations[0];
	assert!(complete.member_slots.is_empty());

	let (recovered_module, recovered_checked, recovered_headers) = project(&format!(
		"{valid}func broken(value: Missing): Missing = value\n"
	));
	let ModuleEnvironment::Recovered(recovered) = recover_module_environment(
		identity(),
		&recovered_module,
		&recovered_checked,
		&recovered_headers,
	) else {
		panic!("expected recovered environment")
	};
	let recovered = recovered
		.implementations
		.iter()
		.find(|implementation| implementation.id == complete.id)
		.expect("nested implementation retains its checker-owned identity");
	assert_eq!(recovered.availability, SemanticAvailability::Available);
	assert!(recovered.member_slots.is_empty());
}

#[test]
fn recovered_inherent_externals_use_checked_alias_abi_facts() {
	let valid = r#"
type Scalar = int
type Number = float
impl int {
	external(plus) func add(other: int): Scalar
	external(plus) func add_self(other: int): self
	external(max_float) let maximum: Number
}
"#;
	let project = |source: &str| {
		let module = Arc::new(parse(source));
		let module_identity = identity();
		let environment = SemanticEnvironment::from_modules(module_identity.clone(), &[]).unwrap();
		let result = check_module_with_environment(
			module.clone(),
			module_identity.clone(),
			&environment,
			EntryMode::Library,
		);
		let checked = nymph_sema::Checked {
			diags: result.diagnostics.to_vec(),
			facts: result.analysis.checked.as_ref().clone(),
		};
		let headers = declared_headers(module_identity.clone(), &module);
		(module, checked, headers)
	};
	let (complete_module, complete_checked, complete_headers) = project(valid);
	assert!(
		complete_checked.diags.is_empty(),
		"{:?}",
		complete_checked.diags
	);
	let complete = extract_module_interface(
		identity(),
		&complete_module,
		&complete_checked,
		&complete_headers,
	)
	.unwrap();
	let complete = &complete.implementations[0];

	let (recovered_module, recovered_checked, recovered_headers) = project(&format!(
		"{valid}func broken(value: Missing): Missing = value\n"
	));
	let ModuleEnvironment::Recovered(recovered) = recover_module_environment(
		identity(),
		&recovered_module,
		&recovered_checked,
		&recovered_headers,
	) else {
		panic!("expected recovered environment")
	};
	let recovered = recovered
		.implementations
		.iter()
		.find(|implementation| implementation.id == complete.id)
		.expect("recovered inherent implementation retains its exact identity");
	assert_eq!(recovered.availability, SemanticAvailability::Available);
	for (recovered, complete) in recovered.members.iter().zip(&complete.members) {
		assert_eq!(recovered.id, complete.id);
		assert_eq!(recovered.external, complete.external);
	}
}

#[test]
fn recovered_poison_interface_member_constraint_makes_catalog_unavailable() {
	let module = parse(
		r#"
public interface Action {
	func run<T: Constraint<Other = Missing>>(): int = 1
}
public interface Constraint<Other> {}
public struct Box {}
impl Action for Box {}
func broken(value: Unknown): Unknown = value
"#,
	);
	let checked = check(&module);
	assert!(
		checked
			.diags
			.iter()
			.any(nymph_diagnostics::Diagnostic::is_error)
	);
	let headers = declared_headers(identity(), &module);
	let ModuleEnvironment::Recovered(environment) =
		recover_module_environment(identity(), &module, &checked, &headers)
	else {
		panic!("expected recovered environment")
	};
	assert_eq!(environment.implementations.len(), 1);
	assert_eq!(
		environment.implementations[0].availability,
		SemanticAvailability::StructureUnavailable
	);
	assert!(environment.implementations[0].member_slots.is_empty());
}

fn parse(source: &str) -> nymph_ast::decl::Module {
	let parsed = nymph_syntax::parse_module(source, "fixture.nym");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	parsed.tree
}

fn check(module: &nymph_ast::decl::Module) -> nymph_sema::Checked {
	let identity = identity();
	let environment = SemanticEnvironment::from_modules(identity.clone(), &[]).unwrap();
	let result = check_module_with_environment(
		Arc::new(module.clone()),
		identity,
		&environment,
		EntryMode::Library,
	);
	nymph_sema::Checked {
		diags: result.diagnostics.to_vec(),
		facts: result.analysis.checked.as_ref().clone(),
	}
}

#[test]
fn namespace_summary_is_body_independent_and_covers_lexical_declaration_forms() {
	let source = r#"
private func hidden(): int = 1
public let shown: int = 2
private external(max_float) let maximum: float
private type Alias = Missing
private struct Record(value: Missing) {}
private enum Choice { One }
private interface Marker {}
private namespace Tools { func value(): int = 1 }
impl Record {}
"#;
	let edited = source.replace("hidden(): int = 1", "hidden(): int = 99");
	let module_identity = identity();
	let summary = nymph_sema::namespace_summary(module_identity.clone(), &parse(source));
	let edited_summary = nymph_sema::namespace_summary(module_identity.clone(), &parse(&edited));
	assert_eq!(summary, edited_summary);
	assert_eq!(summary.module, module_identity);
	assert_eq!(
		summary
			.declarations
			.iter()
			.map(|declaration| declaration.name.as_str())
			.collect::<Vec<_>>(),
		[
			"hidden", "shown", "maximum", "Alias", "Record", "Choice", "Marker", "Tools"
		]
	);
	assert_eq!(
		summary.declaration("shown").unwrap().visibility,
		nymph_sema::NamespaceVisibility::Importable
	);
	for name in [
		"hidden", "maximum", "Alias", "Record", "Choice", "Marker", "Tools",
	] {
		assert_eq!(
			summary.declaration(name).unwrap().visibility,
			nymph_sema::NamespaceVisibility::Private,
			"{name}"
		);
	}
	let headers = declared_headers(identity(), &parse(source));
	assert_eq!(
		summary
			.declarations
			.iter()
			.map(|declaration| declaration.definition.clone())
			.collect::<Vec<_>>(),
		headers
			.definitions
			.into_iter()
			.map(|(_, definition)| definition)
			.collect::<Vec<_>>()
	);
}

#[test]
fn namespace_summary_preserves_duplicate_occurrence_ids_and_selects_the_lexical_winner() {
	let module =
		parse("private func same(): int = 1\npublic func same(): int = 2\nprivate struct same {}");
	let headers = declared_headers(identity(), &module);
	let summary = nymph_sema::namespace_summary(identity(), &module);
	assert_eq!(summary.declarations.len(), 3);
	assert_eq!(
		summary
			.declarations
			.iter()
			.map(|declaration| declaration.definition.clone())
			.collect::<Vec<_>>(),
		headers
			.definitions
			.iter()
			.map(|(_, definition)| definition.clone())
			.collect::<Vec<_>>()
	);
	assert_eq!(headers.definitions[0].1.key.duplicate(), 0);
	assert_eq!(headers.definitions[1].1.key.duplicate(), 1);
	assert!(matches!(
		headers.definitions[2].1.key,
		DeclarationKey::TopLevel {
			category: DeclarationCategory::Struct,
			..
		}
	));
	let selected = summary.declaration("same").unwrap();
	assert_eq!(selected.definition, headers.definitions[2].1);
	assert_eq!(
		selected.visibility,
		nymph_sema::NamespaceVisibility::Private
	);
}

#[test]
fn recovered_environment_contains_the_exact_importable_duplicate_winner() {
	let module = parse(
		"private func same(): int = 1\npublic func same(): int = 2\nfunc broken(value: Missing): int = 0",
	);
	let headers = declared_headers(identity(), &module);
	let summary = nymph_sema::namespace_summary(identity(), &module);
	let winner = summary.declaration("same").unwrap();
	assert_eq!(winner.definition, headers.definitions[1].1);
	let checked = check(&module);
	let ModuleEnvironment::Recovered(environment) =
		recover_module_environment(identity(), &module, &checked, &headers)
	else {
		panic!("expected recovered environment")
	};
	assert!(
		environment
			.exports
			.iter()
			.any(|definition| definition.id == winner.definition),
		"selected exact namespace identity must be available to consumers"
	);
	assert_eq!(
		environment
			.exports
			.iter()
			.filter(|definition| definition.name == "same")
			.count(),
		1
	);
}

#[test]
fn recovered_environment_respects_private_and_mixed_kind_duplicate_winners() {
	for source in [
		"public func same(): int = 1\nprivate func same(): int = 2\nfunc broken(value: Missing): int = 0",
		"public func same(): int = 1\nprivate struct same {}\nfunc broken(value: Missing): int = 0",
	] {
		let module = parse(source);
		let headers = declared_headers(identity(), &module);
		let summary = nymph_sema::namespace_summary(identity(), &module);
		let winner = summary.declaration("same").unwrap();
		assert_eq!(winner.visibility, nymph_sema::NamespaceVisibility::Private);
		let checked = check(&module);
		let ModuleEnvironment::Recovered(environment) =
			recover_module_environment(identity(), &module, &checked, &headers)
		else {
			panic!("expected recovered environment")
		};
		assert!(
			environment
				.exports
				.iter()
				.all(|definition| definition.name != "same"),
			"a shadowed public declaration must not leak the private winner"
		);
	}
}

#[test]
fn extraction_preserves_transitive_imported_nominal_return_identity() {
	let owner = ModuleIdentity {
		origin: nymph_sema::ModuleOrigin::Project("test".into()),
		project: "test".into(),
		path: "c".into(),
	};
	let answer = DefinitionId::new(
		owner.clone(),
		DeclarationKey::top_level(DeclarationCategory::Struct, "Answer"),
	);
	let dependency = Arc::new(ModuleEnvironment::Complete(ModuleInterface {
		module: owner,
		exports: vec![ExportedDefinition {
			id: answer.clone(),
			name: "Answer".into(),
			visibility: None,
			kind: DefinitionShapeKind::Struct,
			declaration_kind: None,
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
		}],
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	}));
	let current = ModuleIdentity {
		origin: nymph_sema::ModuleOrigin::Project("test".into()),
		project: "test".into(),
		path: "b".into(),
	};
	let environment = SemanticEnvironment::from_modules(current.clone(), &[dependency]).unwrap();
	let module = Arc::new(parse("public func make_answer(): Answer = Answer()"));
	let result = check_module_with_environment(
		module.clone(),
		current.clone(),
		&environment,
		EntryMode::Library,
	);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let checked = nymph_sema::Checked {
		diags: vec![],
		facts: result.analysis.checked.as_ref().clone(),
	};
	let headers = declared_headers(current.clone(), &module);
	let facts = nymph_sema::ExtractionFactSelection::current_module(&module, &checked);
	let interface =
		extract_module_interface_with_facts(current.clone(), &module, &checked, &headers, &facts)
			.expect("an imported nominal remains canonicalizable by stable identity");
	let borrowed_facts =
		nymph_sema::ExtractionFactSelection::current_module_from_facts(&module, &checked.facts);
	assert_eq!(
		interface,
		extract_module_interface_from_facts_with_selection(
			current,
			&module,
			&checked.facts,
			&headers,
			&borrowed_facts,
		)
		.unwrap()
	);
	assert_eq!(
		interface.exports[0].return_type,
		Some(InterfaceType::Named {
			definition: answer,
			positional: vec![],
			named: vec![],
		})
	);
}

#[test]
fn extraction_owns_anonymous_interface_return_binders() {
	let module = parse(
		r#"
public interface Producer<T> { func next(): T }
public interface Stream<T> { func stream(): Producer<T> }
"#,
	);
	let environment = SemanticEnvironment::from_modules(identity(), &[]).unwrap();
	let result = check_module_with_environment(
		Arc::new(module.clone()),
		identity(),
		&environment,
		EntryMode::Library,
	);
	let checked = nymph_sema::Checked {
		diags: result.diagnostics.to_vec(),
		facts: result.analysis.checked.as_ref().clone(),
	};
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();
	let stream = interface
		.exports
		.iter()
		.find(|item| item.name == "Stream")
		.unwrap();
	let member = &stream.members[0];
	assert_eq!(member.binders.len(), 1);
	assert_eq!(member.constraints.len(), 1);
	assert_eq!(member.constraints[0].parameter, member.binders[0].id);
	assert_eq!(member.constraints[0].interface, interface.exports[0].id);
	assert_eq!(
		member.return_type,
		InterfaceType::Generic(member.binders[0].id.clone())
	);
}

#[test]
fn extraction_owns_anonymous_top_level_function_parameter_binders() {
	let module = parse(
		r#"
public interface Area { func area(): int }
public func measure(shape: Area): int = shape.area()
"#,
	);
	let checked = check(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();
	let area = interface
		.exports
		.iter()
		.find(|item| item.name == "Area")
		.unwrap();
	let measure = interface
		.exports
		.iter()
		.find(|item| item.name == "measure")
		.unwrap();
	assert_eq!(measure.binders.len(), 1);
	assert_eq!(measure.constraints.len(), 1);
	assert_eq!(measure.constraints[0].parameter, measure.binders[0].id);
	assert_eq!(measure.constraints[0].interface, area.id);
	assert_eq!(
		measure.parameters[0].ty,
		InterfaceType::Generic(measure.binders[0].id.clone())
	);
	assert_eq!(measure.return_type, Some(InterfaceType::Int));
}

#[test]
fn extraction_uses_finalized_inferred_top_level_let_type() {
	let module = parse(
		r#"
public let answer = 42
public func read(): int = answer
"#,
	);
	let checked = check(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();
	let answer = interface
		.exports
		.iter()
		.find(|item| item.name == "answer")
		.unwrap();
	assert_eq!(answer.ty, Some(InterfaceType::Int));
}

#[test]
fn extraction_uses_deeply_finalized_inferred_function_return_type() {
	let module = parse("public func bits() = #(true, false, true)");
	let checked = check(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();
	assert_eq!(
		interface.exports[0].return_type,
		Some(InterfaceType::Tuple(vec![
			InterfaceType::Boolean,
			InterfaceType::Boolean,
			InterfaceType::Boolean,
		]))
	);
}

#[test]
fn extraction_preserves_generic_enum_alias_external_and_impl_shapes() {
	let module = parse(
		r#"
public interface Show<T> { func show(value: T): string }
public enum Choice<T> { Some(value: T), None }
public type Numbers = #[int]
public external(host_print) func print(value: string): void
public impl<T> Show<T = T> for Choice<T> {
	func show(value: T): string = "choice"
}
"#,
	);
	let environment = SemanticEnvironment::from_modules(identity(), &[]).unwrap();
	let result = check_module_with_environment(
		Arc::new(module.clone()),
		identity(),
		&environment,
		EntryMode::Library,
	);
	let checked = nymph_sema::Checked {
		diags: result.diagnostics.to_vec(),
		facts: result.analysis.checked.as_ref().clone(),
	};
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();

	let choice = interface
		.exports
		.iter()
		.find(|d| d.name == "Choice")
		.unwrap();
	assert_eq!(choice.binders.len(), 1);
	assert_eq!(choice.variants.len(), 2);
	assert_eq!(choice.variants[0].fields.len(), 1);
	let alias = interface
		.exports
		.iter()
		.find(|d| d.name == "Numbers")
		.unwrap();
	assert_eq!(
		alias.ty,
		Some(InterfaceType::List(Box::new(InterfaceType::Int)))
	);
	let external = interface
		.exports
		.iter()
		.find(|d| d.name == "print")
		.unwrap();
	let abi = external.external.as_ref().unwrap();
	assert_eq!(abi.marker, "host_print");
	assert_eq!(abi.callable, nymph_sema::ExternalCallable::Deferred);
	assert_eq!(interface.implementations.len(), 1);
	assert_eq!(interface.implementations[0].members.len(), 1);
}

#[test]
fn extraction_preserves_constraints_superinterfaces_and_all_member_kinds_in_source_order() {
	let module = parse(
		r#"
public interface Parent<T> { func parent(): T }
public interface Child<T: Parent<T = int>>: Parent<T = T> {
	let value: T
	let mut changing: T
	func required(value: T, rest: #[T]): T
	mut func update(value: T): T = value
	namespace func make(value: T): T = value
}
public struct Box<T: Parent<T = int>>(value: T = 1) {
	func get(): T = this.value
	mut func set(value: T): T = value
	namespace func make(value: T): Box<T> = Box(value)
	let cached: T = this.value
	let mut changing: T = this.value
	namespace let empty: int = 0
}
"#,
	);
	let checked = check(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();

	let child = interface
		.exports
		.iter()
		.find(|d| d.name == "Child")
		.unwrap();
	assert_eq!(child.constraints.len(), 1);
	assert_eq!(child.super_interfaces.len(), 1);
	assert_eq!(child.super_interfaces[0].interface, interface.exports[0].id);
	assert_eq!(
		child
			.members
			.iter()
			.map(|m| m.name.as_str())
			.collect::<Vec<_>>(),
		["value", "changing", "required", "update", "make"]
	);
	assert!(child.members[3].has_default);
	assert!(child.members[4].has_default);

	let boxed = interface.exports.iter().find(|d| d.name == "Box").unwrap();
	assert_eq!(boxed.binders.len(), 1);
	assert_eq!(boxed.constraints.len(), 1);
	assert!(boxed.fields[0].has_default);
	assert_eq!(
		boxed.members.iter().map(|m| m.kind).collect::<Vec<_>>(),
		[
			nymph_sema::MemberKind::Function,
			nymph_sema::MemberKind::MutatingFunction,
			nymph_sema::MemberKind::StaticFunction,
			nymph_sema::MemberKind::Value,
			nymph_sema::MemberKind::MutableValue,
			nymph_sema::MemberKind::StaticValue,
		]
	);
}

fn identity() -> ModuleIdentity {
	ModuleIdentity {
		origin: nymph_sema::ModuleOrigin::Project("tests".into()),
		project: "tests".into(),
		path: "fixture".into(),
	}
}

#[test]
fn extraction_keeps_public_shape_and_private_nominal_support() {
	let module = parse(
		r#"
private struct Secret(value: int) {}
public struct Wrapper(secret: Secret) {}
public func expose(value: Secret): Wrapper = Wrapper(value)
private func helper(): int = 1
"#,
	);
	let checked = check(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();

	assert_eq!(
		interface
			.exports
			.iter()
			.map(|item| item.name.as_str())
			.collect::<Vec<_>>(),
		["Wrapper", "expose"]
	);
	assert_eq!(interface.support_definitions.len(), 1);
	assert_eq!(interface.support_definitions[0].definition.name, "Secret");
	assert_eq!(interface.exports[0].kind, DefinitionShapeKind::Struct);
	assert_eq!(interface.fingerprint, interface.structural_fingerprint());
}

#[test]
fn recovery_poisons_only_the_unresolved_slot() {
	let module = parse(
		r#"
public func broken(value: Missing): Missing = value
public func valid(value: int): int = value
"#,
	);
	let checked = check(&module);
	assert!(
		checked
			.diags
			.iter()
			.any(nymph_diagnostics::Diagnostic::is_error)
	);
	let headers = declared_headers(identity(), &module);
	let recovered = recover_module_environment(identity(), &module, &checked, &headers);

	let ModuleEnvironment::Recovered(environment) = recovered else {
		panic!("user errors must produce a recovered environment")
	};
	assert_eq!(environment.exports.len(), 2);
	assert!(matches!(
		environment.exports[0].parameters[0].ty,
		RecoveredInterfaceType::Poison
	));
	assert!(matches!(
		environment.exports[1].parameters[0].ty,
		RecoveredInterfaceType::Known(_)
	));
}

#[test]
fn extraction_includes_inherent_nested_impl_method_facts_and_recursive_support() {
	let module = parse(
		r#"
public interface Marker<T> { func mark<U: Marker<T = T>>(value: U): T }
private struct Secret(value: int) {}
public struct Box<T>(value: T) {
	func reveal<U: Marker<T = Secret>>(value: U) = Secret(1)
	impl Marker<T = Secret> {
		func mark(value: int) = Secret(1)
	}
}
public impl<T: Marker<T = Secret>> Box<T> {
	func extra<U: Marker<T = Secret>>(value: U) = Secret(1)
}
"#,
	);
	let environment = SemanticEnvironment::from_modules(identity(), &[]).unwrap();
	let result = check_module_with_environment(
		Arc::new(module.clone()),
		identity(),
		&environment,
		EntryMode::Library,
	);
	let checked = nymph_sema::Checked {
		diags: result.diagnostics.to_vec(),
		facts: result.analysis.checked.as_ref().clone(),
	};
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();

	assert_eq!(interface.implementations.len(), 2);
	assert!(
		interface
			.implementations
			.iter()
			.any(|i| i.interface.is_none())
	);
	assert!(
		interface
			.implementations
			.iter()
			.any(|i| i.interface.is_some())
	);
	for implementation in &interface.implementations {
		assert_eq!(implementation.members.len(), 1);
		assert!(matches!(
			implementation.members[0].return_type,
			InterfaceType::Named { .. }
		));
	}
	let inherent = interface
		.implementations
		.iter()
		.find(|implementation| implementation.interface.is_none())
		.unwrap();
	assert_eq!(inherent.members[0].binders.len(), 1);
	assert_eq!(inherent.members[0].constraints.len(), 1);
	assert_eq!(interface.support_definitions.len(), 1);
	assert_eq!(interface.support_definitions[0].definition.name, "Secret");
}

#[test]
fn recovery_preserves_all_neighboring_shape_and_ids() {
	let module = parse(
		r#"
public interface Shape<T> {
	func valid(value: int): int
	func broken(value: Missing): T
}
public struct Pair<T>(left: int, right: Missing) {
	func valid(value: int): int = value
	func broken(value: Missing): T = this.broken(value)
	impl Shape<T = T> {
		func valid(value: int): int = value
		func broken(value: Missing): T = this.broken(value)
	}
}
"#,
	);
	let checked = check(&module);
	assert!(
		checked
			.diags
			.iter()
			.any(nymph_diagnostics::Diagnostic::is_error)
	);
	let headers = declared_headers(identity(), &module);
	let ModuleEnvironment::Recovered(environment) =
		recover_module_environment(identity(), &module, &checked, &headers)
	else {
		panic!("expected recovery")
	};
	let pair = environment
		.exports
		.iter()
		.find(|d| d.name == "Pair")
		.unwrap();
	assert_eq!(pair.fields.len(), 2);
	assert!(matches!(
		pair.fields[0].ty,
		RecoveredInterfaceType::Known(_)
	));
	assert!(matches!(pair.fields[1].ty, RecoveredInterfaceType::Poison));
	assert_eq!(pair.members.len(), 2);
	assert_eq!(environment.implementations.len(), 1);
	assert_eq!(
		environment.implementations[0].availability,
		SemanticAvailability::StructureUnavailable
	);
	assert!(environment.implementations[0].member_slots.is_empty());
}

#[test]
fn recovery_preserves_every_header_slot_constraint_and_observable_support() {
	let module = parse(
		r#"
private struct Used(value: int) {}
private struct Unused(value: int) {}
public interface Bound<T> { func get(): T }
public interface Child<T: Missing<T = Used>>: Missing<T = Used> {
	func broken(): Missing
	func valid(): Used
}
public impl<T: Missing<T = Used>> Missing<T = Missing> for Used {
	func broken(): Missing = panic("broken")
	func valid(): Used = Used(1)
}
public impl Bound<T = int> for Missing {
	func get(): int = 1
}
"#,
	);
	let checked = check(&module);
	assert!(
		checked
			.diags
			.iter()
			.any(nymph_diagnostics::Diagnostic::is_error)
	);
	let headers = declared_headers(identity(), &module);
	let ModuleEnvironment::Recovered(environment) =
		recover_module_environment(identity(), &module, &checked, &headers)
	else {
		panic!("expected recovery")
	};

	let child = environment
		.exports
		.iter()
		.find(|d| d.name == "Child")
		.unwrap();
	assert_eq!(child.constraints.len(), 1);
	assert_eq!(
		child.constraints[0].interface,
		RecoveredDefinitionReference::Poison
	);
	assert_eq!(child.constraints[0].positional.len(), 0);
	assert_eq!(child.constraints[0].named.len(), 1);
	assert!(matches!(
		child.constraints[0].named[0].1,
		RecoveredInterfaceType::Known(_)
	));
	assert_eq!(child.super_interfaces.len(), 1);
	assert_eq!(
		child.super_interfaces[0].interface,
		RecoveredDefinitionReference::Poison
	);
	assert_eq!(
		child
			.members
			.iter()
			.map(|m| m.name.as_str())
			.collect::<Vec<_>>(),
		["broken", "valid"]
	);
	assert!(matches!(
		child.members[0].return_type,
		RecoveredInterfaceType::Poison
	));
	assert!(matches!(
		child.members[1].return_type,
		RecoveredInterfaceType::Known(_)
	));

	assert_eq!(environment.implementations.len(), 2);
	let malformed_header = &environment.implementations[0];
	assert_eq!(
		malformed_header.interface,
		Some(RecoveredDefinitionReference::Poison)
	);
	assert_eq!(malformed_header.interface_arguments.len(), 1);
	assert!(matches!(
		malformed_header.interface_arguments[0].1,
		RecoveredInterfaceType::Poison
	));
	assert!(matches!(
		malformed_header.self_type,
		RecoveredInterfaceType::Known(_)
	));
	assert_eq!(malformed_header.constraints.len(), 1);
	assert_eq!(
		malformed_header.constraints[0].interface,
		RecoveredDefinitionReference::Poison
	);
	assert_eq!(malformed_header.constraints[0].named.len(), 1);
	assert!(matches!(
		malformed_header.constraints[0].named[0].1,
		RecoveredInterfaceType::Known(_)
	));
	assert_eq!(
		malformed_header
			.members
			.iter()
			.map(|m| m.name.as_str())
			.collect::<Vec<_>>(),
		["broken", "valid"]
	);
	assert_ne!(
		environment.implementations[0].id,
		environment.implementations[1].id
	);
	assert_eq!(environment.support_definitions.len(), 1);
	assert_eq!(environment.support_definitions[0].definition.name, "Used");
	assert_eq!(
		environment.fingerprint,
		environment.structural_fingerprint()
	);
}

#[test]
fn recovery_impl_constraint_arguments_retain_private_support() {
	let module = parse(
		r#"
private struct ConstraintOnly(value: int) {}
public struct Public(value: int) {}
public impl<T: Missing<ConstraintOnly, Item = ConstraintOnly>> Public {
	func broken(): Missing = panic("broken")
}
"#,
	);
	let checked = check(&module);
	let headers = declared_headers(identity(), &module);
	let ModuleEnvironment::Recovered(environment) =
		recover_module_environment(identity(), &module, &checked, &headers)
	else {
		panic!("expected recovery")
	};
	let constraint = &environment.implementations[0].constraints[0];
	assert_eq!(constraint.positional.len(), 1);
	assert_eq!(constraint.named.len(), 1);
	assert_eq!(environment.support_definitions.len(), 1);
	assert_eq!(
		environment.support_definitions[0].definition.name,
		"ConstraintOnly"
	);
}

#[test]
fn malformed_impl_identity_uses_source_structure_and_is_reorder_stable() {
	fn malformed_id(source: &str, self_name: &str) -> DefinitionId {
		let module = parse(source);
		let checked = check(&module);
		let headers = declared_headers(identity(), &module);
		let ModuleEnvironment::Recovered(environment) =
			recover_module_environment(identity(), &module, &checked, &headers)
		else {
			panic!("expected recovery")
		};
		environment
			.implementations
			.iter()
			.find(|implementation| match &implementation.self_type {
				RecoveredInterfaceType::Known(InterfaceType::Named { definition, .. }) => matches!(
					&definition.key,
					DeclarationKey::TopLevel { name, .. } if name == self_name
				),
				_ => false,
			})
			.unwrap()
			.id
			.clone()
	}

	let base = "public struct A {}\npublic impl MissingOne for A {}";
	let first = malformed_id(base, "A");
	assert_eq!(first, malformed_id(base, "A"));
	assert_ne!(
		first,
		malformed_id("public struct A {}\npublic impl MissingTwo for A {}", "A")
	);
	assert_eq!(
		first,
		malformed_id(
			"public struct A {}\npublic struct B {}\npublic impl OtherMissing for B {}\npublic impl MissingOne for A {}",
			"A"
		)
	);
	assert_eq!(
		first,
		malformed_id(
			"public struct A {}\npublic struct B {}\npublic impl MissingOne for A {}\npublic impl OtherMissing for B {}",
			"A"
		)
	);
}

#[test]
fn nested_impl_constraints_participate_in_recovered_identity() {
	fn nested_id(bound: &str) -> DefinitionId {
		let module = parse(&format!(
			"public struct Box<T> {{ impl<U: {bound}<T>> Missing {{}} }}"
		));
		let checked = check(&module);
		let headers = declared_headers(identity(), &module);
		let ModuleEnvironment::Recovered(environment) =
			recover_module_environment(identity(), &module, &checked, &headers)
		else {
			panic!("expected recovery")
		};
		environment.implementations[0].id.clone()
	}
	assert_ne!(nested_id("FirstBound"), nested_id("SecondBound"));
}

#[test]
fn extraction_uses_checked_external_value_linkage_and_marshal() {
	let module = parse("public external(max_float) let maximum: float");
	let checked = check(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();
	let abi = interface.exports[0].external.as_ref().unwrap();
	assert_eq!(abi.marker, "max_float");
	assert_eq!(
		abi.callable,
		nymph_sema::ExternalCallable::Linked {
			module: "std/math/intrinsics".into(),
			symbol: "max_float".into(),
		}
	);
	assert_eq!(abi.marshal, Some(MarshalKind::Float));
}

#[test]
fn implementation_value_shapes_project_checker_owned_kind_and_marshal() {
	let module = Arc::new(parse(
		"type Scalar = float\ninterface Limit { let mut maximum: Scalar }\nstruct Box {}\nimpl Limit for Box { external(max_float) let maximum: Scalar }",
	));
	let module_identity = identity();
	let environment = SemanticEnvironment::from_modules(module_identity.clone(), &[]).unwrap();
	let result = check_module_with_environment(
		module.clone(),
		module_identity.clone(),
		&environment,
		EntryMode::Library,
	);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let checked = nymph_sema::Checked {
		diags: Vec::new(),
		facts: result.analysis.checked.as_ref().clone(),
	};
	let headers = declared_headers(module_identity.clone(), &module);
	let interface = extract_module_interface(module_identity, &module, &checked, &headers).unwrap();
	let member = &interface.implementations[0].members[0];
	assert_eq!(member.kind, nymph_sema::MemberKind::MutableValue);
	assert_eq!(
		member.external.as_ref().and_then(|abi| abi.marshal),
		Some(MarshalKind::Float)
	);
	let slot = interface.implementations[0]
		.member_slots
		.iter()
		.next()
		.unwrap();
	assert_eq!(slot.kind, member.kind);
	assert_eq!(slot.member_id, member.id);
}

#[test]
fn extraction_preserves_native_primitive_external_semantics() {
	let module = Arc::new(parse(
		"interface Plus<Other, Output> { func plus(other: Other): Output }\n\
		 impl Plus<Other = self, Output = self> for int { external func plus(other: self): self }",
	));
	let module_identity = identity();
	let environment = SemanticEnvironment::from_modules(module_identity.clone(), &[]).unwrap();
	let result = check_module_with_environment(
		module.clone(),
		module_identity.clone(),
		&environment,
		EntryMode::Library,
	);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let checked = nymph_sema::Checked {
		diags: Vec::new(),
		facts: result.analysis.checked.as_ref().clone(),
	};
	let headers = declared_headers(module_identity.clone(), &module);
	let interface = extract_module_interface(module_identity, &module, &checked, &headers).unwrap();
	let abi = interface.implementations[0].members[0]
		.external
		.as_ref()
		.unwrap();
	assert_eq!(
		abi.callable,
		nymph_sema::ExternalCallable::Native(NativeExternal::Binary {
			op: BinOp::Add,
			result: BuiltinResult::Int,
		})
	);
}

#[test]
fn native_external_classification_requires_the_exact_checked_arity() {
	let module = parse("impl int { external func plus(): int }");
	let checked = check(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let headers = declared_headers(identity(), &module);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();
	let abi = interface.implementations[0].members[0]
		.external
		.as_ref()
		.unwrap();

	assert_eq!(abi.callable, nymph_sema::ExternalCallable::Deferred);
}
