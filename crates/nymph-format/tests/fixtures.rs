use std::{
	fs,
	path::{Path, PathBuf},
};

use nymph_ast::token::Token;
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
