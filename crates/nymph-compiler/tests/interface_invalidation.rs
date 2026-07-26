#![cfg(feature = "test-support")]

use std::sync::{Arc, Mutex};

use nymph_compiler::project::{
	AmbientCoreModuleKey, BuiltinRuntimeOwnerShape, CompilerSession, ModulePath, ProjectId,
	SemanticQueryEvent, SourceVersion,
};
use nymph_sema::{
	EntryMode, InterfaceType, ModuleEnvironment, RecoveredDefinitionReference, RecoveredInterfaceType,
};

fn event_session() -> (CompilerSession, Arc<Mutex<Vec<String>>>) {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	(
		CompilerSession::with_event_callback_and_tombstone_threshold(
			move |name| sink.lock().unwrap().push(name.to_string()),
			256,
		),
		events,
	)
}

fn interface_event_session() -> (CompilerSession, Arc<Mutex<Vec<SemanticQueryEvent>>>) {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	(
		CompilerSession::with_detailed_event_callback_for_test(move |event| {
			sink.lock().unwrap().push(event)
		}),
		events,
	)
}

fn count(events: &[SemanticQueryEvent], query: &str, module: &str) -> usize {
	events
		.iter()
		.filter(|event| event.query == query && event.module.as_deref() == Some(module))
		.count()
}

#[test]
fn importable_set_interface_preserves_exact_export_and_nested_iterable_implementation() {
	let session = CompilerSession::new();
	let iterable_id = session
		.ambient_core_module_interface(AmbientCoreModuleKey::new("iter/iterable").unwrap())
		.unwrap()
		.exports
		.iter()
		.find(|definition| definition.name == "Iterable")
		.unwrap()
		.id
		.clone();
	let environment = session
		.importable_std_module_environment_for_test("collections/set")
		.expect("embedded Set environment");
	let ModuleEnvironment::Complete(interface) = &*environment else {
		panic!("the importable Set module must produce a complete interface")
	};
	let set = interface
		.exports
		.iter()
		.find(|definition| definition.name == "Set")
		.expect("exact public Set export");
	let iterable = interface
		.implementations
		.iter()
		.find(|implementation| {
			matches!(
				&implementation.self_type,
				InterfaceType::Named { definition, .. } if definition == &set.id
			) && implementation.interface.as_ref() == Some(&iterable_id)
		})
		.expect("exact nested Iterable implementation");
	assert_eq!(iterable.binders[0].name, "Item");
	assert_eq!(iterable.members[0].name, "iter");
	assert_eq!(iterable.member_slots.len(), 1);
	assert_eq!(iterable.member_slots[0].implementation_id, iterable.id);
	assert_eq!(iterable.member_slots[0].member_id, iterable.members[0].id);
}

#[test]
fn body_only_edit_preserves_compat_interface_value() {
	let (mut session, events) = event_session();
	let project = ProjectId::new("oracle");
	let path = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"public func answer(): int = 1".into(),
		SourceVersion(1),
	);
	let before = session
		.module_interface(
			project.clone(),
			path.clone(),
			path.clone(),
			EntryMode::Library,
		)
		.unwrap();
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		path.clone(),
		"public func answer(): int = 2".into(),
		SourceVersion(2),
	);
	let after = session
		.module_interface(project.clone(), path.clone(), path, EntryMode::Library)
		.unwrap();
	assert_eq!(before, after);
	let observed = events.lock().unwrap();
	assert!(
		observed
			.iter()
			.any(|event| event == "compat_module_analysis")
	);
	assert!(
		observed
			.iter()
			.any(|event| event == "compat_declared_headers")
	);
	assert!(
		observed
			.iter()
			.any(|event| event == "compat_module_environment")
	);
	assert!(
		!observed
			.iter()
			.any(|event| event == "compat_module_interface"),
		"equal environment should backdate the interface producer"
	);
}

#[test]
fn interface_producer_is_backdated_after_body_only_edit() {
	let (mut session, events) = event_session();
	let project = ProjectId::new("backdating");
	let path = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"private func helper(): int = 1\npublic func answer(): int = helper()".into(),
		SourceVersion(1),
	);
	let _ = session.module_interface(
		project.clone(),
		path.clone(),
		path.clone(),
		EntryMode::Library,
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		path.clone(),
		"private func helper(): int = 2\npublic func answer(): int = helper()".into(),
		SourceVersion(2),
	);
	let _ = session.module_interface(
		project.clone(),
		path.clone(),
		path.clone(),
		EntryMode::Library,
	);
	let first = events.lock().unwrap().clone();
	assert!(
		first
			.iter()
			.any(|event| event == "compat_module_environment")
	);
	assert!(!first.iter().any(|event| event == "compat_module_interface"));
	events.lock().unwrap().clear();
	let _ = session.module_interface(project, path.clone(), path, EntryMode::Library);
	assert!(
		events.lock().unwrap().is_empty(),
		"backdated value was recomputed"
	);
}

#[test]
fn recovered_environment_is_not_lowerable() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("recovery");
	let path = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"public func broken(x: Missing): Missing = x".into(),
		SourceVersion(1),
	);
	let environment = session
		.module_environment(
			project.clone(),
			path.clone(),
			path.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert!(matches!(&*environment, ModuleEnvironment::Recovered(_)));
	assert!(
		session
			.module_interface(
				project.clone(),
				path.clone(),
				path.clone(),
				EntryMode::Library,
			)
			.is_none(),
		"a recovered environment must not masquerade as an empty complete interface"
	);
	assert!(
		session
			.environment_is_lowerable(project, path.clone(), path, EntryMode::Library)
			.is_err()
	);
}

#[test]
fn diagnostic_text_and_span_changes_are_not_interface_identity() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("diagnostic-equality");
	let path = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"public func broken(value: Missing): int = 1".into(),
		SourceVersion(1),
	);
	let before = session
		.module_environment(
			project.clone(),
			path.clone(),
			path.clone(),
			EntryMode::Library,
		)
		.unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"\npublic func broken(value: Unknown): int = 1".into(),
		SourceVersion(2),
	);
	let after = session
		.module_environment(project, path.clone(), path, EntryMode::Library)
		.unwrap();
	assert_eq!(before, after);
}

#[test]
fn conversion_failure_is_a_separate_internal_diagnostic_and_blocks_lowering() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("conversion");
	let path = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"public type Mystery = _".into(),
		SourceVersion(1),
	);
	let diagnostics = session
		.module_diagnostics(
			project.clone(),
			path.clone(),
			path.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(diagnostics.len(), 1);
	assert_eq!(diagnostics[0].diag.code, "INTERNAL-INTERFACE-CONVERSION");
	assert!(
		session
			.environment_is_lowerable(project, path.clone(), path, EntryMode::Library)
			.is_err()
	);
}

#[test]
fn compiler_builtins_have_private_identity_and_stable_runtime_owners() {
	let session = CompilerSession::new();
	let artifacts = session.builtin_runtime_owner_artifacts();
	assert!(!artifacts.is_empty());
	assert!(
		artifacts
			.iter()
			.all(|artifact| artifact.definition.module.project == "compiler")
	);
	assert!(artifacts.iter().all(|artifact| match &artifact.shape {
		BuiltinRuntimeOwnerShape::Definition(definition) => {
			definition.id == artifact.definition && definition.runtime_owner.is_some()
		}
		BuiltinRuntimeOwnerShape::Implementation(implementation) => {
			implementation.id == artifact.definition && implementation.runtime_owner.is_some()
		}
	}));
	assert_eq!(
		artifacts,
		CompilerSession::new().builtin_runtime_owner_artifacts()
	);
	for artifact in artifacts.iter() {
		assert_eq!(
			session.builtin_runtime_owner_artifact(&artifact.definition),
			Some(artifact.clone())
		);
	}
}

#[test]
fn every_public_interface_shape_edit_changes_the_interface() {
	let cases = [
		(
			"public func f(value: int): int = value",
			"public func f(value: float): int = 1",
		),
		(
			"public struct S(value: int) {}",
			"public struct S(value: float) {}",
		),
		(
			"public enum E { A(value: int) }",
			"public enum E { A(value: float) }",
		),
		(
			"public interface I { func f(): int }",
			"public interface I { func f(): float }",
		),
		(
			"public interface I {}\npublic struct S {}\npublic impl I for S {}",
			"public interface I {}\npublic struct S {}\npublic impl I for int {}",
		),
	];
	for (index, (before_source, after_source)) in cases.into_iter().enumerate() {
		let mut session = CompilerSession::without_builtin_sources();
		let project = ProjectId::new(format!("shape-{index}"));
		let path = ModulePath::new("main").unwrap();
		session.set_source(
			project.clone(),
			path.clone(),
			before_source.into(),
			SourceVersion(1),
		);
		let before = session
			.module_interface(
				project.clone(),
				path.clone(),
				path.clone(),
				EntryMode::Library,
			)
			.expect("complete before interface");
		session.set_source(
			project.clone(),
			path.clone(),
			after_source.into(),
			SourceVersion(2),
		);
		let after = session
			.module_interface(project, path.clone(), path, EntryMode::Library)
			.expect("complete after interface");
		assert_ne!(before, after, "case {index} did not change the interface");
	}
}

#[test]
fn compatibility_extraction_preserves_import_owner_ids_and_current_impl_facts() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("provenance");
	for (path, source) in [
		(
			"a",
			"public interface Mark { func mark(): int }\npublic struct Same(value: int) {}\npublic impl Mark for Same { func mark(): int = 1 }",
		),
		(
			"b",
			"public interface Mark { func mark(): int }\npublic struct Same(value: int) {}\npublic impl Mark for Same { func mark(): int = 2 }",
		),
		(
			"main",
			"import @/a with (Same as ASame)\nimport @/b with (Same as BSame)\npublic interface LocalMark { func local(): int }\npublic struct Same(value: int) {}\npublic func from_a(value: ASame): ASame = value\npublic func from_b(value: BSame): BSame = value\npublic impl LocalMark for Same { func local(): int = 3 }",
		),
	] {
		session.set_source(
			project.clone(),
			ModulePath::new(path).unwrap(),
			source.into(),
			SourceVersion(1),
		);
	}
	let main = ModulePath::new("main").unwrap();
	let interface = session
		.module_interface(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert!(
		!interface.exports.is_empty(),
		"{:?}",
		session.module_diagnostics(project, main.clone(), main, EntryMode::Library)
	);
	let named_module = |name: &str| {
		let export = interface
			.exports
			.iter()
			.find(|definition| definition.name == name)
			.unwrap();
		let InterfaceType::Named { definition, .. } = export.parameters[0].ty.clone() else {
			panic!("expected named parameter")
		};
		definition.module.path
	};
	assert_eq!(named_module("from_a"), "a");
	assert_eq!(named_module("from_b"), "b");
	assert_eq!(interface.implementations.len(), 1);
	assert_eq!(
		interface.implementations[0]
			.interface
			.as_ref()
			.unwrap()
			.module
			.path,
		"main"
	);
}

#[test]
fn recovered_flattened_implementation_keeps_current_module_provenance_and_shape() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("recovered-provenance");
	for (path, source) in [
		(
			"dependency",
			"public interface Mark { func mark(): int }\npublic struct Same {}\npublic impl Mark for Same { func mark(): int = 1 }",
		),
		(
			"main",
			"import @/dependency\npublic interface Mark { func local(): int }\npublic struct Same {}\npublic impl Mark for Same { func local(): Missing = panic(\"bad\") }",
		),
	] {
		session.set_source(
			project.clone(),
			ModulePath::new(path).unwrap(),
			source.into(),
			SourceVersion(1),
		);
	}
	let main = ModulePath::new("main").unwrap();
	let environment = session
		.module_environment(project, main.clone(), main, EntryMode::Library)
		.unwrap();
	let ModuleEnvironment::Recovered(environment) = &*environment else {
		panic!("expected recovered environment")
	};
	assert_eq!(environment.implementations.len(), 1);
	let implementation = &environment.implementations[0];
	let Some(RecoveredDefinitionReference::Known(interface)) = &implementation.interface else {
		panic!("current interface should resolve")
	};
	assert_eq!(interface.module.path, "main");
	assert_eq!(implementation.members.len(), 1);
	assert_eq!(implementation.members[0].name, "local");
	assert!(matches!(
		implementation.members[0].return_type,
		RecoveredInterfaceType::Poison
	));
}

fn install_chain(session: &mut CompilerSession, project: &ProjectId) {
	for (module, source) in [
		("leaf", "public func value(): int = 1"),
		(
			"middle",
			"import @/leaf with (value)\npublic func forwarded(): int = value()",
		),
		(
			"main",
			"import @/middle with (forwarded)\nfunc main(): int = forwarded()",
		),
	] {
		session.set_source(
			project.clone(),
			ModulePath::new(module).unwrap(),
			source.into(),
			SourceVersion(1),
		);
	}
}

#[test]
fn private_leaf_body_edit_backdates_before_consumers_execute() {
	let (mut session, events) = interface_event_session();
	let project = ProjectId::new("interface-private");
	install_chain(&mut session, &project);
	let main = ModulePath::new("main").unwrap();
	assert!(
		session
			.analyze_module(
				project.clone(),
				main.clone(),
				main.clone(),
				EntryMode::Entry
			)
			.is_some()
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		ModulePath::new("leaf").unwrap(),
		"public func value(): int = 2".into(),
		SourceVersion(2),
	);
	assert!(
		session
			.analyze_module(project, main.clone(), main, EntryMode::Entry)
			.is_some()
	);
	let observed = events.lock().unwrap();
	assert_eq!(count(&observed, "interface_module_analysis", "leaf"), 1);
	assert_eq!(count(&observed, "interface_module_interface", "leaf"), 1);
	assert_eq!(count(&observed, "interface_module_environment", "leaf"), 1);
	assert_eq!(count(&observed, "interface_module_analysis", "middle"), 0);
	assert_eq!(count(&observed, "interface_module_analysis", "main"), 0);
}

#[test]
fn public_leaf_signature_edit_invalidates_only_reachable_consumers() {
	let (mut session, events) = interface_event_session();
	let project = ProjectId::new("interface-public");
	install_chain(&mut session, &project);
	let unrelated_project = ProjectId::new("interface-unrelated");
	let unrelated = ModulePath::new("main").unwrap();
	session.set_source(
		unrelated_project.clone(),
		unrelated.clone(),
		"func main(): int = 0".into(),
		SourceVersion(1),
	);
	let main = ModulePath::new("main").unwrap();
	assert!(
		session
			.analyze_module(
				project.clone(),
				main.clone(),
				main.clone(),
				EntryMode::Entry
			)
			.is_some()
	);
	assert!(
		session
			.analyze_module(
				unrelated_project.clone(),
				unrelated.clone(),
				unrelated.clone(),
				EntryMode::Entry
			)
			.is_some()
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		ModulePath::new("leaf").unwrap(),
		"public func value(): int = 1\npublic func added(): float = 1.0".into(),
		SourceVersion(2),
	);
	let _ = session.analyze_module(project, main.clone(), main, EntryMode::Entry);
	let _ = session.analyze_module(
		unrelated_project,
		unrelated.clone(),
		unrelated,
		EntryMode::Entry,
	);
	let observed = events.lock().unwrap();
	for module in ["leaf", "middle", "main"] {
		assert_eq!(
			count(&observed, "interface_module_analysis", module),
			1,
			"{module}"
		);
	}
	assert_eq!(
		count(
			&observed,
			"interface_module_analysis",
			"interface-unrelated:main"
		),
		0
	);
}

#[test]
fn equal_intermediate_interface_stops_transitive_invalidation() {
	let (mut session, events) = interface_event_session();
	let project = ProjectId::new("interface-stop");
	install_chain(&mut session, &project);
	let main = ModulePath::new("main").unwrap();
	let _ = session.analyze_module(
		project.clone(),
		main.clone(),
		main.clone(),
		EntryMode::Entry,
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		ModulePath::new("middle").unwrap(),
		"import @/leaf with (value)\nprivate func detail(): int = 2\npublic func forwarded(): int = value()".into(),
		SourceVersion(2),
	);
	let _ = session.analyze_module(project, main.clone(), main, EntryMode::Entry);
	let observed = events.lock().unwrap();
	assert_eq!(count(&observed, "interface_module_analysis", "middle"), 1);
	assert_eq!(count(&observed, "interface_module_interface", "middle"), 1);
	assert_eq!(count(&observed, "interface_module_analysis", "main"), 0);
}

#[test]
fn importable_and_ambient_roots_stay_in_the_closed_interface_family() {
	let (mut session, events) = interface_event_session();
	let project = ProjectId::new("interface-roots");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"import std/io\nfunc main(): int = 0".into(),
		SourceVersion(1),
	);
	assert!(
		session
			.analyze_module(project, main.clone(), main, EntryMode::Entry)
			.is_some()
	);
	let observed = events.lock().unwrap();
	assert!(observed.iter().any(|event| {
		event.query == "interface_module_environment"
			&& event
				.module
				.as_deref()
				.is_some_and(|module| module.ends_with("io"))
	}));
	assert!(
		observed
			.iter()
			.any(|event| event.query == "ambient_core_environment")
	);
	assert!(
		!observed
			.iter()
			.any(|event| event.query.starts_with("compat_"))
	);
}
