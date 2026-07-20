//! `textDocument/definition`: jump from an identifier use, a bare enum
//! variant, or (best-effort) a type-position type name, to its declaration.
//!
//! Built on [`nymph_sema::query::definition_at`], which works from the AST
//! plus a freshly rebuilt `DefMap` — *not* from `Checked` annotations, which
//! carry only operator-dispatch metadata, never an identifier -> binder
//! mapping (see that function's doc comment). So, unlike hover, this needs no
//! type-check and no cache: just the same best-effort parse `documentSymbols`
//! and hover both already use.
//!
//! Member/field access after a `.` is not covered (it needs the checker's
//! member resolution) — those requests answer `None`, never a wrong jump.

use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location};

use crate::{document_store::DocumentStore, line_index::LineIndex};

/// Answer a go-to-definition request: `None` when the document isn't open, or
/// when nothing at the cursor resolves (whitespace, a comment, member/field
/// access, an unresolvable name, or an ambiguous bare enum variant).
pub fn definition(
	docs: &DocumentStore,
	params: &GotoDefinitionParams,
) -> Option<GotoDefinitionResponse> {
	let uri = &params.text_document_position_params.text_document.uri;
	let position = params.text_document_position_params.position;
	let doc = docs.get(uri)?;

	let parsed = nymph_syntax::parse_module(&doc.text, uri.path().as_str());
	let index = LineIndex::new(&doc.text);
	let offset = index.offset(&doc.text, position);

	let span = nymph_sema::query::definition_at(&parsed.tree, offset)?;

	Some(GotoDefinitionResponse::Scalar(Location {
		uri: uri.clone(),
		range: index.range(&doc.text, span),
	}))
}

#[cfg(test)]
mod tests {
	use super::*;
	use lsp_types::{
		Position, TextDocumentIdentifier, TextDocumentPositionParams, Uri, WorkDoneProgressParams,
	};

	fn params(uri: &Uri, line: u32, character: u32) -> GotoDefinitionParams {
		GotoDefinitionParams {
			text_document_position_params: TextDocumentPositionParams {
				text_document: TextDocumentIdentifier { uri: uri.clone() },
				position: Position { line, character },
			},
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: Default::default(),
		}
	}

	fn docs_with(uri: &Uri, text: &str) -> DocumentStore {
		let mut docs = DocumentStore::default();
		docs.open(uri.clone(), text.to_string(), 1);
		docs
	}

	fn scalar(response: GotoDefinitionResponse) -> Location {
		match response {
			GotoDefinitionResponse::Scalar(loc) => loc,
			other => panic!("expected the Scalar arm, got {other:?}"),
		}
	}

	#[test]
	fn jumps_a_local_use_to_its_binder() {
		let uri: Uri = "file:///def_local.nym".parse().unwrap();
		let text = "func main(): int = {\n  let x = 1\n  x + 2\n}";
		let docs = docs_with(&uri, text);

		// `x` in `x + 2`, line 2, column 2.
		let result = definition(&docs, &params(&uri, 2, 2));
		let loc = scalar(result.expect("should resolve to the `let x` binder"));
		assert_eq!(loc.uri, uri);
		// The binder `x` sits on line 1 at column 6 ("  let x = 1").
		assert_eq!(loc.range.start.line, 1);
		assert_eq!(loc.range.start.character, 6);
	}

	#[test]
	fn jumps_a_call_to_its_func_declaration() {
		let uri: Uri = "file:///def_call.nym".parse().unwrap();
		let text = "func helper(): int = 1\nfunc main(): int = helper()";
		let docs = docs_with(&uri, text);

		// `helper` in the call, line 1, column 20.
		let result = definition(&docs, &params(&uri, 1, 21));
		let loc = scalar(result.expect("should resolve to `helper`'s declaration"));
		assert_eq!(loc.uri, uri);
		assert_eq!(loc.range.start.line, 0);
		// `helper`'s name starts right after "func ".
		assert_eq!(loc.range.start.character, 5);
	}

	#[test]
	fn returns_none_for_a_member_access() {
		let uri: Uri = "file:///def_member.nym".parse().unwrap();
		let text = "struct Point(x: int)\nfunc main(): int = Point(x = 0).x";
		let docs = docs_with(&uri, text);

		// The trailing `.x` field access — not resolvable without the checker.
		let last_line_len = text.lines().last().unwrap().chars().count() as u32;
		let result = definition(&docs, &params(&uri, 1, last_line_len - 1));
		assert!(
			result.is_none(),
			"member access should never resolve to a (possibly wrong) jump"
		);
	}

	#[test]
	fn returns_none_for_an_unopened_document() {
		let uri: Uri = "file:///def_missing.nym".parse().unwrap();
		let docs = DocumentStore::default();

		let result = definition(&docs, &params(&uri, 0, 0));
		assert!(result.is_none());
	}
}
