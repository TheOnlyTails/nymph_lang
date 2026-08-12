//! `textDocument/documentSymbol`: a flat-per-declaration, nested-per-member
//! outline of the document — pure parser, no checker. Reuses the exact
//! best-effort parse hover already relies on (`nymph_syntax::parse_module`),
//! so an outline stays available even over a syntactically broken buffer.

use lsp_types::{DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Range, SymbolKind};
use nymph_ast::decl::Declaration;
use nymph_sema::DeclarationCategory;

use crate::{
	analysis_scheduler::{CancellationToken, TaskError},
	document_store::DocumentStore,
	line_index::LineIndex,
};

/// Answer a documentSymbol request: `None` when the document isn't open.
/// Otherwise always `Some`, even for a module with zero symbols (an empty
/// list), matching the LSP convention that "no symbols" and "not supported"
/// are distinguished by presence of the capability, not by this return type.
pub fn document_symbols(
	docs: &DocumentStore,
	params: &DocumentSymbolParams,
) -> Option<DocumentSymbolResponse> {
	document_symbols_cancellable(docs, params, &CancellationToken::default())
		.ok()
		.flatten()
}

pub(crate) fn document_symbols_cancellable(
	docs: &DocumentStore,
	params: &DocumentSymbolParams,
	cancellation: &CancellationToken,
) -> Result<Option<DocumentSymbolResponse>, TaskError> {
	let uri = &params.text_document.uri;
	let Some(doc) = docs.get(uri) else {
		return Ok(None);
	};

	let parsed = nymph_syntax::parse_module(&doc.text, uri.path().as_str());
	let index = LineIndex::new(&doc.text);

	let mut symbols = Vec::new();
	for decl in &parsed.tree.members {
		cancellation.checkpoint()?;
		if let Some(symbol) = decl_symbol(decl, &doc.text, &index, cancellation)? {
			symbols.push(symbol);
		}
	}

	Ok(Some(DocumentSymbolResponse::Nested(symbols)))
}

/// `DocumentSymbol::deprecated` is `#[deprecated]` in lsp-types 0.97 (kept
/// only for wire compatibility; clients should read `tags` instead) — this is
/// the one place that must still construct the struct literal.
#[allow(deprecated)]
fn make_symbol(
	name: &str,
	kind: SymbolKind,
	range: Range,
	selection_range: Range,
	children: Option<Vec<DocumentSymbol>>,
) -> DocumentSymbol {
	DocumentSymbol {
		name: name.to_string(),
		detail: None,
		kind,
		tags: None,
		deprecated: None,
		range,
		selection_range,
		children,
	}
}

/// Shared semantic declaration-category to LSP symbol-kind mapping.
#[must_use]
pub(crate) fn symbol_kind(category: DeclarationCategory) -> SymbolKind {
	match category {
		DeclarationCategory::Function => SymbolKind::FUNCTION,
		DeclarationCategory::Method | DeclarationCategory::MethodBody => SymbolKind::METHOD,
		DeclarationCategory::Let | DeclarationCategory::Static => SymbolKind::CONSTANT,
		DeclarationCategory::TypeAlias => SymbolKind::CLASS,
		DeclarationCategory::Struct => SymbolKind::STRUCT,
		DeclarationCategory::Enum => SymbolKind::ENUM,
		DeclarationCategory::Interface => SymbolKind::INTERFACE,
		DeclarationCategory::Namespace => SymbolKind::NAMESPACE,
		DeclarationCategory::Variant => SymbolKind::ENUM_MEMBER,
		DeclarationCategory::Field => SymbolKind::FIELD,
		DeclarationCategory::Implementation => SymbolKind::OBJECT,
		DeclarationCategory::Effect => SymbolKind::CLASS,
	}
}

/// One top-level [`Declaration`] as a [`DocumentSymbol`], or `None` for
/// declarations that introduce no named symbol of their own (`import`, and
/// anonymous `impl`/`impl … for …` blocks — see `nymph_sema::def::build_def_map`,
/// which likewise skips them).
fn decl_symbol(
	decl: &Declaration,
	text: &str,
	index: &LineIndex,
	cancellation: &CancellationToken,
) -> Result<Option<DocumentSymbol>, TaskError> {
	Ok(match decl {
		Declaration::Effect { name, .. } => {
			let selection = index.range(text, name.1);
			Some(make_symbol(
				&name.0,
				symbol_kind(DeclarationCategory::Effect),
				selection,
				selection,
				None,
			))
		}
		Declaration::Func { meta, body, .. } => {
			let selection = index.range(text, meta.name.1);
			let full = index.range(text, meta.name.1.to(body.span));
			Some(make_symbol(
				&meta.name.0,
				symbol_kind(DeclarationCategory::Function),
				full,
				selection,
				None,
			))
		}
		Declaration::ExternalFunc(_, _, meta) => {
			let selection = index.range(text, meta.name.1);
			Some(make_symbol(
				&meta.name.0,
				symbol_kind(DeclarationCategory::Function),
				selection,
				selection,
				None,
			))
		}
		Declaration::Let { meta, value, .. } => {
			let Some(name) = meta.name.0.as_binding() else {
				return Ok(None);
			};
			let selection = index.range(text, name.1);
			let full = index.range(text, name.1.to(value.span));
			let kind = symbol_kind(DeclarationCategory::Let);
			Some(make_symbol(&name.0, kind, full, selection, None))
		}
		Declaration::ExternalLet(_, _, meta) => {
			let Some(name) = meta.name.0.as_binding() else {
				return Ok(None);
			};
			let selection = index.range(text, name.1);
			let kind = symbol_kind(DeclarationCategory::Let);
			Some(make_symbol(&name.0, kind, selection, selection, None))
		}
		Declaration::Struct { name, fields, .. } => {
			let selection = index.range(text, name.1);
			let mut whole = name.1;
			let mut children = Vec::new();
			for f in fields {
				cancellation.checkpoint()?;
				whole = whole.to(f.1);
				let field_selection = index.range(text, f.0.name.1);
				children.push(make_symbol(
					&f.0.name.0,
					symbol_kind(DeclarationCategory::Field),
					field_selection,
					field_selection,
					None,
				));
			}
			let full = index.range(text, whole);
			let children = (!children.is_empty()).then_some(children);
			Some(make_symbol(
				&name.0,
				symbol_kind(DeclarationCategory::Struct),
				full,
				selection,
				children,
			))
		}
		Declaration::Enum {
			name,
			embeddings,
			variants,
			..
		} => {
			let selection = index.range(text, name.1);
			let mut whole = name.1;
			let mut children = Vec::new();
			for embedding in embeddings {
				whole = whole.to(embedding.1);
			}
			for v in variants {
				cancellation.checkpoint()?;
				whole = whole.to(v.1);
				let variant_selection = index.range(text, v.0.name.1);
				children.push(make_symbol(
					&v.0.name.0,
					symbol_kind(DeclarationCategory::Variant),
					variant_selection,
					variant_selection,
					None,
				));
			}
			let full = index.range(text, whole);
			let children = (!children.is_empty()).then_some(children);
			Some(make_symbol(
				&name.0,
				symbol_kind(DeclarationCategory::Enum),
				full,
				selection,
				children,
			))
		}
		Declaration::Interface { name, .. } => {
			let selection = index.range(text, name.1);
			Some(make_symbol(
				&name.0,
				symbol_kind(DeclarationCategory::Interface),
				selection,
				selection,
				None,
			))
		}
		Declaration::Namespace { name, .. } => {
			let selection = index.range(text, name.1);
			Some(make_symbol(
				&name.0,
				symbol_kind(DeclarationCategory::Namespace),
				selection,
				selection,
				None,
			))
		}
		Declaration::TypeAlias { meta, value, .. } => {
			let selection = index.range(text, meta.name.1);
			let full = index.range(text, meta.name.1.to(value.1));
			Some(make_symbol(
				&meta.name.0,
				symbol_kind(DeclarationCategory::TypeAlias),
				full,
				selection,
				None,
			))
		}
		Declaration::Import { .. } | Declaration::Impl { .. } | Declaration::ImplFor { .. } => None,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use lsp_types::{
		DocumentSymbolParams, PartialResultParams, TextDocumentIdentifier, Uri, WorkDoneProgressParams,
	};

	fn docs_with(uri: &Uri, text: &str) -> DocumentStore {
		let mut docs = DocumentStore::default();
		docs.open(uri.clone(), text.to_string(), 1);
		docs
	}

	fn params(uri: &Uri) -> DocumentSymbolParams {
		DocumentSymbolParams {
			text_document: TextDocumentIdentifier { uri: uri.clone() },
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: PartialResultParams::default(),
		}
	}

	fn nested(response: DocumentSymbolResponse) -> Vec<DocumentSymbol> {
		match response {
			DocumentSymbolResponse::Nested(symbols) => symbols,
			DocumentSymbolResponse::Flat(_) => panic!("expected the Nested arm"),
		}
	}

	#[test]
	fn lists_every_top_level_declaration_kind() {
		let uri: Uri = "file:///symbols.nym".parse().unwrap();
		let text = "\
func add(a: int, b: int): int = a + b
let total = 0
effect Io
struct Point(x: int, y: int)
enum Color { Red, Green, Blue }
interface Shape { func area(): float }
namespace Constants { namespace let pi = 3 }
type Pair<A, B> = #(A, B)
";
		let docs = docs_with(&uri, text);

		let response = document_symbols(&docs, &params(&uri)).expect("document is open");
		let symbols = nested(response);

		let by_name: std::collections::HashMap<_, _> =
			symbols.iter().map(|s| (s.name.as_str(), s)).collect();

		assert_eq!(by_name["add"].kind, SymbolKind::FUNCTION);
		assert_eq!(by_name["total"].kind, SymbolKind::CONSTANT);
		assert_eq!(by_name["Io"].kind, SymbolKind::CLASS);
		assert_eq!(by_name["Point"].kind, SymbolKind::STRUCT);
		assert_eq!(by_name["Color"].kind, SymbolKind::ENUM);
		assert_eq!(by_name["Shape"].kind, SymbolKind::INTERFACE);
		assert_eq!(by_name["Constants"].kind, SymbolKind::NAMESPACE);
		assert_eq!(by_name["Pair"].kind, SymbolKind::CLASS);
		assert_eq!(symbols.len(), 8);
	}

	#[test]
	fn nests_struct_fields_and_enum_variants_as_children() {
		let uri: Uri = "file:///symbols_nested.nym".parse().unwrap();
		let text = "struct Point(x: int, y: int)\nenum Color { Red, Green }\n";
		let docs = docs_with(&uri, text);

		let response = document_symbols(&docs, &params(&uri)).expect("document is open");
		let symbols = nested(response);

		let point = symbols.iter().find(|s| s.name == "Point").unwrap();
		let fields: Vec<&str> = point
			.children
			.as_ref()
			.expect("Point should have field children")
			.iter()
			.map(|f| f.name.as_str())
			.collect();
		assert_eq!(fields, vec!["x", "y"]);
		assert!(
			point
				.children
				.as_ref()
				.unwrap()
				.iter()
				.all(|f| f.kind == SymbolKind::FIELD)
		);

		let color = symbols.iter().find(|s| s.name == "Color").unwrap();
		let variants: Vec<&str> = color
			.children
			.as_ref()
			.expect("Color should have variant children")
			.iter()
			.map(|v| v.name.as_str())
			.collect();
		assert_eq!(variants, vec!["Red", "Green"]);
		assert!(
			color
				.children
				.as_ref()
				.unwrap()
				.iter()
				.all(|v| v.kind == SymbolKind::ENUM_MEMBER)
		);
	}

	#[test]
	fn selection_range_is_contained_in_the_full_range() {
		let uri: Uri = "file:///symbols_ranges.nym".parse().unwrap();
		let text = "func add(a: int, b: int): int = a + b\n";
		let docs = docs_with(&uri, text);

		let response = document_symbols(&docs, &params(&uri)).expect("document is open");
		let symbols = nested(response);
		let add = &symbols[0];

		assert!(add.range.start <= add.selection_range.start);
		assert!(add.selection_range.end <= add.range.end);
	}

	#[test]
	fn skips_imports_and_anonymous_impl_blocks() {
		let uri: Uri = "file:///symbols_skip.nym".parse().unwrap();
		let text = "import @/math\nstruct Point(x: int)\nimpl Point { func zero(): int = 0 }\n";
		let docs = docs_with(&uri, text);

		let response = document_symbols(&docs, &params(&uri)).expect("document is open");
		let symbols = nested(response);

		// Only `Point` gets a top-level symbol — the import and the impl
		// block are anonymous.
		assert_eq!(symbols.len(), 1);
		assert_eq!(symbols[0].name, "Point");
	}

	#[test]
	fn survives_a_syntactically_broken_buffer() {
		let uri: Uri = "file:///symbols_broken.nym".parse().unwrap();
		// Missing closing brace — the parser recovers a best-effort tree.
		let text = "func add(a: int, b: int): int = a + b\nstruct Broken {";
		let docs = docs_with(&uri, text);

		let response = document_symbols(&docs, &params(&uri));
		assert!(
			response.is_some(),
			"expected an outline even over a broken buffer"
		);
	}

	#[test]
	fn returns_none_for_an_unopened_document() {
		let uri: Uri = "file:///symbols_missing.nym".parse().unwrap();
		let docs = DocumentStore::default();

		assert_eq!(document_symbols(&docs, &params(&uri)), None);
	}

	#[test]
	fn cancellation_interrupts_struct_children_after_progress() {
		let uri: Uri = "file:///symbols_cancel.nym".parse().unwrap();
		let docs = docs_with(&uri, "struct Many(first: int, second: int, third: int)");
		let cancellation = CancellationToken::cancel_after(2);
		assert!(matches!(
			document_symbols_cancellable(&docs, &params(&uri), &cancellation),
			Err(TaskError::Cancelled)
		));
	}
}
