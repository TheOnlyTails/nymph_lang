//! Semantic prepare-rename and rename edits over a complete project snapshot.

use std::{path::PathBuf, sync::Arc};

use lsp_types::{
	DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier, Position, PrepareRenameResponse,
	Range, TextDocumentEdit, TextEdit, WorkspaceEdit,
};
use nymph_ast::token::Token;

use crate::{
	compiler_state::{AnalysisSnapshot, CompilerState},
	document_store::DocumentStore,
	line_index::LineIndex,
};

pub(crate) struct RenameCandidate {
	prepare: PrepareRenameResponse,
	edit: WorkspaceEdit,
	disk_sources: Vec<(PathBuf, Arc<str>)>,
}

impl RenameCandidate {
	pub(crate) fn validate_prepare(self) -> Option<PrepareRenameResponse> {
		self.disk_sources_are_current().then_some(self.prepare)
	}

	pub(crate) fn validate_disk_sources(self) -> Option<WorkspaceEdit> {
		self.disk_sources_are_current().then_some(self.edit)
	}

	fn disk_sources_are_current(&self) -> bool {
		for (path, expected) in &self.disk_sources {
			let Ok(current) = std::fs::read_to_string(path) else {
				return false;
			};
			if current.as_str() != expected.as_ref() {
				return false;
			}
		}
		true
	}
}

#[must_use]
pub(crate) fn valid_new_name(name: &str) -> bool {
	is_identifier_source(name)
}

fn is_identifier_source(source: &str) -> bool {
	let lexed = nymph_syntax::lex(source);
	lexed.diagnostics.is_empty()
		&& matches!(lexed.tokens.as_slice(), [token] if matches!(token.0, Token::Identifier(_)) && token.1.start == 0 && token.1.end == source.len())
}

pub(crate) fn rename_candidate(
	docs: &DocumentStore,
	state: &CompilerState,
	snapshot: &AnalysisSnapshot,
	position: Position,
	new_name: &str,
) -> Option<RenameCandidate> {
	let offset = LineIndex::new(&snapshot.source).exact_offset(&snapshot.source, position)?;
	let symbol = nymph_sema::query::symbol_at(&snapshot.analysis.semantic, offset)?;
	if matches!(symbol, nymph_sema::query::SymbolIdentity::Module(_)) {
		return None;
	}
	let mut modules = state.reference_modules(docs, snapshot, &symbol)?;
	if modules
		.iter()
		.any(|module| !module.occurrences_are_uniquely_editable)
	{
		return None;
	}
	if !modules
		.iter()
		.flat_map(|module| &module.occurrences)
		.any(|occurrence| occurrence.is_declaration)
	{
		return None;
	}
	let selected = nymph_sema::query::rename_occurrences(&snapshot.analysis.semantic, &symbol)?
		.into_iter()
		.find(|occurrence| occurrence.span.start <= offset && offset < occurrence.span.end)?;
	let selected_range = LineIndex::new(&snapshot.source).range(&snapshot.source, selected.span);
	let placeholder = snapshot
		.source
		.get(selected.span.start..selected.span.end)?
		.to_string();
	if !is_identifier_source(&placeholder) {
		return None;
	}

	modules.sort_by(|left, right| left.uri.as_str().cmp(right.uri.as_str()));
	let mut document_edits = Vec::new();
	let mut disk_sources = Vec::new();
	for module in modules {
		if module.occurrences.iter().any(|occurrence| {
			module
				.source
				.get(occurrence.span.start..occurrence.span.end)
				.is_none_or(|source| !is_identifier_source(source))
		}) {
			return None;
		}
		if module.requires_disk_validation {
			disk_sources.push((
				crate::workspace::uri_to_path(&module.uri)?,
				module.source.clone(),
			));
		}
		let index = LineIndex::new(&module.source);
		let mut ranges = module
			.occurrences
			.into_iter()
			.map(|occurrence| index.range(&module.source, occurrence.span))
			.collect::<Vec<_>>();
		ranges.sort_by(range_order);
		ranges.dedup();
		if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
			return None;
		}
		if ranges.is_empty() {
			continue;
		}
		document_edits.push(TextDocumentEdit {
			text_document: OptionalVersionedTextDocumentIdentifier {
				uri: module.uri,
				version: module.document_version,
			},
			edits: ranges
				.into_iter()
				.map(|range| {
					OneOf::Left(TextEdit {
						range,
						new_text: new_name.to_string(),
					})
				})
				.collect(),
		});
	}
	Some(RenameCandidate {
		prepare: PrepareRenameResponse::RangeWithPlaceholder {
			range: selected_range,
			placeholder,
		},
		edit: WorkspaceEdit {
			changes: None,
			document_changes: Some(DocumentChanges::Edits(document_edits)),
			change_annotations: None,
		},
		disk_sources,
	})
}

fn range_order(left: &Range, right: &Range) -> std::cmp::Ordering {
	position_order(left.start, right.start).then_with(|| position_order(left.end, right.end))
}

fn position_order(left: Position, right: Position) -> std::cmp::Ordering {
	left
		.line
		.cmp(&right.line)
		.then_with(|| left.character.cmp(&right.character))
}

#[cfg(test)]
mod tests {
	use super::*;
	use lsp_types::Uri;

	fn candidate(source: &str, needle: &str, occurrence: usize) -> Option<RenameCandidate> {
		let uri: Uri = "untitled:rename-test".parse().unwrap();
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		state
			.open(&mut docs, uri.clone(), source.into(), 1)
			.unwrap();
		let snapshot = state.analysis_for_uri(&docs, &uri)?;
		let offset = source.match_indices(needle).nth(occurrence)?.0;
		let position = LineIndex::new(source).position(source, offset);
		rename_candidate(&docs, &state, &snapshot, position, "replacement")
	}

	fn project(
		files: &[(&str, &str)],
		open: &str,
	) -> (tempfile::TempDir, Uri, DocumentStore, CompilerState) {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='rename'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		for (name, source) in files {
			std::fs::write(temp.path().join("src").join(name), source).unwrap();
		}
		let path = temp.path().join("src").join(open);
		let uri = crate::workspace::path_to_uri(&path).unwrap();
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		state
			.open(
				&mut docs,
				uri.clone(),
				std::fs::read_to_string(path).unwrap(),
				1,
			)
			.unwrap();
		(temp, uri, docs, state)
	}

	#[test]
	fn new_name_requires_one_complete_identifier_token() {
		for valid in ["renamed", "éclair", "Δelta", "变量", "snake_case", "x2"] {
			assert!(valid_new_name(valid), "{valid}");
		}
		for invalid in [
			"",
			"_",
			"func",
			"struct",
			"public",
			"true",
			"int",
			"as",
			"if",
			"let",
			"return",
			"two names",
			"name ",
			" name",
			"123",
			"1.5",
			"\"name\"",
			"'n'",
			"name+",
			"name.name",
			"name/* comment */",
			"// name",
			"$0",
			"🙂",
		] {
			assert!(!valid_new_name(invalid), "{invalid}");
		}
	}

	#[test]
	fn local_rename_is_utf16_exact_and_does_not_absorb_shadowing() {
		let uri: Uri = "untitled:rename-utf16".parse().unwrap();
		let source = "func f(éclair: int): int = { let éclair = 1\n // 😀 éclair\n éclair }";
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		state
			.open(&mut docs, uri.clone(), source.into(), 7)
			.unwrap();
		let snapshot = state.analysis_for_uri(&docs, &uri).unwrap();
		let byte = source.rfind("éclair").unwrap();
		let position = LineIndex::new(source).position(source, byte);
		let candidate = rename_candidate(&docs, &state, &snapshot, position, "local").unwrap();
		assert_eq!(
			candidate.prepare.clone(),
			PrepareRenameResponse::RangeWithPlaceholder {
				range: Range::new(Position::new(2, 1), Position::new(2, 7)),
				placeholder: "éclair".into(),
			}
		);
		let edit = candidate.validate_disk_sources().unwrap();
		let Some(DocumentChanges::Edits(edits)) = edit.document_changes else {
			panic!()
		};
		assert_eq!(edits.len(), 1);
		assert_eq!(edits[0].text_document.version, Some(7));
		assert_eq!(edits[0].edits.len(), 2, "only inner declaration and use");
	}

	#[test]
	fn non_bmp_prefix_has_exact_utf16_range_and_surrogate_interior_is_rejected() {
		let uri: Uri = "untitled:rename-non-bmp".parse().unwrap();
		let source = "func f(): int = { let marker = \"😀\" let éclair = 1 éclair }";
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		state
			.open(&mut docs, uri.clone(), source.into(), 3)
			.unwrap();
		let snapshot = state.analysis_for_uri(&docs, &uri).unwrap();
		let byte = source.rfind("éclair").unwrap();
		let position = LineIndex::new(source).position(source, byte);
		let candidate = rename_candidate(&docs, &state, &snapshot, position, "renamed").unwrap();
		let PrepareRenameResponse::RangeWithPlaceholder { range, placeholder } = candidate.prepare
		else {
			panic!()
		};
		assert_eq!(placeholder, "éclair");
		assert_eq!(
			range.start.character,
			source[..byte].encode_utf16().count() as u32
		);
		assert_eq!(range.end.character, range.start.character + 6);
		let emoji_utf16 = source[..source.find('😀').unwrap()].encode_utf16().count() as u32;
		assert!(
			rename_candidate(
				&docs,
				&state,
				&snapshot,
				Position::new(0, emoji_utf16 + 1),
				"renamed"
			)
			.is_none()
		);
	}

	#[test]
	fn dual_role_struct_pattern_shorthand_rejects_field_and_local_rename() {
		let source =
			"struct Point(x: int)\nfunc read(point: Point): int = match (point) { Point(x) -> x }";
		assert!(
			candidate(source, "x", 1).is_none(),
			"field role at shorthand"
		);
		assert!(
			candidate(source, "x", 2).is_none(),
			"local use whose declaration is shorthand"
		);
		assert!(
			candidate(source, "x", 0).is_none(),
			"field declaration reaches shorthand"
		);
	}

	#[test]
	fn non_symbols_and_recovery_tokens_are_not_rename_candidates() {
		let source = "enum A { Same }\nenum B { Same }\nfunc f(value: int): int = missing + 1\nfunc ambiguous(value: int): int = match (value) { Same -> 0 }\nfunc g(value: int): int = A.Same\nfunc h(value: int): int = value\nfunc call(): int = h(value = 1)";
		for (needle, occurrence) in [
			("missing", 0),
			("Same ->", 0),
			("1", 0),
			("func", 0),
			("value =", 0),
		] {
			assert!(candidate(source, needle, occurrence).is_none(), "{needle}");
		}
		let malformed = "func broken(: int = 1\nfunc valid(): int = 2";
		for needle in ["func", "(", ":"] {
			assert!(
				candidate(malformed, needle, 0).is_none(),
				"recovery {needle}"
			);
		}
	}

	#[test]
	fn modules_builtins_and_ambient_prelude_are_not_user_editable() {
		let source = "func show(value: Option<int>): void = print(\"value\")";
		assert!(
			candidate(source, "Option", 0).is_none(),
			"ambient prelude type"
		);
		assert!(candidate(source, "print", 0).is_none(), "builtin function");

		let (_temp, main_uri, docs, state) = project(
			&[
				(
					"main.nym",
					"import @/target\nfunc use(): int = target.answer()",
				),
				("target.nym", "public func answer(): int = 1"),
			],
			"main.nym",
		);
		let snapshot = state.analysis_for_uri(&docs, &main_uri).unwrap();
		assert!(
			rename_candidate(&docs, &state, &snapshot, Position::new(1, 19), "renamed").is_none(),
			"whole-module import qualifier has module identity, not an editable declaration"
		);
	}

	#[test]
	fn project_edit_preserves_alias_identity_and_excludes_same_spelled_symbols() {
		let main = "import @/target with (answer as renamed)\nfunc use(): int = renamed()";
		let other = "import @/target with (answer)\nfunc use_other(): int = answer()";
		let target = "public func answer(): int = 1";
		let unrelated = "public func answer(): int = 2\nfunc own(): int = answer()";
		let (temp, main_uri, mut docs, mut state) = project(
			&[
				("main.nym", main),
				("other.nym", other),
				("target.nym", target),
				("unrelated.nym", unrelated),
			],
			"main.nym",
		);
		let target_uri = crate::workspace::path_to_uri(&temp.path().join("src/target.nym")).unwrap();
		state
			.open(&mut docs, target_uri.clone(), target.into(), 9)
			.unwrap();
		let snapshot = state.analysis_for_uri(&docs, &main_uri).unwrap();
		let offset = main.rfind("renamed").unwrap();
		let position = LineIndex::new(main).position(main, offset);
		let edit = rename_candidate(&docs, &state, &snapshot, position, "changed")
			.unwrap()
			.validate_disk_sources()
			.unwrap();
		let Some(DocumentChanges::Edits(edits)) = edit.document_changes else {
			panic!()
		};
		assert!(
			edits
				.windows(2)
				.all(|pair| { pair[0].text_document.uri.as_str() < pair[1].text_document.uri.as_str() })
		);
		assert_eq!(edits.len(), 3, "unrelated module receives no edits");
		let mut edited_tokens = Vec::new();
		for document in &edits {
			let source = if document.text_document.uri == main_uri {
				assert_eq!(document.text_document.version, Some(1));
				main
			} else if document.text_document.uri == target_uri {
				assert_eq!(document.text_document.version, Some(9));
				target
			} else {
				assert_eq!(document.text_document.version, None);
				other
			};
			let index = LineIndex::new(source);
			let mut previous = None;
			for edit in &document.edits {
				let OneOf::Left(edit) = edit else { panic!() };
				if let Some(previous) = previous {
					assert!(
						previous < edit.range.start,
						"sorted, deduplicated, non-overlapping edits"
					);
				}
				previous = Some(edit.range.end);
				let start = index.exact_offset(source, edit.range.start).unwrap();
				let end = index.exact_offset(source, edit.range.end).unwrap();
				edited_tokens.push(source[start..end].to_string());
			}
		}
		edited_tokens.sort();
		assert_eq!(
			edited_tokens,
			["answer", "answer", "answer", "answer", "renamed", "renamed"],
			"source import token, alias token/use, unaliased import/use, and declaration share identity"
		);
	}

	#[test]
	fn changed_or_deleted_closed_source_rejects_the_complete_edit() {
		for delete in [false, true] {
			let (temp, main_uri, docs, state) = project(
				&[
					(
						"main.nym",
						"import @/target with (answer)\nfunc use(): int = answer()",
					),
					("target.nym", "public func answer(): int = 1"),
				],
				"main.nym",
			);
			let snapshot = state.analysis_for_uri(&docs, &main_uri).unwrap();
			let candidate =
				rename_candidate(&docs, &state, &snapshot, Position::new(1, 19), "renamed").unwrap();
			let target = temp.path().join("src/target.nym");
			if delete {
				std::fs::remove_file(target).unwrap();
			} else {
				std::fs::write(target, "public func answer(): int = 2").unwrap();
			}
			assert!(candidate.validate_disk_sources().is_none());
		}
	}
}
