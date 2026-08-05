use std::sync::{Arc, Mutex};

use nymph_compiler::{CompilerSession, ModulePath, ProjectId, SourceVersion};
use nymph_sema::EntryMode;

fn path(value: &str) -> ModulePath {
	ModulePath::new(value).unwrap()
}

#[test]
fn module_paths_are_canonical() {
	for invalid in ["/main", "a/../b", "main.nym"] {
		assert!(ModulePath::new(invalid).is_err(), "accepted {invalid}");
	}
}

#[test]
fn projects_and_source_lifecycle_are_isolated() {
	let mut session = CompilerSession::new();
	let a = ProjectId::new("workspace-a");
	let b = ProjectId::new("workspace-b");
	session.set_source(
		a.clone(),
		path("main"),
		"import @/dep".into(),
		SourceVersion(1),
	);
	session.set_source(a.clone(), path("dep"), "let a = 1".into(), SourceVersion(1));
	session.set_source(
		b.clone(),
		path("main"),
		"let b = 2".into(),
		SourceVersion(4),
	);
	assert_eq!(
		session.graph_order(a.clone(), path("main"), EntryMode::Library),
		[path("dep"), path("main")]
	);
	assert_eq!(
		session.graph_order(b, path("main"), EntryMode::Library),
		[path("main")]
	);
	session.remove_source(a.clone(), path("dep"));
	let diagnostics = session.check_project(a.clone(), path("main"), EntryMode::Library);
	assert_eq!(diagnostics[0].module, "main");
	session.set_source(a.clone(), path("dep"), "let a = 3".into(), SourceVersion(2));
	assert!(
		session
			.check_project(a, path("main"), EntryMode::Library)
			.is_empty()
	);
}

#[test]
fn graph_rejects_syntax_cycles_and_ignores_unreachable_errors() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("p");
	session.set_source(
		project.clone(),
		path("main"),
		"import @/a".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		path("a"),
		"import @/main".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		path("unrelated"),
		"let =".into(),
		SourceVersion(1),
	);
	let diagnostics = session.check_project(project.clone(), path("main"), EntryMode::Library);
	assert!(
		diagnostics
			.iter()
			.any(|item| item.diag.code == "IMPORT-CYCLE")
	);
	session.set_source(project.clone(), path("a"), "let =".into(), SourceVersion(2));
	let diagnostics = session.check_project(project, path("main"), EntryMode::Library);
	assert!(diagnostics.iter().any(|item| item.diag.is_error()));
}

#[test]
fn versions_do_not_invalidate_and_tombstones_rebuild() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		2,
	);
	let project = ProjectId::new("p");
	session.set_source(
		project.clone(),
		path("main"),
		"import @/dep".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		path("dep"),
		"let x = 1".into(),
		SourceVersion(1),
	);
	assert!(
		session
			.check_project(project.clone(), path("main"), EntryMode::Library)
			.is_empty()
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		path("dep"),
		"let x = 1".into(),
		SourceVersion(2),
	);
	assert!(
		session
			.check_project(project.clone(), path("main"), EntryMode::Library)
			.is_empty()
	);
	assert!(events.lock().unwrap().is_empty());
	session.set_source(
		project.clone(),
		path("dep"),
		"let x = 2".into(),
		SourceVersion(3),
	);
	assert!(
		session
			.check_project(project.clone(), path("main"), EntryMode::Library)
			.is_empty()
	);
	assert_eq!(
		events
			.lock()
			.unwrap()
			.iter()
			.filter(|name| name.as_str() == "parse")
			.count(),
		1
	);
	session.remove_source(project.clone(), path("dep"));
	assert_eq!(session.tombstone_count(), 1);
	session.set_source(
		project.clone(),
		path("other"),
		"let y = 1".into(),
		SourceVersion(8),
	);
	session.remove_source(project.clone(), path("other"));
	assert_eq!(session.tombstone_count(), 0);
	assert_eq!(
		session.source_version(project.clone(), path("main")),
		Some(SourceVersion(1))
	);
	assert!(session.has_source(project, path("main")));
}

#[test]
fn absent_entry_is_a_graph_error_even_for_an_absent_project() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		2,
	);
	let project = ProjectId::new("absent");
	let diagnostics = session.check_project(project.clone(), path("main"), EntryMode::Library);
	assert_eq!(diagnostics.len(), 1);
	assert_eq!(diagnostics[0].module, "main");
	assert_eq!(diagnostics[0].diag.code, "IMPORT-UNRESOLVED");
	events.lock().unwrap().clear();
	let diagnostics = session.check_project(project.clone(), path("main"), EntryMode::Library);
	assert_eq!(diagnostics[0].diag.code, "IMPORT-UNRESOLVED");
	assert_eq!(
		events
			.lock()
			.unwrap()
			.iter()
			.filter(|event| event.as_str() == "project_graph")
			.count(),
		0,
		"an absent project must reuse its stable ProjectInput"
	);

	session.set_source(
		project.clone(),
		path("main"),
		"let value = 1".into(),
		SourceVersion(1),
	);
	events.lock().unwrap().clear();
	assert!(
		session
			.check_project(project.clone(), path("main"), EntryMode::Library)
			.is_empty()
	);
	assert_eq!(
		session.graph_order(project, path("main"), EntryMode::Library),
		[path("main")]
	);
	assert_eq!(
		events
			.lock()
			.unwrap()
			.iter()
			.filter(|event| event.as_str() == "project_graph")
			.count(),
		1,
		"adding the first source must update the registered empty ProjectInput"
	);
}

#[test]
fn builtin_imports_are_traversed_but_not_exposed_as_project_paths() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		2,
	);
	let project = ProjectId::new("p");
	session.set_source(
		project.clone(),
		path("main"),
		"import std/io".into(),
		SourceVersion(7),
	);
	assert!(
		session
			.check_project(project.clone(), path("main"), EntryMode::Library)
			.is_empty()
	);
	assert_eq!(
		session.graph_order(project.clone(), path("main"), EntryMode::Library),
		[path("main")]
	);
	let first_parse_count = events
		.lock()
		.unwrap()
		.iter()
		.filter(|event| event.as_str() == "parse")
		.count();
	assert!(
		first_parse_count >= 2,
		"expected user and builtin parse executions"
	);
	events.lock().unwrap().clear();
	assert!(
		session
			.check_project(project.clone(), path("main"), EntryMode::Library)
			.is_empty()
	);
	assert!(
		events.lock().unwrap().is_empty(),
		"memoized builtin traversal was not reused"
	);

	// Force reclamation and prove both the user version and rebuilt builtin inputs survive.
	for name in ["dead-a", "dead-b"] {
		session.set_source(
			project.clone(),
			path(name),
			"let x = 1".into(),
			SourceVersion(1),
		);
		session.remove_source(project.clone(), path(name));
	}
	assert_eq!(
		session.source_version(project.clone(), path("main")),
		Some(SourceVersion(7))
	);
	events.lock().unwrap().clear();
	assert!(
		session
			.check_project(project, path("main"), EntryMode::Library)
			.is_empty()
	);
	assert!(
		events
			.lock()
			.unwrap()
			.iter()
			.filter(|event| event.as_str() == "parse")
			.count()
			>= 2
	);
}

#[test]
fn branching_graph_order_is_deterministic_across_repeated_requests() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("p");
	for (name, source) in [
		("main", "import @/b\nimport @/a"),
		("a", "import @/shared"),
		("b", "import @/shared"),
		("shared", "let value = 1"),
	] {
		session.set_source(project.clone(), path(name), source.into(), SourceVersion(1));
	}
	let expected = vec![path("shared"), path("b"), path("a"), path("main")];
	assert_eq!(
		session.graph_order(project.clone(), path("main"), EntryMode::Library),
		expected
	);
	assert_eq!(
		session.graph_order(project, path("main"), EntryMode::Library),
		expected
	);
}

#[test]
fn dependency_relations_deduplicate_repeated_imports_in_source_order() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("deduplicated-dependencies");
	for (module, source) in [
		(
			"main",
			"import @/b\nimport @/a\nimport @/b\nimport @/missing",
		),
		("a", "let a = 1"),
		("b", "let b = 1"),
	] {
		session.set_source(
			project.clone(),
			path(module),
			source.into(),
			SourceVersion(1),
		);
	}

	assert_eq!(
		session.direct_dependencies(
			project.clone(),
			path("main"),
			path("main"),
			EntryMode::Library,
		),
		[path("b"), path("a")]
	);
	assert_eq!(
		session.reverse_importers(project, path("main"), path("b"), EntryMode::Library),
		[path("main")]
	);
}

#[test]
fn dependency_relations_track_import_edits_without_reparsing_unrelated_modules() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		256,
	);
	let project = ProjectId::new("dependency-relations");
	for (module, source) in [
		("main", "import @/left"),
		("left", "import @/leaf"),
		("right", "import @/leaf"),
		("leaf", "let value = 1"),
		("unrelated", "let untouched = 1"),
	] {
		session.set_source(
			project.clone(),
			path(module),
			source.into(),
			SourceVersion(1),
		);
	}

	assert_eq!(
		session.direct_dependencies(
			project.clone(),
			path("main"),
			path("main"),
			EntryMode::Library,
		),
		[path("left")]
	);
	assert_eq!(
		session.reverse_importers(
			project.clone(),
			path("main"),
			path("leaf"),
			EntryMode::Library,
		),
		[path("left")]
	);

	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		path("main"),
		"import @/right".into(),
		SourceVersion(2),
	);
	assert_eq!(
		session.direct_dependencies(
			project.clone(),
			path("main"),
			path("main"),
			EntryMode::Library,
		),
		[path("right")]
	);
	assert_eq!(
		session.reverse_importers(project, path("main"), path("leaf"), EntryMode::Library,),
		[path("right")]
	);
	let observed = events.lock().unwrap();
	for (query, expected) in [("parse", 2), ("direct_imports", 2), ("project_graph", 1)] {
		assert_eq!(
			observed
				.iter()
				.filter(|event| event.as_str() == query)
				.count(),
			expected,
			"import edit should execute {query} only for the changed root and newly reachable branch: {observed:?}"
		);
	}
	assert!(
		!observed
			.iter()
			.any(|event| event == "interface_module_analysis"),
		"dependency inspection must not trigger semantic checking: {observed:?}"
	);
}
