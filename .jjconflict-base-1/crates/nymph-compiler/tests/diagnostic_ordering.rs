use nymph_compiler::check_project_library;

#[test]
fn parser_recovery_suppresses_cascades_but_keeps_independent_diagnostics_in_order() {
	let source = "42\nfunc independent(): int = true\n";
	let check = || check_project_library("main", &|module| (module == "main").then(|| source.into()));

	let first = check();
	let second = check();
	assert_eq!(first, second);
	let messages = first
		.iter()
		.map(|diagnostic| diagnostic.diag.message.as_str())
		.collect::<Vec<_>>();
	assert!(
		messages
			.first()
			.is_some_and(|message| message.contains("expected a declaration")),
		"parser root cause must remain first: {messages:?}"
	);
	assert!(
		messages
			.iter()
			.any(|message| message.contains("expected `int`, found `boolean`")),
		"independent type error was lost after parser recovery: {messages:?}"
	);
	assert_eq!(
		messages.len(),
		2,
		"recovery emitted a cascade: {messages:?}"
	);
}
