//! `textDocument/hover`: the type of the smallest checked expression under
//! the cursor.
//!
//! Diagnostics (see [`crate::diagnostics`]) go through the prelude-aware
//! facade (`nymph_compiler::check`/`check_project_library`), which never
//! hands back the `Checked` annotations a type-at-position query needs — so
//! hover runs its own, separate check directly against `nymph_sema`
//! (`nymph_sema::check_module`, no prelude; see
//! `nymph_sema::query::type_at`'s doc comment for exactly what that trades
//! away: operator-only expressions relying on the prelude, e.g. bare
//! `1 + 2`, may under-resolve, while every literal, binding, and
//! user-declared ADT still resolves correctly). [`HoverCache`] keeps that
//! check's result around per document version so repeated hovers over the
//! same unchanged buffer don't re-check it.

use std::collections::HashMap;

use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};
use nymph_ast::decl::Module;
use nymph_sema::Checked;

use crate::{document_store::DocumentStore, line_index::LineIndex};

struct CacheEntry {
	version: i32,
	module: Module,
	checked: Checked,
}

/// A one-entry-per-document cache of the last `(version, parse, check)`, so
/// a hover request re-checks a document only when its text actually
/// changed since the last hover.
#[derive(Default)]
pub struct HoverCache {
	entries: HashMap<String, CacheEntry>,
}

impl HoverCache {
	fn get_or_check(&mut self, uri_key: &str, version: i32, text: &str, path: &str) -> &CacheEntry {
		let stale = match self.entries.get(uri_key) {
			Some(entry) => entry.version != version,
			None => true,
		};
		if stale {
			let parsed = nymph_syntax::parse_module(text, path);
			let checked = nymph_sema::check_module(&parsed.tree);
			self.entries.insert(
				uri_key.to_string(),
				CacheEntry {
					version,
					module: parsed.tree,
					checked,
				},
			);
		}
		self
			.entries
			.get(uri_key)
			.expect("just inserted or already present")
	}
}

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
pub fn hover(docs: &DocumentStore, cache: &mut HoverCache, params: &HoverParams) -> Option<Hover> {
	let uri = &params.text_document_position_params.text_document.uri;
	let position = params.text_document_position_params.position;
	let doc = docs.get(uri)?;

	let uri_key = uri.as_str().to_string();
	let entry = cache.get_or_check(&uri_key, doc.version, &doc.text, uri.path().as_str());

	let index = LineIndex::new(&doc.text);
	let offset = index.offset(&doc.text, position);

	let value = if let Some(code) = nymph_sema::query::type_at(&entry.module, &entry.checked, offset)
	{
		format!(
			r"```nymph
{code}
```"
		)
	} else {
		let kw_doc = nymph_sema::query::keyword_doc_at(&doc.text, offset)?;
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
mod tests {
	use super::*;
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
		let mut cache = HoverCache::default();

		// The `1` initializer sits on line 1 (0-based), at column 15 — see
		// the layout comment below.
		//   "  let x: int = 1"
		//    0123456789012345
		let result = hover(&docs, &mut cache, &params(&uri, 1, 15));
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
		let mut cache = HoverCache::default();

		// Column 12 on line 0 is the single space between `:` and `void` in
		// the return-type annotation — `:` is not a keyword (so
		// `keyword_doc_at` doesn't claim its inclusive-end boundary), and
		// the signature is never an `Expr` (so `type_at` never annotates
		// it either).
		//   "func main(): void = {"
		//    0123456789012345678901
		let result = hover(&docs, &mut cache, &params(&uri, 0, 12));
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
		let mut cache = HoverCache::default();

		let result = hover(&docs, &mut cache, &params(&uri, 0, 5));
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
		let mut cache = HoverCache::default();

		//   "  let x: int = 1"
		//    0123456789012345
		let result = hover(&docs, &mut cache, &params(&uri, 1, 3));
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
		let mut cache = HoverCache::default();

		//   "  x"
		//    012
		let result = hover(&docs, &mut cache, &params(&uri, 2, 2));
		let hover = result.expect("hovering a var use should resolve a type");
		match hover.contents {
			HoverContents::Markup(MarkupContent { value, .. }) => assert_eq!(value, code("int")),
			other => panic!("expected plain-text markup, got {other:?}"),
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
		let mut cache = HoverCache::default();

		//   "func main(): int = helper()"
		//    0123456789012345678901234567
		// Column 26 is the `)` — covered by the `Call` but not by the
		// `helper` `Identifier` (which ends at column 25).
		let result = hover(&docs, &mut cache, &params(&uri, 1, 26));
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
		let mut cache = HoverCache::default();

		// Column 21 lands inside `helper`, the callee identifier itself.
		let result = hover(&docs, &mut cache, &params(&uri, 1, 21));
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
		let mut cache = HoverCache::default();

		//   "func id<V>(v: V): V = v"
		//    01234567890123456789012
		let result = hover(&docs, &mut cache, &params(&uri, 0, 22));
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
		let mut cache = HoverCache::default();

		let offset = text.find("Point").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover(&docs, &mut cache, &params(&uri, line, character));
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
		let mut cache = HoverCache::default();

		let offset = text.find("Shape").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover(&docs, &mut cache, &params(&uri, line, character));
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
		let mut cache = HoverCache::default();

		let offset = text.find("add").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover(&docs, &mut cache, &params(&uri, line, character));
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
		let mut cache = HoverCache::default();

		let offset = text.find("T: Area").unwrap(); // the param's own `T`
		let (line, character) = line_col(text, offset);
		let result = hover(&docs, &mut cache, &params(&uri, line, character));
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
		let text = "func main(): void = {\n  for i in 1..3 { }\n}";
		let docs = docs_with(&uri, text);
		let mut cache = HoverCache::default();

		let offset = text.find("for").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover(&docs, &mut cache, &params(&uri, line, character));
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
		let mut cache = HoverCache::default();

		let offset = text.find('+').unwrap();
		let (line, character) = line_col(text, offset);
		let result = hover(&docs, &mut cache, &params(&uri, line, character));
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
		let mut cache = HoverCache::default();

		let offset = text.find("Circle(radius) ->").unwrap() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover(&docs, &mut cache, &params(&uri, line, character));
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
		let mut cache = HoverCache::default();

		let offset = text.find("Circle(radius) ->").unwrap() + "Circle(".len() + 1;
		let (line, character) = line_col(text, offset);
		let result = hover(&docs, &mut cache, &params(&uri, line, character));
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
		let text = "func main(): int = {\n  let xs = #[1, 2, 3]\n  for x in xs { x }\n  0\n}";
		let docs = docs_with(&uri, text);
		let mut cache = HoverCache::default();

		let offset = text.find("for x in").unwrap() + "for ".len();
		let (line, character) = line_col(text, offset);
		let result = hover(&docs, &mut cache, &params(&uri, line, character));
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
