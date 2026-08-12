use nymph_compiler::{ReplInputStatus, ReplSession, repl_input_status};

fn submit(session: &mut ReplSession, source: &str) {
	let staged = session
		.stage(source)
		.unwrap_or_else(|error| panic!("{error:?}"));
	let retained = staged.modules().keys().cloned().collect::<Vec<_>>();
	session.commit(staged, &retained);
}

#[test]
fn completeness_uses_syntax_for_blocks_interpolation_and_comments() {
	for source in [
		"func choose(): int = {",
		"\"value ${if (true) {",
		"1 + /* open",
	] {
		assert_eq!(
			repl_input_status(source),
			ReplInputStatus::Incomplete,
			"{source:?}"
		);
	}
	for source in [
		"func choose(): int = { 1 }",
		"\"value ${if (true) { 1 } else { 0 }}\"",
		"1 + /* closed */ 1",
		"let = 1",
		"\"${let}\"",
		"\"${1 2}\"",
	] {
		assert_eq!(repl_input_status(source), ReplInputStatus::Complete);
	}
}

#[test]
fn compiler_generated_repl_identifier_prefix_is_reserved() {
	let session = ReplSession::loose();
	assert!(session.stage("let __nymph_repl_marker_0 = 1").is_err());
	assert_eq!(session.committed_submissions(), 0);
}

#[test]
fn imported_names_cannot_collide_with_compiler_generated_repl_identifiers() {
	let session = ReplSession::new(|module| match module {
		"dep" => Some("public let __nymph_repl_import_probe_0 = 1".to_string()),
		"enum_dep" => Some("public enum E { __nymph_repl_marker_0 }".to_string()),
		_ => None,
	});
	assert!(session.stage("import @/dep").is_err());
	assert!(
		session
			.stage("import @/dep with (__nymph_repl_import_probe_0)")
			.is_err()
	);
	assert!(session.stage("import @/enum_dep with (E)").is_err());
	assert!(
		session
			.stage("enum Local { __nymph_repl_marker_0 }")
			.is_err()
	);
}

#[test]
fn declarations_values_and_shadowing_persist_lexically() {
	let mut session = ReplSession::loose();
	submit(&mut session, "let x = 1");
	submit(&mut session, "let old = x");
	submit(&mut session, "let x = 2");
	submit(&mut session, "#(x, old)");
	assert_eq!(session.committed_submissions(), 4);
}

#[test]
fn syntax_type_and_runtime_failures_leave_last_good_state() {
	let mut session = ReplSession::loose();
	submit(&mut session, "let stable = 7");
	submit(&mut session, "let old = stable");
	submit(&mut session, "let stable = 9");

	assert!(session.stage("let = 1").is_err());
	assert_eq!(session.committed_submissions(), 3);
	assert!(session.stage("let wrong: int = true").is_err());
	assert_eq!(session.committed_submissions(), 3);

	submit(&mut session, "func recurse(): int = recurse()");
	let staged = session.stage("let failed = recurse()").unwrap();
	drop(staged);
	assert_eq!(session.committed_submissions(), 4);
	submit(&mut session, "#(stable, old)");
	assert!(session.stage("failed").is_err());
}

#[test]
fn debug_rendering_project_imports_and_embedded_std_use_the_project_pipeline() {
	let mut session = ReplSession::new(|module| match module {
		"dep" => Some("public func answer(): int = 42".to_string()),
		_ => None,
	});
	submit(&mut session, "import @/dep with (answer)");
	submit(&mut session, "answer()");
	submit(&mut session, "import std/io with (println)");
	submit(&mut session, "#[\"a\", \"b\"]");
	submit(
		&mut session,
		"struct Marker(v: int) { impl Debug { func debug(): string = \"<marker>\" } }",
	);
	submit(&mut session, "Marker(v = 1)");
}

#[test]
fn imports_and_declarations_shadow_each_other_latest_first() {
	let loader = |module: &str| (module == "dep").then(|| "public let answer = 42".to_string());
	let mut declaration_wins = ReplSession::new(loader);
	submit(&mut declaration_wins, "import @/dep with (answer)");
	submit(&mut declaration_wins, "let answer = 7");
	submit(&mut declaration_wins, "answer");

	let mut import_wins = ReplSession::new(loader);
	submit(&mut import_wins, "let answer = 7");
	submit(&mut import_wins, "import @/dep with (answer)");
	submit(&mut import_wins, "answer");

	let mut private_is_repl_visible = ReplSession::loose();
	submit(&mut private_is_repl_visible, "private let secret = 9");
	submit(&mut private_is_repl_visible, "secret");
}

#[test]
fn shadowing_one_selected_import_keeps_its_siblings_and_namespace() {
	let loader =
		|module: &str| (module == "dep").then(|| "public let a = 1\npublic let b = 2".to_string());
	let mut session = ReplSession::new(loader);
	submit(&mut session, "import @/dep with (a, b)");
	submit(&mut session, "let a = 9");
	submit(&mut session, "#(a, b, dep.b)");
}

#[test]
fn a_loaded_project_dependency_cannot_silently_change_under_the_worker() {
	let source = std::sync::Arc::new(std::sync::Mutex::new(
		"public func answer(): int = 1".to_string(),
	));
	let loader_source = source.clone();
	let mut session = ReplSession::new(move |module| {
		(module == "dep").then(|| loader_source.lock().unwrap().clone())
	});
	submit(&mut session, "import @/dep with (answer)");
	*source.lock().unwrap() = "public func answer(): int = 2".to_string();
	let error = match session.stage("answer()") {
		Ok(_) => panic!("a changed loaded dependency must be rejected"),
		Err(error) => error,
	};
	let diagnostics = error.diagnostics().unwrap().0;
	assert!(
		diagnostics.iter().any(|item| item
			.diag
			.message
			.contains("loaded project module `dep` changed")),
		"{diagnostics:?}"
	);
}
