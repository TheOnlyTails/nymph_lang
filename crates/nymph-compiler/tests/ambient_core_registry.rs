use nymph_compiler::{AmbientCoreModuleKey, CompilerSession, ModulePath, ProjectId, SourceVersion};
use nymph_sema::ModuleEnvironment;
use std::sync::{Arc, Mutex};

fn key(value: &str) -> AmbientCoreModuleKey {
	AmbientCoreModuleKey::new(value).expect("canonical core key")
}

#[test]
fn ambient_core_is_private_complete_and_absent_from_the_project_graph() {
	let mut session = CompilerSession::new();
	let keys = session.ambient_core_module_keys();
	assert!(keys.contains(&key("option")));
	assert!(keys.contains(&key("range")));
	assert!(keys.contains(&key("iter/iterable")));

	for (module, declaration) in [
		("option", "Option"),
		("range", "RangeBounds"),
		("iter/iterable", "Iterable"),
	] {
		let diagnostics = session
			.ambient_core_module_diagnostics(key(module))
			.unwrap();
		assert!(diagnostics.is_empty(), "{module}: {diagnostics:?}");
		let interface = session
			.ambient_core_module_interface(key(module))
			.expect("core interface");
		assert!(
			interface
				.exports
				.iter()
				.any(|export| export.name == declaration),
			"{module} did not export {declaration}: {:?}",
			interface.exports
		);
		assert!(matches!(
			&*session
				.ambient_core_module_environment(key(module))
				.expect("core environment"),
			ModuleEnvironment::Complete(_)
		));
	}

	let project = ProjectId::new("ambient-graph");
	session.set_source(
		project.clone(),
		ModulePath::new("main").unwrap(),
		"func value(): Option<int> = None".into(),
		SourceVersion(1),
	);
	assert_eq!(
		session.graph_order(
			project,
			ModulePath::new("main").unwrap(),
			nymph_sema::EntryMode::Library,
		),
		[ModulePath::new("main").unwrap()]
	);
}

#[test]
fn core_ids_are_collision_free_deterministic_and_survive_rebuild() {
	let first = CompilerSession::new();
	let expected = first
		.ambient_core_module_interface(key("option"))
		.unwrap()
		.exports
		.iter()
		.map(|export| export.id.clone())
		.collect::<Vec<_>>();
	let second = CompilerSession::new();
	assert_eq!(
		expected,
		second
			.ambient_core_module_interface(key("option"))
			.unwrap()
			.exports
			.iter()
			.map(|export| export.id.clone())
			.collect::<Vec<_>>()
	);

	let mut rebuilding = CompilerSession::with_event_callback_and_tombstone_threshold(|_| {}, 1);
	let project = ProjectId::new("rebuild");
	rebuilding.set_source(
		project.clone(),
		ModulePath::new("temporary").unwrap(),
		"let temporary = 1".into(),
		SourceVersion(1),
	);
	rebuilding.remove_source(project, ModulePath::new("temporary").unwrap());
	assert_eq!(
		expected,
		rebuilding
			.ambient_core_module_interface(key("option"))
			.unwrap()
			.exports
			.iter()
			.map(|export| export.id.clone())
			.collect::<Vec<_>>()
	);
}

#[test]
fn disabling_importable_std_does_not_partially_disable_ambient_core() {
	let session = CompilerSession::without_builtin_sources();
	assert!(
		session
			.ambient_core_module_interface(key("option"))
			.is_some()
	);
}

#[test]
fn ambient_body_edit_invalidates_producers_but_backdates_equal_owner_descriptors() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |event| sink.lock().unwrap().push(event.to_string()),
		256,
	);
	let before = session.builtin_runtime_owner_artifacts();
	events.lock().unwrap().clear();
	let source = session.ambient_core_source_for_test(key("option")).unwrap();
	let edited = source.replacen("None -> false", "None -> !true", 1);
	assert_ne!(source, edited, "fixture must perform a body-only edit");
	session.set_ambient_core_source_for_test(key("option"), edited);
	let after = session.builtin_runtime_owner_artifacts();
	assert_eq!(before, after);
	let events = events.lock().unwrap();
	for producer in [
		"ambient_core_parse",
		"ambient_core_analysis",
		"ambient_core_environment",
	] {
		assert!(
			events.iter().any(|event| event == producer),
			"missing {producer}: {events:?}"
		);
	}
	assert!(
		!events
			.iter()
			.any(|event| event == "ambient_runtime_owner_artifacts"),
		"equal environments should backdate the descriptor producer"
	);
}
