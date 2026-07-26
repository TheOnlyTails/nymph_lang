use nymph_sema::{
	DefinitionShapeKind, InterfaceType, ModuleEnvironment, ModuleIdentity, RecoveredInterfaceType,
	check_module, declared_headers, extract_module_interface, recover_module_environment,
};

fn parse(source: &str) -> nymph_ast::decl::Module {
	let parsed = nymph_syntax::parse_module(source, "fixture.nym");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	parsed.tree
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
	let checked = check_module(&module);
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
	assert_eq!(external.external.as_ref().unwrap().symbol, "host_print");
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
	let checked = check_module(&module);
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
	let checked = check_module(&module);
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
	let checked = check_module(&module);
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
	let checked = check_module(&module);
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
	let checked = check_module(&module);
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
}
