use nymph_sema::check_module;
use nymph_syntax::parse_module;

fn messages(source: &str) -> Vec<String> {
	let source = format!("enum Option<T> {{ Some(value: T), None }}\n{source}");
	let parsed = parse_module(&source, "test");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	check_module(&parsed.tree)
		.diags
		.into_iter()
		.map(|diagnostic| diagnostic.message.to_string())
		.collect()
}

#[test]
fn jumps_require_a_lexically_enclosing_loop() {
	for (source, keyword) in [
		("func f(): void = { break }", "break"),
		("func f(): void = { continue }", "continue"),
	] {
		let found = messages(source);
		assert!(
			found.iter().any(|message| message.contains(keyword)),
			"{found:?}"
		);
	}
}

#[test]
fn callable_is_a_loop_control_boundary() {
	let found = messages("func f(): void = while (true) { let g = () -> break }");
	assert!(
		found.iter().any(|message| message.contains("break")),
		"{found:?}"
	);
}

#[test]
fn one_loop_cannot_mix_bare_and_valued_breaks() {
	let found =
		messages("func f(flag: boolean) = while (true) { if (flag) { break } else { break 1 } }");
	assert!(
		found.iter().any(|message| message.contains("cannot mix")),
		"{found:?}"
	);
}

#[test]
fn nested_loop_breaks_do_not_determine_the_outer_result() {
	let found = messages("func f(): void = while (true) { while (true) { break 1 } continue }");
	assert!(found.is_empty(), "{found:?}");
}

#[test]
fn loop_result_contracts_type_check() {
	for source in [
		"func no_break(): void = while (false) {}",
		"func bare(): Option<#()> = while (false) { break }",
		"func valued(): Option<int> = while (false) { if (false) { break 1 } break 2 }",
		"func lexical(): Option<int> = while (false) { break 1 }",
		"func branch(flag: boolean): Option<int> = while (true) { let value = if (flag) { break 1 } else { 2 } break value }",
		"func arm(value: int): Option<int> = while (true) { let found = match (value) { 0 -> break 1, _ -> 2 } break found }",
		"func nested_break(): Option<int> = while (true) { break (break 1) }",
		"func all_arms(value: boolean): Option<int> = while (true) { 1 + match (value) { true -> break 1, false -> break 2 } }",
		"func guarded_arm(value: int): Option<int> = while (true) { 1 + match (value) { 0 if true -> break 1, _ -> break 2 } }",
		"func short_circuit(): Option<int> = while (true) { false && break 1\ntrue || break 2\ntrue && break 3 }",
		"func prefix(): Option<int> = while (true) { -(break 1) }",
		"func callee(): Option<int> = while (true) { (break 1)() }",
		"func member(): Option<int> = while (true) { (break 1).field }",
		"func index(): Option<int> = while (true) { #[0][break 1] }",
		"func cast(): Option<int> = while (true) { (break 1) as int }",
	] {
		let found = messages(source);
		assert!(found.is_empty(), "{source}: {found:?}");
	}
}

#[test]
fn loop_headers_do_not_target_an_outer_loop_or_ice() {
	for source in [
		"func f(): void = while (true) { while (break 1) {} }",
		"func f(): void = while (true) { for (_ in break 1) {} }",
	] {
		let found = messages(source);
		assert!(
			found
				.iter()
				.any(|message| message.contains("only valid inside a loop")),
			"{source}: {found:?}"
		);
	}
}

#[test]
fn valued_breaks_must_unify() {
	let found =
		messages("func f(flag: boolean) = while (true) { if (flag) { break 1 } else { break true } }");
	assert!(
		found.iter().any(|message| message.contains("mismatched")),
		"{found:?}"
	);
}
