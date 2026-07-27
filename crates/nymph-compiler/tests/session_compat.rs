use std::sync::{Arc, Mutex};

use nymph_compiler::{CompilerSession, ModulePath, ProjectId, SourceVersion};
use nymph_sema::EntryMode;
use rustc_hash::FxHashMap;

fn path(value: &str) -> ModulePath {
	ModulePath::new(value).unwrap()
}

fn session(files: &FxHashMap<&str, &str>) -> (CompilerSession, ProjectId) {
	let project = ProjectId::new("compat");
	let mut session = CompilerSession::new();
	for (module, source) in files {
		session.set_source(
			project.clone(),
			path(module),
			(*source).to_string(),
			SourceVersion(1),
		);
	}
	(session, project)
}

#[test]
fn module_analysis_type_at_handles_complex_pattern_binders() {
	for (source, needle, expected) in [
		(
			"enum Shape { Circle(radius: int) }\nfunc f(s: Shape): int = match (s) { Circle(radius) -> radius }",
			"Circle(radius) ->",
			"Shape.Circle(radius: int)",
		),
		(
			"enum Shape { Circle(radius: int) }\nfunc f(s: Shape): int = match (s) { Circle(radius) -> radius }",
			"radius) ->",
			"int",
		),
		(
			"func main(): int = {\n  let xs = #[1, 2, 3]\n  for (x in xs) { x }\n  0\n}",
			"x in xs",
			"int",
		),
	] {
		let files = FxHashMap::from_iter([("main", source)]);
		let (session, project) = session(&files);
		let diagnostics = session.check_project(project.clone(), path("main"), EntryMode::Library);
		assert!(
			diagnostics.is_empty(),
			"unexpected diagnostics: {diagnostics:?}"
		);
		let analysis = session
			.analyze_module(project, path("main"), path("main"), EntryMode::Library)
			.unwrap();
		let offset = source.find(needle).unwrap() + usize::from(!needle.starts_with("x in"));
		assert_eq!(analysis.type_at(offset).as_deref(), Some(expected));
	}
}

#[test]
fn session_checks_and_compiles_a_representative_project() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/dep with (answer)\nfunc main(): void = {}\nfunc value(): int = answer()",
		),
		("dep", "public func answer(): int = 42"),
	]);
	let (session, project) = session(&files);
	assert!(
		session
			.check_project(project.clone(), path("main"), EntryMode::Entry)
			.is_empty()
	);
	let compiled = session
		.compile_project(project, path("main"), EntryMode::Entry)
		.unwrap();
	assert_eq!(compiled.entry_main, "main");
	assert_eq!(compiled.entry_symbol("value"), "$m1$value");
	assert!(compiled.js.contains("function main"));
}

fn assert_session_emission(files: &FxHashMap<&str, &str>) {
	let (session, project) = session(files);
	let (sources, entry_tag) = session
		.inspect_emitted_project(project.clone(), path("main"), EntryMode::Entry)
		.expect("session module emission should succeed");
	assert!(sources.contains_key("main"));
	let compiled = session
		.compile_project(project, path("main"), EntryMode::Entry)
		.expect("session compilation should succeed");
	assert_eq!(compiled.entry_tag, entry_tag);
	assert_eq!(
		compiled.entry_symbol("result"),
		format!("$m{entry_tag}$result")
	);
	assert!(!compiled.js.is_empty());
}

#[test]
fn representative_runtime_projects_emit_and_bundle() {
	for files in [
		FxHashMap::from_iter([
			(
				"main",
				"import @/math with (double)\nfunc main(): void = {}\nfunc result(): int = double(21)",
			),
			("math", "public func double(value: int): int = value * 2"),
		]),
		FxHashMap::from_iter([
			(
				"main",
				"import @/left with (values)\nimport @/right with (other)\nfunc main(): void = {}\nfunc result(): int = values()[0] + other()[0]",
			),
			("left", "public func values(): #[int] = #[1, 2]"),
			("right", "public func other(): #[int] = #[3, 4]"),
		]),
		FxHashMap::from_iter([
			(
				"main",
				"import @/left with (left_value)\nimport @/right with (right_value)\nfunc main(): void = {}\nfunc result(): int = left_value() + right_value()",
			),
			(
				"owner",
				"public enum Boxed<T> { Value(value: T) }\nimpl<T> Boxed<T> { namespace func wrap(value: T): Boxed<T> = Boxed.Value(value = value) }",
			),
			(
				"left",
				"import @/owner with (Boxed)\npublic func left_value(): int = match (Boxed.wrap(2)) { Value(value) -> value }",
			),
			(
				"right",
				"import @/owner with (Boxed)\npublic func right_value(): int = match (Boxed.wrap(3)) { Value(value) -> value }",
			),
		]),
		FxHashMap::from_iter([(
			"main",
			"import std/io with (println)\nfunc main(): void = {}\nfunc result(): void = println(1)",
		)]),
	] {
		assert_session_emission(&files);
	}
}

#[test]
fn unchanged_repeat_executes_no_stable_query_body() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		256,
	);
	let project = ProjectId::new("repeat");
	session.set_source(
		project.clone(),
		path("main"),
		"func main(): void = {}".into(),
		SourceVersion(1),
	);
	assert!(
		session
			.compile_project(project.clone(), path("main"), EntryMode::Entry)
			.is_ok()
	);
	assert!(
		events
			.lock()
			.unwrap()
			.iter()
			.any(|event| event == "interface_module_analysis"),
		"initial check did not execute interface analysis"
	);
	events.lock().unwrap().clear();
	assert!(
		session
			.compile_project(project, path("main"), EntryMode::Entry)
			.is_ok()
	);
	assert!(
		events.lock().unwrap().is_empty(),
		"unexpected query executions: {:?}",
		events.lock().unwrap()
	);
}

#[test]
fn session_reports_entry_library_binding_and_checker_errors() {
	let cases = [
		(
			EntryMode::Entry,
			FxHashMap::from_iter([("main", "func value(): int = missing")]),
		),
		(
			EntryMode::Library,
			FxHashMap::from_iter([("main", "let value: int = true")]),
		),
	];
	for (mode, files) in cases {
		let (session, project) = session(&files);
		assert!(
			!session
				.check_project(project, path("main"), mode)
				.is_empty()
		);
	}
}

#[test]
fn semantic_error_is_reported_without_emission_work() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		256,
	);
	let project = ProjectId::new("gate");
	session.set_source(
		project.clone(),
		path("main"),
		"func bad(): int = true".into(),
		SourceVersion(1),
	);
	let diagnostics = session.check_project(project, path("main"), EntryMode::Library);
	assert!(!diagnostics.is_empty());
	let events = events.lock().unwrap();
	assert!(
		events
			.iter()
			.any(|event| event == "interface_module_analysis")
	);
	assert!(
		!events
			.iter()
			.any(|event| event == "emitted_interface_project")
	);
}

#[test]
fn graph_error_short_circuits_all_analysis_query_bodies() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		256,
	);
	let project = ProjectId::new("graph-gate");
	session.set_source(
		project.clone(),
		path("main"),
		"import @/missing".into(),
		SourceVersion(1),
	);
	let diagnostics = session.check_project(project, path("main"), EntryMode::Library);
	assert_eq!(diagnostics[0].diag.code, "IMPORT-UNRESOLVED");
	assert!(
		!events
			.lock()
			.unwrap()
			.iter()
			.any(|event| event == "compat_module_analysis")
	);
}

#[test]
fn public_compile_executes_stable_query_bodies_and_no_compatibility_query() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		256,
	);
	let project = ProjectId::new("compile-events");
	session.set_source(
		project.clone(),
		path("main"),
		"func main(): void = {}".into(),
		SourceVersion(1),
	);
	session
		.compile_project(project, path("main"), EntryMode::Entry)
		.unwrap();
	let events = events.lock().unwrap();
	for expected in [
		"interface_module_analysis",
		"emitted_interface_project",
		"compiled_interface_project",
	] {
		assert!(
			events.iter().any(|event| event == expected),
			"missing {expected}: {events:?}"
		);
	}
	assert!(
		!events.iter().any(|event| event.starts_with("compat_")),
		"compatibility query executed: {events:?}"
	);
}

#[test]
fn check_error_prevents_lower_emit_and_bundle_work() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		256,
	);
	let project = ProjectId::new("check-gate");
	session.set_source(
		project.clone(),
		path("main"),
		"func main(): void = {}\nfunc bad(): int = true".into(),
		SourceVersion(1),
	);
	assert!(
		session
			.compile_project(project, path("main"), EntryMode::Entry)
			.is_err()
	);
	let observed = events.lock().unwrap().clone();
	assert!(
		observed
			.iter()
			.any(|name| name == "interface_module_analysis")
	);
	for forbidden in ["emitted_interface_module"] {
		assert!(
			!observed.iter().any(|name| name == forbidden),
			"{observed:?}"
		);
	}
}

#[test]
fn reachable_edit_reruns_the_compile_chain_and_changes_output() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		256,
	);
	let project = ProjectId::new("edit");
	for (module, source) in [
		(
			"main",
			"import @/dep with (answer)\nfunc main(): void = {}\nfunc result(): int = answer()",
		),
		("dep", "public func answer(): int = 1"),
	] {
		session.set_source(
			project.clone(),
			path(module),
			source.into(),
			SourceVersion(1),
		);
	}
	let before = session
		.compile_project(project.clone(), path("main"), EntryMode::Entry)
		.unwrap();
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		path("dep"),
		"public func answer(): int = 2".into(),
		SourceVersion(2),
	);
	let after = session
		.compile_project(project, path("main"), EntryMode::Entry)
		.unwrap();
	assert_ne!(before.js, after.js);
	let observed = events.lock().unwrap().clone();
	for expected in [
		"parse",
		"direct_imports",
		"project_graph",
		"interface_module_analysis",
		"emitted_interface_module",
		"emitted_interface_project",
		"compiled_interface_project",
	] {
		assert!(
			observed.iter().any(|name| name == expected),
			"missing {expected}: {observed:?}"
		);
	}
}

#[test]
fn stable_keys_isolate_projects_roots_and_modes() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |name| sink.lock().unwrap().push(name.to_string()),
		256,
	);
	let source = "func main(): void = {}\nfunc result(): int = 1";
	for project in [ProjectId::new("a"), ProjectId::new("b")] {
		for root in ["first", "second"] {
			session.set_source(project.clone(), path(root), source.into(), SourceVersion(1));
		}
	}
	for project in [ProjectId::new("a"), ProjectId::new("b")] {
		for root in ["first", "second"] {
			session
				.compile_project(project.clone(), path(root), EntryMode::Entry)
				.unwrap();
		}
	}
	assert_eq!(
		events
			.lock()
			.unwrap()
			.iter()
			.filter(|name| name.as_str() == "compiled_interface_project")
			.count(),
		4
	);
	events.lock().unwrap().clear();
	let entry = session
		.inspect_emitted_project(ProjectId::new("a"), path("first"), EntryMode::Entry)
		.unwrap();
	let library = session
		.inspect_emitted_project(ProjectId::new("a"), path("first"), EntryMode::Library)
		.unwrap();
	assert_ne!(entry.0["first"], library.0["first"]);
	assert!(
		events
			.lock()
			.unwrap()
			.iter()
			.any(|name| name == "emitted_interface_project")
	);
}
