#![cfg(feature = "test-support")]

use std::sync::{Arc, Mutex};

use nymph_compiler::project::{
	CompilerSession, ModulePath, ProjectId, RuntimeDefinitionError, SemanticQueryEvent, SourceVersion,
};
use nymph_sema::{DeclarationCategory, DeclarationKey, DefinitionId, EntryMode, RuntimePayload};

fn id(project: &str, name: &str) -> DefinitionId {
	DefinitionId::new(
		nymph_sema::ModuleIdentity {
			origin: nymph_sema::ModuleOrigin::Project(project.into()),
			project: project.into(),
			path: "main".into(),
		},
		DeclarationKey::top_level(DeclarationCategory::Function, name),
	)
}

fn session() -> (CompilerSession, Arc<Mutex<Vec<SemanticQueryEvent>>>) {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	(
		CompilerSession::with_detailed_event_callback_for_test(move |event| {
			sink.lock().unwrap().push(event)
		}),
		events,
	)
}

#[test]
fn exhaustive_runtime_bearing_declaration_matrix_has_exact_queryable_ids() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("runtime-matrix");
	let main = ModulePath::new("main").unwrap();
	let source = r#"
public func public_func(): int = 1
private func private_func(): int = 2
public let public_value: int = 3
private let private_value: int = 4
public external(host_func) func external_func(): int
private external(max_float) let external_value: float
type Alias = int

public interface Feature {
	func abstract_func(): int
	let abstract_value: int
	func default_one(): int = 10
	func default_two(): int = 11
	let default_value: int = 12
}
public interface Blanket { func blanket_method(): int }

public struct Record(value: int) {
	func inline_func(): int = this.value
	mut func inline_mut_func(): int = this.value
	namespace func inline_static(): int = 13
	namespace let inline_static_value: int = 14
	external(host_member_func) func external_member_func(): int
	external(max_float) let external_member_value: float
	impl Feature {
		func abstract_func(): int = this.value
		let abstract_value: int = 15
	}
}
public enum Choice { One
	func enum_func(): int = 16
	mut func enum_mut_func(): int = 17
	namespace func enum_static(): int = 18
	namespace let enum_static_value: int = 19
}
impl Record {
	func inherent_one(): int = this.value
	func inherent_two(): int = this.value + 1
	let inherent_value: int = 20
}
impl Feature for Choice {
	func abstract_func(): int = 21
	let abstract_value: int = 22
}
impl<T> Blanket for T { func blanket_method(): int = 23 }
namespace Utilities {
	func namespaced_func(): int = 24
	let namespaced_value: int = 25
}
"#;
	session.set_source(
		project.clone(),
		main.clone(),
		source.into(),
		SourceVersion(1),
	);
	let definitions = session
		.runtime_definitions_for_test(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Library,
		)
		.expect("matrix owner exists");
	assert!(
		!definitions.is_empty(),
		"matrix must project runtime artifacts: diagnostics={:?}, extraction={:?}",
		session.tooling_diagnostics(project.clone(), main.clone(), false),
		session.runtime_definition(
			project.clone(),
			main.clone(),
			id("runtime-matrix", "public_func"),
			EntryMode::Library
		)
	);
	let exact_ids = definitions
		.iter()
		.map(|definition| definition.definition.clone())
		.collect::<std::collections::BTreeSet<_>>();
	assert_eq!(
		exact_ids.len(),
		definitions.len(),
		"every artifact ID is unique"
	);
	for definition in &definitions {
		assert_eq!(
			*definition,
			session
				.runtime_definition(
					project.clone(),
					main.clone(),
					definition.definition.clone(),
					EntryMode::Library,
				)
				.expect("inspection ID is exactly queryable")
		);
	}
	let rendered = exact_ids
		.iter()
		.map(|id| format!("{id:?}"))
		.collect::<Vec<_>>();
	for expected in [
		"public_func",
		"private_func",
		"public_value",
		"private_value",
		"external_func",
		"external_value",
		"Record",
		"Choice",
		"inline_func",
		"inline_mut_func",
		"inline_static",
		"inline_static_value",
		"external_member_func",
		"external_member_value",
		"enum_func",
		"enum_mut_func",
		"enum_static",
		"enum_static_value",
		"inherent_one",
		"inherent_two",
		"inherent_value",
		"default_one",
		"default_two",
		"default_value",
		"blanket_method",
		"namespaced_func",
		"namespaced_value",
	] {
		assert!(
			rendered.iter().any(|id| id.contains(expected)),
			"missing {expected}: {rendered:#?}"
		);
	}
	assert!(!exact_ids.iter().any(|id| matches!(
		&id.key,
		DeclarationKey::Member { owner, name, .. }
			if matches!(&owner.key, DeclarationKey::TopLevel { category: DeclarationCategory::Interface, name: owner_name, .. } if owner_name == "Feature")
				&& (name == "abstract_func" || name == "abstract_value")
	)));
	assert!(!rendered.iter().any(|id| id.contains("Alias")));
}

#[test]
fn shared_placement_members_have_four_distinct_exact_entities() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("shared-placement");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		r#"interface Defaults { func first(): int = 1
func second(): int = 2 }
struct Item
impl Item { func third(): int = 3
func fourth(): int = 4 }"#
			.into(),
		SourceVersion(1),
	);
	let definitions = session
		.runtime_definitions_for_test(project, main.clone(), main, EntryMode::Library)
		.unwrap();
	let selected = definitions
		.iter()
		.filter(|definition| {
			["first", "second", "third", "fourth"]
				.iter()
				.any(|name| format!("{:?}", definition.definition).contains(name))
		})
		.collect::<Vec<_>>();
	assert_eq!(
		selected.len(),
		4,
		"four shared-placement artifacts: {definitions:#?}"
	);
	assert_eq!(
		selected
			.iter()
			.map(|definition| &definition.definition)
			.collect::<std::collections::BTreeSet<_>>()
			.len(),
		4,
		"no owner slot may overwrite a sibling"
	);
}

#[test]
fn struct_shell_isolated_from_method_body_and_signature_invalidation() {
	let (mut session, events) = session();
	let project = ProjectId::new("shell-isolation");
	let main = ModulePath::new("main").unwrap();
	let sources = [
		"struct Box(value: int) { func get(): int = this.value }",
		"struct Box(value: int) { func get(): int = this.value + 1 }",
		"struct Box(value: int) { func get(extra: int): int = this.value + extra }",
	];
	session.set_source(
		project.clone(),
		main.clone(),
		sources[0].into(),
		SourceVersion(1),
	);
	let definitions = session
		.runtime_definitions_for_test(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let shell_id = definitions
		.iter()
		.find(|definition| matches!(definition.payload, RuntimePayload::Struct(_)))
		.unwrap()
		.definition
		.clone();
	let method_id = definitions
		.iter()
		.find(|definition| format!("{:?}", definition.definition).contains("get"))
		.unwrap()
		.definition
		.clone();
	let before_shell = session
		.runtime_definition_consumer_for_test(
			project.clone(),
			main.clone(),
			shell_id.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let RuntimePayload::Struct(shell) = &before_shell.payload else {
		unreachable!()
	};
	assert_eq!(
		shell.fields.len(),
		1,
		"shell contains layout only; methods are exact siblings"
	);
	let mut previous_method = session
		.runtime_definition_consumer_for_test(
			project.clone(),
			main.clone(),
			method_id.clone(),
			EntryMode::Library,
		)
		.unwrap();
	for (version, source) in sources.iter().enumerate().skip(1) {
		events.lock().unwrap().clear();
		session.set_source(
			project.clone(),
			main.clone(),
			(*source).into(),
			SourceVersion(version as i64 + 1),
		);
		let shell = session
			.runtime_definition_consumer_for_test(
				project.clone(),
				main.clone(),
				shell_id.clone(),
				EntryMode::Library,
			)
			.unwrap();
		let method = session
			.runtime_definition_consumer_for_test(
				project.clone(),
				main.clone(),
				method_id.clone(),
				EntryMode::Library,
			)
			.unwrap();
		assert_eq!(before_shell, shell);
		assert_ne!(previous_method, method);
		assert!(!events.lock().unwrap().iter().any(|event| {
			event.query == "runtime_definition_consumer" && event.definition.as_ref() == Some(&shell_id)
		}));
		assert!(events.lock().unwrap().iter().any(|event| {
			event.query == "runtime_definition_consumer" && event.definition.as_ref() == Some(&method_id)
		}));
		previous_method = method;
	}
}

#[test]
fn importable_std_default_is_retrieved_by_owner_identity_without_ambient_core() {
	let (mut session, events) = session();
	let project = ProjectId::new("std-runtime-owner");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"import std/math/complex\nfunc local(): int = 1".into(),
		SourceVersion(1),
	);
	let stable = session
		.builtin_interface_member_ids_for_test(
			project.clone(),
			main.clone(),
			"math/complex",
			EntryMode::Library,
		)
		.into_iter()
		.next()
		.expect("importable std interface implementation member");
	let before = session
		.runtime_definition(
			project.clone(),
			main.clone(),
			stable.clone(),
			EntryMode::Library,
		)
		.expect("owner is derived from the std stable ID");
	let warmed = session
		.runtime_definition_consumer_for_test(
			project.clone(),
			main.clone(),
			stable.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(before, warmed);
	events.lock().unwrap().clear();
	let repeated = session
		.runtime_definition_consumer_for_test(
			project.clone(),
			main.clone(),
			stable.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(before, repeated);
	assert!(events.lock().unwrap().is_empty());
	session.set_source(
		project.clone(),
		main.clone(),
		"import std/math/complex\nfunc local(): int = 2".into(),
		SourceVersion(2),
	);
	let after = session
		.runtime_definition_consumer_for_test(project, main, stable, EntryMode::Library)
		.unwrap();
	assert_eq!(before, after);
}

#[test]
fn exact_runtime_entities_backdate_unchanged_siblings() {
	let (mut session, events) = session();
	let project = ProjectId::new("runtime");
	let main = ModulePath::new("main").unwrap();
	let a = id("runtime", "a");
	let b = id("runtime", "b");
	session.set_source(
		project.clone(),
		main.clone(),
		"public func a(): int = 1\npublic func b(): int = 2".into(),
		SourceVersion(1),
	);
	let before_a = session
		.runtime_definition(project.clone(), main.clone(), a.clone(), EntryMode::Library)
		.expect("A artifact");
	let before_b = session
		.runtime_definition(project.clone(), main.clone(), b.clone(), EntryMode::Library)
		.expect("B artifact");
	assert_ne!(before_a.definition, before_b.definition);
	assert!(matches!(before_a.payload, RuntimePayload::NymphBody(_)));
	let _ = session.runtime_definition_consumer_for_test(
		project.clone(),
		main.clone(),
		a.clone(),
		EntryMode::Library,
	);
	let _ = session.runtime_definition_consumer_for_test(
		project.clone(),
		main.clone(),
		b.clone(),
		EntryMode::Library,
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		main.clone(),
		"public func a(): int = 1\npublic func b(): int = 3".into(),
		SourceVersion(2),
	);
	let after_a = session
		.runtime_definition_consumer_for_test(project.clone(), main.clone(), a, EntryMode::Library)
		.unwrap();
	let after_b = session
		.runtime_definition_consumer_for_test(project, main, b, EntryMode::Library)
		.unwrap();
	assert_eq!(before_a, after_a);
	assert_ne!(before_b, after_b);
	let observed = events.lock().unwrap();
	assert!(
		!observed
			.iter()
			.any(|event| event.query == "runtime_definition_consumer"
				&& event.definition.as_ref() == Some(&before_a.definition))
	);
	assert!(
		observed
			.iter()
			.any(|event| event.query == "runtime_definition_consumer"
				&& event.definition.as_ref() == Some(&before_b.definition))
	);
}

#[test]
fn declaration_insertion_does_not_change_exact_artifact() {
	let (mut session, events) = session();
	let project = ProjectId::new("stable");
	let main = ModulePath::new("main").unwrap();
	let a = id("stable", "a");
	session.set_source(
		project.clone(),
		main.clone(),
		"public func a(value: int): boolean = value == 1".into(),
		SourceVersion(1),
	);
	let before = session
		.runtime_definition_consumer_for_test(
			project.clone(),
			main.clone(),
			a.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let RuntimePayload::NymphBody(before_body) = &before.payload else {
		panic!("function body payload");
	};
	assert!(!before_body.annotations.types.is_empty());
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		main.clone(),
		"private func unrelated(): int = 0\npublic func a(value: int): boolean = value == 1".into(),
		SourceVersion(2),
	);
	let after = session
		.runtime_definition_consumer_for_test(project, main, a.clone(), EntryMode::Library)
		.unwrap();
	assert_eq!(before, after);
	assert!(!events.lock().unwrap().iter().any(|event| {
		event.query == "runtime_definition_consumer" && event.definition.as_ref() == Some(&a)
	}));
	events.lock().unwrap().clear();
	session.set_source(
		ProjectId::new("stable"),
		ModulePath::new("main").unwrap(),
		"public func a(value: int): boolean = value == 1\nprivate func unrelated(): int = 0".into(),
		SourceVersion(3),
	);
	let reordered = session
		.runtime_definition_consumer_for_test(
			ProjectId::new("stable"),
			ModulePath::new("main").unwrap(),
			a.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(before, reordered);
	assert!(!events.lock().unwrap().iter().any(|event| {
		event.query == "runtime_definition_consumer" && event.definition.as_ref() == Some(&a)
	}));
}

#[test]
fn import_target_change_updates_exact_runtime_annotations() {
	let (mut session, events) = session();
	let project = ProjectId::new("import-change");
	let main = ModulePath::new("main").unwrap();
	for dependency in ["left", "right"] {
		session.set_source(
			project.clone(),
			ModulePath::new(dependency).unwrap(),
			"public func answer(): int = 1".into(),
			SourceVersion(1),
		);
	}
	let definition = id("import-change", "forwarded");
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/left with (answer)\npublic func forwarded(): int = answer()".into(),
		SourceVersion(1),
	);
	let before = session
		.runtime_definition_consumer_for_test(
			project.clone(),
			main.clone(),
			definition.clone(),
			EntryMode::Library,
		)
		.unwrap();
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/right with (answer)\npublic func forwarded(): int = answer()".into(),
		SourceVersion(2),
	);
	let after = session
		.runtime_definition_consumer_for_test(project, main, definition.clone(), EntryMode::Library)
		.unwrap();
	assert_ne!(before, after);
	let target_module = |artifact: &nymph_sema::RuntimeDefinition| {
		let RuntimePayload::NymphBody(body) = &artifact.payload else {
			panic!("function body payload");
		};
		body.annotations.definition_targets[0].1.module.path.clone()
	};
	assert_eq!(target_module(&before), "left");
	assert_eq!(target_module(&after), "right");
	assert!(events.lock().unwrap().iter().any(|event| {
		event.query == "runtime_definition_consumer" && event.definition.as_ref() == Some(&definition)
	}));
}

#[test]
fn member_local_generic_types_are_preserved_in_exact_runtime_annotations() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("member-generics");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"interface Items<T> { func first<U>(values: #[U]): U = values[0] }".into(),
		SourceVersion(1),
	);
	let definitions = session
		.runtime_definitions_for_test(project, main.clone(), main, EntryMode::Library)
		.expect("member-local generic types must canonicalize into the exact runtime artifact");
	let body = definitions
		.iter()
		.find_map(|definition| match &definition.payload {
			RuntimePayload::NymphBody(body) => Some(body),
			_ => None,
		})
		.expect("generic default body artifact");
	assert!(!body.annotations.types.is_empty());
}

#[test]
fn signature_changes_are_part_of_exact_runtime_payload_equality() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("signature-runtime");
	let main = ModulePath::new("main").unwrap();
	let a = id("signature-runtime", "a");
	session.set_source(
		project.clone(),
		main.clone(),
		"public func a(value: int): int = value".into(),
		SourceVersion(1),
	);
	let before = session
		.runtime_definition(project.clone(), main.clone(), a.clone(), EntryMode::Library)
		.unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"public func a(value: uint): uint = value".into(),
		SourceVersion(2),
	);
	let after = session
		.runtime_definition(project, main, a, EntryMode::Library)
		.unwrap();
	assert_ne!(before, after);
}

#[test]
fn recovered_modules_publish_no_runtime_artifacts() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("recovered-runtime");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"public func broken(x: Missing): Missing = x".into(),
		SourceVersion(1),
	);
	assert_eq!(
		session.runtime_definition(
			project,
			main,
			id("recovered-runtime", "broken"),
			EntryMode::Library
		),
		Err(RuntimeDefinitionError::Recovered)
	);
	assert_eq!(
		session.runtime_definition(
			ProjectId::new("recovered-runtime"),
			ModulePath::new("main").unwrap(),
			id("recovered-runtime", "anything_else"),
			EntryMode::Library,
		),
		Err(RuntimeDefinitionError::Recovered)
	);
}
