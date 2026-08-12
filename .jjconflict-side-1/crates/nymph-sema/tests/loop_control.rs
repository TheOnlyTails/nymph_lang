use nymph_sema::check_module;
use nymph_syntax::parse_module;

fn messages(source: &str) -> Vec<String> {
	let source = format!(
		"enum Option<T> {{ Some(value: T), None }}\nenum Result<T, E> {{ Ok(value: T), Error(error: E) }}\n{source}"
	);
	let parsed = parse_module(&source, "test");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	check_module(&parsed.tree)
		.diags
		.into_iter()
		.map(|diagnostic| diagnostic.message.to_string())
		.collect()
}

#[test]
fn question_propagation_checks_family_target_and_result_error() {
	for source in [
		"func option(o: Option<int>): Option<string> = { let value = o? Some(\"${value}\") }",
		"func result(r: Result<int, string>): Result<boolean, string> = { let value = r? Ok(value > 0) }",
		"func labeled(o: Option<int>): Option<string> = target@{ let value = o?@target Some(\"${value}\") }",
	] {
		let found = messages(source);
		assert!(found.is_empty(), "{source}: {found:?}");
	}

	for source in [
		"func mixed(o: Option<int>): Result<int, string> = { let value = o? Ok(value) }",
		"func mixed(r: Result<int, string>): Option<int> = { let value = r? Some(value) }",
	] {
		let found = messages(source);
		assert!(
			found
				.iter()
				.any(|message| message.contains("cannot propagate")),
			"{source}: {found:?}"
		);
	}

	let found = messages(
		"func mismatch(r: Result<int, string>): Result<int, boolean> = { let value = r? Ok(value) }",
	);
	assert!(
		found
			.iter()
			.any(|message| message.contains("mismatched types")),
		"{found:?}"
	);
}

#[test]
fn question_labels_do_not_cross_callable_boundaries() {
	let found = messages(
		"func outer(o: Option<int>): Option<int> = target@{ let inner = () -> { o?@target Some(1) } inner() }",
	);
	assert!(
		found
			.iter()
			.any(|message| message.contains("unknown control label")),
		"{found:?}"
	);

	let found = messages(
		"func nearest(o: Option<int>): int = { let inner: () -> Option<string> = () -> { let value = o? Some(\"${value}\") } inner() 7 }",
	);
	assert!(found.is_empty(), "{found:?}");

	let source = "enum Option<T> { Some(value: T), None }\nlet invalid = Some(1)?";
	let parsed = parse_module(source, "test");
	let found = check_module(&parsed.tree)
		.diags
		.into_iter()
		.map(|diagnostic| diagnostic.message.to_string())
		.collect::<Vec<_>>();
	assert!(
		found
			.iter()
			.any(|message| message.contains("inside a callable")),
		"{found:?}"
	);
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
	let found = messages("func f(): void = for (_ in #[#()]) { let g = () -> break }");
	assert!(
		found.iter().any(|message| message.contains("break")),
		"{found:?}"
	);
}

#[test]
fn labels_resolve_by_kind_and_do_not_cross_callables() {
	assert!(
		messages(
			"func f(): Option<int> = for@outer (_ in #[#()]) { for (_ in #[#()]) { break@outer 7 } }"
		)
		.is_empty()
	);
	assert!(
		messages("func f(): void = outer@{ break@outer }")
			.iter()
			.any(|m| m.contains("wrong kind"))
	);
	for source in [
		"func f(): void = outer@{ continue@outer }",
		"func f(): void = { break@f }",
		"func f(): void = { continue@f }",
	] {
		assert!(
			messages(source).iter().any(|m| m.contains("wrong kind")),
			"{source}"
		);
	}
	assert!(
		messages("func f(): void = for@outer (_ in #[#()]) { return@outer }")
			.iter()
			.any(|m| m.contains("wrong kind"))
	);
	assert!(
		messages("func f(): void = for@outer (_ in #[#()]) { let g = () -> break@outer }")
			.iter()
			.any(|m| m.contains("unknown"))
	);
	assert!(messages("func f(): int = { return@f 3 }").is_empty());
}

#[test]
fn state_loops_check_named_replacements_against_the_old_state_contract() {
	for source in [
		"func swap(): #(int, int) = loop (let left = 1, let right = left + 1) { if (left == 3) break #(left, right) continue(left = right, right = left) }",
		"func labeled(): int = loop@outer (let value = 0) { for (_ in #[#()]) { break@outer value } }",
		"func bare(): void = loop (let value = 0) { break }",
	] {
		let found = messages(source);
		assert!(found.is_empty(), "{source}: {found:?}");
	}

	for (source, expected) in [
		(
			"func unknown(): void = loop (let value = 0) { continue(other = 1) }",
			"not a state binding",
		),
		(
			"func duplicate(): void = loop (let value = 0) { continue(value = 1, value = 2) }",
			"replaced more than once",
		),
		(
			"func incompatible(): void = loop (let value = 0) { continue(value = true) }",
			"mismatched types",
		),
		(
			"func wrong_target(): void = for (_ in #[#()]) { continue(value = 1) }",
			"require a state loop target",
		),
	] {
		let found = messages(source);
		assert!(
			found.iter().any(|message| message.contains(expected)),
			"{source}: {found:?}"
		);
	}
}

#[test]
fn duplicate_active_labels_are_ambiguous_but_names_can_repeat_across_callables() {
	let source = "func f(): int = outer@{ for@outer (_ in #[#()]) { return@outer 1 } 2 }";
	let parsed = parse_module(source, "test");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let checked = check_module(&parsed.tree);
	let duplicate = checked
		.diags
		.iter()
		.find(|diagnostic| diagnostic.message.contains("already active"))
		.expect("expected duplicate-label diagnostic");
	assert_eq!(&source[duplicate.span.start..duplicate.span.end], "outer");
	assert_eq!(duplicate.labels.len(), 1);
	assert_eq!(
		&source[duplicate.labels[0].span.start..duplicate.labels[0].span.end],
		"outer"
	);

	assert!(messages("func f(): int = outer@{ 1 }\nfunc g(): int = outer@{ 2 }").is_empty());
}

#[test]
fn anonymous_closures_do_not_capture_outer_control_labels() {
	for source in [
		"func f(): void = for@outer (_ in #[#()]) { let g: (boolean) -> boolean = { if ($) { break@outer } true } break }",
		"func f(): int = { let g: (boolean) -> boolean = { if ($) { return@f 1 } true } 0 }",
	] {
		let found = messages(source);
		assert!(
			!found.is_empty(),
			"{source} unexpectedly accepted an escaping jump"
		);
		assert!(
			!found
				.iter()
				.any(|message| message.contains("only valid inside a loop")),
			"{source}: {found:?}"
		);
	}
}

#[test]
fn named_method_forms_install_callable_labels() {
	for source in [
		"struct S { func inherent(): int = { return@inherent 1 } }",
		"struct S {} impl S { namespace func make(): int = { return@make 1 } func change(): int = { return@change 2 } }",
		"interface I { func defaulted(): int = { return@defaulted 1 } } struct S {} impl I for S {}",
		"interface I { func value(): int } struct S {} impl I for S { func value(): int = { return@value 1 } }",
	] {
		let found = messages(source);
		assert!(found.is_empty(), "{source}: {found:?}");
	}
}

#[test]
fn labeled_block_returns_unify_with_the_tail() {
	assert!(
		messages("func f(flag: boolean): int = result@{ if (flag) { return@result 1 } 2 }").is_empty()
	);
	assert!(
		messages("func f(): int = result@{ if (true) { return@result true } 2 }")
			.iter()
			.any(|m| m.contains("type"))
	);
}

#[test]
fn bare_returns_unify_void_with_the_target_result() {
	for source in [
		"func f(): int = { return }",
		"func f(): int = value@{ return@value }",
	] {
		let found = messages(source);
		assert!(
			found
				.iter()
				.any(|message| message.contains("mismatched types")),
			"{source}: {found:?}"
		);
	}
}

#[test]
fn one_loop_cannot_mix_bare_and_valued_breaks() {
	let found =
		messages("func f(flag: boolean) = for (_ in #[#()]) { if (flag) { break } else { break 1 } }");
	assert!(
		found.iter().any(|message| message.contains("cannot mix")),
		"{found:?}"
	);
}

#[test]
fn nested_loop_breaks_do_not_determine_the_outer_result() {
	let found =
		messages("func f(): void = for (_ in #[#()]) { for (_ in #[#()]) { break 1 } continue }");
	assert!(found.is_empty(), "{found:?}");
}

#[test]
fn loop_result_contracts_type_check() {
	for source in [
		"func no_break(): void = for (_ in #[]) {}",
		"func bare(): Option<#()> = for (_ in #[]) { break }",
		"func valued(): Option<int> = for (_ in #[]) { if (false) { break 1 } break 2 }",
		"func labeled_loop(): Option<int> = for@outer (_ in #[#()]) { break 1 }",
		"func labeled_for(): Option<int> = for@outer (_ in #[1]) { break 1 }",
		"func nested_unlabeled(): void = for@outer (_ in #[]) { for (_ in #[#()]) { break 1 } }",
		"func lexical(): Option<int> = for (_ in #[]) { break 1 }",
		"func branch(flag: boolean): Option<int> = for (_ in #[#()]) { let value = if (flag) { break 1 } else { 2 } break value }",
		"func arm(value: int): Option<int> = for (_ in #[#()]) { let found = match (value) { 0 -> break 1, _ -> 2 } break found }",
		"func nested_break(): Option<int> = for (_ in #[#()]) { break (break 1) }",
		"func all_arms(value: boolean): Option<int> = for (_ in #[#()]) { 1 + match (value) { true -> break 1, false -> break 2 } }",
		"func guarded_arm(value: int): Option<int> = for (_ in #[#()]) { 1 + match (value) { 0 if true -> break 1, _ -> break 2 } }",
		"func short_circuit(): Option<int> = for (_ in #[#()]) { false && break 1\ntrue || break 2\ntrue && break 3 }",
		"func prefix(): Option<int> = for (_ in #[#()]) { -(break 1) }",
		"func callee(): Option<int> = for (_ in #[#()]) { (break 1)() }",
		"func member(): Option<int> = for (_ in #[#()]) { (break 1).field }",
		"func index(): Option<int> = for (_ in #[#()]) { #[0][break 1] }",
		"func cast(): Option<int> = for (_ in #[#()]) { (break 1) as int }",
		"func outer_in_nested_break_value(): Option<int> = for@outer (_ in #[#()]) { for (_ in #[#()]) { break (break@outer 7) } }",
	] {
		let found = messages(source);
		assert!(found.is_empty(), "{source}: {found:?}");
	}
}

#[test]
fn loop_headers_preserve_outer_targets_without_adding_the_new_loop() {
	for source in [
		"func f(): void = for (_ in #[#()]) { for (_ in break 1) {} }",
		"func f(): void = for (_ in #[#()]) { for (_ in break 1) {} }",
	] {
		let found = messages(source);
		assert!(
			!found
				.iter()
				.any(|message| message.contains("only valid inside a loop")),
			"{source}: {found:?}"
		);
	}
}

#[test]
fn valued_breaks_must_unify() {
	let found = messages(
		"func f(flag: boolean) = for (_ in #[#()]) { if (flag) { break 1 } else { break true } }",
	);
	assert!(
		found.iter().any(|message| message.contains("mismatched")),
		"{found:?}"
	);
}
