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
			< labels.iter().position(|label| *label == "while").unwrap()
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
fn project_completion_matches_variant_precedence_ambiguity_and_mutability() {
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
		"public enum Remote { Unique, TopCollision, Ambiguous, LocalAmbiguous }\npublic let mut counter: int = 0",
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
	assert_eq!(counter.kind, Some(lsp_types::CompletionItemKind::VARIABLE));
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
	let source = "import @/dep as dependency with (Box as ImportedBox, Choice, Config, make as build, amount as imported_amount)\nfunc use(box: ImportedBox): Option<Choice> = Some(value = build(box))\nfunc choose(): Choice = Choice.One\nfunc read(box: ImportedBox): int = box.get() + box.value + Config.count + dependency.amount + imported_amount\nfunc construct(): ImportedBox = ImportedBox(value = 1)\nfunc static_construct(): ImportedBox = ImportedBox.create()\nfunc qualified_pattern(choice: Choice): int = match (choice) { Choice.One -> 1 }";
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
