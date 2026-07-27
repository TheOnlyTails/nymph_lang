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
	for module in &keys {
		let diagnostics = session
			.ambient_core_module_diagnostics(module.clone())
			.unwrap();
		assert!(
			diagnostics.is_empty(),
			"{}: {diagnostics:?}",
			module.as_str()
		);
		assert!(matches!(
			&*session
				.ambient_core_module_environment(module.clone())
				.expect("core environment"),
			ModuleEnvironment::Complete(_)
		));
	}

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
fn ambient_option_runtime_owner_is_exact_and_lowerable() {
	let mut session = CompilerSession::new();
	let interface = session
		.ambient_core_module_interface(key("option"))
		.unwrap();
	let option = interface
		.exports
		.iter()
		.find(|item| item.name == "Option")
		.unwrap();
	let owner = option.runtime_owner.clone().expect("Option runtime owner");
	assert_eq!(owner.module.path, "option");
	let project = ProjectId::new("ambient-runtime");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"func main() = None".into(),
		SourceVersion(1),
	);
	let artifact = session
		.runtime_definition(
			project.clone(),
			main.clone(),
			owner.clone(),
			nymph_sema::EntryMode::Library,
		)
		.expect("exact ambient Option artifact");
	assert_eq!(artifact.source_owner.path, "option");
	assert!(matches!(
		artifact.payload,
		nymph_sema::RuntimePayload::Enum(_)
	));
	let lowered = session
		.lower_runtime_definition(project, main, owner, nymph_sema::EntryMode::Library)
		.expect("lower exact ambient Option");
	assert_eq!(lowered.definition().module.path, "option");
}

#[test]
fn ambient_list_iterator_uses_the_imported_iterator_next_identity() {
	let session = CompilerSession::new();
	let iterator_module = session
		.ambient_core_module_interface(key("iter"))
		.expect("Iterator ambient interface");
	let iterator = iterator_module
		.exports
		.iter()
		.find(|definition| definition.name == "Iterator")
		.expect("Iterator export");
	let next = iterator
		.members
		.iter()
		.find(|member| member.name == "next")
		.expect("Iterator.next");
	let iterable_module = session
		.ambient_core_module_interface(key("iter/iterable"))
		.expect("Iterable ambient interface");
	let list_iter = iterable_module
		.exports
		.iter()
		.find(|definition| definition.name == "ListIter")
		.expect("ListIter export");
	let implementation = iterable_module
		.implementations
		.iter()
		.find(|implementation| {
			implementation.interface.as_ref() == Some(&iterator.id)
				&& matches!(
					&implementation.self_type,
					nymph_sema::InterfaceType::Named { definition, .. } if definition == &list_iter.id
				)
		})
		.expect("ListIter Iterator implementation");
	let slot = implementation
		.member_slots
		.iter()
		.find(|slot| slot.name == "next")
		.expect("ListIter Iterator.next slot");
	assert_eq!(slot.interface_member_id, next.id);
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
fn undeclared_ambient_sibling_is_not_visible() {
	let mut session = CompilerSession::new();
	let source = session.ambient_core_source_for_test(key("math")).unwrap();
	session.set_ambient_core_source_for_test(
		key("math"),
		format!("public func leaked(value: Option<int>) = value\n{source}"),
	);
	let diagnostics = session
		.ambient_core_module_diagnostics(key("math"))
		.unwrap();
	assert!(
		diagnostics
			.iter()
			.any(nymph_diagnostics::Diagnostic::is_error),
		"an undeclared sibling reference was silently accepted: {diagnostics:?}"
	);
}

#[test]
fn declared_ambient_import_cycle_is_reported_without_panicking() {
	let mut session = CompilerSession::new();
	let option = session.ambient_core_source_for_test(key("option")).unwrap();
	let result = session.ambient_core_source_for_test(key("result")).unwrap();
	session.set_ambient_core_source_for_test(
		key("option"),
		format!("import @/result with (Result)\n{option}"),
	);
	session.set_ambient_core_source_for_test(
		key("result"),
		format!("import @/option with (Option)\n{result}"),
	);
	let diagnostics = session
		.ambient_core_module_diagnostics(key("option"))
		.unwrap();
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.code == "CORE-IMPORT-CYCLE"),
		"missing typed cycle diagnostic: {diagnostics:?}"
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
