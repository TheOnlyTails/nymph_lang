//! `render` must treat `Span` offsets as byte offsets (as
//! documented on `nymph_ast::Span`) rather than feeding them to `ariadne` as
//! char indices. A multi-byte UTF-8 character anywhere at or before a
//! diagnostic's span must not shift the reported `line:col`, and must not
//! blank out the source excerpt.

use ecow::EcoString;
use nymph_ast::Span;
use nymph_diagnostics::Diagnostic;

/// A multi-byte character earlier in the source must not shift the reported
/// column of a later diagnostic.
#[test]
fn render_reports_correct_column_after_an_earlier_multibyte_character() {
	let source = "// café comment\nfunc f(): int = true\n";
	let byte_start = source.find("true").expect("fixture contains `true`");
	let byte_end = byte_start + "true".len();
	let span = Span::new(byte_start, byte_end);

	let diagnostic = Diagnostic::error(
		EcoString::from("9999"),
		"expected `bool`, found `true`",
		span,
	);

	let rendered = nymph_diagnostics::render("test.nym", source, &[diagnostic]);

	// `café` has one 2-byte character, so byte offset 33 lands at char
	// column 18, but the correct 1-based *char* column is 17.
	assert!(
		rendered.contains("test.nym:2:17"),
		"expected the byte-vs-char-correct locator `test.nym:2:17`, got:\n{rendered}"
	);
	assert!(
		!rendered.contains("test.nym:2:18"),
		"locator used the wrong (byte-as-char) column:\n{rendered}"
	);
}

/// A multi-byte character *inside* the erroring span itself must not blank
/// out the printed source excerpt.
#[test]
fn render_still_shows_the_source_excerpt_when_the_span_contains_a_multibyte_char() {
	let source = "func f(): int = \"héllo\" + true";
	let byte_start = source
		.find("\"héllo\"")
		.expect("fixture contains the string literal");
	let byte_end = byte_start + "\"héllo\"".len();
	let span = Span::new(byte_start, byte_end);

	let diagnostic = Diagnostic::error(
		EcoString::from("2024"),
		"`plus` is not implemented for `string`",
		span,
	);

	let rendered = nymph_diagnostics::render("test.nym", source, &[diagnostic]);
	let plain = strip_ansi(&rendered);

	assert!(
		plain.contains("héllo"),
		"source excerpt should still show the offending code, got:\n{rendered}"
	);
}

/// Strip ANSI escape sequences (ariadne colors each character of a label
/// individually, which would otherwise split a substring like `"héllo"`
/// across escape codes).
fn strip_ansi(input: &str) -> String {
	let mut out = String::with_capacity(input.len());
	let mut chars = input.chars();
	while let Some(c) = chars.next() {
		if c == '\u{1b}' {
			// Skip until the final byte of the CSI sequence (an ASCII letter).
			for next in chars.by_ref() {
				if next.is_ascii_alphabetic() {
					break;
				}
			}
		} else {
			out.push(c);
		}
	}
	out
}
