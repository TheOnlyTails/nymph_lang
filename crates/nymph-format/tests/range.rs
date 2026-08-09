use nymph_ast::Span;
use nymph_format::{FormattedRange, format_range};
use nymph_syntax::parse_module;

fn byte_span(source: &str, needle: &str) -> Span {
	let start = source
		.find(needle)
		.unwrap_or_else(|| panic!("{needle:?} absent from {source:?}"));
	Span::new(start, start + needle.len())
}

fn apply(source: &str, edit: &FormattedRange) -> String {
	assert!(source.is_char_boundary(edit.range.start));
	assert!(source.is_char_boundary(edit.range.end));
	let mut output = source.to_owned();
	output.replace_range(edit.range.start..edit.range.end, &edit.text);
	output
}

fn assert_bounded_range(source: &str, selection: Span) -> FormattedRange {
	let edit = format_range(source, "range.nym", selection)
		.expect("range format failed")
		.expect("eligible range produced no edit");
	assert!(
		edit.range.start <= selection.start && edit.range.end >= selection.end,
		"edit must cover selection: {edit:?}"
	);
	let applied = apply(source, &edit);
	let parsed = parse_module(&applied, "range-result.nym");
	assert!(
		parsed.diagnostics.is_empty(),
		"range edit broke syntax: {:?}",
		parsed.diagnostics
	);
	edit
}

#[test]
fn utf8_spans_are_bytes_not_scalar_or_utf16_offsets() {
	let source = "let λ = #[  1,2 ]\n";
	let selection = byte_span(source, "1,2");
	assert_eq!(selection.start, 13, "test must cross two-byte lambda");
	let edit = assert_bounded_range(source, selection);
	assert!(source.is_char_boundary(edit.range.start) && source.is_char_boundary(edit.range.end));
}

#[test]
fn invalid_utf8_boundaries_and_out_of_bounds_ranges_are_ineligible() {
	let source = "let λ = 1\n";
	for range in [
		Span::new(5, 5),
		Span::new(source.len() + 1, source.len() + 1),
		Span::new(0, source.len() + 1),
		Span::new(4, 3),
	] {
		assert!(
			format_range(source, "invalid-range.nym", range)
				.expect("invalid byte range must not panic or fail parsing")
				.is_none()
		);
	}
}

#[test]
fn cursor_and_surrounding_whitespace_select_the_nearest_format_unit() {
	let source = "let value = foo(  1,2 )\n";
	for offset in [
		source.find("1").unwrap(),
		source.find("  1").unwrap(),
		source.find(',').unwrap() + 1,
	] {
		assert_bounded_range(source, Span::new(offset, offset));
	}
}

#[test]
fn list_and_operator_selections_expand_to_syntactically_safe_boundaries() {
	for (source, needle) in [
		("let x=#[alpha,beta,gamma]\n", "beta"),
		("let x=(alpha+beta)*gamma\n", "+"),
		("let x=call(first,second,third)\n", "second"),
	] {
		assert_bounded_range(source, byte_span(source, needle));
	}
}

#[test]
fn comments_are_not_detached_or_dropped_by_range_expansion() {
	let source = "// lead\nlet x=1/* operator */+2 // tail\nlet y = 3\n";
	let edit = assert_bounded_range(source, byte_span(source, "+"));
	assert!(edit.text.contains("/* operator */"));
	assert!(apply(source, &edit).contains("// tail"));
}

#[test]
fn unmatched_closer_selection_does_not_widen_to_unrelated_declarations() {
	let source =
		"let before=0\r\nnamespace Util{\r\nfunc id(value:int):int=value\r\n}\r\nlet after=2\r\n";
	let close = source.rfind("}\r\n").unwrap();
	assert!(
		format_range(source, "closer-range.nym", Span::new(close, close + 1))
			.expect("valid closer selection")
			.is_none(),
		"a closer-only selection must not rewrite from byte zero"
	);
}

#[test]
fn safe_block_removal_expands_over_the_entire_block() {
	let source = "func answer() = { 42 }\n";
	let edit = assert_bounded_range(source, byte_span(source, "42"));
	assert!(edit.range.start <= source.find('{').unwrap());
	assert!(edit.range.end > source.find('}').unwrap());
	assert_eq!(apply(source, &edit), "func answer() = 42\n");
}

#[test]
fn malformed_whole_document_fails_even_when_selection_is_locally_valid() {
	let source = "let good = #[ 1,2 ]\nlet broken = {\n";
	assert!(format_range(source, "broken.nym", byte_span(source, "1,2")).is_err());
}

#[test]
fn unchanged_or_ineligible_ranges_return_none() {
	for (source, span) in [
		("let x = 1\n", Span::new(8, 8)),
		("// only a comment\n", Span::new(3, 7)),
		("\n", Span::new(0, 0)),
	] {
		assert!(
			format_range(source, "unchanged.nym", span)
				.expect("range request should be valid")
				.is_none()
		);
	}
}

#[test]
fn module_level_selection_can_expand_across_declarations() {
	let source = "import   @/one\nimport   @/two\nlet x=1\n";
	let selection = Span::new(source.find("one").unwrap(), source.find("two").unwrap() + 3);
	let edit = assert_bounded_range(source, selection);
	assert_eq!(edit.range.start, 0);
	assert_eq!(edit.range.end, source.find("let").unwrap());
	assert!(apply(source, &edit).ends_with("let x=1\n"));
}

#[test]
fn selection_inside_balanced_interpolation_formats_its_expression() {
	let source = "let text = \"value ${{let λ=1 λ+2}}\"\n";
	let edit = assert_bounded_range(source, byte_span(source, "λ+2"));
	assert!(edit.range.start > source.find("${{").unwrap());
	assert!(apply(source, &edit).contains("λ + 2"));
}
