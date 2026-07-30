#![cfg(feature = "test-support")]

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nymph_compiler::project::{
	CompilerSession, GraphShape, ModulePath, ProjectId, SemanticQueryEvent, SourceVersion,
};
use nymph_sema::{DefinitionId, EntryMode};

fn project_events<'a>(
	events: &'a [SemanticQueryEvent],
	modules: &'a HashSet<String>,
) -> impl Iterator<Item = &'a SemanticQueryEvent> {
	events.iter().filter(|event| {
		event
			.module
			.as_ref()
			.is_some_and(|module| modules.contains(module))
	})
}

fn count(events: &[SemanticQueryEvent], query: &str, module: Option<&str>) -> usize {
	events
		.iter()
		.filter(|event| {
			event.query == query && module.is_none_or(|module| event.module.as_deref() == Some(module))
		})
		.count()
}

fn install_sources(
	session: &mut CompilerSession,
	project: &ProjectId,
	sources: &BTreeMap<String, String>,
	version: SourceVersion,
) {
	for (module, source) in sources {
		session.set_source(
			project.clone(),
			ModulePath::new(module).unwrap(),
			source.clone(),
			version,
		);
	}
}

fn run_root_value(mut js: String, root_symbol: &str) {
	js.push_str(&format!("\nconsole.log({root_symbol}().v);\n"));
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let path = std::env::temp_dir().join(format!(
		"nymph_incremental_{}_{unique}.mjs",
		std::process::id()
	));
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(js.as_bytes()).unwrap();
	let output = Command::new("node")
		.arg(&path)
		.env("NO_COLOR", "1")
		.env_remove("FORCE_COLOR")
		.output()
		.expect("run generated fixture under Node");
	let _ = std::fs::remove_file(path);
	assert!(
		output.status.success(),
		"node failed: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "0");
}

#[test]
fn tiny_check_records_cold_query_demand_and_fully_backdates() {
	let fixture = GraphShape::Single.generate();
	let events = Arc::new(Mutex::new(Vec::<SemanticQueryEvent>::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_detailed_event_callback_for_test(move |event| {
		sink.lock().unwrap().push(event)
	});
	let project = ProjectId::new("incremental-single-check");
	let entry = ModulePath::new(fixture.entry()).unwrap();
	install_sources(&mut session, &project, fixture.sources(), SourceVersion(1));

	events.lock().unwrap().clear();
	let diagnostics = session.check_project(project.clone(), entry.clone(), EntryMode::Library);
	assert!(diagnostics.is_empty(), "tiny check: {diagnostics:?}");
	let first_events = events.lock().unwrap().clone();
	assert_eq!(
		count(&first_events, "interface_module_analysis", Some("main")),
		1
	);
	assert_eq!(count(&first_events, "lower_interface_module", None), 0);
	assert_eq!(count(&first_events, "emitted_interface_module", None), 0);
	let ambient_counts = [
		"ambient_core_parse",
		"ambient_core_analysis",
		"ambient_core_headers",
		"ambient_core_environment",
		"ambient_core_interface",
		"ambient_core_diagnostics",
		"ambient_runtime_owner_artifacts",
	]
	.map(|query| (query, count(&first_events, query, None)));
	eprintln!("tiny check ambient-core query counts: {ambient_counts:?}");

	events.lock().unwrap().clear();
	let diagnostics = session.check_project(project, entry, EntryMode::Library);
	assert!(diagnostics.is_empty(), "warm tiny check: {diagnostics:?}");
	let second_events = events.lock().unwrap();
	assert!(
		second_events.is_empty(),
		"identical second check produced events: {second_events:#?}"
	);
}

fn assert_baseline(shape: GraphShape, project_name: &str, changed_leaf: &str) {
	let fixture = shape.generate();
	assert_eq!(fixture.unresolved_imports(), Vec::<String>::new());
	let mut sources = fixture.sources().clone();
	assert!(
		sources.contains_key(changed_leaf),
		"selected leaf must exist"
	);
	let leaf_source = sources.get_mut(changed_leaf).unwrap();
	let public_definition = leaf_source
		.find("public func value_")
		.expect("generated leaf must contain its public value definition");
	leaf_source.replace_range(public_definition.., "private func leaf_helper(): int = 0\n");
	let modules = sources.keys().cloned().collect::<HashSet<_>>();
	let module_count = modules.len();
	let events = Arc::new(Mutex::new(Vec::<SemanticQueryEvent>::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_detailed_event_callback_for_test(move |event| {
		sink.lock().unwrap().push(event)
	});
	let project = ProjectId::new(project_name);
	let entry = ModulePath::new(fixture.entry()).unwrap();
	install_sources(&mut session, &project, &sources, SourceVersion(1));

	let compiled = session
		.compile_interface_project_for_test(project.clone(), entry.clone(), EntryMode::Library)
		.unwrap_or_else(|diagnostics| panic!("fixture should compile cleanly: {diagnostics:?}"));
	assert!(!compiled.js.is_empty());
	run_root_value(compiled.js.clone(), &compiled.entry_symbol("root_value"));
	let initial = events.lock().unwrap().clone();
	let initial_project_events = project_events(&initial, &modules)
		.cloned()
		.collect::<Vec<_>>();
	assert_eq!(
		count(&initial_project_events, "interface_module_analysis", None),
		module_count
	);
	assert_eq!(
		count(&initial_project_events, "emitted_interface_module", None),
		module_count
	);
	assert!(count(&initial_project_events, "runtime_definition", None) >= module_count);
	let project_definitions = initial_project_events
		.iter()
		.filter_map(|event| event.definition.clone())
		.collect::<HashSet<DefinitionId>>();
	let target_definitions = initial_project_events
		.iter()
		.filter(|event| {
			event.query == "runtime_definition" && event.module.as_deref() == Some(changed_leaf)
		})
		.filter_map(|event| event.definition.clone())
		.collect::<HashSet<DefinitionId>>();
	assert_eq!(
		target_definitions.len(),
		1,
		"the adjusted leaf must query exactly its one private definition"
	);
	let target_definition = target_definitions.into_iter().next().unwrap();

	events.lock().unwrap().clear();
	session
		.compile_interface_project_for_test(project.clone(), entry.clone(), EntryMode::Library)
		.expect("identical fixture should compile cleanly");
	assert!(
		events.lock().unwrap().is_empty(),
		"an identical stable compile must be fully cached"
	);

	let changed_module = changed_leaf.to_string();
	let changed_source = sources.get_mut(&changed_module).unwrap();
	let marker = changed_source
		.find("private func leaf_helper(): int = ")
		.expect("selected leaf has one private helper body")
		+ "private func leaf_helper(): int".len();
	let value_start = marker + 3;
	let value_end = changed_source[value_start..].find('\n').unwrap() + value_start;
	changed_source.replace_range(value_start..value_end, "999");
	install_sources(&mut session, &project, &sources, SourceVersion(2));
	events.lock().unwrap().clear();
	session
		.compile_interface_project_for_test(project, entry, EntryMode::Library)
		.expect("body-edited fixture should compile cleanly");
	let edited = events.lock().unwrap();
	for query in [
		"interface_module_analysis",
		"interface_module_interface",
		"interface_module_environment",
		"lower_interface_module",
		"emitted_interface_module",
	] {
		assert_eq!(
			count(&edited, query, Some(&changed_module)),
			1,
			"{query}: {edited:#?}"
		);
	}
	for module in modules.iter().filter(|module| **module != changed_module) {
		assert_eq!(
			count(&edited, "interface_module_analysis", Some(module)),
			0,
			"unchanged module {module} was invalidated: {edited:#?}"
		);
	}
	for query in ["runtime_definition", "lower_runtime_definition"] {
		assert_eq!(
			edited
				.iter()
				.filter(|event| {
					event.query == query && event.definition.as_ref() == Some(&target_definition)
				})
				.count(),
			1,
			"{query}: {edited:#?}"
		);
		assert!(
			!edited.iter().any(|event| {
				event.query == query
					&& event.definition.as_ref().is_some_and(|definition| {
						project_definitions.contains(definition) && definition != &target_definition
					})
			}),
			"unchanged project definition reran {query}: {edited:#?}"
		);
	}
}

#[test]
fn wide_graph_has_stable_query_invalidation_and_output() {
	assert_baseline(
		GraphShape::Wide { leaves: 16 },
		"incremental-wide",
		"wide/leaf_000",
	);
}

#[test]
fn deep_graph_has_stable_query_invalidation_and_output() {
	assert_baseline(
		GraphShape::Deep { depth: 16 },
		"incremental-deep",
		"deep/level_015",
	);
}

#[test]
fn mixed_graph_has_stable_query_invalidation_and_output() {
	assert_baseline(
		GraphShape::Mixed { width: 4, depth: 4 },
		"incremental-mixed",
		"mixed/branch_000/level_003",
	);
}
