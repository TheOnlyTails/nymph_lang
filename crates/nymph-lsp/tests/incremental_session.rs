use std::{
	cell::Cell,
	fs,
	path::Path,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
};

use lsp_types::{
	CompletionItem, CompletionParams, CompletionResponse, HoverContents, HoverParams, MarkupContent,
	Position, SemanticToken, SemanticTokensParams, SemanticTokensResult, TextDocumentIdentifier,
	TextDocumentPositionParams, Uri, WorkDoneProgressParams,
};
use nymph_lsp::{
	compiler_state::CompilerState, completion, document_store::DocumentStore, hover, semantic_tokens,
	workspace,
};

fn uri(path: &Path) -> Uri {
	workspace::path_to_uri(path).unwrap()
}

fn hover_code(
	compiler: &CompilerState,
	docs: &DocumentStore,
	uri: &Uri,
	line: u32,
	character: u32,
) -> String {
	hover_value(compiler, docs, uri, line, character).unwrap()
}

fn hover_value(
	compiler: &CompilerState,
	docs: &DocumentStore,
	uri: &Uri,
	line: u32,
	character: u32,
) -> Option<String> {
	let snapshot = compiler.analysis_for_uri(docs, uri).unwrap();
	let hover = hover::hover(
		&snapshot,
		&HoverParams {
			text_document_position_params: TextDocumentPositionParams {
				text_document: TextDocumentIdentifier { uri: uri.clone() },
				position: Position { line, character },
			},
			work_done_progress_params: WorkDoneProgressParams::default(),
		},
	)?;
	match hover.contents {
		HoverContents::Markup(MarkupContent { value, .. }) => Some(value),
		other => panic!("expected Markdown hover, got {other:?}"),
	}
}

fn hover_needle(
	compiler: &CompilerState,
	docs: &DocumentStore,
	uri: &Uri,
	source: &str,
	needle: &str,
) -> Option<String> {
	let offset = source.find(needle).unwrap();
	let line = source[..offset]
		.bytes()
		.filter(|byte| *byte == b'\n')
		.count() as u32;
	let character = offset
		- source[..offset]
			.rfind('\n')
			.map_or(0, |newline| newline + 1);
	hover_value(compiler, docs, uri, line, character as u32)
}

fn completion_items(
	compiler: &CompilerState,
	docs: &DocumentStore,
	uri: &Uri,
	line: u32,
	character: u32,
) -> Vec<CompletionItem> {
	let snapshot = compiler.completion_for_uri(docs, uri).unwrap();
	let response = completion::completion_snapshot(
		&snapshot,
		&CompletionParams {
			text_document_position: TextDocumentPositionParams {
				text_document: TextDocumentIdentifier { uri: uri.clone() },
				position: Position { line, character },
			},
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: Default::default(),
			context: None,
		},
	);
	match response {
		CompletionResponse::Array(items) => items,
		CompletionResponse::List(list) => list.items,
	}
}

#[test]
fn project_completion_uses_resolved_imports_ranking_kinds_and_shadowing() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='completion'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let source = "import @/dep with (call as imported_alias, spare, Shape, Choice as Selected, hidden)\nfunc z_local_top(): int = 1\nfunc main(): int = {\n  let imported_alias = 1\n  imported_alias\n  imported_ali";
	fs::write(&main_path, source).unwrap();
	fs::write(
		&dep_path,
		"public func call(): int = 1\npublic func spare(): int = 1\npublic struct Shape(value: int)\npublic enum Choice { First, Second(value: int) }\nprivate func hidden(): int = 1",
	)
	.unwrap();
	let main_uri = uri(&main_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();

	let items = completion_items(&compiler, &docs, &main_uri, 4, 2);
	let labels = items
		.iter()
		.map(|item| item.label.as_str())
		.collect::<Vec<_>>();
	assert_eq!(
		labels
			.iter()
			.filter(|label| **label == "imported_alias")
			.count(),
		1,
		"lexical shadowing must de-duplicate the imported spelling: {labels:?}"
	);
	let alias = items
		.iter()
		.find(|item| item.label == "imported_alias")
		.unwrap();
	assert_eq!(alias.kind, Some(lsp_types::CompletionItemKind::VARIABLE));
	let shape = items.iter().find(|item| item.label == "Shape").unwrap();
	assert_eq!(shape.kind, Some(lsp_types::CompletionItemKind::STRUCT));
	let spare = items.iter().find(|item| item.label == "spare").unwrap();
	assert_eq!(spare.kind, Some(lsp_types::CompletionItemKind::FUNCTION));
	let selected = items.iter().find(|item| item.label == "Selected").unwrap();
	assert_eq!(selected.kind, Some(lsp_types::CompletionItemKind::ENUM));
	for variant in ["First", "Second"] {
		let variant = items.iter().find(|item| item.label == variant).unwrap();
		assert_eq!(
			variant.kind,
			Some(lsp_types::CompletionItemKind::ENUM_MEMBER)
		);
	}
	assert!(
		!labels.contains(&"call"),
		"only the visible alias completes"
	);
	assert!(
		!labels.contains(&"hidden"),
		"private imports never complete"
	);
	assert!(
		labels.iter().position(|label| *label == "Shape").unwrap()
			< labels
				.iter()
				.position(|label| *label == "z_local_top")
				.unwrap()
	);
	assert!(
		labels
			.iter()
			.position(|label| *label == "z_local_top")
			.unwrap()
			< labels.iter().position(|label| *label == "loop").unwrap()
	);
	let mut client_sorted = items.clone();
	client_sorted.sort_by(|left, right| {
		left
			.sort_text
			.as_deref()
			.unwrap_or(&left.label)
			.cmp(right.sort_text.as_deref().unwrap_or(&right.label))
	});
	assert_eq!(
		client_sorted
			.iter()
			.map(|item| item.label.as_str())
			.collect::<Vec<_>>(),
		labels,
		"LSP sortText must preserve tier order in clients"
	);
	let shadowed_prefix = completion_items(&compiler, &docs, &main_uri, 5, 14);
	let alias = shadowed_prefix
		.iter()
		.find(|item| item.label == "imported_alias")
		.expect("the lexical binding should complete from its typed prefix");
	assert_eq!(alias.kind, Some(lsp_types::CompletionItemKind::VARIABLE));
}

#[test]
fn effect_syntax_participates_in_completion_hover_and_semantic_tokens() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='effect-tooling'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let source = "effect Io\nfunc marker(): void = {}\nfunc apply<!E>(callback: () -> void + !E): !Io + !E = callback()\nfunc use(): !Io = apply(marker)\n";
	let path = temp.path().join("src/main.nym");
	fs::write(&path, source).unwrap();
	let uri = uri(&path);
	let mut compiler = CompilerState::default();
	let mut docs = DocumentStore::default();
	compiler
		.open(&mut docs, uri.clone(), source.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &uri)
			.unwrap()
			.is_empty()
	);

	let completions = completion_items(&compiler, &docs, &uri, 4, 0);
	let effect = completions
		.iter()
		.find(|item| item.label == "Io")
		.expect("effect completion");
	assert_eq!(effect.kind, Some(lsp_types::CompletionItemKind::CLASS));

	let hover = hover_needle(&compiler, &docs, &uri, source, "apply(marker)")
		.expect("effectful callable hover");
	assert!(hover.contains("!E"), "{hover}");
	assert!(hover.contains("!Io"), "{hover}");

	let declaration = semantic_token_at(&compiler, &docs, &uri, source, "Io\nfunc");
	assert_eq!(declaration.token_type, 2);
	assert_eq!(declaration.token_modifiers_bitset & 1, 1);
	let nominal_reference = semantic_token_at(&compiler, &docs, &uri, source, "Io + !E");
	assert_eq!(nominal_reference.token_type, 2);
	assert_eq!(nominal_reference.token_modifiers_bitset & 1, 0);
	let parameter_declaration = semantic_token_at(&compiler, &docs, &uri, source, "E>(callback");
	assert_eq!(parameter_declaration.token_type, 2);
	assert_eq!(parameter_declaration.token_modifiers_bitset & 1, 1);
	let parameter_reference = semantic_token_at(&compiler, &docs, &uri, source, "E): !Io");
	assert_eq!(parameter_reference.token_type, 2);
	assert_eq!(parameter_reference.token_modifiers_bitset & 1, 0);
}

#[test]
fn project_completion_matches_variant_precedence_and_ambiguity() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='completion-collisions'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let source = "import @/first with (Remote, counter)\nimport @/second with (Other)\nenum Local { LocalAmbiguous }\nfunc TopCollision(): int = 1\nfunc main(): int = 1";
	fs::write(&main_path, source).unwrap();
	fs::write(
		temp.path().join("src/first.nym"),
		"public enum Remote { Unique, TopCollision, Ambiguous, LocalAmbiguous }\npublic let counter: int = 0",
	)
	.unwrap();
	fs::write(
		temp.path().join("src/second.nym"),
		"public enum Other { Ambiguous }",
	)
	.unwrap();
	let main_uri = uri(&main_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();

	let items = completion_items(&compiler, &docs, &main_uri, 4, 0);
	let unique = items.iter().find(|item| item.label == "Unique").unwrap();
	assert_eq!(
		unique.kind,
		Some(lsp_types::CompletionItemKind::ENUM_MEMBER)
	);
	let top_collision = items
		.iter()
		.find(|item| item.label == "TopCollision")
		.unwrap();
	assert_eq!(
		top_collision.kind,
		Some(lsp_types::CompletionItemKind::FUNCTION),
		"an ordinary local definition wins semantic lookup over a variant"
	);
	for ambiguous in ["Ambiguous", "LocalAmbiguous"] {
		assert!(
			!items.iter().any(|item| item.label == ambiguous),
			"an ambiguous bare variant must not be suggested: {ambiguous}"
		);
	}
	let counter = items.iter().find(|item| item.label == "counter").unwrap();
	assert_eq!(counter.kind, Some(lsp_types::CompletionItemKind::CONSTANT));
}

#[test]
fn project_completion_uses_latest_dependency_overlay_with_partial_source() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='completion-overlay'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let source = "import @/dep\nfunc main(): int = {\n  ";
	fs::write(&main_path, source).unwrap();
	fs::write(&dep_path, "public func disk_name(): int = 1").unwrap();
	let main_uri = uri(&main_path);
	let dep_uri = uri(&dep_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();
	let stale_snapshot = compiler.completion_for_uri(&docs, &main_uri).unwrap();
	let before = completion_items(&compiler, &docs, &main_uri, 2, 2);
	assert!(before.iter().any(|item| item.label == "disk_name"));

	compiler
		.open(
			&mut docs,
			dep_uri.clone(),
			"public func overlay_name(): int = 1\npublic enum OverlayChoice { Known }\nfunc broken("
				.into(),
			1,
		)
		.unwrap();
	let after = completion_items(&compiler, &docs, &main_uri, 2, 2);
	assert!(after.iter().any(|item| item.label == "overlay_name"));
	assert!(after.iter().any(|item| item.label == "OverlayChoice"));
	assert!(after.iter().any(|item| item.label == "Known"));
	assert!(!after.iter().any(|item| item.label == "disk_name"));
	let mut stale_was_published = false;
	nymph_lsp::compiler_state::publish_completion_if_current(
		&docs,
		&main_uri,
		&stale_snapshot,
		(),
		|()| stale_was_published = true,
	);
	assert!(!stale_was_published);

	compiler.close(&mut docs, &dep_uri).unwrap();
	let restored = completion_items(&compiler, &docs, &main_uri, 2, 2);
	assert!(restored.iter().any(|item| item.label == "disk_name"));
	assert!(!restored.iter().any(|item| item.label == "overlay_name"));
}

#[test]
fn project_member_completion_is_semantic_and_replaces_partial_prefix() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='member-completion'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let source = "struct Point(x: int, y: string)\nfunc main(): int = {\n  let point = Point(x = 1, y = \"\")\n  point.x\n  point.y\n}";
	fs::write(&main_path, source).unwrap();
	let main_uri = uri(&main_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();

	let items = completion_items(&compiler, &docs, &main_uri, 4, 9);
	assert_eq!(items.len(), 1);
	assert_eq!(items[0].label, "y");
	assert_eq!(items[0].kind, Some(lsp_types::CompletionItemKind::FIELD));
	assert_eq!(items[0].detail.as_deref(), Some("string"));
	let Some(lsp_types::CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
		panic!("member completion must carry a replacement edit")
	};
	assert_eq!(edit.range.start.character, 8);
	assert_eq!(edit.range.end.character, 9);
}

#[test]
fn project_member_completion_is_available_inside_qualified_calls() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='call-member-completion'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let path = temp.path().join("src/main.nym");
	let source = "namespace Tools { func run<T>(value: T): T = value }\nstruct Point { func get(): int = 1 }\nstruct Vault { namespace func make(): Vault = Vault() }\nenum Choice { Pick }\ninterface Default { func default(): self }\nfunc use<R: Default>(point: Point): int = { point.get()\nVault.make()\nChoice.Pick()\nTools.run(1)\nR.default()\npoint.get() }";
	fs::write(&path, source).unwrap();
	let uri = uri(&path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, uri.clone(), source.into(), 1)
		.unwrap();

	for (line, character, expected) in [
		(5, 53, "get"),
		(6, 10, "make"),
		(7, 11, "Pick"),
		(8, 9, "run"),
		(9, 9, "default"),
	] {
		assert!(
			completion_items(&compiler, &docs, &uri, line, character)
				.iter()
				.any(|item| item.label == expected),
			"missing {expected} completion"
		);
	}
}

#[test]
fn project_member_completion_enforces_visibility_and_static_context() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='member-boundaries'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let source = "import @/dep with (Vault, Tools)\nfunc inspect(v: Vault): int = v.r";
	fs::write(&main_path, source).unwrap();
	fs::write(
		temp.path().join("src/dep.nym"),
		"public struct Vault(public shown: int, private secret: int) {\n  public func read(): int = this.shown\n  private func erase(): int = 0\n  public namespace func make(value: int): Vault = Vault(shown = value, secret = 0)\n}\npublic namespace Tools {\n  public func run(value: int): string = \"\"\n  public let counter: int = 0\n  private func hidden(): int = 0\n}",
	)
	.unwrap();
	let main_uri = uri(&main_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();

	let instance = completion_items(&compiler, &docs, &main_uri, 1, 33);
	assert_eq!(
		instance
			.iter()
			.map(|item| item.label.as_str())
			.collect::<Vec<_>>(),
		vec!["read"]
	);
	assert!(
		!instance
			.iter()
			.any(|item| matches!(item.label.as_str(), "secret" | "erase" | "make"))
	);
	let namespace_source = r"import @/dep with (Vault, Tools)
func inspect(): int = Tools.r";
	compiler
		.change(&mut docs, &main_uri, namespace_source.into(), 2)
		.unwrap();
	let namespace = completion_items(&compiler, &docs, &main_uri, 1, 29);
	assert_eq!(
		namespace
			.iter()
			.map(|item| item.label.as_str())
			.collect::<Vec<_>>(),
		vec!["run"]
	);
	assert_eq!(namespace[0].detail.as_deref(), Some("(int) -> string"));
	let static_source = "import @/dep with (Vault, Tools)\nfunc inspect(): int = Vault.m";
	compiler
		.change(&mut docs, &main_uri, static_source.into(), 3)
		.unwrap();
	let static_items = completion_items(&compiler, &docs, &main_uri, 1, 29);
	assert_eq!(
		static_items
			.iter()
			.map(|item| item.label.as_str())
			.collect::<Vec<_>>(),
		vec!["make"]
	);
	assert_eq!(static_items[0].detail.as_deref(), Some("(int) -> Vault"));
}

#[test]
fn member_completion_replaces_astral_prefix_with_exact_utf16_range() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='unicode-members'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let path = temp.path().join("src/main.nym");
	let source = "struct Glyph(astral𐐀: int)\nfunc read(g: Glyph): int = g.astral𐐀";
	fs::write(&path, source).unwrap();
	let uri = uri(&path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, uri.clone(), source.into(), 1)
		.unwrap();
	let items = completion_items(&compiler, &docs, &uri, 1, 37);
	let item = items.iter().find(|item| item.label == "astral𐐀").unwrap();
	let Some(lsp_types::CompletionTextEdit::Edit(edit)) = &item.text_edit else {
		panic!("missing edit")
	};
	assert_eq!(
		(edit.range.start.character, edit.range.end.character),
		(29, 37)
	);

	let middle = completion_items(&compiler, &docs, &uri, 1, 31);
	let item = middle.iter().find(|item| item.label == "astral𐐀").unwrap();
	let Some(lsp_types::CompletionTextEdit::Edit(edit)) = &item.text_edit else {
		panic!("missing edit")
	};
	assert_eq!(
		(edit.range.start.character, edit.range.end.character),
		(29, 37),
		"completion in the middle of a member must replace its entire UTF-16 token"
	);
}

#[test]
fn project_member_completion_supports_bare_dot_and_nested_access() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='member-dot'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let path = temp.path().join("src/main.nym");
	let source = "struct Inner(answer: int)\nstruct Outer(inner: Inner)\nfunc read(value: Outer): int = value.inner.";
	fs::write(&path, source).unwrap();
	let uri = uri(&path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, uri.clone(), source.into(), 1)
		.unwrap();
	let items = completion_items(&compiler, &docs, &uri, 2, 44);
	let answer = items
		.iter()
		.find(|item| item.label == "answer")
		.expect("nested receiver field");
	assert_eq!(answer.kind, Some(lsp_types::CompletionItemKind::FIELD));
	assert_eq!(answer.detail.as_deref(), Some("int"));
}

#[test]
fn project_dependency_overlay_changes_member_candidates() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='member-overlay'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main = temp.path().join("src/main.nym");
	let dep = temp.path().join("src/dep.nym");
	let source = "import @/dep with (Record)\nfunc read(value: Record): int = value.disk";
	fs::write(&main, source).unwrap();
	fs::write(&dep, "public struct Record(public disk: int)").unwrap();
	let main_uri = uri(&main);
	let dep_uri = uri(&dep);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();
	assert!(
		completion_items(&compiler, &docs, &main_uri, 1, 38)
			.iter()
			.any(|item| item.label == "disk")
	);
	compiler
		.open(
			&mut docs,
			dep_uri,
			"public struct Record(public overlay: int)".into(),
			1,
		)
		.unwrap();
	let changed = completion_items(&compiler, &docs, &main_uri, 1, 38);
	assert!(changed.iter().any(|item| item.label == "overlay"));
	assert!(!changed.iter().any(|item| item.label == "disk"));
}

#[test]
fn project_member_completion_includes_ambient_string_members() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='ambient-member'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let path = temp.path().join("src/main.nym");
	let source = "func read(value: string): uint = value.length";
	fs::write(&path, source).unwrap();
	let uri = uri(&path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, uri.clone(), source.into(), 1)
		.unwrap();
	let items = completion_items(&compiler, &docs, &uri, 0, 39);
	let length = items
		.iter()
		.find(|item| item.label == "length")
		.expect("ambient string.length");
	assert_eq!(length.kind, Some(lsp_types::CompletionItemKind::METHOD));
	assert_eq!(length.detail.as_deref(), Some("() -> uint"));
}

fn semantic_token_at(
	compiler: &CompilerState,
	docs: &DocumentStore,
	uri: &Uri,
	source: &str,
	needle: &str,
) -> SemanticToken {
	let offset = source.find(needle).unwrap();
	let line = source[..offset]
		.bytes()
		.filter(|byte| *byte == b'\n')
		.count() as u32;
	let character = (offset
		- source[..offset]
			.rfind('\n')
			.map_or(0, |newline| newline + 1)) as u32;
	let snapshot = compiler.analysis_for_uri(docs, uri).unwrap();
	let result = semantic_tokens::semantic_tokens_full(
		&snapshot,
		&SemanticTokensParams {
			text_document: TextDocumentIdentifier { uri: uri.clone() },
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: Default::default(),
		},
	)
	.unwrap();
	let SemanticTokensResult::Tokens(tokens) = result else {
		panic!("expected full semantic tokens");
	};
	let mut token_line = 0;
	let mut token_character = 0;
	for token in tokens.data {
		let SemanticToken {
			delta_line,
			delta_start,
			..
		} = token;
		if delta_line == 0 {
			token_character += delta_start;
		} else {
			token_line += delta_line;
			token_character = delta_start;
		}
		if (token_line, token_character) == (line, character) {
			return token;
		}
	}
	panic!("no semantic token for {needle:?} at {line}:{character}");
}

fn semantic_token_type_at(
	compiler: &CompilerState,
	docs: &DocumentStore,
	uri: &Uri,
	source: &str,
	needle: &str,
) -> u32 {
	semantic_token_at(compiler, docs, uri, source, needle).token_type
}

#[test]
fn immutable_state_loops_participate_in_completion_hover_and_semantic_tokens() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='state-loop-tooling'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let source = "func count(): int = loop (let value = 0) { if (value == 2) break value continue(value = value + 1) }\n";
	let path = temp.path().join("src/main.nym");
	fs::write(&path, source).unwrap();
	let uri = uri(&path);
	let mut compiler = CompilerState::default();
	let mut docs = DocumentStore::default();
	compiler
		.open(&mut docs, uri.clone(), source.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &uri)
			.unwrap()
			.is_empty()
	);

	let completions = completion_items(&compiler, &docs, &uri, 1, 0);
	assert!(completions.iter().any(|item| item.label == "loop"));
	let declaration = semantic_token_at(&compiler, &docs, &uri, source, "value = 0");
	assert_eq!(declaration.token_type, 5);
	assert_eq!(declaration.token_modifiers_bitset, 3);
	let replacement = semantic_token_at(&compiler, &docs, &uri, source, "value = value + 1");
	assert_eq!(replacement.token_type, 5);
	assert_eq!(replacement.token_modifiers_bitset, 0);
	let hover = hover_needle(&compiler, &docs, &uri, source, "value = value + 1")
		.expect("replacement name resolves to its state declaration");
	assert!(hover.contains("int"), "{hover}");
}

#[test]
fn unchanged_features_share_one_analysis_and_change_recomputes_once() {
	let parse = Arc::new(AtomicUsize::new(0));
	let check = Arc::new(AtomicUsize::new(0));
	let p = parse.clone();
	let c = check.clone();
	let mut compiler = CompilerState::with_event_callback(move |event| match event {
		"parse" => _ = p.fetch_add(1, Ordering::Relaxed),
		"interface_module_analysis" => _ = c.fetch_add(1, Ordering::Relaxed),
		_ => {}
	});
	let mut docs = DocumentStore::default();
	let uri: Uri = "file:///tmp/shared-analysis.nym".parse().unwrap();
	let source = "func value(): int = 1";

	compiler
		.open(&mut docs, uri.clone(), source.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &uri)
			.unwrap()
			.is_empty()
	);
	let first = compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert!(
		first
			.analysis
			.diagnostics
			.iter()
			.filter(|diagnostic| diagnostic.module == first.module.as_str())
			.collect::<Vec<_>>()
			.is_empty()
	);
	let hover_params = HoverParams {
		text_document_position_params: TextDocumentPositionParams {
			text_document: TextDocumentIdentifier { uri: uri.clone() },
			position: Position {
				line: 0,
				character: 20,
			},
		},
		work_done_progress_params: WorkDoneProgressParams::default(),
	};
	assert!(hover::hover(&first, &hover_params).is_some());
	assert!(hover::hover(&first, &hover_params).is_some());
	let token_params = SemanticTokensParams {
		text_document: TextDocumentIdentifier { uri: uri.clone() },
		work_done_progress_params: WorkDoneProgressParams::default(),
		partial_result_params: Default::default(),
	};
	assert!(semantic_tokens::semantic_tokens_full(&first, &token_params).is_some());
	let first_completion = compiler.completion_for_uri(&docs, &uri).unwrap();
	let initial_parse_count = parse.load(Ordering::Relaxed);
	assert!(initial_parse_count >= 1);
	assert_eq!(check.load(Ordering::Relaxed), 1);

	compiler.change(&mut docs, &uri, source.into(), 2).unwrap();
	let unchanged = compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert!(Arc::ptr_eq(&first.analysis, &unchanged.analysis));
	let unchanged_completion = compiler.completion_for_uri(&docs, &uri).unwrap();
	assert_eq!(unchanged_completion.document_version, 2);
	let mut stale_completion_sent = false;
	nymph_lsp::compiler_state::publish_completion_if_current(
		&docs,
		&uri,
		&first_completion,
		(),
		|()| stale_completion_sent = true,
	);
	assert!(!stale_completion_sent);
	assert_eq!(parse.load(Ordering::Relaxed), initial_parse_count);
	assert_eq!(check.load(Ordering::Relaxed), 1);

	compiler
		.change(&mut docs, &uri, "func value(): int = 2".into(), 3)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &uri)
			.unwrap()
			.is_empty()
	);
	compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert_eq!(parse.load(Ordering::Relaxed), initial_parse_count + 1);
	assert_eq!(check.load(Ordering::Relaxed), 2);
}

#[test]
fn retained_async_edits_keep_hover_diagnostics_and_tokens_on_one_version() {
	let mut compiler = CompilerState::default();
	let mut docs = DocumentStore::default();
	let uri: Uri = "file:///tmp/async-retained.nym".parse().unwrap();
	let valid = "async func child(): Result<int, string> = Ok(value = 1)\n\
		async func value() = {\n\
		\tlet expected = child().await\n\
		\tlet handle = child().spawn()\n\
		\tlet observed = handle.await\n\
		\tobserved\n\
		}";
	compiler
		.open(&mut docs, uri.clone(), valid.into(), 1)
		.unwrap();
	let first = compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert_eq!(first.document_version, 1);
	assert!(
		hover_needle(&compiler, &docs, &uri, valid, "value()")
			.unwrap()
			.contains("async func value(): Result<Result<int, string>, HandleError>")
	);
	assert!(
		hover_needle(&compiler, &docs, &uri, valid, "expected")
			.unwrap()
			.contains("Result<int, string>")
	);
	assert!(
		hover_needle(&compiler, &docs, &uri, valid, "observed")
			.unwrap()
			.contains("Result<Result<int, string>, HandleError>")
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &uri, valid, "await"),
		0,
		"await must remain a keyword token"
	);

	let invalid = valid.replacen("async func value", "func value", 1);
	compiler.change(&mut docs, &uri, invalid.into(), 2).unwrap();
	let second = compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert_eq!(second.document_version, 2);
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &uri)
			.unwrap()
			.iter()
			.any(|diagnostic| diagnostic
				.diag
				.message
				.contains("only valid inside an async"))
	);

	compiler.change(&mut docs, &uri, valid.into(), 3).unwrap();
	let third = compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert_eq!(third.document_version, 3);
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &uri)
			.unwrap()
			.is_empty()
	);
}

#[test]
fn close_and_reopen_same_uri_and_version_uses_new_effective_source() {
	let parse = Arc::new(AtomicUsize::new(0));
	let analysis = Arc::new(AtomicUsize::new(0));
	let parse_events = parse.clone();
	let analysis_events = analysis.clone();
	let mut compiler = CompilerState::with_event_callback(move |event| match event {
		"parse" => _ = parse_events.fetch_add(1, Ordering::Relaxed),
		"interface_module_analysis" => _ = analysis_events.fetch_add(1, Ordering::Relaxed),
		_ => {}
	});
	let mut docs = DocumentStore::default();
	let temp = tempfile::tempdir().unwrap();
	let source_path = temp.path().join("reopened.nym");
	let uri = uri(&source_path);

	compiler
		.open(&mut docs, uri.clone(), "func value(): int = 1".into(), 1)
		.unwrap();
	let first = compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert_eq!(
		hover_code(&compiler, &docs, &uri, 0, 20),
		"```nymph\nint\n```"
	);
	let first_counts = (
		parse.load(Ordering::Relaxed),
		analysis.load(Ordering::Relaxed),
	);
	let unchanged = compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert!(Arc::ptr_eq(&first.analysis, &unchanged.analysis));
	assert_eq!(
		(
			parse.load(Ordering::Relaxed),
			analysis.load(Ordering::Relaxed)
		),
		first_counts,
		"an unchanged effective source should reuse Salsa analysis"
	);

	compiler.close(&mut docs, &uri).unwrap();
	assert!(docs.get(&uri).is_none());
	compiler
		.open(
			&mut docs,
			uri.clone(),
			"func value(): boolean = true".into(),
			1,
		)
		.unwrap();
	let reopened = compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert_eq!(reopened.document_version, first.document_version);
	assert_eq!(reopened.source.as_ref(), "func value(): boolean = true");
	assert!(!Arc::ptr_eq(&first.analysis, &reopened.analysis));
	assert_eq!(
		hover_code(&compiler, &docs, &uri, 0, 27),
		"```nymph\nboolean\n```"
	);
	assert!(parse.load(Ordering::Relaxed) > first_counts.0);
	assert!(analysis.load(Ordering::Relaxed) > first_counts.1);
}

#[test]
fn closing_dependency_overlay_restores_disk_hover_with_project_and_prelude_context() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='hover-close'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let main_source = "import @/dep with (value)\nfunc use(): void = {\n  let imported = value()\n  let sum = 1 + 2\n}";
	let disk_dep = "public func value(): int = 1";
	fs::write(&main_path, main_source).unwrap();
	fs::write(&dep_path, disk_dep).unwrap();
	let main_uri = uri(&main_path);
	let dep_uri = uri(&dep_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();

	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();
	compiler
		.open(
			&mut docs,
			dep_uri.clone(),
			"public func value(): boolean = true".into(),
			1,
		)
		.unwrap();
	let overlay = compiler.analysis_for_uri(&docs, &main_uri).unwrap();
	assert_eq!(
		hover_code(&compiler, &docs, &main_uri, 2, 20),
		"```nymph\n() -> boolean\n```",
		"hover must use the open dependency overlay in the shared project graph"
	);
	assert_eq!(
		hover_code(&compiler, &docs, &main_uri, 3, 12),
		"```nymph\nint\n```",
		"the same project analysis must retain ambient prelude operator semantics"
	);

	compiler.close(&mut docs, &dep_uri).unwrap();
	assert!(docs.get(&dep_uri).is_none());
	assert_eq!(compiler.source_for_uri(&dep_uri).as_deref(), Some(disk_dep));
	let stale_sent = Cell::new(false);
	nymph_lsp::compiler_state::publish_if_current(&docs, &main_uri, &overlay, (), |_| {
		stale_sent.set(true);
	});
	assert!(
		!stale_sent.get(),
		"a dependency overlay change must invalidate importer response publication"
	);
	let restored = compiler.analysis_for_uri(&docs, &main_uri).unwrap();
	assert_eq!(overlay.project, restored.project);
	assert_eq!(overlay.package, restored.package);
	assert_eq!(overlay.module, restored.module);
	assert!(!Arc::ptr_eq(&overlay.analysis, &restored.analysis));
	assert_eq!(
		hover_code(&compiler, &docs, &main_uri, 2, 20),
		"```nymph\n() -> int\n```",
		"closing the dependency must reveal disk semantics, not overlay-era analysis"
	);
}

#[test]
fn closing_dirty_importee_restores_disk_semantics_and_deletion_breaks_the_importer() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='x'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main = temp.path().join("src/main.nym");
	let dep = temp.path().join("src/dep.nym");
	let main_source = "import @/dep with (value)\nfunc use(): int = value()";
	let disk_dep = "public func value(): int = 1";
	fs::write(&main, main_source).unwrap();
	fs::write(&dep, disk_dep).unwrap();
	let main_uri = uri(&main);
	let dep_uri = uri(&dep);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();

	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();
	compiler
		.open(
			&mut docs,
			dep_uri.clone(),
			"public func value(): int = true".into(),
			1,
		)
		.unwrap();
	assert!(
		!compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);
	compiler.close(&mut docs, &dep_uri).unwrap();
	assert_eq!(compiler.source_for_uri(&dep_uri).as_deref(), Some(disk_dep));
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);

	compiler
		.open(
			&mut docs,
			dep_uri.clone(),
			"public func value(): int = true".into(),
			2,
		)
		.unwrap();
	fs::remove_file(&dep).unwrap();
	compiler.close(&mut docs, &dep_uri).unwrap();
	assert!(compiler.source_for_uri(&dep_uri).is_none());
	let diagnostics = compiler.diagnostics_for_uri(&docs, &main_uri).unwrap();
	assert_eq!(diagnostics.len(), 1);
	assert_eq!(diagnostics[0].module, "main");
	assert_eq!(diagnostics[0].diag.code, "IMPORT-UNRESOLVED");
	assert_eq!(
		&main_source[diagnostics[0].diag.span.start..diagnostics[0].diag.span.end],
		"dep"
	);
}

#[test]
fn close_during_manifest_error_does_not_leak_the_overlay_after_project_recovery() {
	let temp = tempfile::tempdir().unwrap();
	let manifest_path = temp.path().join("nymph.toml");
	let manifest = "[package]\nname='manifest-recovery'\nversion='0.1.0'\n";
	fs::write(&manifest_path, manifest).unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let main_source = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&main_path, main_source).unwrap();
	fs::write(&dep_path, "public func value(): int = 1").unwrap();
	let main_uri = uri(&main_path);
	let dep_uri = uri(&dep_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();

	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();
	compiler
		.open(
			&mut docs,
			dep_uri.clone(),
			"public func value(): float = 1.0".into(),
			1,
		)
		.unwrap();
	assert!(
		!compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);

	fs::write(&manifest_path, "not = [toml").unwrap();
	compiler
		.change(
			&mut docs,
			&dep_uri,
			"public func value(): float = 2.0".into(),
			2,
		)
		.unwrap();
	assert!(
		compiler.analysis_for_uri(&docs, &dep_uri).is_none(),
		"retained lifecycle identity must not expose stale analysis during a manifest error"
	);
	compiler.close(&mut docs, &dep_uri).unwrap();
	assert_eq!(
		compiler.source_for_uri(&dep_uri).as_deref(),
		Some("public func value(): int = 1")
	);
	fs::write(&manifest_path, manifest).unwrap();
	compiler
		.change(&mut docs, &main_uri, main_source.into(), 2)
		.unwrap();

	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty(),
		"closing during a manifest error must restore disk state before the project recovers"
	);
}

#[test]
fn project_to_loose_close_rescans_disk_when_the_project_returns() {
	let temp = tempfile::tempdir().unwrap();
	let manifest_path = temp.path().join("nymph.toml");
	let manifest = "[package]\nname='project-return'\nversion='0.1.0'\n";
	fs::write(&manifest_path, manifest).unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let main_source = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&main_path, main_source).unwrap();
	fs::write(&dep_path, "public func value(): int = 1").unwrap();
	let main_uri = uri(&main_path);
	let dep_uri = uri(&dep_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();

	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();
	compiler
		.open(
			&mut docs,
			dep_uri.clone(),
			"public func value(): float = 1.0".into(),
			1,
		)
		.unwrap();
	assert!(
		!compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);

	fs::remove_file(&manifest_path).unwrap();
	compiler
		.change(
			&mut docs,
			&dep_uri,
			"public func value(): float = 2.0".into(),
			2,
		)
		.unwrap();
	compiler.close(&mut docs, &dep_uri).unwrap();
	fs::write(&manifest_path, manifest).unwrap();
	compiler
		.change(&mut docs, &main_uri, main_source.into(), 2)
		.unwrap();

	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty(),
		"project recovery must reload the closed dependency's disk source"
	);
}

#[test]
fn changing_an_open_buffer_does_not_reread_unopened_project_files() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='x'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main = temp.path().join("src/main.nym");
	let dep = temp.path().join("src/dep.nym");
	let source = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&main, source).unwrap();
	fs::write(&dep, "public func value(): int = 1").unwrap();
	let main_uri = uri(&main);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();

	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);
	fs::write(&dep, "public func value(): int = true").unwrap();
	compiler
		.change(&mut docs, &main_uri, format!("{source}\n"), 2)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);
}

#[test]
fn loose_file_identity_is_stable_and_uses_library_mode() {
	let temp = tempfile::tempdir().unwrap();
	let path = temp.path().join("scratch.nym");
	let uri = uri(&path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();

	compiler
		.open(&mut docs, uri.clone(), "func helper(): int = 1".into(), 1)
		.unwrap();
	let first = compiler.analysis_for_uri(&docs, &uri).unwrap();
	compiler
		.change(&mut docs, &uri, "func helper(): int = 2".into(), 2)
		.unwrap();
	let second = compiler.analysis_for_uri(&docs, &uri).unwrap();
	assert_eq!(first.project, second.project);
	assert_eq!(first.package, second.package);
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &uri)
			.unwrap()
			.is_empty()
	);
}

#[test]
fn project_hover_uses_imports_aliases_std_prelude_generics_and_reuses_the_checked_world() {
	let analyses = Arc::new(AtomicUsize::new(0));
	let analysis_events = analyses.clone();
	let mut compiler = CompilerState::with_event_callback(move |event| {
		if event == "interface_module_analysis" {
			_ = analysis_events.fetch_add(1, Ordering::Relaxed);
		}
	});
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='project-hover'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let source = "import @/dep with (Box, id as identify)\nimport std/collections/linked_list with (LinkedList)\nfunc use(box: Box<int>, list: LinkedList<string>): Option<string> = {\n  let imported_box = box\n  let imported_std = list\n  let sum = 1 + 2\n  Some(value = identify(\"ok\"))\n}\nfunc unwrap(option: Option<int>): int = match (option) { Some(value) -> value, None -> 0 }";
	let dependency = "public struct Box<T>(public value: T)\npublic func id<T>(value: T): T = value";
	fs::write(&main_path, source).unwrap();
	fs::write(&dep_path, dependency).unwrap();
	let main_uri = uri(&main_path);
	let mut docs = DocumentStore::default();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);

	assert_eq!(
		hover_needle(&compiler, &docs, &main_uri, source, "Box<int>"),
		Some("```nymph\nBox<int>\n```".to_string())
	);
	let initial_analyses = analyses.load(Ordering::Relaxed);
	assert!(initial_analyses > 0);
	let cases = [
		("LinkedList<string>", "LinkedList<string>"),
		("Option<string>", "Option<string>"),
		("box\n", "Box<int>"),
		("list\n", "LinkedList<string>"),
		("sum =", "int"),
		("identify(\"ok\")", "(string) -> string"),
		("Some(value", "Option.Some(value: T)"),
		("Some(value) ->", "Option.Some(value: T)"),
		("value) ->", "int"),
		("None ->", "Option.None"),
	];
	for (needle, expected) in cases {
		assert_eq!(
			hover_needle(&compiler, &docs, &main_uri, source, needle),
			Some(format!("```nymph\n{expected}\n```")),
			"hover fixture {needle:?}"
		);
	}
	assert_eq!(
		analyses.load(Ordering::Relaxed),
		initial_analyses,
		"repeated project hover must reuse the one checked Salsa snapshot"
	);
}

#[test]
fn project_semantic_tokens_use_imports_prelude_and_dependency_overlays() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='project-semantic-tokens'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let source = "import @/dep as dependency with (Box as ImportedBox, Choice, Config, make as build, amount as imported_amount)\nfunc use(box: ImportedBox): Option<Choice> = Some(value = build(box))\nfunc choose(): Choice = Choice.One\nfunc read(box: ImportedBox): int = box.get() + box.value + Config.count + dependency.amount + imported_amount\nfunc construct(): ImportedBox = ImportedBox(value = 1)\nfunc static_construct(): ImportedBox = ImportedBox.create()\nfunc qualified_pattern(choice: Choice): int = match (choice) { Choice.One -> 1 }\nfunc destructure(box: ImportedBox): int = match (box) { ImportedBox(value = field) -> field }";
	let dependency = "public struct Box(public value: int) { func get(): int = this.value namespace func create(): Box = Box(value = 1) }\npublic enum Choice { One }\npublic namespace Config { public let count = 1 }\npublic func make(box: Box): Choice = Choice.One\npublic let amount: int = 1";
	fs::write(&main_path, source).unwrap();
	fs::write(&dep_path, dependency).unwrap();
	let main_uri = uri(&main_path);
	let dep_uri = uri(&dep_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();
	assert!(
		compiler.analysis_for_uri(&docs, &main_uri).is_some(),
		"semantic fixture did not analyze: {:?}",
		compiler.diagnostics_for_uri(&docs, &main_uri)
	);

	// Fixed legend indices: type=2, function=3, variable=5,
	// enumMember=8, namespace=12. Declaration and readonly are bits 0 and 1.
	let dependency_path = semantic_token_at(&compiler, &docs, &main_uri, source, "dep as");
	assert_eq!(dependency_path.token_type, 12);
	assert_eq!(dependency_path.token_modifiers_bitset, 0);
	let dependency_alias = semantic_token_at(&compiler, &docs, &main_uri, source, "dependency with");
	assert_eq!(dependency_alias.token_type, 12);
	assert_eq!(dependency_alias.token_modifiers_bitset, 1);
	for needle in ["Box as", "ImportedBox,", "Choice, "] {
		let binding = semantic_token_at(&compiler, &docs, &main_uri, source, needle);
		assert_eq!(binding.token_type, 2, "{needle}");
		assert_eq!(binding.token_modifiers_bitset, 1, "{needle}");
	}
	let namespace_binding = semantic_token_at(&compiler, &docs, &main_uri, source, "Config,");
	assert_eq!(namespace_binding.token_type, 12);
	assert_eq!(namespace_binding.token_modifiers_bitset, 1);
	for needle in ["make as", "build,"] {
		let binding = semantic_token_at(&compiler, &docs, &main_uri, source, needle);
		assert_eq!(binding.token_type, 3, "{needle}");
		assert_eq!(binding.token_modifiers_bitset, 1, "{needle}");
	}
	for needle in ["amount as", "imported_amount)"] {
		let binding = semantic_token_at(&compiler, &docs, &main_uri, source, needle);
		assert_eq!(binding.token_type, 5, "{needle}");
		assert_eq!(binding.token_modifiers_bitset, 3, "{needle}");
	}
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "ImportedBox):"),
		2
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "ImportedBox(value"),
		2
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "build(box)"),
		3
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "Some(value"),
		8
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "Choice.One"),
		2
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "One\nfunc"),
		8
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "get()"),
		4
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "value + Config"),
		7
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "count + dependency"),
		5
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "Config.count"),
		12
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "dependency.amount"),
		12
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "ImportedBox.create"),
		2
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "create()"),
		4
	);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "Choice.One ->"),
		2
	);
	let field_binding = semantic_token_at(&compiler, &docs, &main_uri, source, "field) ->");
	assert_eq!(field_binding.token_type, 5);
	assert_eq!(field_binding.token_modifiers_bitset, 3);
	let field_use = semantic_token_at(&compiler, &docs, &main_uri, source, "field }");
	assert_eq!(field_use.token_type, 5);
	assert_eq!(field_use.token_modifiers_bitset, 0);
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "One ->"),
		8
	);
	assert_eq!(
		semantic_token_type_at(
			&compiler,
			&docs,
			&main_uri,
			source,
			"amount + imported_amount"
		),
		5
	);
	let imported_amount_use = semantic_token_at(
		&compiler,
		&docs,
		&main_uri,
		source,
		"imported_amount\nfunc construct",
	);
	assert_eq!(imported_amount_use.token_type, 5);
	assert_eq!(imported_amount_use.token_modifiers_bitset, 0);

	// An unsaved dependency buffer is authoritative for the whole project.
	// Even though changing `make` from a function to a value makes the call
	// malformed, semantic tokens remain best-effort and use the new role.
	compiler
		.open(
			&mut docs,
			dep_uri,
			"public struct Box(public value: int) { func get(): int = this.value namespace func create(): Box = Box(value = 1) }\npublic enum Choice { One }\npublic let make: int = 1\npublic let amount: int = 2".into(),
			2,
		)
		.unwrap();
	assert_eq!(
		semantic_token_type_at(&compiler, &docs, &main_uri, source, "build(box)"),
		5
	);

	let malformed = "import @/missing with (\nfunc still_here(): int = build";
	compiler
		.change(&mut docs, &main_uri, malformed.into(), 3)
		.unwrap();
	assert!(
		compiler.analysis_for_uri(&docs, &main_uri).is_none(),
		"this fixture must exercise the no-snapshot recovery path"
	);
	assert!(
		semantic_tokens::semantic_tokens_for_open_document(
			&docs,
			&SemanticTokensParams {
				text_document: TextDocumentIdentifier {
					uri: main_uri.clone(),
				},
				work_done_progress_params: WorkDoneProgressParams::default(),
				partial_result_params: Default::default(),
			},
		)
		.is_some(),
		"a malformed import must still return best-effort tokens"
	);
}

#[test]
fn loose_file_hover_uses_library_mode_with_the_ambient_prelude() {
	let temp = tempfile::tempdir().unwrap();
	let path = temp.path().join("scratch.nym");
	let uri = uri(&path);
	let source = "func keep(value: Option<int>): Option<int> = {\n  let sum = 1 + 2\n  value\n}";
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, uri.clone(), source.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &uri)
			.unwrap()
			.is_empty()
	);

	assert_eq!(
		hover_needle(&compiler, &docs, &uri, source, "Option<int>"),
		Some("```nymph\nOption<int>\n```".to_string())
	);
	assert_eq!(
		hover_needle(&compiler, &docs, &uri, source, "value\n}"),
		Some("```nymph\nOption<int>\n```".to_string())
	);
}

#[test]
fn loose_files_in_one_directory_remain_isolated_when_both_are_open() {
	let temp = tempfile::tempdir().unwrap();
	let main_uri = uri(&temp.path().join("main.nym"));
	let dependency_uri = uri(&temp.path().join("dependency.nym"));
	let main_source =
		"import @/dependency with (value)\nfunc use(): Option<int> = Some(value = value())";
	let dependency_source = "public func value(): int = 1";
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();

	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();
	let before = compiler.diagnostics_for_uri(&docs, &main_uri).unwrap();
	assert!(
		before
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-UNRESOLVED")
	);
	assert!(compiler.analysis_for_uri(&docs, &main_uri).is_none());

	let affected = compiler
		.open(
			&mut docs,
			dependency_uri.clone(),
			dependency_source.into(),
			1,
		)
		.unwrap();
	assert_eq!(affected, vec![dependency_uri]);
	let after = compiler.diagnostics_for_uri(&docs, &main_uri).unwrap();
	assert!(
		after
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-UNRESOLVED"),
		"opening a loose sibling must not create a project import graph: {after:?}"
	);
	assert!(compiler.analysis_for_uri(&docs, &main_uri).is_none());
}

#[test]
fn private_unresolved_and_malformed_project_hover_return_none_without_panicking() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='negative-hover'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let main_uri = uri(&main_path);
	let private_source = "import @/dep with (hidden)\nfunc use(): int = hidden()";
	fs::write(&main_path, private_source).unwrap();
	fs::write(&dep_path, "private func hidden(): int = 1").unwrap();
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), private_source.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-PRIVATE-NAME")
	);
	assert_eq!(
		hover_needle(&compiler, &docs, &main_uri, private_source, "hidden()"),
		None
	);

	let unresolved = "import @/missing with (answer)\nfunc use(): int = answer()";
	compiler
		.change(&mut docs, &main_uri, unresolved.into(), 2)
		.unwrap();
	assert!(compiler.analysis_for_uri(&docs, &main_uri).is_none());

	let malformed = "func broken(): int = {\n  let value =";
	compiler
		.change(&mut docs, &main_uri, malformed.into(), 3)
		.unwrap();
	assert!(compiler.analysis_for_uri(&docs, &main_uri).is_none());
}

#[test]
fn transitive_importers_follow_overlay_and_close_while_unrelated_analysis_is_reused() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='x'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let files = [
		(
			"main",
			"import @/middle with (middle)\nfunc use(): int = middle()",
		),
		(
			"middle",
			"import @/leaf with (value)\npublic func middle(): int = value()",
		),
		("leaf", "public func value(): int = 1"),
		("unrelated", "func stable(): int = 0"),
	];
	for (module, source) in files {
		fs::write(temp.path().join(format!("src/{module}.nym")), source).unwrap();
	}
	let main_uri = uri(&temp.path().join("src/main.nym"));
	let leaf_uri = uri(&temp.path().join("src/leaf.nym"));
	let unrelated_uri = uri(&temp.path().join("src/unrelated.nym"));
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();

	compiler
		.open(&mut docs, main_uri.clone(), files[0].1.into(), 1)
		.unwrap();
	compiler
		.open(&mut docs, unrelated_uri.clone(), files[3].1.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);
	let unrelated_before = compiler.analysis_for_uri(&docs, &unrelated_uri).unwrap();

	compiler
		.open(
			&mut docs,
			leaf_uri.clone(),
			"public func value(): float = 1.0".into(),
			2,
		)
		.unwrap();
	let diagnostics = compiler.diagnostics_for_uri(&docs, &main_uri).unwrap();
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.module == "middle" && diagnostic.diag.is_error()),
		"leaf overlay did not invalidate its transitive importer chain: {diagnostics:?}"
	);
	let unrelated_after = compiler.analysis_for_uri(&docs, &unrelated_uri).unwrap();
	assert!(
		Arc::ptr_eq(&unrelated_before.analysis, &unrelated_after.analysis),
		"an unrelated rooted analysis was recomputed after the leaf overlay"
	);

	compiler.close(&mut docs, &leaf_uri).unwrap();
	assert_eq!(
		compiler.source_for_uri(&leaf_uri).as_deref(),
		Some(files[2].1)
	);
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty(),
		"closing the overlay did not reveal the valid disk source"
	);
}

#[test]
fn watched_unopened_module_create_change_and_delete_refresh_project_analysis() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='watch-source'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let main_source = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&main_path, main_source).unwrap();
	let main_uri = uri(&main_path);
	let dep_uri = uri(&dep_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();
	assert!(
		compiler.diagnostics_for_uri(&docs, &main_uri).unwrap()[0]
			.diag
			.code
			.eq("IMPORT-UNRESOLVED")
	);

	fs::write(&dep_path, "public func value(): int = 1").unwrap();
	let created = compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&dep_uri))
		.unwrap();
	assert_eq!(created.len(), 1);
	assert!(created[0].affected.contains(&main_uri));
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);

	fs::write(&dep_path, "public func value(): int = true").unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&dep_uri))
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.iter()
			.any(|diagnostic| diagnostic.module == "dep" && diagnostic.diag.is_error())
	);

	fs::remove_file(&dep_path).unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&dep_uri))
		.unwrap();
	let deleted = compiler.diagnostics_for_uri(&docs, &main_uri).unwrap();
	assert!(deleted.iter().any(|diagnostic| {
		diagnostic.module == "main" && diagnostic.diag.code == "IMPORT-UNRESOLVED"
	}));
}

#[test]
fn watched_disk_change_preserves_equivalent_uri_overlay_until_close() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='watch-overlay'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let main_source = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&main_path, main_source).unwrap();
	fs::write(&dep_path, "public func value(): int = 1").unwrap();
	let main_uri = uri(&main_path);
	let dep_uri = uri(&dep_path);
	let equivalent_uri: Uri = dep_uri
		.as_str()
		.replace("dep.nym", "%64ep.nym")
		.parse()
		.unwrap();
	let overlay = "public func value(): int = true";
	let disk_after = "public func value(): int = 2";
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();
	compiler
		.open(&mut docs, equivalent_uri.clone(), overlay.into(), 7)
		.unwrap();

	fs::write(&dep_path, disk_after).unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&dep_uri))
		.unwrap();
	assert_eq!(compiler.source_for_uri(&dep_uri).as_deref(), Some(overlay));
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.iter()
			.any(|diagnostic| diagnostic.module == "dep" && diagnostic.diag.is_error())
	);

	compiler.close(&mut docs, &equivalent_uri).unwrap();
	assert_eq!(
		compiler.source_for_uri(&dep_uri).as_deref(),
		Some(disk_after)
	);
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);
}

#[test]
fn watched_manifest_change_preserves_equivalent_uri_overlay_authority() {
	let temp = tempfile::tempdir().unwrap();
	let manifest_path = temp.path().join("nymph.toml");
	let manifest = "[package]\nname='manifest-overlay'\nversion='0.1.0'\n";
	fs::write(&manifest_path, manifest).unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let path = temp.path().join("src/main.nym");
	fs::write(&path, "func disk(): int = 0").unwrap();
	let canonical_uri = uri(&path);
	let equivalent_uri: Uri = canonical_uri
		.as_str()
		.replace("main.nym", "%6dain.nym")
		.parse()
		.unwrap();
	let canonical_overlay = "func canonical(): int = 1";
	let authoritative_overlay = "func authoritative(): int = 2";
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(
			&mut docs,
			canonical_uri.clone(),
			canonical_overlay.into(),
			1,
		)
		.unwrap();
	compiler
		.open(&mut docs, equivalent_uri, authoritative_overlay.into(), 2)
		.unwrap();
	assert_eq!(
		compiler.source_for_uri(&canonical_uri).as_deref(),
		Some(authoritative_overlay)
	);

	fs::write(&manifest_path, manifest).unwrap();
	compiler
		.watched_files_changed(&mut docs, &[uri(&manifest_path)])
		.unwrap();
	assert_eq!(
		compiler.source_for_uri(&canonical_uri).as_deref(),
		Some(authoritative_overlay)
	);
}

#[test]
fn watched_manifest_create_error_recovery_and_delete_reclassify_open_document() {
	let temp = tempfile::tempdir().unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let manifest_path = temp.path().join("nymph.toml");
	let manifest_uri = uri(&manifest_path);
	let source = "func value(): int = 1";
	fs::write(&main_path, source).unwrap();
	let main_uri = uri(&main_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();
	let loose_project = compiler.analysis_for_uri(&docs, &main_uri).unwrap().project;

	let manifest = "[package]\nname='watch-manifest'\nversion='0.1.0'\n";
	fs::write(&manifest_path, manifest).unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&manifest_uri))
		.unwrap();
	let project = compiler.analysis_for_uri(&docs, &main_uri).unwrap().project;
	assert_ne!(project, loose_project);

	fs::write(&manifest_path, "not = [toml").unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&manifest_uri))
		.unwrap();
	assert!(compiler.has_manifest_error(&main_uri));
	assert!(compiler.analysis_for_uri(&docs, &main_uri).is_none());

	fs::write(&manifest_path, manifest).unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&manifest_uri))
		.unwrap();
	assert_eq!(
		compiler.analysis_for_uri(&docs, &main_uri).unwrap().project,
		project
	);

	fs::remove_file(&manifest_path).unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&manifest_uri))
		.unwrap();
	assert_eq!(
		compiler.analysis_for_uri(&docs, &main_uri).unwrap().project,
		loose_project
	);
}

#[test]
fn watched_manifest_recovery_does_not_restore_deleted_unopened_module() {
	let temp = tempfile::tempdir().unwrap();
	let manifest_path = temp.path().join("nymph.toml");
	let manifest_uri = uri(&manifest_path);
	let manifest = "[package]\nname='manifest-stale'\nversion='0.1.0'\n";
	fs::write(&manifest_path, manifest).unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let source = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&main_path, source).unwrap();
	fs::write(&dep_path, "public func value(): int = 1").unwrap();
	let main_uri = uri(&main_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), source.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);

	fs::remove_file(&manifest_path).unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&manifest_uri))
		.unwrap();
	fs::remove_file(&dep_path).unwrap();
	fs::write(&manifest_path, manifest).unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&manifest_uri))
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-UNRESOLVED")
	);
}

#[test]
fn watched_source_root_transition_replaces_closed_module_identities() {
	let temp = tempfile::tempdir().unwrap();
	let manifest_path = temp.path().join("nymph.toml");
	fs::write(
		&manifest_path,
		"[package]\nname='root-transition'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir_all(temp.path().join("src/new")).unwrap();
	let main_path = temp.path().join("src/new/main.nym");
	let dep_path = temp.path().join("src/new/dep.nym");
	let main_source = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&main_path, main_source).unwrap();
	fs::write(&dep_path, "public func value(): int = 1").unwrap();
	let main_uri = uri(&main_path);
	let dep_uri = uri(&dep_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();

	fs::write(
		&manifest_path,
		"[package]\nname='root-transition'\nversion='0.1.0'\nsrc='src/new'\n",
	)
	.unwrap();
	compiler
		.watched_files_changed(&mut docs, &[uri(&manifest_path)])
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);

	fs::write(&dep_path, "public func value(): int = true").unwrap();
	let refreshes = compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&dep_uri))
		.unwrap();
	assert!(
		refreshes
			.iter()
			.flat_map(|refresh| &refresh.affected)
			.any(|affected| affected == &main_uri),
		"the transitioned dependency identity lost its reverse importer"
	);
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.iter()
			.any(|diagnostic| diagnostic.module == "dep" && diagnostic.diag.is_error())
	);
}

#[test]
fn watched_source_root_error_and_recovery_rescan_deleted_modules() {
	let temp = tempfile::tempdir().unwrap();
	let manifest_path = temp.path().join("nymph.toml");
	let manifest = "[package]\nname='root-recovery'\nversion='0.1.0'\n";
	fs::write(&manifest_path, manifest).unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let main_source = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&main_path, main_source).unwrap();
	fs::write(&dep_path, "public func value(): int = 1").unwrap();
	let main_uri = uri(&main_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();

	fs::write(
		&manifest_path,
		"[package]\nname='root-recovery'\nversion='0.1.0'\nsrc='lib'\n",
	)
	.unwrap();
	compiler
		.watched_files_changed(&mut docs, &[uri(&manifest_path)])
		.unwrap();
	assert!(compiler.has_manifest_error(&main_uri));
	fs::remove_file(&dep_path).unwrap();
	fs::write(&manifest_path, manifest).unwrap();
	compiler
		.watched_files_changed(&mut docs, &[uri(&manifest_path)])
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-UNRESOLVED"),
		"recovery reused the source graph from before the invalid source root"
	);
}

#[cfg(unix)]
#[test]
fn watched_manifest_rescan_removes_an_unreadable_stale_source() {
	use std::os::unix::fs::symlink;

	let temp = tempfile::tempdir().unwrap();
	let manifest_path = temp.path().join("nymph.toml");
	fs::write(
		&manifest_path,
		"[package]\nname='unreadable-rescan'\nversion='0.1.0'\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("src")).unwrap();
	let main_path = temp.path().join("src/main.nym");
	let dep_path = temp.path().join("src/dep.nym");
	let main_source = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&main_path, main_source).unwrap();
	fs::write(&dep_path, "public func value(): int = 1").unwrap();
	let main_uri = uri(&main_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, main_uri.clone(), main_source.into(), 1)
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.is_empty()
	);

	fs::remove_file(&dep_path).unwrap();
	symlink(temp.path().join("missing-target"), &dep_path).unwrap();
	compiler
		.watched_files_changed(&mut docs, &[uri(&manifest_path)])
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &main_uri)
			.unwrap()
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-UNRESOLVED"),
		"the failed rescan read retained the previous dependency source"
	);
}

#[test]
fn watched_nested_manifest_preserves_outer_refresh_and_isolates_inner_sources() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("nymph.toml"),
		"[package]\nname='outer'\nversion='0.1.0'\n",
	)
	.unwrap();
	let outer_src = temp.path().join("src");
	let inner_root = outer_src.join("child");
	let inner_src = inner_root.join("src");
	fs::create_dir_all(&inner_src).unwrap();
	let outer_main_path = outer_src.join("main.nym");
	let outer_dep_path = outer_src.join("outer_dep.nym");
	let inner_main_path = inner_src.join("main.nym");
	let inner_dep_path = inner_src.join("dep.nym");
	let outer_main = "import @/child/src/dep with (child_value)\nimport @/outer_dep with (outer_value)\nfunc use(): int = outer_value()";
	let inner_main = "import @/dep with (value)\nfunc use(): int = value()";
	fs::write(&outer_main_path, outer_main).unwrap();
	fs::write(&outer_dep_path, "public func outer_value(): int = 1").unwrap();
	fs::write(&inner_main_path, inner_main).unwrap();
	fs::write(&inner_dep_path, "public func value(): int = 1").unwrap();
	let outer_main_uri = uri(&outer_main_path);
	let outer_dep_uri = uri(&outer_dep_path);
	let inner_main_uri = uri(&inner_main_path);
	let inner_dep_uri = uri(&inner_dep_path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(&mut docs, outer_main_uri.clone(), outer_main.into(), 1)
		.unwrap();
	compiler
		.open(&mut docs, inner_main_uri.clone(), inner_main.into(), 1)
		.unwrap();

	let inner_manifest = inner_root.join("nymph.toml");
	fs::write(
		&inner_manifest,
		"[package]\nname='inner'\nversion='0.1.0'\n",
	)
	.unwrap();
	compiler
		.watched_files_changed(&mut docs, &[uri(&inner_manifest)])
		.unwrap();
	assert!(
		compiler
			.diagnostics_for_uri(&docs, &outer_main_uri)
			.unwrap()
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "IMPORT-UNRESOLVED"),
		"the enclosing project retained a source now owned by the nested manifest"
	);

	fs::write(&inner_dep_path, "public func value(): float = 1.0").unwrap();
	let inner_refreshes = compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&inner_dep_uri))
		.unwrap();
	let inner_affected: Vec<_> = inner_refreshes
		.iter()
		.flat_map(|refresh| &refresh.affected)
		.collect();
	assert!(inner_affected.contains(&&inner_main_uri));
	assert!(!inner_affected.contains(&&outer_main_uri));

	fs::write(&outer_dep_path, "public func outer_value(): int = true").unwrap();
	let outer_refreshes = compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&outer_dep_uri))
		.unwrap();
	assert!(
		outer_refreshes
			.iter()
			.flat_map(|refresh| &refresh.affected)
			.any(|affected| affected == &outer_main_uri),
		"the nested transition disabled the still-open enclosing project"
	);
}

#[test]
fn watched_filesystem_revision_rejects_pre_event_snapshot_publication() {
	let temp = tempfile::tempdir().unwrap();
	let path = temp.path().join("loose.nym");
	let file_uri = uri(&path);
	let mut docs = DocumentStore::default();
	let mut compiler = CompilerState::new();
	compiler
		.open(
			&mut docs,
			file_uri.clone(),
			"func value(): int = 1".into(),
			1,
		)
		.unwrap();
	let before_ignored_batch = docs.revision();
	compiler
		.watched_files_changed(
			&mut docs,
			&[
				"untitled:ignored".parse().unwrap(),
				uri(&temp.path().join("ignored.txt")),
			],
		)
		.unwrap();
	assert_eq!(docs.revision(), before_ignored_batch);
	let snapshot = compiler.analysis_for_uri(&docs, &file_uri).unwrap();
	compiler
		.watched_files_changed(&mut docs, std::slice::from_ref(&file_uri))
		.unwrap();
	let published = Cell::new(false);
	nymph_lsp::compiler_state::publish_if_current(&docs, &file_uri, &snapshot, (), |_| {
		published.set(true);
	});
	assert!(!published.get());
}
