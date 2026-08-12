#![cfg(feature = "test-support")]

use std::sync::{Arc, Mutex};

use nymph_compiler::project::{
	CompilerSession, ModulePath, ProjectId, SemanticQueryEvent, SourceVersion,
};
use nymph_sema::{
	BodyNodeId, DeclarationCategory, DeclarationKey, DefinitionId, EntryMode, ModuleEnvironment,
	ModuleIdentity, ModuleOrigin, RecoveredInterfaceType, RuntimePayload, StableModuleAssemblyError,
};

fn path(value: &str) -> ModulePath {
	ModulePath::new(value).unwrap()
}

fn definition(project: &str, module: &str, name: &str) -> DefinitionId {
	DefinitionId::new(
		ModuleIdentity {
			origin: ModuleOrigin::Project(project.into()),
			project: project.into(),
			path: module.into(),
		},
		DeclarationKey::top_level(DeclarationCategory::Function, name),
	)
}

fn install(session: &mut CompilerSession, project: &ProjectId, module: &str, source: &str) {
	session.set_source(
		project.clone(),
		path(module),
		source.into(),
		SourceVersion(1),
	);
}

#[test]
fn independent_sessions_repeat_complete_runtime_hir_name_and_emission_snapshots() {
	let project = ProjectId::new("stable-contract");
	let main = path("main");
	let answer = definition("stable-contract", "main", "answer");
	let mut first = CompilerSession::without_builtin_sources();
	let mut second = CompilerSession::without_builtin_sources();
	for session in [&mut first, &mut second] {
		install(session, &project, "main", "public func answer(): int = 42");
	}

	let first_environment = first
		.module_environment(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let second_environment = second
		.module_environment(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(first_environment, second_environment);
	let ModuleEnvironment::Complete(interface) = first_environment.as_ref() else {
		panic!("valid source must produce a complete interface")
	};
	assert_eq!(interface.fingerprint, 1_079_886_840_365_780_136);
	assert_eq!(interface.fingerprint, interface.structural_fingerprint());
	assert_eq!(interface.exports[0].id, answer);

	let first_runtime = first
		.runtime_definition(
			project.clone(),
			main.clone(),
			answer.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let second_runtime = second
		.runtime_definition(
			project.clone(),
			main.clone(),
			answer.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(first_runtime, second_runtime);
	let RuntimePayload::NymphBody(body) = &first_runtime.payload else {
		panic!("answer must retain one exact checked body")
	};
	assert_eq!(body.stable.root.id, BodyNodeId(0));

	let first_hir = first
		.lower_interface_module_for_test(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let second_hir = second
		.lower_interface_module_for_test(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(first_hir, second_hir);
	assert_eq!(first_hir.module, answer.module);
	assert_eq!(first_hir.own_definitions, std::slice::from_ref(&answer));
	assert_eq!(first_hir.hir.funcs.len(), 1);
	assert_eq!(first_hir.hir.funcs[0].name, "$m0$answer");
	assert_eq!(
		first_hir.hir.funcs[0].body,
		nymph_hir::hir::HirExpr::Int(42)
	);

	let first_name = first
		.binding_name_for_test(
			project.clone(),
			main.clone(),
			answer.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let second_name = second
		.binding_name_for_test(project.clone(), main.clone(), answer, EntryMode::Library)
		.unwrap();
	assert_eq!(first_name, second_name);
	assert_eq!(first_name.as_str(), "$m0$answer");

	let first_emitted = first
		.emit_interface_project_for_test(project.clone(), main.clone(), EntryMode::Library)
		.unwrap();
	let second_emitted = second
		.emit_interface_project_for_test(project, main, EntryMode::Library)
		.unwrap();
	assert_eq!(first_emitted, second_emitted);
	assert!(
		first_emitted.module_sources["main"].contains("let $m0$answer = nymphCallable(function("),
		"{}",
		first_emitted.module_sources["main"]
	);
}

#[test]
fn independent_sessions_repeat_recovered_snapshot_and_emission_rejects_it() {
	let project = ProjectId::new("stable-recovered");
	let main = path("main");
	let source = "public func broken(value: Missing): Missing = value";
	let mut first = CompilerSession::without_builtin_sources();
	let mut second = CompilerSession::without_builtin_sources();
	for session in [&mut first, &mut second] {
		install(session, &project, "main", source);
	}

	let environment = |session: &CompilerSession| {
		session
			.module_environment(
				project.clone(),
				main.clone(),
				main.clone(),
				EntryMode::Library,
			)
			.unwrap()
	};
	let first_environment = environment(&first);
	let second_environment = environment(&second);
	assert_eq!(first_environment, second_environment);
	let ModuleEnvironment::Recovered(interface) = first_environment.as_ref() else {
		panic!("invalid source must produce the canonical recovered interface")
	};
	assert_eq!(interface.fingerprint, 979_268_332_040_006_257);
	assert_eq!(interface.fingerprint, interface.structural_fingerprint());
	assert!(matches!(
		interface.exports[0].return_type,
		Some(RecoveredInterfaceType::Poison)
	));

	for session in [&first, &second] {
		assert!(matches!(
			session.lower_interface_module_for_test(
				project.clone(),
				main.clone(),
				main.clone(),
				EntryMode::Library,
			),
			Err(StableModuleAssemblyError::RecoveredEnvironment { .. })
		));
		let diagnostics = session
			.emit_interface_module_for_test(
				project.clone(),
				main.clone(),
				main.clone(),
				EntryMode::Library,
			)
			.expect_err("stable emission must reject recovered environments");
		assert_eq!(diagnostics.len(), 1);
		assert_eq!(diagnostics[0].diag.code, "STABLE-EMISSION-RECOVERED");
	}
}

fn event_session() -> (CompilerSession, Arc<Mutex<Vec<SemanticQueryEvent>>>) {
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

fn definition_executed(events: &[SemanticQueryEvent], query: &str, id: &DefinitionId) -> bool {
	events
		.iter()
		.any(|event| event.query == query && event.definition.as_ref() == Some(id))
}

#[test]
fn body_and_header_edits_invalidate_only_the_stable_facts_they_change() {
	let (mut session, events) = event_session();
	let project = ProjectId::new("stable-invalidation");
	let unrelated_project = ProjectId::new("stable-unrelated");
	let leaf = path("leaf");
	let main = path("main");
	let unrelated = path("main");
	let value = definition("stable-invalidation", "leaf", "value");
	install(
		&mut session,
		&project,
		"leaf",
		"public func value(): int = 1",
	);
	install(
		&mut session,
		&project,
		"main",
		"import @/leaf with (value)\npublic func forwarded(): int = value()",
	);
	install(
		&mut session,
		&unrelated_project,
		"main",
		"public func separate(): int = 0",
	);

	let initial_interface = session
		.module_interface(
			project.clone(),
			main.clone(),
			leaf.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let initial_runtime = session
		.runtime_definition_consumer_for_test(
			project.clone(),
			main.clone(),
			value.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let _ = session
		.lower_runtime_definition(
			project.clone(),
			main.clone(),
			value.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let _ = session
		.analyze_module(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let _ = session
		.analyze_module(
			unrelated_project.clone(),
			unrelated.clone(),
			unrelated.clone(),
			EntryMode::Library,
		)
		.unwrap();
	events.lock().unwrap().clear();

	session.set_source(
		project.clone(),
		leaf.clone(),
		"public func value(): int = 2".into(),
		SourceVersion(2),
	);
	let body_interface = session
		.module_interface(
			project.clone(),
			main.clone(),
			leaf.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let body_runtime = session
		.runtime_definition_consumer_for_test(
			project.clone(),
			main.clone(),
			value.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let _ = session
		.lower_runtime_definition(
			project.clone(),
			main.clone(),
			value.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let _ = session.analyze_module(
		project.clone(),
		main.clone(),
		main.clone(),
		EntryMode::Library,
	);
	let _ = session.analyze_module(
		unrelated_project.clone(),
		unrelated.clone(),
		unrelated.clone(),
		EntryMode::Library,
	);
	assert_eq!(initial_interface, body_interface);
	assert_eq!(initial_interface.fingerprint, body_interface.fingerprint);
	assert_ne!(initial_runtime, body_runtime);
	{
		let observed = events.lock().unwrap();
		assert_eq!(count(&observed, "interface_module_analysis", "leaf"), 2);
		assert_eq!(count(&observed, "interface_module_analysis", "main"), 0);
		assert_eq!(
			count(
				&observed,
				"interface_module_analysis",
				"stable-unrelated:main"
			),
			0
		);
		assert!(definition_executed(
			&observed,
			"runtime_definition_consumer",
			&value
		));
		assert!(definition_executed(
			&observed,
			"lower_runtime_definition",
			&value
		));
	}
	events.lock().unwrap().clear();

	session.set_source(
		project.clone(),
		leaf.clone(),
		"public func value(): int = 2\npublic func added(): int = 3".into(),
		SourceVersion(3),
	);
	let header_interface = session
		.module_interface(project.clone(), main.clone(), leaf, EntryMode::Library)
		.unwrap();
	let header_runtime = session
		.runtime_definition_consumer_for_test(
			project.clone(),
			main.clone(),
			value.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let _ = session
		.lower_runtime_definition(
			project.clone(),
			main.clone(),
			value.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let _ = session.analyze_module(project, main.clone(), main, EntryMode::Library);
	let _ = session.analyze_module(
		unrelated_project,
		unrelated.clone(),
		unrelated,
		EntryMode::Library,
	);
	assert_ne!(body_interface, header_interface);
	assert_ne!(body_interface.fingerprint, header_interface.fingerprint);
	assert_eq!(body_runtime, header_runtime);
	let observed = events.lock().unwrap();
	assert_eq!(count(&observed, "interface_module_analysis", "leaf"), 2);
	assert_eq!(count(&observed, "interface_module_analysis", "main"), 1);
	assert_eq!(
		count(
			&observed,
			"interface_module_analysis",
			"stable-unrelated:main"
		),
		0
	);
	assert!(!definition_executed(
		&observed,
		"runtime_definition_consumer",
		&value
	));
	assert!(!definition_executed(
		&observed,
		"lower_runtime_definition",
		&value
	));
}
