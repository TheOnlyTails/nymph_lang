//! `textDocument/definition`: jump from an identifier use or type-position
//! name to its declaration.
//!
//! Local lookup retains the parse-only [`nymph_sema::query::definition_at`]
//! behavior. Imported lookup uses the compiler session's immutable checked
//! snapshot: semantic targets retain their stable declaration identity, and
//! the target module supplies authoritative declaration provenance and source.
//!
//! Ordinary members, fields, variants, compiler providers, and embedded std
//! sources have no project-file navigation policy and answer `None`, never a
//! fabricated location.

use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location};

use crate::{
	compiler_state::{AnalysisSnapshot, CompilerState},
	document_store::DocumentStore,
	line_index::LineIndex,
	position::query_with_whitespace_left_bias,
};

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

	let span = query_with_whitespace_left_bias(&doc.text, offset, |candidate| {
		nymph_sema::query::definition_at(&parsed.tree, candidate)
	})?;

	Some(GotoDefinitionResponse::Scalar(Location {
		uri: uri.clone(),
		range: index.range(&doc.text, span),
	}))
}

/// Snapshot-backed definition lookup. Local parse behavior intentionally runs
/// first; only an unresolved local query falls back to checked cross-file semantics.
pub fn definition_snapshot(
	docs: &DocumentStore,
	state: &CompilerState,
	snapshot: &AnalysisSnapshot,
	params: &GotoDefinitionParams,
) -> Option<GotoDefinitionResponse> {
	definition_snapshot_candidate(docs, state, snapshot, params)?.validate_disk_source()
}

pub(crate) struct DefinitionResponseCandidate {
	response: GotoDefinitionResponse,
	disk_source: Option<(std::path::PathBuf, std::sync::Arc<str>)>,
}

impl DefinitionResponseCandidate {
	/// Validate unopened targets after releasing server-state locks. A session
	/// intentionally retains disk sources between filesystem events, so an
	/// externally changed or deleted file must not receive a stale location.
	pub(crate) fn validate_disk_source(self) -> Option<GotoDefinitionResponse> {
		if let Some((path, expected)) = self.disk_source {
			let current = std::fs::read_to_string(path).ok()?;
			if current.as_str() != expected.as_ref() {
				return None;
			}
		}
		Some(self.response)
	}
}

pub(crate) fn definition_snapshot_candidate(
	docs: &DocumentStore,
	state: &CompilerState,
	snapshot: &AnalysisSnapshot,
	params: &GotoDefinitionParams,
) -> Option<DefinitionResponseCandidate> {
	if let Some(local) = definition(docs, params) {
		return Some(DefinitionResponseCandidate {
			response: local,
			disk_source: None,
		});
	}
	let position = params.text_document_position_params.position;
	let index = LineIndex::new(&snapshot.source);
	let offset = index.offset(&snapshot.source, position);
	let target = query_with_whitespace_left_bias(&snapshot.source, offset, |candidate| {
		state.definition_target(docs, snapshot, candidate)
	})?;
	let target_index = LineIndex::new(&target.source);
	let disk_source = if target.requires_disk_validation {
		Some((
			crate::workspace::uri_to_path(&target.uri)?,
			target.source.clone(),
		))
	} else {
		None
	};
	let response = GotoDefinitionResponse::Scalar(Location {
		uri: target.uri,
		range: target_index.range(&target.source, target.span),
	});
	Some(DefinitionResponseCandidate {
		response,
		disk_source,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use lsp_types::{
		Position, Range, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
		WorkDoneProgressParams,
	};
	use std::path::PathBuf;

	struct ProjectFixture {
		_temp: tempfile::TempDir,
		target_path: PathBuf,
		main_uri: Uri,
		target_uri: Uri,
		docs: DocumentStore,
		state: CompilerState,
	}

	impl ProjectFixture {
		fn new(main: &str, target: &str) -> Self {
			let temp = tempfile::tempdir().unwrap();
			std::fs::write(
				temp.path().join("nymph.toml"),
				"[package]\nname='definitions'\nversion='0.1.0'\n",
			)
			.unwrap();
			std::fs::create_dir(temp.path().join("src")).unwrap();
			let main_path = temp.path().join("src/main.nym");
			let target_path = temp.path().join("src/target.nym");
			std::fs::write(&main_path, main).unwrap();
			std::fs::write(&target_path, target).unwrap();
			let main_uri = crate::workspace::path_to_uri(&main_path).unwrap();
			let target_uri = crate::workspace::path_to_uri(&target_path).unwrap();
			let mut docs = DocumentStore::default();
			let mut state = CompilerState::new();
			state
				.open(&mut docs, main_uri.clone(), main.into(), 1)
				.unwrap();
			Self {
				_temp: temp,
				target_path,
				main_uri,
				target_uri,
				docs,
				state,
			}
		}

		fn location(&self, line: u32, character: u32) -> Option<Location> {
			let snapshot = self.state.analysis_for_uri(&self.docs, &self.main_uri)?;
			definition_snapshot(
				&self.docs,
				&self.state,
				&snapshot,
				&params(&self.main_uri, line, character),
			)
			.map(scalar)
		}

		fn open_target(&mut self, uri: Uri, source: &str, version: i32) {
			self
				.state
				.open(&mut self.docs, uri, source.into(), version)
				.unwrap();
		}
	}

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
	fn imported_alias_and_namespace_use_target_authoritative_overlay_range() {
		let main = "import @/target as Target with (answer as renamed)\nfunc direct(): int = renamed()\nfunc qualified(): int = Target.answer()";
		let disk_target = "public func answer(): int = 1";
		let mut fixture = ProjectFixture::new(main, disk_target);
		// UTF-16 prefix and changed line prove that target conversion uses the
		// current target overlay, not the importer's or disk source.
		let overlay = "// 😀\n\npublic func answer(): int = 2";
		fixture.open_target(fixture.target_uri.clone(), overlay, 2);

		for (line, character) in [(1, 22), (2, 32)] {
			let location = fixture
				.location(line, character)
				.expect("imported definition");
			assert_eq!(location.uri, fixture.target_uri);
			assert_eq!(
				location.range,
				Range::new(Position::new(2, 12), Position::new(2, 18))
			);
		}
	}

	#[test]
	fn every_supported_import_category_targets_its_exact_declaration_name() {
		let target = "// 😀\npublic let count: int = 1\npublic func answer(): int = count\npublic struct Item(value: int)\npublic enum Choice { One }\npublic interface Show { func show(): string }\npublic type Alias = Item";
		let main = "import @/target with (count as renamed_count, answer, Item, Choice, Show, Alias)\nfunc use_value(): int = renamed_count\nfunc use_func(): int = answer()\nfunc use_struct(value: Item): Item = value\nfunc use_enum(value: Choice): Choice = value\nfunc use_interface(value: Show): Show = value\nfunc use_alias(value: Alias): Alias = value";
		let fixture = ProjectFixture::new(main, target);
		let cases = [
			(1, 24, 1, 11, 16, "renamed value"),
			(2, 23, 2, 12, 18, "function"),
			(3, 23, 3, 14, 18, "struct type"),
			(4, 21, 4, 12, 18, "enum type"),
			(5, 26, 5, 17, 21, "interface type"),
			(6, 22, 6, 12, 17, "alias type"),
		];
		for (use_line, use_character, target_line, start, end, label) in cases {
			let location = fixture
				.location(use_line, use_character)
				.unwrap_or_else(|| panic!("missing {label} definition"));
			assert_eq!(location.uri, fixture.target_uri, "{label}");
			assert_eq!(
				location.range,
				Range::new(
					Position::new(target_line, start),
					Position::new(target_line, end),
				),
				"{label}"
			);
		}
	}

	#[test]
	fn checked_type_targets_obey_shadowing_and_exact_half_open_name_spans() {
		let main = "import @/target with (Alias, answer)\nfunc imported(value: Alias): Alias = value\nfunc shadowed<Alias>(value: Alias): Alias = value\nfunc invalid(value: answer): int = 0";
		let fixture = ProjectFixture::new(
			main,
			"public type Alias = int\npublic func answer(): int = 1",
		);

		assert!(fixture.location(1, 21).is_some(), "checked imported type");
		assert!(
			fixture.location(1, 25).is_some(),
			"last type-name byte is included"
		);
		assert!(
			fixture.location(1, 26).is_none(),
			"type-name end is excluded"
		);
		assert!(
			fixture.location(2, 28).is_none(),
			"generic shadows imported alias"
		);
		assert!(
			fixture.location(3, 20).is_none(),
			"a function is not a type target"
		);
	}

	#[test]
	fn imported_interfaces_resolve_in_generic_bounds_and_impl_headers() {
		let main = "import @/target with (Show, Item)\nfunc bounded<T: Show>(value: T): T = value\nimpl Show for Item { func show(): string = \"item\" }\ninterface Child: Show {}";
		let fixture = ProjectFixture::new(
			main,
			"public interface Show { func show(): string }\npublic struct Item(value: int)",
		);

		for (line, character) in [(1, 16), (2, 5), (3, 17)] {
			let location = fixture
				.location(line, character)
				.expect("imported interface declaration");
			assert_eq!(location.uri, fixture.target_uri);
			assert_eq!(
				location.range,
				Range::new(Position::new(0, 17), Position::new(0, 21))
			);
		}
	}

	#[test]
	fn canonical_uri_uses_the_exact_analysis_source_from_an_equivalent_overlay_uri() {
		let main = "import @/target with (answer)\nfunc use(): int = answer()";
		let mut fixture = ProjectFixture::new(main, "public func answer(): int = 1");
		let alternate: Uri = fixture
			.target_uri
			.as_str()
			.replace("target.nym", "%74arget.nym")
			.parse()
			.unwrap();
		fixture.open_target(
			alternate,
			"private let marker = \"😀\" public func answer(): int = 2",
			2,
		);

		let location = fixture.location(1, 18).expect("overlay target");
		assert_eq!(location.uri, fixture.target_uri);
		assert_eq!(
			location.range,
			Range::new(Position::new(0, 38), Position::new(0, 44))
		);
	}

	#[test]
	fn closing_one_equivalent_uri_keeps_the_other_overlay_authoritative() {
		let main = "import @/target with (answer)\nfunc use(): int = answer()";
		let mut fixture = ProjectFixture::new(main, "public func answer(): int = 1");
		let alternate: Uri = fixture
			.target_uri
			.as_str()
			.replace("target.nym", "%74arget.nym")
			.parse()
			.unwrap();
		fixture.open_target(
			fixture.target_uri.clone(),
			"\npublic func answer(): int = 2",
			2,
		);
		fixture.open_target(alternate.clone(), "\n\npublic func answer(): int = 3", 3);
		assert_eq!(fixture.location(1, 18).unwrap().range.start.line, 2);
		fixture
			.state
			.close(&mut fixture.docs, &fixture.target_uri)
			.unwrap();
		assert_eq!(fixture.location(1, 18).unwrap().range.start.line, 2);

		fixture.open_target(
			fixture.target_uri.clone(),
			"\npublic func answer(): int = 4",
			4,
		);
		assert_eq!(fixture.location(1, 18).unwrap().range.start.line, 1);
		fixture
			.state
			.close(&mut fixture.docs, &fixture.target_uri)
			.unwrap();

		assert_eq!(fixture.location(1, 18).unwrap().range.start.line, 2);
	}

	#[test]
	fn close_restores_disk_provenance_and_deletion_removes_the_target() {
		let main = "import @/target with (answer)\nfunc use(): int = answer()";
		let disk = "public func answer(): int = 1";
		let mut fixture = ProjectFixture::new(main, disk);
		fixture.open_target(
			fixture.target_uri.clone(),
			"\n\npublic func answer(): int = 2",
			2,
		);
		assert_eq!(fixture.location(1, 18).unwrap().range.start.line, 2);

		fixture
			.state
			.close(&mut fixture.docs, &fixture.target_uri)
			.unwrap();
		assert_eq!(fixture.location(1, 18).unwrap().range.start.line, 0);

		fixture.open_target(
			fixture.target_uri.clone(),
			"\npublic func answer(): int = 3",
			3,
		);
		std::fs::remove_file(&fixture.target_path).unwrap();
		fixture
			.state
			.close(&mut fixture.docs, &fixture.target_uri)
			.unwrap();
		assert!(fixture.location(1, 18).is_none());
	}

	#[test]
	fn externally_stale_or_deleted_unopened_targets_do_not_produce_locations() {
		let main = "import @/target with (answer)\nfunc use(): int = answer()";
		let stale = ProjectFixture::new(main, "public func answer(): int = 1");
		std::fs::write(&stale.target_path, "\npublic func answer(): int = 2").unwrap();
		assert!(stale.location(1, 18).is_none());

		let deleted = ProjectFixture::new(main, "public func answer(): int = 1");
		std::fs::remove_file(&deleted.target_path).unwrap();
		assert!(deleted.location(1, 18).is_none());
	}

	#[test]
	fn stale_dependency_snapshot_and_invalid_import_targets_return_none() {
		let main = "import @/target with (answer)\nfunc use(): int = answer()";
		let mut fixture = ProjectFixture::new(main, "public func answer(): int = 1");
		let stale = fixture
			.state
			.analysis_for_uri(&fixture.docs, &fixture.main_uri)
			.unwrap();
		fixture.open_target(
			fixture.target_uri.clone(),
			"\npublic func answer(): int = 2",
			2,
		);
		assert!(
			definition_snapshot(
				&fixture.docs,
				&fixture.state,
				&stale,
				&params(&fixture.main_uri, 1, 18),
			)
			.is_none()
		);

		for (main, target, line, character) in [
			(
				"import @/target with (secret)\nfunc use(): int = secret()",
				"private func secret(): int = 1",
				1,
				18,
			),
			(
				"import @/target with (missing)\nfunc use(): int = missing()",
				"public func answer(): int = 1",
				1,
				18,
			),
			(
				"import @/target as Target with (answer)\nfunc use(): int = Target.missing()",
				"public func answer(): int = 1",
				1,
				25,
			),
			(
				"import @/target with (answer\nfunc use(): int = answer()",
				"public func answer(): int = 1",
				1,
				18,
			),
		] {
			let fixture = ProjectFixture::new(main, target);
			assert!(
				fixture.location(line, character).is_none(),
				"main source: {main}"
			);
		}
	}

	#[test]
	fn embedded_std_and_out_of_scope_members_do_not_fabricate_project_locations() {
		let std_main =
			"import std/collections/tree with (Tree)\nfunc use(value: Tree<int>): Tree<int> = value";
		let std_fixture = ProjectFixture::new(std_main, "public func unused(): int = 0");
		assert!(std_fixture.location(1, 16).is_none());

		let main = "import @/target with (Item, Choice)\nfunc field(): int = Item(value = 1).value\nfunc variant(): Choice = Choice.One";
		let fixture = ProjectFixture::new(
			main,
			"public struct Item(value: int)\npublic enum Choice { One }",
		);
		assert!(fixture.location(1, 36).is_none(), "ordinary field");
		assert!(
			fixture.location(2, 32).is_none(),
			"enum variant is out of scope"
		);
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
	fn definition_left_biases_across_whitespace_but_not_punctuation() {
		let uri: Uri = "file:///def_bias.nym".parse().unwrap();
		for (suffix, resolves) in [("   ", true), (",   ", false), (" // note", false)] {
			let text = format!("func f(a: int): int = a{suffix}");
			let docs = docs_with(&uri, &text);
			let character = text.encode_utf16().count() as u32;
			let result = definition(&docs, &params(&uri, 0, character));
			assert_eq!(result.is_some(), resolves, "suffix {suffix:?}");
		}
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
