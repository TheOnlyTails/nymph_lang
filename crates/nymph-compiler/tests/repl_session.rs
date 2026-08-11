use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use nymph_compiler::{ReplInputStatus, ReplSession, repl_input_status};

fn run_node(js: &str) -> std::process::Output {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let id = COUNTER.fetch_add(1, Ordering::Relaxed);
	let path = std::env::temp_dir().join(format!(
		"nymph_repl_session_{}_{id}.mjs",
		std::process::id()
	));
	std::fs::write(&path, js).unwrap();
	let output = Command::new("node").arg(&path).output().unwrap();
	let _ = std::fs::remove_file(path);
	output
}

fn submit(session: &mut ReplSession, source: &str) -> String {
	let staged = session
		.stage(source)
		.unwrap_or_else(|error| panic!("{error:?}"));
	let output = run_node(&staged.execution_js());
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	session.commit(staged);
	String::from_utf8_lossy(&output.stdout).into_owned()
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
	] {
		assert_eq!(repl_input_status(source), ReplInputStatus::Complete);
	}
}

#[test]
fn declarations_values_and_shadowing_persist_lexically() {
	let mut session = ReplSession::loose();
	assert_eq!(submit(&mut session, "let x = 1"), "");
	assert_eq!(submit(&mut session, "let old = x"), "");
	assert_eq!(submit(&mut session, "let x = 2"), "");
	assert_eq!(submit(&mut session, "#(x, old)"), "#(2, 1)\n");
	assert_eq!(session.committed_submissions(), 4);
}

#[test]
fn syntax_type_and_runtime_failures_leave_last_good_state() {
	let mut session = ReplSession::loose();
	submit(&mut session, "let stable = 7");

	assert!(session.stage("let = 1").is_err());
	assert_eq!(session.committed_submissions(), 1);
	assert!(session.stage("let wrong: int = true").is_err());
	assert_eq!(session.committed_submissions(), 1);

	submit(&mut session, "func recurse(): int = recurse()");
	let staged = session.stage("let failed = recurse()").unwrap();
	assert!(!run_node(&staged.execution_js()).status.success());
	drop(staged);
	assert_eq!(session.committed_submissions(), 2);
	assert_eq!(submit(&mut session, "stable"), "7\n");
	assert!(session.stage("failed").is_err());
}

#[test]
fn debug_rendering_project_imports_and_embedded_std_use_the_project_pipeline() {
	let mut session = ReplSession::new(|module| match module {
		"dep" => Some("public func answer(): int = 42".to_string()),
		_ => None,
	});
	submit(&mut session, "import @/dep with (answer)");
	assert_eq!(submit(&mut session, "answer()"), "42\n");
	submit(&mut session, "import std/io with (println)");
	assert_eq!(submit(&mut session, "#[\"a\", \"b\"]"), "#[\"a\", \"b\"]\n");
	submit(
		&mut session,
		"struct Marker(v: int) { impl Debug { func debug(): string = \"<marker>\" } }",
	);
	assert_eq!(submit(&mut session, "Marker(v = 1)"), "<marker>\n");
}

#[test]
fn imports_and_declarations_shadow_each_other_latest_first() {
	let loader = |module: &str| (module == "dep").then(|| "public let answer = 42".to_string());
	let mut declaration_wins = ReplSession::new(loader);
	submit(&mut declaration_wins, "import @/dep with (answer)");
	submit(&mut declaration_wins, "let answer = 7");
	assert_eq!(submit(&mut declaration_wins, "answer"), "7\n");

	let mut import_wins = ReplSession::new(loader);
	submit(&mut import_wins, "let answer = 7");
	submit(&mut import_wins, "import @/dep with (answer)");
	assert_eq!(submit(&mut import_wins, "answer"), "42\n");

	let mut private_is_repl_visible = ReplSession::loose();
	submit(&mut private_is_repl_visible, "private let secret = 9");
	assert_eq!(submit(&mut private_is_repl_visible, "secret"), "9\n");
}
