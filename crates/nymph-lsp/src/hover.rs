//! `textDocument/hover`: the type of the smallest checked expression under
//! the cursor.
//!
//! Hover consumes the compiler session's immutable analysis snapshot. Project
//! files therefore use the same effective sources, imports, aliases, embedded
//! standard library, ambient prelude, generic substitutions, and dependency
//! overlays as diagnostics. Loose saved files use a one-module library project
//! with the ambient prelude; untitled files use an isolated one-module project
//! over their open text. The compiler-owned `ModuleAnalysis::type_at` seam
//! pairs annotations with the exact semantic definition arena that produced
//! them.

use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};

use crate::{
	compiler_state::AnalysisSnapshot, line_index::LineIndex,
	position::query_with_whitespace_left_bias,
};

/// Answer a hover request: `None` when the document isn't open, or when
/// neither a checked expression/declaration (see
/// `nymph_sema::query::type_at`) nor a keyword (see
/// `nymph_sema::query::keyword_doc_at`) covers the requested position
/// (whitespace, a comment, an operator, or a pattern/binder position).
///
/// The two query functions answer with two different Markdown *shapes*: a
/// CODE snippet (a type, a declaration's structure, a signature) belongs in
/// a syntax-highlighted ` ```nymph ` fence; a keyword's short prose
/// explanation does not — it's already readable Markdown on its own, and
/// fencing it would render it as if it were Nymph source. `type_at` always
/// wins when both could apply (it's tried first), so a keyword covered by
/// some other hoverable node — impossible in practice, since a keyword and
/// an expression/declaration never occupy the same span — never loses its
/// code hover to a doc.
pub fn hover(snapshot: &AnalysisSnapshot, params: &HoverParams) -> Option<Hover> {
	hover_snapshot(snapshot, params)
}

pub(crate) fn hover_snapshot(snapshot: &AnalysisSnapshot, params: &HoverParams) -> Option<Hover> {
	let position = params.text_document_position_params.position;
	let text = snapshot.source.as_ref();
	let index = LineIndex::new(text);
	let offset = index.offset(text, position);

	let value = if let Some(code) = query_with_whitespace_left_bias(text, offset, |candidate| {
		snapshot.analysis.type_at(candidate)
	}) {
		format!(
			r"```nymph
{code}
```"
		)
	} else {
		let kw_doc = query_with_whitespace_left_bias(text, offset, |candidate| {
			nymph_sema::query::keyword_doc_at(text, candidate)
		})?;
		kw_doc.to_string()
	};

	Some(Hover {
		contents: HoverContents::Markup(MarkupContent {
			kind: MarkupKind::Markdown,
			value,
		}),
		range: None,
	})
}

#[cfg(test)]
fn hover_fixture(
	docs: &crate::document_store::DocumentStore,
	state: &mut crate::compiler_state::CompilerState,
	params: &HoverParams,
) -> Option<Hover> {
	let uri = &params.text_document_position_params.text_document.uri;
	let document = docs.get(uri)?;
	let mut owned_docs = docs.clone();
	state
		.open(
			&mut owned_docs,
			uri.clone(),
			document.text.to_string(),
			document.version,
		)
		.ok()?;
	let snapshot = state.analysis_for_uri(&owned_docs, uri)?;
	hover_snapshot(&snapshot, params)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::document_store::DocumentStore;
	use lsp_types::{
		Position, TextDocumentIdentifier, TextDocumentPositionParams, Uri, WorkDoneProgressParams,
	};

	fn params(uri: &Uri, line: u32, character: u32) -> HoverParams {
		HoverParams {
			text_document_position_params: TextDocumentPositionParams {
				text_document: TextDocumentIdentifier { uri: uri.clone() },
				position: Position { line, character },
			},
			work_done_progress_params: WorkDoneProgressParams::default(),
		}
	}

	fn docs_with(uri: &Uri, text: &str) -> DocumentStore {
		let mut docs = DocumentStore::default();
		docs.open(uri.clone(), text.to_string(), 1);
		docs
	}

	/// The exact fenced-Markdown shape `hover` wraps a `type_at`/decl code
	/// snippet in.
	fn code(snippet: &str) -> String {
		format!(
			r"```nymph
{snippet}
```"
		)
	}

	#[test]
	fn hovering_a_typed_initializer_returns_its_type() {
		let uri: Uri = "file:///hover.nym".parse().unwrap();
		let text = "func main(): void = {\n  let x: int = 1\n}";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		// The `1` initializer sits on line 1 (0-based), at column 15 — see
		// the layout comment below.
		//   "  let x: int = 1"
		//    0123456789012345
		let result = hover_fixture(&docs, &mut cache, &params(&uri, 1, 15));
		let hover = result.expect("hovering a literal should resolve a type");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => assert_eq!(value, code("int")),
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hovering_whitespace_outside_any_expression_returns_none() {
		let uri: Uri = "file:///hover_ws.nym".parse().unwrap();
		let text = "func main(): void = {\n  let x: int = 1\n}";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		// Column 12 on line 0 is the single space between `:` and `void` in
		// the return-type annotation — `:` is not a keyword (so
		// `keyword_doc_at` doesn't claim its inclusive-end boundary), and
		// the signature is never an `Expr` (so `type_at` never annotates
		// it either).
		//   "func main(): void = {"
		//    0123456789012345678901
		let result = hover_fixture(&docs, &mut cache, &params(&uri, 0, 12));
		assert!(
			result.is_none(),
			"expected no hover over the function signature's whitespace, got {result:?}"
		);
	}

	#[test]
	fn hovering_a_comment_returns_none() {
		let uri: Uri = "file:///hover_comment.nym".parse().unwrap();
		let text = "// just a comment\nfunc main(): void = {}";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let result = hover_fixture(&docs, &mut cache, &params(&uri, 0, 5));
		assert!(
			result.is_none(),
			"expected no hover over a comment, got {result:?}"
		);
	}

	#[test]
	fn hovering_the_let_keyword_shows_its_doc_not_the_enclosing_block() {
		// BUG 1 end-to-end, now layered with keyword docs: the smallest expr
		// covering the `let` keyword is the enclosing `Block` (which IS
		// annotated, with its trailing expression's type) — hover must still
		// suppress that container (never leak the block's type). With
		// `keyword_doc_at` wired in, this position now resolves to the
		// `let` keyword's own prose doc instead of `None`.
		let uri: Uri = "file:///hover_let_kw.nym".parse().unwrap();
		let text = "func main(): int = {\n  let x: int = 1\n  x\n}";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		//   "  let x: int = 1"
		//    0123456789012345
		let result = hover_fixture(&docs, &mut cache, &params(&uri, 1, 3));
		let hover = result.expect("hovering the `let` keyword should resolve its own doc");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert!(
					!value.starts_with("```"),
					"a keyword doc must not be wrapped in a code fence, got {value:?}"
				);
				assert!(
					value.contains("`let`"),
					"expected the `let` keyword's own doc, not a leaked block type, got {value:?}"
				);
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hovering_a_var_use_returns_its_type() {
		let uri: Uri = "file:///hover_var.nym".parse().unwrap();
		let text = "func main(): int = {\n  let x: int = 1\n  x\n}";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		//   "  x"
		//    012
		let result = hover_fixture(&docs, &mut cache, &params(&uri, 2, 2));
		let hover = result.expect("hovering a var use should resolve a type");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => assert_eq!(value, code("int")),
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hover_left_biases_only_across_whitespace_with_utf16_positions() {
		let uri: Uri = "file:///hover_bias.nym".parse().unwrap();
		let cases = [
			("func main(): string = \"𝔘\"   ", 29, true),
			("func main(): string = \"𝔘\".   ", 30, false),
			("func main(): string = \"𝔘\" // note   ", 37, false),
		];

		for (text, utf16_column, resolves) in cases {
			let docs = docs_with(&uri, text);
			let mut cache = crate::compiler_state::CompilerState::new();
			let result = hover_fixture(&docs, &mut cache, &params(&uri, 0, utf16_column));
			assert_eq!(result.is_some(), resolves, "fixture {text:?}");
		}
	}

	#[test]
	fn hovering_a_calls_closing_paren_returns_none() {
		// `Call` is a suppressed container: the parens (covered only by the
		// `Call` span, not by any smaller child expr) must not leak the
		// call's return type.
		let uri: Uri = "file:///hover_call.nym".parse().unwrap();
		let text = "func helper(): int = 1\nfunc main(): int = helper()";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		//   "func main(): int = helper()"
		//    0123456789012345678901234567
		// Column 26 is the `)` — covered by the `Call` but not by the
		// `helper` `Identifier` (which ends at column 25).
		let result = hover_fixture(&docs, &mut cache, &params(&uri, 1, 26));
		assert!(
			result.is_none(),
			"expected None hovering a call's closing paren, got {result:?}"
		);
	}

	#[test]
	fn hovering_the_calls_callee_still_resolves_its_function_type() {
		// Upgraded: a call-site callee now shows the full NAMED signature
		// (`func helper(): int`), not just the unnamed `() -> int` `Fn` type.
		let uri: Uri = "file:///hover_call_callee.nym".parse().unwrap();
		let text = "func helper(): int = 1\nfunc main(): int = helper()";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		// Column 21 lands inside `helper`, the callee identifier itself.
		let result = hover_fixture(&docs, &mut cache, &params(&uri, 1, 21));
		let hover = result.expect("hovering the callee should resolve its function type");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert_eq!(value, code("func helper(): int"));
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hovering_a_generic_typed_value_shows_the_source_param_name() {
		// BUG 2 end-to-end: `V` must render as `V`, not the internal `T0`.
		let uri: Uri = "file:///hover_generic.nym".parse().unwrap();
		let text = "func id<V>(v: V): V = v";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		//   "func id<V>(v: V): V = v"
		//    01234567890123456789012
		let result = hover_fixture(&docs, &mut cache, &params(&uri, 0, 22));
		let hover = result.expect("hovering the returned `v` should resolve a type");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => assert_eq!(value, code("V")),
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	// ── Rich hover: declaration structures ──────────────────────────────────

	#[test]
	fn hovering_a_struct_decl_name_shows_its_full_structure() {
		let uri: Uri = "file:///hover_struct.nym".parse().unwrap();
		let text = "struct Point(x: int, y: int)\nfunc main(): void = {}";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let offset = text.find("Point").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover_fixture(&docs, &mut cache, &params(&uri, line, character));
		let hover = result.expect("hovering the struct decl name should resolve");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert_eq!(value, code("struct Point(x: int, y: int)"));
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hovering_an_enum_decl_name_shows_every_variant_and_its_fields() {
		let uri: Uri = "file:///hover_enum.nym".parse().unwrap();
		let text = "enum Shape { Circle(radius: int), Square }\nfunc main(): void = {}";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let offset = text.find("Shape").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover_fixture(&docs, &mut cache, &params(&uri, line, character));
		let hover = result.expect("hovering the enum decl name should resolve");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert_eq!(value, code("enum Shape { Circle(radius: int), Square }"));
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hovering_a_func_decl_name_shows_the_full_named_signature() {
		let uri: Uri = "file:///hover_func_sig.nym".parse().unwrap();
		let text = "func add(a: int, b: int): int = a + b";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let offset = text.find("add").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover_fixture(&docs, &mut cache, &params(&uri, line, character));
		let hover = result.expect("hovering the func decl name should resolve");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert_eq!(value, code("func add(a: int, b: int): int"));
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hovering_a_generic_param_decl_shows_its_bound() {
		let uri: Uri = "file:///hover_generic_bound.nym".parse().unwrap();
		let text = "interface Area {}\nfunc measure<T: Area>(t: T): int = 1";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let offset = text.find("T: Area").unwrap(); // the param's own `T`
		let (line, character) = line_col(text, offset);
		let result = hover_fixture(&docs, &mut cache, &params(&uri, line, character));
		let hover = result.expect("hovering the generic param decl should resolve");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert_eq!(value, code("T: Area"));
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	// ── Rich hover: keyword documentation (prose, not fenced) ───────────────

	#[test]
	fn hovering_the_for_keyword_shows_its_doc_as_prose_not_a_leaked_type() {
		let uri: Uri = "file:///hover_for_kw.nym".parse().unwrap();
		let text = "func main(): void = {\n  for (i in 1..3) { }\n}";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let offset = text.find("for").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover_fixture(&docs, &mut cache, &params(&uri, line, character));
		let hover = result.expect("hovering the `for` keyword should resolve its doc");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert!(
					!value.starts_with("```"),
					"a keyword doc must not be wrapped in a code fence, got {value:?}"
				);
				assert!(
					value.contains("`for`"),
					"expected the `for` keyword's own doc, got {value:?}"
				);
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hovering_an_operator_still_returns_none() {
		// Operators/delimiters return `None` from both `type_at` (already
		// pinned in `nymph_sema`) and `keyword_doc_at` (not a keyword token)
		// — the prior-slice "operator ⇒ None" guarantee must still hold with
		// keyword docs layered in.
		let uri: Uri = "file:///hover_operator.nym".parse().unwrap();
		let text = "func main(): int = 1 + 2";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let offset = text.find('+').unwrap();
		let (line, character) = line_col(text, offset);
		let result = hover_fixture(&docs, &mut cache, &params(&uri, line, character));
		assert!(
			result.is_none(),
			"expected no hover over an operator, got {result:?}"
		);
	}

	// ── Pattern hovers: match-arm variant name + field/element binder ───────

	#[test]
	fn hovering_a_match_arm_variant_name_shows_its_declaration() {
		let uri: Uri = "file:///hover_pattern_variant.nym".parse().unwrap();
		let text = "enum Shape { Circle(radius: int) }\nfunc f(s: Shape): int = match (s) { Circle(radius) -> radius }";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let offset = text.find("Circle(radius) ->").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover_fixture(&docs, &mut cache, &params(&uri, line, character));
		let hover = result.expect("hovering the arm's variant name should resolve");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert_eq!(value, code("Shape.Circle(radius: int)"));
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hovering_a_match_arm_field_binder_shows_its_type() {
		let uri: Uri = "file:///hover_pattern_binder.nym".parse().unwrap();
		let text = "enum Shape { Circle(radius: int) }\nfunc f(s: Shape): int = match (s) { Circle(radius) -> radius }";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let offset = text.find("Circle(radius) ->").unwrap() + "Circle(".len() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover_fixture(&docs, &mut cache, &params(&uri, line, character));
		let hover = result.expect("hovering the arm's field binder should resolve");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert_eq!(value, code("int"));
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	#[test]
	fn hovering_a_for_loop_binder_shows_the_element_type() {
		let uri: Uri = "file:///hover_for_binder.nym".parse().unwrap();
		let text = "func main(): int = {\n  let xs = #[1, 2, 3]\n  for (x in xs) { x }\n  0\n}";
		let docs = docs_with(&uri, text);
		let mut cache = crate::compiler_state::CompilerState::new();

		let offset = text.find("for (x in").unwrap() + "for (".len();
		let (line, character) = line_col(text, offset);
		let result = hover_fixture(&docs, &mut cache, &params(&uri, line, character));
		let hover = result.expect("hovering the for-loop binder should resolve");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => {
				assert_eq!(value, code("int"));
			}
			other => panic!("expected plain-text markup, got {other:?}"),
		}
	}

	/// Convert a byte `offset` into `text` to an LSP `(line, character)` pair
	/// (both zero-based, `character` counted in UTF-16 units — ASCII-only
	/// fixtures here, so byte == UTF-16 unit).
	fn line_col(text: &str, offset: usize) -> (u32, u32) {
		let before = &text[..offset];
		let line = before.matches('\n').count() as u32;
		let col = match before.rfind('\n') {
			Some(nl) => before[nl + 1..].chars().count(),
			None => before.chars().count(),
		};
		(line, col as u32)
	}
}
