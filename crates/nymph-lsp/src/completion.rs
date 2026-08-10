//! `textDocument/completion`: checker-approved members and non-member names
//! visible at the cursor, ranked
//! by lexical scope, resolved project imports, same-module declarations, then
//! keywords. Project requests consume the compiler's immutable analysis
//! snapshot, so dependency overlays, import visibility, and aliases stay under
//! compiler/sema ownership. Loose files retain lexical/same-file completion.
//!
//! Member applicability is computed by sema while its solver, generic bounds,
//! and place-mutability facts are live. This module only converts that immutable
//! snapshot to LSP items.

use std::collections::HashSet;

use lsp_types::{
	CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, CompletionTextEdit,
	TextEdit,
};
use nymph_ast::decl::Declaration;

use crate::{
	compiler_state::CompletionSnapshot, document_store::DocumentStore, line_index::LineIndex,
	position::query_with_whitespace_left_bias,
};

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

/// Answer a loose-file completion request: `None` when the document isn't open.
/// Otherwise always `Some`; member completion is empty because loose files do
/// not have the project semantic snapshot required for checker-owned candidates.
pub fn completion(docs: &DocumentStore, params: &CompletionParams) -> Option<CompletionResponse> {
	let uri = &params.text_document_position.text_document.uri;
	let doc = docs.get(uri)?;
	let parsed = nymph_syntax::parse_module(&doc.text, uri.path().as_str());
	Some(complete(&doc.text, &parsed.tree, &[], None, params))
}

/// Complete from one immutable, revision-tagged project analysis snapshot.
#[must_use]
pub fn completion_snapshot(
	snapshot: &CompletionSnapshot,
	params: &CompletionParams,
) -> CompletionResponse {
	let parsed = nymph_syntax::parse_module(
		&snapshot.source,
		params
			.text_document_position
			.text_document
			.uri
			.path()
			.as_str(),
	);
	complete(
		&snapshot.source,
		&parsed.tree,
		&snapshot.imported_names,
		snapshot.semantic.as_deref(),
		params,
	)
}

fn complete(
	text: &str,
	module: &nymph_ast::decl::Module,
	imported_names: &[nymph_sema::query::ImportedName],
	semantic: Option<&nymph_sema::SemanticAnalysis>,
	params: &CompletionParams,
) -> CompletionResponse {
	let position = params.text_document_position.position;

	let index = LineIndex::new(text);
	let offset = index.offset(text, position);
	let (prefix, prefix_scope_offset, prefix_token_end) = identifier_prefix(text, offset);
	let prefix_start = offset.saturating_sub(prefix.len());
	if prefix_start > 0 && text.as_bytes().get(prefix_start - 1) == Some(&b'.') {
		let Some(semantic) = semantic else {
			return CompletionResponse::Array(Vec::new());
		};
		let range = lsp_types::Range {
			start: index.position(text, prefix_start),
			end: index.position(text, prefix_token_end),
		};
		// Sema's position contract is strictly half-open. Query at the previous
		// Unicode scalar (the dot for an empty prefix, otherwise the final prefix
		// scalar), while the edit replaces the complete identifier token.
		let query_offset = text[..offset]
			.char_indices()
			.next_back()
			.map_or(offset, |(scalar, _)| scalar);
		let mut candidates = nymph_sema::query::member_completions_at(semantic, query_offset)
			.into_iter()
			.filter(|candidate| candidate.name.starts_with(&prefix))
			.collect::<Vec<_>>();
		candidates.sort_by(|a, b| a.name.cmp(&b.name).then(a.kind.cmp(&b.kind)));
		candidates.dedup_by(|a, b| a.name == b.name);
		return CompletionResponse::Array(
			candidates
				.into_iter()
				.map(|candidate| CompletionItem {
					label: candidate.name.to_string(),
					kind: Some(member_kind(candidate.kind)),
					detail: Some(candidate.detail),
					sort_text: Some(format!("0:{}", candidate.name)),
					text_edit: Some(CompletionTextEdit::Edit(TextEdit {
						range,
						new_text: candidate.name.to_string(),
					})),
					..Default::default()
				})
				.collect(),
		);
	}

	let mut items = Vec::new();
	let mut seen: HashSet<String> = HashSet::new();

	// Locals/params in scope, innermost (nearest shadowing candidate) first.
	let scope_names = query_with_whitespace_left_bias(text, offset, |candidate| {
		nymph_sema::query::scope_names_at_exact(module, candidate)
	})
	.or_else(|| {
		prefix_scope_offset
			.and_then(|candidate| nymph_sema::query::scope_names_at_exact(module, candidate))
	})
	.unwrap_or_default();
	push_tier(
		&mut items,
		&mut seen,
		scope_names
			.into_iter()
			.map(|name| (name, CompletionItemKind::VARIABLE)),
		&prefix,
		0,
	);

	push_tier(
		&mut items,
		&mut seen,
		imported_names
			.iter()
			.map(|imported| (imported.name.clone(), imported_kind(imported.kind))),
		&prefix,
		1,
	);

	// Top-level declarations, with a proper kind each.
	push_tier(&mut items, &mut seen, top_level_items(module), &prefix, 2);

	push_tier(
		&mut items,
		&mut seen,
		KEYWORDS
			.iter()
			.map(|keyword| ((*keyword).to_string(), CompletionItemKind::KEYWORD)),
		&prefix,
		3,
	);

	CompletionResponse::Array(items)
}

fn member_kind(kind: nymph_sema::MemberCompletionKind) -> CompletionItemKind {
	match kind {
		nymph_sema::MemberCompletionKind::Field => CompletionItemKind::FIELD,
		nymph_sema::MemberCompletionKind::Method => CompletionItemKind::METHOD,
		nymph_sema::MemberCompletionKind::Function => CompletionItemKind::FUNCTION,
		nymph_sema::MemberCompletionKind::Value => CompletionItemKind::CONSTANT,
		nymph_sema::MemberCompletionKind::Variable => CompletionItemKind::VARIABLE,
		nymph_sema::MemberCompletionKind::Variant => CompletionItemKind::ENUM_MEMBER,
	}
}

fn push_tier(
	items: &mut Vec<CompletionItem>,
	seen: &mut HashSet<String>,
	candidates: impl IntoIterator<Item = (String, CompletionItemKind)>,
	prefix: &str,
	rank: u8,
) {
	let mut tier = candidates
		.into_iter()
		.filter(|(name, _)| prefix.is_empty() || name.starts_with(prefix))
		.filter(|(name, _)| seen.insert(name.clone()))
		.map(|(label, kind)| CompletionItem {
			sort_text: Some(format!("{rank}:{label}")),
			label,
			kind: Some(kind),
			..Default::default()
		})
		.collect::<Vec<_>>();
	tier.sort_by(|left, right| left.label.cmp(&right.label));
	items.extend(tier);
}

fn imported_kind(kind: nymph_sema::query::ImportedNameKind) -> CompletionItemKind {
	use nymph_sema::query::ImportedNameKind;
	match kind {
		ImportedNameKind::Function => CompletionItemKind::FUNCTION,
		ImportedNameKind::Value => CompletionItemKind::CONSTANT,
		ImportedNameKind::Variable => CompletionItemKind::VARIABLE,
		ImportedNameKind::TypeAlias => CompletionItemKind::CLASS,
		ImportedNameKind::Struct => CompletionItemKind::STRUCT,
		ImportedNameKind::Enum => CompletionItemKind::ENUM,
		ImportedNameKind::Interface => CompletionItemKind::INTERFACE,
		ImportedNameKind::Namespace => CompletionItemKind::MODULE,
		ImportedNameKind::Variant => CompletionItemKind::ENUM_MEMBER,
	}
}

/// The lexer-recognized identifier characters immediately before `offset`,
/// a scalar position inside that token for completion's half-open scope retry,
/// and the token end for a replacement edit. This intentionally reuses the
/// language's Unicode identifier rules.
fn identifier_prefix(text: &str, offset: usize) -> (String, Option<usize>, usize) {
	let offset = offset.min(text.len());
	let Some(token) = nymph_syntax::lex(text)
		.tokens
		.into_iter()
		.find(|token| token.1.start < offset && offset <= token.1.end)
	else {
		return (String::new(), None, offset);
	};
	let lexeme = &text[token.1.start..token.1.end];
	if !matches!(token.0, nymph_ast::token::Token::Identifier(_))
		&& lexeme != "_"
		&& !KEYWORDS.contains(&lexeme)
	{
		return (String::new(), None, offset);
	}
	let prefix = &text[token.1.start..offset];
	let scope_offset = prefix
		.char_indices()
		.next_back()
		.map(|(relative, _)| token.1.start + relative);
	(prefix.to_string(), scope_offset, token.1.end)
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
	fn completes_a_parameter_at_the_half_open_end_of_a_typed_prefix() {
		let uri: Uri = "file:///complete_parameter_prefix.nym".parse().unwrap();
		let text = "func main(parameter: int): int = par";
		let docs = docs_with(&uri, text);
		let character = text.encode_utf16().count() as u32;

		let result = completion(&docs, &params(&uri, 0, character, None));
		let labels = labels(result.expect("document is open"));

		assert!(labels.contains(&"parameter".to_string()), "got {labels:?}");
	}

	#[test]
	fn filters_with_an_astral_unicode_identifier_prefix() {
		let uri: Uri = "file:///complete_unicode_prefix.nym".parse().unwrap();
		let text = "func 𝔘value(): int = 1\nfunc main(): int = 𝔘v";
		let docs = docs_with(&uri, text);
		let character = text.lines().last().unwrap().encode_utf16().count() as u32;

		let result = completion(&docs, &params(&uri, 1, character, None));
		let labels = labels(result.expect("document is open"));

		assert_eq!(labels, vec!["𝔘value".to_string()]);
	}

	#[test]
	fn completion_left_biases_after_whitespace_but_not_punctuation_or_comments() {
		let uri: Uri = "file:///complete_bias.nym".parse().unwrap();
		for (tail, includes_parameter) in [
			("parameter   ", true),
			("parameter,   ", false),
			("parameter // note", false),
		] {
			let text = format!("func main(parameter: int): int = {tail}");
			let docs = docs_with(&uri, &text);
			let character = text.encode_utf16().count() as u32;
			let result = completion(&docs, &params(&uri, 0, character, None));
			let labels = labels(result.expect("document is open"));
			assert_eq!(
				labels.contains(&"parameter".to_string()),
				includes_parameter,
				"tail {tail:?}: {labels:?}"
			);
		}
	}

	#[test]
	fn a_loose_file_dot_trigger_returns_an_empty_list_not_a_wrong_guess() {
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
			"loose files have no semantic member snapshot; expected no guesses, got {labels:?}"
		);
	}

	#[test]
	fn returns_none_for_an_unopened_document() {
		let uri: Uri = "file:///complete_missing.nym".parse().unwrap();
		let docs = DocumentStore::default();

		assert!(completion(&docs, &params(&uri, 0, 0, None)).is_none());
	}
}
