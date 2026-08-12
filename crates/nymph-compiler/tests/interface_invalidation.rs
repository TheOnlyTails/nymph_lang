#![cfg(feature = "test-support")]

use std::sync::{Arc, Mutex};

use nymph_compiler::project::{
	AmbientCoreModuleKey, BuiltinRuntimeOwnerShape, CompilerSession, ModulePath, ProjectId,
	SemanticQueryEvent, SourceVersion,
};
use nymph_sema::{
	EntryMode, InterfaceType, ModuleEnvironment, RecoveredDefinitionReference, RecoveredInterfaceType,
};

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
fn unrelated_anonymous_binder_insertion_does_not_change_public_interface() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("anonymous-binder-stability");
	let main = ModulePath::new("main").unwrap();
	let public = "public interface Area { func area(): int }\npublic interface Consumer { func measure(shape: Area): int }\npublic func measure(shape: Area): int = shape.area()";
	session.set_source(
		project.clone(),
		main.clone(),
		public.into(),
		SourceVersion(1),
	);
	let before = session
		.module_interface(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Library,
		)
		.expect("complete interface before insertion");
	let top_level = before
		.exports
		.iter()
		.find(|definition| definition.name == "measure")
		.expect("top-level measure export");
	let member = before
		.exports
		.iter()
		.find(|definition| definition.name == "Consumer")
		.expect("Consumer export")
		.members
		.first()
		.expect("Consumer.measure member");
	assert_eq!(top_level.binders[0].name, "$anonymous0");
	assert_eq!(member.binders[0].name, "$anonymous0");
	session.set_source(
		project.clone(),
		main.clone(),
		format!("private func unrelated(shape: Area): int = shape.area()\n{public}"),
		SourceVersion(2),
	);
	let after = session
		.module_interface(project, main.clone(), main, EntryMode::Library)
		.expect("complete interface after insertion");
	assert_eq!(before, after);
}

#[test]
fn recovered_implementation_keeps_current_module_provenance_and_shape() {
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
fn private_namespace_body_edit_recomputes_only_the_dependency_summary() {
	let (mut session, events) = interface_event_session();
	let project = ProjectId::new("namespace-summary-body");
	let leaf = ModulePath::new("leaf").unwrap();
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		leaf.clone(),
		"private func helper(): int = 1\npublic func value(): int = 1".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/leaf with (value)\nfunc read(): int = leaf.helper()\nfunc main(): void = { let ignored = read() }"
			.into(),
		SourceVersion(1),
	);
	let _ = session.analyze_module(
		project.clone(),
		main.clone(),
		main.clone(),
		EntryMode::Entry,
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		leaf,
		"private func helper(): int = 2\npublic func value(): int = 1".into(),
		SourceVersion(2),
	);
	let _ = session.analyze_module(project, main.clone(), main, EntryMode::Entry);
	let observed = events.lock().unwrap();
	assert_eq!(count(&observed, "namespace_summary", "leaf"), 1);
	assert_eq!(count(&observed, "resolved_module_imports", "main"), 0);
	assert_eq!(count(&observed, "interface_module_analysis", "main"), 0);
}

#[test]
fn private_namespace_name_edit_revalidates_the_consumer_privacy_decision() {
	let (mut session, events) = interface_event_session();
	let project = ProjectId::new("namespace-summary-private-name");
	let leaf = ModulePath::new("leaf").unwrap();
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		leaf.clone(),
		"private func helper(): int = 1\npublic func value(): int = 1".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/leaf with (value)\nfunc main(): int = leaf.helper()".into(),
		SourceVersion(1),
	);
	let initial = session
		.analyze_module(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Entry,
		)
		.unwrap();
	assert!(
		initial
			.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-PRIVATE-NAME")
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		leaf,
		"private func renamed(): int = 1\npublic func value(): int = 1".into(),
		SourceVersion(2),
	);
	let changed = session
		.analyze_module(project, main.clone(), main, EntryMode::Entry)
		.unwrap();
	assert!(
		changed
			.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-UNRESOLVED-NAME")
	);
	let observed = events.lock().unwrap();
	assert_eq!(count(&observed, "namespace_summary", "leaf"), 1);
	assert_eq!(count(&observed, "interface_module_analysis", "main"), 1);
}

#[test]
fn private_to_public_edit_revalidates_and_allows_namespace_access() {
	let (mut session, events) = interface_event_session();
	let project = ProjectId::new("namespace-summary-visibility");
	let leaf = ModulePath::new("leaf").unwrap();
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		leaf.clone(),
		"private func helper(): int = 1\npublic func value(): int = 1".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/leaf with (value)\nfunc read(): int = leaf.helper()\nfunc main(): void = { let ignored = read() }"
			.into(),
		SourceVersion(1),
	);
	let _ = session.analyze_module(
		project.clone(),
		main.clone(),
		main.clone(),
		EntryMode::Entry,
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		leaf,
		"public func helper(): int = 1\npublic func value(): int = 1".into(),
		SourceVersion(2),
	);
	let changed = session
		.analyze_module(project, main.clone(), main, EntryMode::Entry)
		.unwrap();
	assert!(changed.diagnostics.is_empty(), "{:?}", changed.diagnostics);
	let observed = events.lock().unwrap();
	assert_eq!(count(&observed, "namespace_summary", "leaf"), 1);
	assert_eq!(count(&observed, "interface_module_analysis", "main"), 1);
}

#[test]
fn recovery_to_complete_reclassifies_private_namespace_access() {
	let (mut session, events) = interface_event_session();
	let project = ProjectId::new("namespace-summary-recovery");
	let leaf = ModulePath::new("leaf").unwrap();
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		leaf.clone(),
		"private func helper(): int = 1\npublic func value(input: Missing): int = 1".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/leaf\nfunc main(): int = leaf.helper()".into(),
		SourceVersion(1),
	);
	let _ = session.analyze_module(
		project.clone(),
		main.clone(),
		main.clone(),
		EntryMode::Entry,
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		leaf,
		"private func helper(): int = 1\npublic func value(input: int): int = input".into(),
		SourceVersion(2),
	);
	let changed = session
		.analyze_module(project, main.clone(), main, EntryMode::Entry)
		.unwrap();
	assert!(
		changed
			.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-PRIVATE-NAME")
	);
	let observed = events.lock().unwrap();
	assert_eq!(count(&observed, "namespace_summary", "leaf"), 1);
	assert_eq!(count(&observed, "interface_module_analysis", "main"), 1);
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
}
