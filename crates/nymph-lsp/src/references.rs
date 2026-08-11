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
	#[cfg(test)]
	pub(crate) fn validate_disk_sources(self) -> Option<Vec<Location>> {
		self.validate_disk_sources_result().ok()
	}

	pub(crate) fn validate_disk_sources_result(self) -> Result<Vec<Location>, ()> {
		for (path, expected) in self.disk_sources {
			let current = std::fs::read_to_string(path).map_err(|_| ())?;
			if current.as_str() != expected.as_ref() {
				return Err(());
			}
		}
		Ok(self.locations)
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
	fn query_generic_identity_covers_declaration_and_signature_uses() {
		let source = "func identity<T>(value: T): T = value";
		let (_temp, uri, docs, state) = project(&[("main.nym", source)], "main.nym");
		let snapshot = state.analysis_for_uri(&docs, &uri).unwrap();
		let offset = source.find("<T>").unwrap() + 1;
		let symbol = nymph_sema::query::symbol_at(&snapshot.analysis.semantic, offset).unwrap();
		let occurrences = nymph_sema::query::references_to(&snapshot.analysis.semantic, &symbol);
		assert_eq!(
			occurrences
				.iter()
				.map(|occurrence| occurrence.span)
				.collect::<Vec<_>>(),
			source
				.match_indices('T')
				.map(|(offset, _)| nymph_ast::Span::new(offset, offset + 1))
				.collect::<Vec<_>>()
		);
		assert!(occurrences[0].is_declaration);
		assert!(
			occurrences[1..]
				.iter()
				.all(|occurrence| !occurrence.is_declaration)
		);
	}

	#[test]
	fn same_spelled_function_generics_have_disjoint_references() {
		let source = "func first<T>(value: T): T = value\nfunc second<T>(value: T): T = value";
		let (_temp, uri, docs, state) = project(&[("main.nym", source)], "main.nym");
		for generic_occurrence in [0, 3] {
			let cursor = position_of(source, "T", generic_occurrence);
			let found = locations(
				&docs,
				&state,
				&params(&uri, cursor.line, cursor.character, true),
			)
			.unwrap();
			assert_eq!(found.len(), 3);
			assert_eq!(
				found
					.iter()
					.map(|location| location.range.start.line)
					.collect::<Vec<_>>(),
				vec![cursor.line; 3],
			);
		}
	}

	#[test]
	fn interface_owner_and_shadowing_method_generics_are_distinct() {
		let source = "interface Mapper<T: Equals<Other = T>> {\n  func keep(value: T): T\n  func map<T: Equals<Other = T>>(value: T): T\n}";
		let (_temp, uri, docs, state) = project(&[("main.nym", source)], "main.nym");
		let owner = position_of(source, "T", 0);
		let owner_refs = locations(
			&docs,
			&state,
			&params(&uri, owner.line, owner.character, true),
		)
		.unwrap();
		assert_eq!(owner_refs.len(), 3);
		assert!(
			owner_refs
				.iter()
				.all(|location| location.range.start.line <= 1)
		);

		let method = position_of(source, "T", 4);
		let method_refs = locations(
			&docs,
			&state,
			&params(&uri, method.line, method.character, true),
		)
		.unwrap();
		assert_eq!(
			method_refs.len(),
			4,
			"constraint/default type use shares shadowing method binding"
		);
		assert!(
			method_refs
				.iter()
				.all(|location| location.range.start.line == 2)
		);
	}

	#[test]
	fn nested_impl_generic_shadows_owner_as_a_distinct_declaration_backed_symbol() {
		let source = "interface Marker { func apply(value: int): int }\nstruct Box<T> {\n  impl<T> Marker { func apply(value: T): T = value }\n}";
		let (_temp, uri, docs, state) = project(&[("main.nym", source)], "main.nym");

		let owner = position_of(source, "T", 0);
		let owner_refs = locations(
			&docs,
			&state,
			&params(&uri, owner.line, owner.character, true),
		)
		.expect("owner generic declaration is a symbol");
		assert_eq!(owner_refs.len(), 1);
		assert_eq!(owner_refs[0].range.start, owner);

		let nested = position_of(source, "T", 1);
		let nested_refs = locations(
			&docs,
			&state,
			&params(&uri, nested.line, nested.character, true),
		)
		.expect("nested impl generic declaration is a symbol");
		assert_eq!(
			nested_refs
				.iter()
				.map(|location| location.range.start)
				.collect::<Vec<_>>(),
			vec![
				position_of(source, "T", 1),
				position_of(source, "T", 2),
				position_of(source, "T", 3),
			],
		);
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
	fn constructor_and_shorthand_pattern_fields_keep_checker_owned_member_identity() {
		let source = "struct Point(x: int)\nfunc read(point: Point): int = match (point) { Point(x) -> x }\nfunc make(): Point = Point(x = 1)";
		let (_temp, uri, docs, state) = project(&[("main.nym", source)], "main.nym");

		let field_cursor = position_of(source, "x", 3);
		let fields = locations(
			&docs,
			&state,
			&params(&uri, field_cursor.line, field_cursor.character, true),
		)
		.expect("named constructor label resolves to the declared field");
		assert_eq!(
			fields
				.iter()
				.map(|location| location.range.start)
				.collect::<Vec<_>>(),
			vec![
				position_of(source, "x", 0),
				position_of(source, "x", 1),
				position_of(source, "x", 3),
			],
			"field declaration, shorthand field, and constructor label"
		);

		let local_cursor = position_of(source, "x", 2);
		let local = locations(
			&docs,
			&state,
			&params(&uri, local_cursor.line, local_cursor.character, true),
		)
		.expect("shorthand binding use resolves to its local declaration");
		assert_eq!(
			local
				.iter()
				.map(|location| location.range.start)
				.collect::<Vec<_>>(),
			vec![position_of(source, "x", 1), position_of(source, "x", 2),],
			"field identity must not absorb the shorthand's local binding"
		);
	}

	#[test]
	fn materialized_defaults_resolve_to_source_members_while_overrides_stay_distinct() {
		let source = "interface Named { func name(): int = 1 }\nstruct Defaulted\nimpl Named for Defaulted {}\nstruct Overridden\nimpl Named for Overridden { func name(): int = 2 }\nfunc read_default(value: Defaulted): int = value.name()\nfunc read_override(value: Overridden): int = value.name()";
		let (_temp, uri, docs, state) = project(&[("main.nym", source)], "main.nym");

		let default_cursor = position_of(source, "name", 2);
		let defaulted = locations(
			&docs,
			&state,
			&params(&uri, default_cursor.line, default_cursor.character, true),
		)
		.expect("materialized default resolves to its user-written interface member");
		assert_eq!(
			defaulted
				.iter()
				.map(|location| location.range.start)
				.collect::<Vec<_>>(),
			vec![
				position_of(source, "name", 0),
				position_of(source, "name", 2),
			],
		);

		let override_cursor = position_of(source, "name", 3);
		let overridden = locations(
			&docs,
			&state,
			&params(&uri, override_cursor.line, override_cursor.character, true),
		)
		.expect("override call resolves to the concrete override declaration");
		assert_eq!(
			overridden
				.iter()
				.map(|location| location.range.start)
				.collect::<Vec<_>>(),
			vec![
				position_of(source, "name", 1),
				position_of(source, "name", 3),
			],
		);
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
	fn equivalent_open_uris_use_one_authoritative_overlay_without_stale_cursor_mapping() {
		let source = "func main(): int = {\n  let value = 1\n  value\n}";
		let (_temp, canonical_uri, mut docs, mut state) = project(&[("main.nym", source)], "main.nym");
		let alternate_uri: Uri = canonical_uri
			.as_str()
			.replace("main.nym", "%6dain.nym")
			.parse()
			.unwrap();
		state
			.open(&mut docs, alternate_uri.clone(), source.into(), 2)
			.unwrap();

		let canonical = locations(&docs, &state, &params(&canonical_uri, 2, 3, true)).unwrap();
		assert_eq!(canonical.len(), 2);
		assert!(
			canonical
				.iter()
				.all(|location| location.uri == alternate_uri),
			"one authoritative URI spelling owns all ranges for the logical module"
		);

		let shifted = format!("\n{source}");
		state.change(&mut docs, &alternate_uri, shifted, 3).unwrap();
		assert!(
			locations(&docs, &state, &params(&canonical_uri, 2, 3, true)).is_none(),
			"a non-authoritative alias must not map its cursor into different overlay text"
		);
		let authoritative = locations(&docs, &state, &params(&alternate_uri, 3, 3, true)).unwrap();
		assert_eq!(authoritative.len(), 2);
		assert!(
			authoritative
				.iter()
				.all(|location| location.uri == alternate_uri)
		);
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

	#[test]
	fn deleted_unopened_project_source_rejects_the_snapshot() {
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
		std::fs::remove_file(temp.path().join("src/target.nym")).unwrap();
		assert!(locations(&docs, &state, &params(&main_uri, 1, 19, true)).is_none());
	}
}
