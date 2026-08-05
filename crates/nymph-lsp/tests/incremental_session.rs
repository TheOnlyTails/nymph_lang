use std::{
	fs,
	path::Path,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
};

use lsp_types::{
	HoverParams, Position, SemanticTokensParams, TextDocumentIdentifier, TextDocumentPositionParams,
	Uri, WorkDoneProgressParams,
};
use nymph_lsp::{
	compiler_state::CompilerState, document_store::DocumentStore, hover, semantic_tokens, workspace,
};

fn uri(path: &Path) -> Uri {
	workspace::path_to_uri(path).unwrap()
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
	let initial_parse_count = parse.load(Ordering::Relaxed);
	assert!(initial_parse_count >= 1);
	assert_eq!(check.load(Ordering::Relaxed), 1);

	compiler
		.change(&mut docs, &uri, "func value(): int = 2".into(), 2)
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
