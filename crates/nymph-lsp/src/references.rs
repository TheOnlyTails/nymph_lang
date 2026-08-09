//! `textDocument/references`: semantic declaration and use locations from one
//! immutable compiler project snapshot.

use std::{path::PathBuf, sync::Arc};

use lsp_types::{Location, ReferenceParams};

use crate::{
	compiler_state::{AnalysisSnapshot, CompilerState},
	document_store::DocumentStore,
	line_index::LineIndex,
};

pub(crate) struct ReferencesResponseCandidate {
	locations: Vec<Location>,
	disk_sources: Vec<(PathBuf, Arc<str>)>,
}

impl ReferencesResponseCandidate {
	/// Reject the whole immutable result if an unopened project source changed
	/// after the session snapshot was built.
	pub(crate) fn validate_disk_sources(self) -> Option<Vec<Location>> {
		for (path, expected) in self.disk_sources {
			let current = std::fs::read_to_string(path).ok()?;
			if current.as_str() != expected.as_ref() {
				return None;
			}
		}
		Some(self.locations)
	}
}

pub(crate) fn references_snapshot_candidate(
	docs: &DocumentStore,
	state: &CompilerState,
	snapshot: &AnalysisSnapshot,
	params: &ReferenceParams,
) -> Option<ReferencesResponseCandidate> {
	let position = params.text_document_position.position;
	let index = LineIndex::new(&snapshot.source);
	let offset = index.exact_offset(&snapshot.source, position)?;
	let symbol = nymph_sema::query::symbol_at(&snapshot.analysis.semantic, offset)?;
	let modules = state.reference_modules(docs, snapshot, &symbol)?;
	let mut locations = Vec::new();
	let mut disk_sources = Vec::new();
	for module in modules {
		if module.requires_disk_validation {
			disk_sources.push((
				crate::workspace::uri_to_path(&module.uri)?,
				module.source.clone(),
			));
		}
		let index = LineIndex::new(&module.source);
		for occurrence in module.occurrences {
			if occurrence.is_declaration && !params.context.include_declaration {
				continue;
			}
			locations.push(Location {
				uri: module.uri.clone(),
				range: index.range(&module.source, occurrence.span),
			});
		}
	}
	locations.sort_by(|left, right| {
		left
			.uri
			.as_str()
			.cmp(right.uri.as_str())
			.then_with(|| left.range.start.line.cmp(&right.range.start.line))
			.then_with(|| left.range.start.character.cmp(&right.range.start.character))
			.then_with(|| left.range.end.line.cmp(&right.range.end.line))
			.then_with(|| left.range.end.character.cmp(&right.range.end.character))
	});
	locations.dedup();
	Some(ReferencesResponseCandidate {
		locations,
		disk_sources,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use lsp_types::{
		Position, ReferenceContext, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
		WorkDoneProgressParams,
	};

	fn params(uri: &Uri, line: u32, character: u32, include_declaration: bool) -> ReferenceParams {
		ReferenceParams {
			text_document_position: TextDocumentPositionParams {
				text_document: TextDocumentIdentifier { uri: uri.clone() },
				position: Position::new(line, character),
			},
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: Default::default(),
			context: ReferenceContext {
				include_declaration,
			},
		}
	}

	fn project(
		files: &[(&str, &str)],
		open: &str,
	) -> (tempfile::TempDir, Uri, DocumentStore, CompilerState) {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='references'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		for (name, source) in files {
			std::fs::write(temp.path().join("src").join(name), source).unwrap();
		}
		let path = temp.path().join("src").join(open);
		let uri = crate::workspace::path_to_uri(&path).unwrap();
		let source = std::fs::read_to_string(path).unwrap();
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		state.open(&mut docs, uri.clone(), source, 1).unwrap();
		(temp, uri, docs, state)
	}

	fn locations(
		docs: &DocumentStore,
		state: &CompilerState,
		params: &ReferenceParams,
	) -> Option<Vec<Location>> {
		let snapshot =
			state.analysis_for_uri(docs, &params.text_document_position.text_document.uri)?;
		references_snapshot_candidate(docs, state, &snapshot, params)?.validate_disk_sources()
	}

	fn position_of(source: &str, needle: &str, occurrence: usize) -> Position {
		let offset = source
			.match_indices(needle)
			.nth(occurrence)
			.expect("test token occurrence")
			.0;
		let prefix = &source[..offset];
		Position::new(
			prefix.bytes().filter(|byte| *byte == b'\n').count() as u32,
			prefix.rsplit('\n').next().unwrap().encode_utf16().count() as u32,
		)
	}

	#[test]
	fn member_static_namespace_and_field_cursors_have_exact_declaration_policy() {
		let source = "struct Point(x: int) {\n  func get(): int = this.x\n  namespace func origin(): Point = Point(x = 0)\n}\nnamespace Host { func answer(): int = 1 }\nfunc use(point: Point): int = point.get() + Point.origin().x + Host.answer()";
		let (_temp, uri, docs, state) = project(&[("main.nym", source)], "main.nym");

		for (needle, cursor_occurrence, declaration_occurrence) in [
			("get", 1, 0),
			("origin", 1, 0),
			("answer", 1, 0),
			("x", 3, 0),
		] {
			let cursor = position_of(source, needle, cursor_occurrence);
			let without = locations(
				&docs,
				&state,
				&params(&uri, cursor.line, cursor.character, false),
			)
			.unwrap_or_else(|| panic!("missing symbol at {needle} use"));
			let with = locations(
				&docs,
				&state,
				&params(&uri, cursor.line, cursor.character, true),
			)
			.unwrap();
			assert_eq!(with.len(), without.len() + 1, "{needle}");
			let declaration = position_of(source, needle, declaration_occurrence);
			assert!(
				!without
					.iter()
					.any(|location| location.range.start == declaration),
				"{needle} declaration excluded"
			);
			assert_eq!(
				with
					.iter()
					.filter(|location| location.range.start == declaration)
					.count(),
				1,
				"{needle} declaration included exactly once"
			);
		}
	}

	#[test]
	fn project_references_include_aliases_unopened_importers_and_exact_declaration_policy() {
		let (_temp, main_uri, docs, state) = project(
			&[
				(
					"main.nym",
					"import @/target with (answer as renamed)\nfunc main_use(): int = renamed()",
				),
				(
					"other.nym",
					"import @/target with (answer)\nfunc other_use(): int = answer()",
				),
				("target.nym", "// 😀\npublic func answer(): int = 1"),
			],
			"main.nym",
		);

		let without = locations(&docs, &state, &params(&main_uri, 1, 24, false)).unwrap();
		assert_eq!(without.len(), 5);
		assert!(without.windows(2).all(|pair| {
			(
				pair[0].uri.as_str(),
				pair[0].range.start.line,
				pair[0].range.start.character,
			) <= (
				pair[1].uri.as_str(),
				pair[1].range.start.line,
				pair[1].range.start.character,
			)
		}));
		assert!(
			without
				.iter()
				.all(|location| location.range.start.line <= 1)
		);

		let with = locations(&docs, &state, &params(&main_uri, 1, 24, true)).unwrap();
		assert_eq!(with.len(), 6);
		let declaration = with
			.iter()
			.find(|location| location.uri.path().as_str().ends_with("target.nym"))
			.expect("target declaration");
		assert_eq!(declaration.range.start, Position::new(1, 12));
		assert_eq!(declaration.range.end, Position::new(1, 18));
	}

	#[test]
	fn loose_local_references_obey_shadowing_and_stay_isolated() {
		let uri: Uri = "untitled:isolated-references".parse().unwrap();
		let source = "func main(): int = {\n  let value = 1\n  { let value = 2 value }\n  value\n}";
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		state
			.open(&mut docs, uri.clone(), source.into(), 1)
			.unwrap();

		let found = locations(&docs, &state, &params(&uri, 3, 3, true)).unwrap();
		assert_eq!(found.len(), 2);
		assert!(found.iter().all(|location| location.uri == uri));
		assert_eq!(found[0].range.start, Position::new(1, 6));
		assert_eq!(found[1].range.start, Position::new(3, 2));
		assert!(locations(&docs, &state, &params(&uri, 0, 4, true)).is_none());
		assert!(locations(&docs, &state, &params(&uri, 3, 1, true)).is_none());
		assert!(locations(&docs, &state, &params(&uri, 99, 0, true)).is_none());
	}

	#[test]
	fn whole_module_import_and_qualifier_share_stable_module_identity() {
		let (_temp, main_uri, docs, state) = project(
			&[
				(
					"main.nym",
					"import @/target\nimport @/target with (Host)\nfunc use(): int = target.answer() + Host.answer()",
				),
				(
					"target.nym",
					"public func answer(): int = 1\npublic namespace Host { func answer(): int = 2 }",
				),
			],
			"main.nym",
		);

		let found = locations(&docs, &state, &params(&main_uri, 2, 20, true)).unwrap();
		assert_eq!(found.len(), 2, "import namespace and exact qualifier token");
		assert!(found.iter().all(|location| location.uri == main_uri));
		assert_eq!(found[0].range.start, Position::new(0, 9));
		assert_eq!(found[1].range.start, Position::new(2, 18));
	}

	#[test]
	fn checker_owned_pattern_bindings_merge_unions_and_reject_ambiguous_variants() {
		let uri: Uri = "untitled:pattern-references".parse().unwrap();
		let source = "enum A { Same }\nenum B { Same }\nfunc merged(input: int): int = match (input) { value | value -> value }\nfunc ambiguous(input: int): int = match (input) { Same -> 0 }";
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		state
			.open(&mut docs, uri.clone(), source.into(), 1)
			.unwrap();

		let merged = locations(&docs, &state, &params(&uri, 2, 49, true)).unwrap();
		assert_eq!(merged.len(), 3, "two union declarations and one use");
		assert_eq!(merged[0].range.start, Position::new(2, 47));
		assert_eq!(merged[1].range.start, Position::new(2, 55));
		assert_eq!(merged[2].range.start, Position::new(2, 64));
		assert!(locations(&docs, &state, &params(&uri, 3, 52, true)).is_none());
	}

	#[test]
	fn semantic_categories_cover_types_qualified_values_and_variant_patterns_without_duplicates() {
		let (_temp, main_uri, docs, state) = project(
			&[
				(
					"main.nym",
					"import @/types with (Box, Choice)\nfunc make(value: Box): Box = Box(value = value)\nfunc choose(value: Choice): int = match (value) { Choice.One -> 1 }",
				),
				(
					"types.nym",
					"public struct Box(value: int)\npublic enum Choice { One }",
				),
			],
			"main.nym",
		);

		let boxes = locations(&docs, &state, &params(&main_uri, 1, 17, true)).unwrap();
		assert_eq!(
			boxes.len(),
			5,
			"declaration, import, two types, constructor"
		);
		let variants = locations(&docs, &state, &params(&main_uri, 2, 58, true)).unwrap();
		assert_eq!(
			variants.len(),
			2,
			"variant declaration and qualified pattern"
		);
		assert_ne!(variants[0], variants[1]);
	}

	#[test]
	fn authoritative_overlay_supplies_utf16_ranges_and_malformed_siblings_do_not_add_guesses() {
		let (temp, main_uri, mut docs, mut state) = project(
			&[
				(
					"main.nym",
					"import @/target with (answer as renamed)\nfunc use(): int = renamed()",
				),
				("target.nym", "public func answer(): int = 1"),
				("broken.nym", "func broken(: = answer answer"),
			],
			"main.nym",
		);
		let target_uri = crate::workspace::path_to_uri(&temp.path().join("src/target.nym")).unwrap();
		state
			.open(
				&mut docs,
				target_uri.clone(),
				"private let marker = \"😀\" public func answer(): int = answer()".into(),
				2,
			)
			.unwrap();

		let found = locations(&docs, &state, &params(&main_uri, 1, 19, true)).unwrap();
		assert_eq!(found.len(), 5);
		let target = found
			.iter()
			.filter(|location| location.uri == target_uri)
			.collect::<Vec<_>>();
		assert_eq!(target.len(), 2);
		assert_eq!(target[0].range.start, Position::new(0, 38));
		assert_eq!(target[0].range.end, Position::new(0, 44));

		std::fs::write(temp.path().join("src/broken.nym"), "changed invalid source").unwrap();
		assert!(locations(&docs, &state, &params(&main_uri, 1, 19, true)).is_none());
	}

	#[test]
	fn stale_unopened_project_source_rejects_the_snapshot() {
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
		std::fs::write(
			temp.path().join("src/target.nym"),
			"\npublic func answer(): int = 2",
		)
		.unwrap();
		assert!(locations(&docs, &state, &params(&main_uri, 1, 19, true)).is_none());
	}
}
