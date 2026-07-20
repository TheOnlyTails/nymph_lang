//! `textDocument/completion`: a conservative MVP — in-scope identifiers
//! (locals/params visible at the cursor, plus every top-level declaration
//! name) and the language's keywords, ranked exact-prefix-first.
//!
//! Member completion after a `.` (fields/methods available on a receiver's
//! type) needs the checker's member resolution (`InherentRegistry`, built
//! from the private `Checker`) and the receiver's inferred `Ty` — neither is
//! reachable additively from this crate without touching `check.rs`, which
//! is out of scope here. So a dot-triggered request answers an empty list
//! rather than guessing; a follow-up query.rs addition ("members of a `Ty`")
//! would be the natural way to add it later.

use std::collections::HashSet;

use lsp_types::{CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse};
use nymph_ast::decl::Declaration;

use crate::{document_store::DocumentStore, line_index::LineIndex};

/// Every keyword in the surface grammar (`nymph_syntax`'s `keyword_or_ident`
/// lexer table). `_` is a pattern placeholder, not a keyword, so it's
/// intentionally excluded.
const KEYWORDS: &[&str] = &[
	"true",
	"false",
	"public",
	"internal",
	"private",
	"import",
	"with",
	"async",
	"await",
	"type",
	"struct",
	"enum",
	"let",
	"mut",
	"external",
	"func",
	"interface",
	"impl",
	"namespace",
	"for",
	"while",
	"if",
	"else",
	"match",
	"int",
	"uint",
	"float",
	"boolean",
	"char",
	"string",
	"void",
	"never",
	"self",
	"as",
	"is",
	"in",
	"return",
	"break",
	"continue",
	"this",
];

/// Answer a completion request: `None` when the document isn't open.
/// Otherwise always `Some` (an empty list is a valid answer, e.g. right after
/// a `.` — see the module doc comment).
pub fn completion(docs: &DocumentStore, params: &CompletionParams) -> Option<CompletionResponse> {
	let uri = &params.text_document_position.text_document.uri;
	let position = params.text_document_position.position;
	let doc = docs.get(uri)?;

	let triggered_by_dot = params
		.context
		.as_ref()
		.and_then(|c| c.trigger_character.as_deref())
		== Some(".");
	if triggered_by_dot {
		return Some(CompletionResponse::Array(Vec::new()));
	}

	let parsed = nymph_syntax::parse_module(&doc.text, uri.path().as_str());
	let index = LineIndex::new(&doc.text);
	let offset = index.offset(&doc.text, position);
	let prefix = identifier_prefix(&doc.text, offset);

	let mut items = Vec::new();
	let mut seen: HashSet<String> = HashSet::new();

	// Locals/params in scope, innermost (nearest shadowing candidate) first.
	for name in nymph_sema::query::scope_names_at(&parsed.tree, offset) {
		if seen.insert(name.clone()) {
			items.push(CompletionItem {
				label: name,
				kind: Some(CompletionItemKind::VARIABLE),
				..Default::default()
			});
		}
	}

	// Top-level declarations, with a proper kind each.
	for (name, kind) in top_level_items(&parsed.tree) {
		if seen.insert(name.clone()) {
			items.push(CompletionItem {
				label: name,
				kind: Some(kind),
				..Default::default()
			});
		}
	}

	// Keywords last — locals and top-level names are more likely to be
	// what's wanted, so they get first crack at the sort's tie-breaks below.
	for kw in KEYWORDS {
		if seen.insert((*kw).to_string()) {
			items.push(CompletionItem {
				label: (*kw).to_string(),
				kind: Some(CompletionItemKind::KEYWORD),
				..Default::default()
			});
		}
	}

	if !prefix.is_empty() {
		items.retain(|item| item.label.starts_with(prefix.as_str()));
	}
	// Exact-prefix matches first (all of them are, post-retain, so this is
	// really just a stable alphabetical order — kept as its own sort step
	// since a future relevance signal, e.g. preferring locals, would slot in
	// here without disturbing the rest).
	items.sort_by(|a, b| a.label.cmp(&b.label));

	Some(CompletionResponse::Array(items))
}

/// The identifier characters immediately before `offset`, i.e. the partial
/// word being typed — ASCII-only, matching every keyword and every
/// user-declared name in the surface grammar.
fn identifier_prefix(text: &str, offset: usize) -> String {
	let bytes = text.as_bytes();
	let mut start = offset.min(bytes.len());
	while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
		start -= 1;
	}
	text[start..offset.min(bytes.len())].to_string()
}

/// Every top-level declaration's own name and a matching [`CompletionItemKind`]
/// — the same set `document_symbols` lists, minus the nesting/ranges this
/// doesn't need. Skips `import`/anonymous `impl` blocks, which introduce no
/// name of their own.
fn top_level_items(module: &nymph_ast::decl::Module) -> Vec<(String, CompletionItemKind)> {
	module
		.members
		.iter()
		.filter_map(|decl| match decl {
			Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
				Some((meta.name.0.to_string(), CompletionItemKind::FUNCTION))
			}
			Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
				let name = meta.name.0.as_binding()?;
				let kind = if meta.is_mutable() {
					CompletionItemKind::VARIABLE
				} else {
					CompletionItemKind::CONSTANT
				};
				Some((name.0.to_string(), kind))
			}
			Declaration::Struct { name, .. } => Some((name.0.to_string(), CompletionItemKind::STRUCT)),
			Declaration::Enum { name, .. } => Some((name.0.to_string(), CompletionItemKind::ENUM)),
			Declaration::Interface { name, .. } => {
				Some((name.0.to_string(), CompletionItemKind::INTERFACE))
			}
			Declaration::Namespace { name, .. } => Some((name.0.to_string(), CompletionItemKind::MODULE)),
			Declaration::TypeAlias { meta, .. } => {
				Some((meta.name.0.to_string(), CompletionItemKind::CLASS))
			}
			Declaration::Import { .. } | Declaration::Impl { .. } | Declaration::ImplFor { .. } => None,
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use lsp_types::{
		CompletionContext, CompletionTriggerKind, Position, TextDocumentIdentifier,
		TextDocumentPositionParams, Uri, WorkDoneProgressParams,
	};

	fn docs_with(uri: &Uri, text: &str) -> DocumentStore {
		let mut docs = DocumentStore::default();
		docs.open(uri.clone(), text.to_string(), 1);
		docs
	}

	fn params(
		uri: &Uri,
		line: u32,
		character: u32,
		context: Option<CompletionContext>,
	) -> CompletionParams {
		CompletionParams {
			text_document_position: TextDocumentPositionParams {
				text_document: TextDocumentIdentifier { uri: uri.clone() },
				position: Position { line, character },
			},
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: Default::default(),
			context,
		}
	}

	fn labels(response: CompletionResponse) -> Vec<String> {
		match response {
			CompletionResponse::Array(items) => items.into_iter().map(|i| i.label).collect(),
			CompletionResponse::List(list) => list.items.into_iter().map(|i| i.label).collect(),
		}
	}

	#[test]
	fn includes_top_level_names_locals_and_keywords() {
		let uri: Uri = "file:///complete.nym".parse().unwrap();
		let text = "func helper(): int = 1\nfunc main(): int = {\n  let x = 1\n  x\n}";
		let docs = docs_with(&uri, text);

		// The start of the last line's indentation, before any identifier
		// character — so the prefix is empty and nothing gets filtered out.
		let result = completion(&docs, &params(&uri, 3, 0, None));
		let labels = labels(result.expect("document is open"));

		assert!(
			labels.contains(&"helper".to_string()),
			"expected top-level `helper`, got {labels:?}"
		);
		assert!(
			labels.contains(&"x".to_string()),
			"expected local `x`, got {labels:?}"
		);
		assert!(
			labels.contains(&"func".to_string()),
			"expected keyword `func`, got {labels:?}"
		);
	}

	#[test]
	fn filters_by_the_prefix_being_typed() {
		let uri: Uri = "file:///complete_prefix.nym".parse().unwrap();
		let text = "func first(): int = 1\nfunc second(): int = 1\nfunc main(): int = fi";
		let docs = docs_with(&uri, text);

		// Right after "fi" on the last line.
		let last_line = text.lines().last().unwrap();
		let col = last_line.chars().count() as u32;
		let result = completion(&docs, &params(&uri, 2, col, None));
		let labels = labels(result.expect("document is open"));

		assert!(labels.contains(&"first".to_string()));
		assert!(!labels.contains(&"second".to_string()));
		assert!(
			!labels.contains(&"func".to_string()),
			"keyword `func` doesn't start with `fi`"
		);
	}

	#[test]
	fn a_dot_trigger_returns_an_empty_list_not_a_wrong_guess() {
		let uri: Uri = "file:///complete_dot.nym".parse().unwrap();
		let text = "struct Point(x: int)\nfunc main(): int = Point(x = 0).";
		let docs = docs_with(&uri, text);

		let context = Some(CompletionContext {
			trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
			trigger_character: Some(".".to_string()),
		});
		let last_line = text.lines().last().unwrap();
		let col = last_line.chars().count() as u32;
		let result = completion(&docs, &params(&uri, 1, col, context));
		let labels = labels(result.expect("document is open"));

		assert!(
			labels.is_empty(),
			"member completion is deferred; expected an empty (not wrong) list, got {labels:?}"
		);
	}

	#[test]
	fn returns_none_for_an_unopened_document() {
		let uri: Uri = "file:///complete_missing.nym".parse().unwrap();
		let docs = DocumentStore::default();

		assert!(completion(&docs, &params(&uri, 0, 0, None)).is_none());
	}
}
