use std::{
	fs,
	path::{Path, PathBuf},
};

use nymph_ast::{
	decl::Declaration,
	expr::{ExprKind, Statement, StringPart},
	token::Token,
};
use nymph_format::format;
use nymph_syntax::{lex, parse_module};

fn fixture_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn successful_fixtures() -> Vec<PathBuf> {
	let mut pending = vec![fixture_root()];
	let mut fixtures = Vec::new();
	while let Some(dir) = pending.pop() {
		for entry in fs::read_dir(dir).expect("read fixture directory") {
			let path = entry.expect("read fixture entry").path();
			if path.is_dir() {
				if path.file_name().unwrap() != "malformed" {
					pending.push(path);
				}
			} else if path.to_string_lossy().ends_with(".input.nym") {
				fixtures.push(path);
			}
		}
	}
	fixtures.sort();
	fixtures
}

fn parse_clean(source: &str, context: &Path) {
	let parsed = parse_module(source, context.to_string_lossy());
	assert!(
		parsed.diagnostics.is_empty(),
		"{context:?} did not parse cleanly: {:?}",
		parsed.diagnostics
	);
}

fn sole_string_value(source: &str) -> String {
	let parsed = parse_module(source, "string-value.nym");
	assert!(
		parsed.diagnostics.is_empty(),
		"string test source must parse"
	);
	let [Declaration::Let { value, .. }] = parsed.tree.members.as_slice() else {
		panic!("expected one let declaration");
	};
	let ExprKind::String(parts) = &value.kind else {
		panic!("expected a string value");
	};
	let mut value = String::new();
	for part in parts {
		match &part.0 {
			StringPart::Text(text) => value.push_str(text),
			StringPart::EscapeSequence(escape) => value.push(escape.to_char().expect("character escape")),
			StringPart::InterpolatedExpr(_) => panic!("unexpected interpolation"),
		}
	}
	value
}

/// A location-independent semantic fingerprint. Formatting may remove ordinary
/// grouping/block delimiters, but may not alter or reorder any other token. The
/// collection openers remain significant, as do literal values and identifiers.
fn semantic_fingerprint(source: &str) -> Vec<String> {
	let result = lex(source);
	assert!(
		result.diagnostics.is_empty(),
		"lex diagnostics: {:?}",
		result.diagnostics
	);
	result
		.tokens
		.into_iter()
		.filter_map(|token| match token.0 {
			Token::LParen | Token::RParen | Token::LBrace | Token::RBrace | Token::Comma => None,
			other => Some(without_nested_spans(format!("{other:?}"))),
		})
		.collect()
}

// Interpolation tokens recursively contain `Spanned<Token>` values. Their byte
// locations necessarily move when whitespace changes, so erase only those Debug
// fragments while retaining the complete nested token shape and values.
fn without_nested_spans(mut debug: String) -> String {
	while let Some(start) = debug.find("Span { start: ") {
		let Some(relative_end) = debug[start..].find('}') else {
			break;
		};
		debug.replace_range(start..=start + relative_end, "Span");
	}
	debug = debug.replace("Spanned(Comma, Span), ", "");
	debug
}

#[test]
fn all_successful_file_fixtures_are_exact_idempotent_parseable_and_semantic() {
	let fixtures = successful_fixtures();
	assert!(
		fixtures.len() >= 8,
		"fixture discovery unexpectedly found only {}",
		fixtures.len()
	);
	for input_path in fixtures {
		let expected_path = PathBuf::from(
			input_path
				.to_string_lossy()
				.replace(".input.nym", ".expected.nym"),
		);
		let input = fs::read_to_string(&input_path).expect("read input fixture");
		let expected = fs::read_to_string(&expected_path).expect("read expected fixture");
		parse_clean(&input, &input_path);
		let actual = format(&input, &input_path.to_string_lossy())
			.unwrap_or_else(|error| panic!("format failed for {input_path:?}: {error:?}"));
		assert_eq!(actual, expected, "fixture mismatch: {input_path:?}");
		assert_eq!(
			actual.as_bytes().last(),
			Some(&b'\n'),
			"missing final LF: {input_path:?}"
		);
		assert!(
			!actual.ends_with("\n\n"),
			"more than one final newline: {input_path:?}"
		);
		assert!(
			!actual.contains('\r'),
			"CR survived normalization: {input_path:?}"
		);
		assert_eq!(
			format(&actual, "idempotence.nym").expect("second format"),
			actual,
			"not idempotent: {input_path:?}"
		);
		parse_clean(&actual, &expected_path);
		assert_eq!(
			semantic_fingerprint(&input),
			semantic_fingerprint(&actual),
			"semantic token structure changed: {input_path:?}"
		);
	}
}

#[test]
fn malformed_and_recovered_documents_never_produce_partial_output() {
	let malformed = fixture_root().join("malformed");
	let mut count = 0;
	for entry in fs::read_dir(malformed).expect("malformed fixtures") {
		let path = entry.expect("fixture entry").path();
		if path.extension().and_then(|it| it.to_str()) != Some("nym") {
			continue;
		}
		count += 1;
		let source = fs::read_to_string(&path).expect("read malformed fixture");
		assert!(
			format(&source, &path.to_string_lossy()).is_err(),
			"malformed/recovered input returned output: {path:?}"
		);
	}
	assert!(count >= 8);
}

#[test]
fn literal_spelling_and_import_order_are_byte_stable() {
	let source = "import @/z\nimport @/a\nlet values = #[0xDEAD_F00D, 0b0101, 1_000u, 6.02e23, '\\u03bb', \"\\u03bb\"]\n";
	assert_eq!(
		format(source, "spelling.nym").expect("format literals"),
		source
	);
}

#[test]
fn block_elision_preserves_control_flow_precedence_and_dangling_else_binding() {
	let source = "func g(): int = { return@g 1 } + 2\n\
		func call(): int = ({return@call 1})(2)\n\
		func branch(a: bool, b: bool): int = if (a) {if (b) 1} else 2\n\
		func nested(a: bool, b: bool, c: bool): int = if (a) {if (b) 1 else if (c) 2} else 3\n\
		func safe(a: bool): int = if(a){break@safe 1}else{2}\n";
	let expected = "func g(): int = {\n\
		\treturn@g 1\n\
		} + 2\n\
		func call(): int = ({\n\
		\treturn@call 1\n\
		})(2)\n\
		func branch(a: bool, b: bool): int = if (a) {\n\
		\tif (b) 1\n\
		} else 2\n\
		func nested(a: bool, b: bool, c: bool): int = if (a) {\n\
		\tif (b) 1 else if (c) 2\n\
		} else 3\n\
		func safe(a: bool): int = if (a) break@safe 1 else 2\n";
	let actual = format(source, "block-elision.nym").expect("format block expressions");
	assert_eq!(actual, expected);
	assert_eq!(format(&actual, "block-elision.nym").unwrap(), actual);
	assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&actual));
}

#[test]
fn call_argument_elision_does_not_turn_positional_assignments_into_named_arguments() {
	let source = "func sink(x: int): int = x\n\
		func repro(): int = {\n\
		\tlet mut x = 0\n\
		\tsink((x = 1))\n\
		\tsink({ x = 2 })\n\
		\tx\n\
		}\n";
	let actual = format(source, "call-argument-elision.nym").expect("format call arguments");
	assert_eq!(
		format(&actual, "call-argument-elision.nym").unwrap(),
		actual
	);

	let parsed = parse_module(&actual, "call-argument-elision.nym");
	assert!(parsed.diagnostics.is_empty());
	let Declaration::Func { body, .. } = &parsed.tree.members[1] else {
		panic!("expected repro function");
	};
	let ExprKind::Block { body, .. } = &body.kind else {
		panic!("expected repro block");
	};
	for statement in &body[1..=2] {
		let Statement::Expr(call) = &statement.0 else {
			panic!("expected call statement");
		};
		let ExprKind::Call { args, .. } = &call.kind else {
			panic!("expected call expression");
		};
		assert!(args[0].0.name.is_none(), "positional call became named");
	}
}

#[test]
fn block_elision_preserves_else_ownership_through_prefix_and_range_operands() {
	let source = "func prefix(a: bool, b: bool): bool = if (a) { !if (b) true } else false\n\
		func range(a: bool, b: bool) = if (a) { 0..if (b) 1 } else 2\n";
	let actual = format(source, "dangling-else-operands.nym").expect("format dangling else cases");
	assert_eq!(
		format(&actual, "dangling-else-operands.nym").unwrap(),
		actual
	);
	let parsed = parse_module(&actual, "dangling-else-operands.nym");
	assert!(parsed.diagnostics.is_empty());
	for declaration in &parsed.tree.members {
		let Declaration::Func { body, .. } = declaration else {
			panic!("expected function declaration");
		};
		let ExprKind::If {
			then, otherwise, ..
		} = &body.kind
		else {
			panic!("expected outer if expression");
		};
		assert!(otherwise.is_some(), "outer else changed ownership");
		assert!(
			matches!(then.kind, ExprKind::Block { .. }),
			"protective then block was removed"
		);
	}
}

#[test]
fn required_precedence_groups_have_canonical_spacing_and_are_idempotent() {
	let source = "func left(a:int,b:int,c:int)= (a + b) * c\n\
		func right(a:int,b:int,c:int)= a * (b + c)\n\
		func unary(a:int,b:int)= -(a + b)\n\
		func power()=2**3**2\n\
		func left_power()=(2**3)**2\n\
		func power_assign()={let mut x=2 x**=3 x}\n";
	let expected = "func left(a: int, b: int, c: int) = (a + b) * c\n\
		func right(a: int, b: int, c: int) = a * (b + c)\n\
		func unary(a: int, b: int) = -(a + b)\n\
		func power() = 2 ** 3 ** 2\n\
		func left_power() = (2 ** 3) ** 2\n\
		func power_assign() = {\n\
		\tlet mut x = 2\n\
		\tx **= 3\n\
		\tx\n\
		}\n";
	let actual = format(source, "groups.nym").expect("format groups");
	assert_eq!(actual, expected);
	assert_eq!(format(&actual, "groups.nym").unwrap(), actual);
	assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&actual));
}

#[test]
fn interpolation_line_comments_keep_their_terminating_newline() {
	let source = "let text = \"${value // keep\n}\"\n";
	let actual = format(source, "interpolation-comment.nym").expect("format interpolation");
	assert_eq!(actual, source);
	assert_eq!(
		format(&actual, "interpolation-comment.nym").unwrap(),
		actual
	);
	parse_clean(&actual, Path::new("interpolation-comment.nym"));
}

#[test]
fn raw_carriage_returns_in_strings_are_escaped_without_changing_the_value() {
	let source = "let text = \"first\r\nsecond\rmiddle\"\r\n";
	let actual = format(source, "string-cr.nym").expect("format raw string CRs");
	assert_eq!(actual, "let text = \"first\\r\nsecond\\rmiddle\"\n");
	assert!(!actual.contains('\r'));
	assert_eq!(format(&actual, "string-cr.nym").unwrap(), actual);
	assert_eq!(sole_string_value(source), sole_string_value(&actual));
}

#[test]
fn width_uses_the_full_line_and_unicode_display_columns() {
	let ascii = format(
		"func width() = aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa(one,two)\n",
		"ascii-width.nym",
	)
	.expect("format ASCII width boundary");
	assert!(ascii.contains("aaaaaaaaaa(\n\tone,\n\ttwo,\n)"));

	let unicode = format(
		&format!("func width() = {}(one,two)\n", "界".repeat(39)),
		"unicode-width.nym",
	)
	.expect("format Unicode width boundary");
	assert!(unicode.contains("界(\n\tone,\n\ttwo,\n)"));
	assert_eq!(format(&unicode, "unicode-width.nym").unwrap(), unicode);
}

#[test]
fn unicode_xid_identifiers_always_advance_the_lossless_scanner() {
	let source = "let ℘ = 1\nlet a\u{301} = ℘\n";
	let actual = format(source, "unicode-identifiers.nym").expect("format Unicode identifiers");
	assert_eq!(actual, source);
	assert_eq!(format(&actual, "unicode-identifiers.nym").unwrap(), actual);
}
