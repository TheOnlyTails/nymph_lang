//! Formatting requests backed exclusively by the client's open document.

use crate::document_store::DocumentStore;
use crate::line_index::LineIndex;
use lsp_types::{DocumentFormattingParams, DocumentRangeFormattingParams, Range, TextEdit};
use nymph_ast::Span;

#[must_use]
pub fn document_formatting(
	docs: &DocumentStore,
	params: &DocumentFormattingParams,
) -> Option<Vec<TextEdit>> {
	let document = docs.get(&params.text_document.uri)?;
	let path = params.text_document.uri.as_str();
	let formatted = nymph_format::format(&document.text, path).ok()?;
	if formatted == document.text {
		return Some(Vec::new());
	}
	let index = LineIndex::new(&document.text);
	Some(vec![TextEdit {
		range: index.range(&document.text, Span::new(0, document.text.len())),
		new_text: formatted,
	}])
}

#[must_use]
pub fn document_range_formatting(
	docs: &DocumentStore,
	params: &DocumentRangeFormattingParams,
) -> Option<Vec<TextEdit>> {
	let document = docs.get(&params.text_document.uri)?;
	let index = LineIndex::new(&document.text);
	let start = index.offset(&document.text, params.range.start);
	let end = index.offset(&document.text, params.range.end);
	if start > end {
		return Some(Vec::new());
	}
	let formatted = nymph_format::format_range(
		&document.text,
		params.text_document.uri.as_str(),
		Span::new(start, end),
	)
	.ok()??;
	if formatted.range.start > formatted.range.end || formatted.range.end > document.text.len() {
		return Some(Vec::new());
	}
	let range: Range = index.range(&document.text, formatted.range);
	if document
		.text
		.get(formatted.range.start..formatted.range.end)
		== Some(&formatted.text)
	{
		return Some(Vec::new());
	}
	Some(vec![TextEdit {
		range,
		new_text: formatted.text,
	}])
}

#[cfg(test)]
mod tests {
	use super::*;
	use lsp_types::{Position, Uri};

	fn document_params(uri: &Uri) -> DocumentFormattingParams {
		serde_json::from_value(serde_json::json!({
			"textDocument": { "uri": uri.as_str() },
			"options": { "tabSize": 8, "insertSpaces": true }
		}))
		.unwrap()
	}

	fn range_params(uri: &Uri, range: Range) -> DocumentRangeFormattingParams {
		serde_json::from_value(serde_json::json!({
			"textDocument": { "uri": uri.as_str() },
			"range": range,
			"options": { "tabSize": 4, "insertSpaces": true }
		}))
		.unwrap()
	}

	fn apply(text: &str, edit: &TextEdit) -> String {
		let index = LineIndex::new(text);
		let start = index.offset(text, edit.range.start);
		let end = index.offset(text, edit.range.end);
		let mut output = text.to_owned();
		output.replace_range(start..end, &edit.new_text);
		output
	}

	#[test]
	fn document_formatting_uses_open_buffer_and_ignores_client_style_options() {
		let uri: Uri = "file:///format-document.nym".parse().unwrap();
		let mut docs = DocumentStore::default();
		docs.open(uri.clone(), "let λ=#[1,2]\r\n".into(), 7);
		let edits = document_formatting(&docs, &document_params(&uri)).unwrap();
		assert_eq!(edits.len(), 1);
		assert_eq!(edits[0].new_text, "let λ = #[1, 2]\n");
		assert_eq!(edits[0].range.start, Position::new(0, 0));
	}

	#[test]
	fn document_formatting_returns_no_edits_when_unchanged_and_none_when_malformed() {
		let clean: Uri = "file:///clean.nym".parse().unwrap();
		let malformed: Uri = "file:///malformed.nym".parse().unwrap();
		let mut docs = DocumentStore::default();
		docs.open(clean.clone(), "let value = 1\n".into(), 1);
		docs.open(malformed.clone(), "let value = {\n".into(), 1);
		assert_eq!(
			document_formatting(&docs, &document_params(&clean)),
			Some(Vec::new())
		);
		assert!(document_formatting(&docs, &document_params(&malformed)).is_none());
	}

	#[test]
	fn range_formatting_uses_utf16_and_keeps_bytes_outside_the_edit() {
		let uri: Uri = "file:///format-range.nym".parse().unwrap();
		let text = "let before=0\nlet λ=alpha+beta\nlet after=2\n";
		let mut docs = DocumentStore::default();
		docs.open(uri.clone(), text.into(), 3);
		let index = LineIndex::new(text);
		let plus = text.find('+').unwrap();
		let range = Range::new(index.position(text, plus), index.position(text, plus + 1));
		let edits = document_range_formatting(&docs, &range_params(&uri, range)).unwrap();
		assert_eq!(edits.len(), 1);
		let output = apply(text, &edits[0]);
		assert_eq!(output, "let before=0\nlet λ=alpha + beta\nlet after=2\n");
		assert!(
			edits[0].range.start.character > 0,
			"range unexpectedly replaced the document"
		);
	}

	#[test]
	fn range_formatting_is_safe_for_malformed_and_out_of_order_ranges() {
		let uri: Uri = "file:///format-range-invalid.nym".parse().unwrap();
		let mut docs = DocumentStore::default();
		docs.open(uri.clone(), "let value = {\n".into(), 1);
		let reversed = Range::new(Position::new(1, 0), Position::new(0, 0));
		assert_eq!(
			document_range_formatting(&docs, &range_params(&uri, reversed)),
			Some(Vec::new())
		);
		let ordinary = Range::new(Position::new(0, 4), Position::new(0, 9));
		assert!(document_range_formatting(&docs, &range_params(&uri, ordinary)).is_none());
	}

	#[test]
	fn document_formatting_matches_the_shared_canonical_fixture() {
		let uri: Uri = "file:///format-parity.nym".parse().unwrap();
		let mut docs = DocumentStore::default();
		docs.open(
			uri.clone(),
			include_str!("../../nymph-format/testdata/comments_whitespace/boundaries.input.nym").into(),
			1,
		);
		let edits = document_formatting(&docs, &document_params(&uri)).unwrap();
		assert_eq!(
			edits[0].new_text,
			include_str!("../../nymph-format/testdata/comments_whitespace/boundaries.expected.nym")
		);
	}
}
