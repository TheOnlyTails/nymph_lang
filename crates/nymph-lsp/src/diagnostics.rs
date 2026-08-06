//! Re-check a document and republish its diagnostics.
//!
//! Both loose and project documents are checked through the shared,
//! long-lived [`nymph_compiler::CompilerSession`] owned by [`CompilerState`].
//! Project sessions include the compiler-owned embedded standard-library
//! modules; only project modules with real workspace sources are published.

use std::sync::{Arc, Mutex};

use lsp_server::Connection;
use lsp_types::{
	Diagnostic as LspDiagnostic, DiagnosticSeverity, NumberOrString, PublishDiagnosticsParams, Uri,
	notification::{Notification as _, PublishDiagnostics},
};
use nymph_diagnostics::{Diagnostic, Severity};

use crate::{compiler_state::CompilerState, document_store::DocumentStore, line_index::LineIndex};

/// Re-check `uri`'s current text and publish its full diagnostic set
/// (replacing whatever was previously published for it, so a fix clears
/// stale marks). A no-op if the document isn't open (e.g. raced with a
/// `didClose`).
///
/// If `uri` sits inside a discovered `nymph.toml` project (see
/// [`workspace::detect`]), the whole project graph reachable from `uri`'s
/// own module is checked in library mode, including transitive embedded-std
/// dependencies. Every touched project module's diagnostics are republished
/// against its own file; compiler-owned modules have no workspace URI and
/// are not publication targets. Otherwise `uri` is checked in loose mode.
pub fn check_and_publish_state(
	connection: &Connection,
	docs: &Arc<Mutex<DocumentStore>>,
	compiler: &Arc<Mutex<CompilerState>>,
	uri: &Uri,
) -> anyhow::Result<()> {
	// Copy the document table first, then release its mutex before entering the
	// compiler. Publication below holds neither mutex.
	let docs_snapshot = docs.lock().unwrap().clone();
	let mut compiler = compiler.lock().unwrap();
	if let Some((message, stale_targets)) = compiler.manifest_error_snapshot(uri) {
		drop(compiler);
		let Some(document) = docs_snapshot.get(uri) else {
			return Ok(());
		};
		for stale_uri in stale_targets {
			clear(connection, &stale_uri)?;
		}
		publish_manifest_error(connection, uri, &message, document.version)?;
		return Ok(());
	}
	let snapshot = compiler.diagnostics_snapshot(&docs_snapshot, uri);
	drop(compiler);
	let Some(snapshot) = snapshot else {
		return Ok(());
	};
	// This is deliberately the final state read before any send: if the root
	// changed while analysis ran, publish no partial project result.
	if docs.lock().unwrap().version(uri) != Some(snapshot.requested_version) {
		return Ok(());
	}
	for module in snapshot.modules {
		publish(
			connection,
			&module.uri,
			&module.source,
			&module.diagnostics,
			module.version,
		)?;
	}
	Ok(())
}

pub fn clear(connection: &Connection, uri: &Uri) -> anyhow::Result<()> {
	publish(connection, uri, "", &[], None)
}

fn publish_manifest_error(
	connection: &Connection,
	uri: &Uri,
	message: &str,
	version: i32,
) -> anyhow::Result<()> {
	let diagnostic = LspDiagnostic {
		range: Default::default(),
		severity: Some(DiagnosticSeverity::ERROR),
		code: Some(NumberOrString::String("MANIFEST".to_string())),
		source: Some("nymph".to_string()),
		message: message.to_string(),
		..Default::default()
	};
	let params = PublishDiagnosticsParams {
		uri: uri.clone(),
		diagnostics: vec![diagnostic],
		version: Some(version),
	};
	connection.sender.send(lsp_server::Message::Notification(
		lsp_server::Notification::new(
			PublishDiagnostics::METHOD.to_string(),
			serde_json::to_value(params)?,
		),
	))?;
	Ok(())
}

#[cfg(test)]
pub fn check_and_publish(
	connection: &Connection,
	docs: &Arc<Mutex<DocumentStore>>,
	uri: &Uri,
) -> anyhow::Result<()> {
	let mut state = CompilerState::new();
	let docs_snapshot = docs.lock().unwrap().clone();
	state.synchronize_open_document(&docs_snapshot, uri)?;
	check_and_publish_state(connection, docs, &Arc::new(Mutex::new(state)), uri)
}

/// Publish `diags` (already anchored against `text`) for `uri`.
fn publish(
	connection: &Connection,
	uri: &Uri,
	text: &str,
	diags: &[Diagnostic],
	version: Option<i32>,
) -> anyhow::Result<()> {
	let index = LineIndex::new(text);
	let lsp_diags: Vec<LspDiagnostic> = diags.iter().map(|d| to_lsp(d, text, &index)).collect();
	let params = PublishDiagnosticsParams {
		uri: uri.clone(),
		diagnostics: lsp_diags,
		version,
	};
	connection.sender.send(lsp_server::Message::Notification(
		lsp_server::Notification::new(
			PublishDiagnostics::METHOD.to_string(),
			serde_json::to_value(params)?,
		),
	))?;
	Ok(())
}

fn to_lsp(diag: &Diagnostic, text: &str, index: &LineIndex) -> LspDiagnostic {
	LspDiagnostic {
		range: index.range(text, diag.span),
		severity: Some(to_lsp_severity(diag.severity)),
		code: Some(NumberOrString::String(diag.code.to_string())),
		source: Some("nymph".to_string()),
		message: diag.message.to_string(),
		..Default::default()
	}
}

fn to_lsp_severity(severity: Severity) -> DiagnosticSeverity {
	match severity {
		Severity::Error => DiagnosticSeverity::ERROR,
		Severity::Warning => DiagnosticSeverity::WARNING,
		Severity::Info => DiagnosticSeverity::INFORMATION,
		Severity::Hint => DiagnosticSeverity::HINT,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::document_store::DocumentStore;
	use lsp_server::{Connection, Message};

	fn open_doc(uri: &Uri, text: &str) -> Arc<Mutex<DocumentStore>> {
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		docs.lock().unwrap().open(uri.clone(), text.to_string(), 1);
		docs
	}

	#[test]
	fn a_type_error_publishes_one_diagnostic_at_the_expected_range() {
		let uri: Uri = "file:///err.nym".parse().unwrap();
		// `let x: int = true` — mismatched initializer type; the checker
		// should flag it.
		let text = "func main(): void = {\n  let x: int = true\n}";
		let docs = open_doc(&uri, text);
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &uri).unwrap();

		let msg = client.receiver.recv().unwrap();
		let Message::Notification(not) = msg else {
			panic!("expected a notification")
		};
		assert_eq!(not.method, PublishDiagnostics::METHOD);
		let params: PublishDiagnosticsParams = serde_json::from_value(not.params).unwrap();
		assert_eq!(params.uri, uri);
		assert_eq!(
			params.diagnostics.len(),
			1,
			"expected exactly one diagnostic, got {:?}",
			params.diagnostics
		);
		let d = &params.diagnostics[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
		// The mismatched initializer `true` sits on line 1 (0-based).
		assert_eq!(d.range.start.line, 1);
	}

	#[test]
	fn a_stdlib_source_file_checks_with_no_self_duplication() {
		// Opening a real stdlib source file through the LSP must not inject a
		// second copy of it via the ambient `core` prelude — see
		// `nymph_compiler::is_stdlib_source_path`/`check_without_prelude`.
		let path =
			std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib/src/ops/mod.nym");
		let uri = crate::workspace::path_to_uri(&path).unwrap();
		let text = std::fs::read_to_string(&path).unwrap();
		let docs = open_doc(&uri, &text);
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &uri).unwrap();

		let msg = client.receiver.recv().unwrap();
		let Message::Notification(not) = msg else {
			panic!("expected a notification")
		};
		let params: PublishDiagnosticsParams = serde_json::from_value(not.params).unwrap();
		let errors: Vec<_> = params
			.diagnostics
			.iter()
			.filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
			.collect();
		assert!(
			errors.is_empty(),
			"expected no self-duplication errors for a stdlib source file, got: {errors:?}"
		);
	}

	#[test]
	fn a_normal_user_file_still_gets_the_ambient_prelude() {
		// A plain user file (outside the embedded stdlib/src tree) with a type
		// error must still be checked WITH the ambient prelude and report
		// exactly that error — the stdlib short-circuit must not affect it.
		let uri: Uri = "file:///user_file.nym".parse().unwrap();
		let text = "func main(): void = {\n  let x: int = true\n}";
		let docs = open_doc(&uri, text);
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &uri).unwrap();

		let msg = client.receiver.recv().unwrap();
		let Message::Notification(not) = msg else {
			panic!("expected a notification")
		};
		let params: PublishDiagnosticsParams = serde_json::from_value(not.params).unwrap();
		assert_eq!(
			params.diagnostics.len(),
			1,
			"expected exactly one diagnostic, got {:?}",
			params.diagnostics
		);
		assert_eq!(
			params.diagnostics[0].severity,
			Some(DiagnosticSeverity::ERROR)
		);
	}

	#[test]
	fn a_clean_program_publishes_zero_diagnostics() {
		let uri: Uri = "file:///ok.nym".parse().unwrap();
		let text = "func main(): void = {\n  let x: int = 1\n}";
		let docs = open_doc(&uri, text);
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &uri).unwrap();

		let msg = client.receiver.recv().unwrap();
		let Message::Notification(not) = msg else {
			panic!("expected a notification")
		};
		let params: PublishDiagnosticsParams = serde_json::from_value(not.params).unwrap();
		assert_eq!(params.diagnostics, Vec::new());
	}

	#[test]
	fn a_found_invalid_manifest_publishes_a_manifest_diagnostic_not_source_analysis() {
		let tmp = TempDir::new();
		let manifest = tmp.0.join("nymph.toml");
		std::fs::write(&manifest, "not = [toml").unwrap();
		std::fs::create_dir_all(tmp.0.join("src")).unwrap();
		let source_path = tmp.0.join("src/main.nym");
		let uri = crate::workspace::path_to_uri(&source_path).unwrap();
		let docs = open_doc(&uri, "this is also invalid Nymph");
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &uri).unwrap();

		let Message::Notification(notification) = client.receiver.recv().unwrap() else {
			panic!("expected diagnostics notification")
		};
		let params: PublishDiagnosticsParams = serde_json::from_value(notification.params).unwrap();
		assert_eq!(params.diagnostics.len(), 1);
		assert_eq!(
			params.diagnostics[0].code,
			Some(NumberOrString::String("MANIFEST".to_string()))
		);
		assert!(params.diagnostics[0].message.contains("malformed TOML"));
		assert!(
			params.diagnostics[0]
				.message
				.contains(&manifest.display().to_string())
		);
	}

	/// A scratch directory under the system temp dir, removed on drop —
	/// project-mode checking needs real files on disk (the loader shells
	/// out to `std::fs::read_to_string`).
	struct TempDir(std::path::PathBuf);

	impl TempDir {
		fn new() -> Self {
			use std::sync::atomic::{AtomicU64, Ordering};
			static COUNTER: AtomicU64 = AtomicU64::new(0);
			let n = COUNTER.fetch_add(1, Ordering::Relaxed);
			let dir = std::env::temp_dir().join(format!(
				"nymph-lsp-diag-test-{}-{n}-{:?}",
				std::process::id(),
				std::time::SystemTime::now()
					.duration_since(std::time::UNIX_EPOCH)
					.unwrap()
					.as_nanos()
			));
			std::fs::create_dir_all(&dir).unwrap();
			Self(dir)
		}
	}

	impl Drop for TempDir {
		fn drop(&mut self) {
			let _ = std::fs::remove_dir_all(&self.0);
		}
	}

	#[test]
	fn a_two_module_project_surfaces_its_diagnostic_on_the_importee() {
		let tmp = TempDir::new();
		std::fs::write(
			tmp.0.join("nymph.toml"),
			"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
		)
		.unwrap();
		std::fs::create_dir_all(tmp.0.join("src")).unwrap();
		let a_path = tmp.0.join("src/a.nym");
		let b_path = tmp.0.join("src/b.nym");
		std::fs::write(&a_path, "import @/b with (broken)\n").unwrap();
		// A genuine type mismatch: `broken`'s declared return type is `int`
		// but its body is a `boolean` literal.
		std::fs::write(&b_path, "public func broken(): int = true\n").unwrap();

		let a_uri = crate::workspace::path_to_uri(&a_path).unwrap();
		let b_uri = crate::workspace::path_to_uri(&b_path).unwrap();
		let docs = open_doc(&a_uri, "import @/b with (broken)\n");
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &a_uri).unwrap();

		// Two modules are touched (a: clean, entry; b: the error) — collect
		// both published notifications, keyed by the uri's string form
		// (`Uri` itself has interior-mutable caching internals unsuited to
		// being a hash key — `clippy::mutable_key_type`).
		let mut published: std::collections::HashMap<String, Vec<LspDiagnostic>> = Default::default();
		for _ in 0..2 {
			let msg = client.receiver.recv().unwrap();
			let Message::Notification(not) = msg else {
				panic!("expected a notification")
			};
			let params: PublishDiagnosticsParams = serde_json::from_value(not.params).unwrap();
			published.insert(params.uri.as_str().to_string(), params.diagnostics);
		}

		let a_diags = published
			.get(a_uri.as_str())
			.expect("a.nym should be republished");
		assert_eq!(a_diags, &Vec::new(), "a.nym has no error of its own");

		let b_diags = published
			.get(b_uri.as_str())
			.expect("b.nym's diagnostic should be published against its own file");
		assert_eq!(
			b_diags.len(),
			1,
			"expected exactly one diagnostic on b.nym, got {b_diags:?}"
		);
		assert_eq!(b_diags[0].severity, Some(DiagnosticSeverity::ERROR));
	}

	#[test]
	fn fixed_dependency_is_republished_with_empty_diagnostics() {
		let tmp = TempDir::new();
		std::fs::write(
			tmp.0.join("nymph.toml"),
			"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
		)
		.unwrap();
		std::fs::create_dir_all(tmp.0.join("src")).unwrap();
		let importer_path = tmp.0.join("src/importer.nym");
		let dependency_path = tmp.0.join("src/dependency.nym");
		let importer_text = "import @/dependency with (value)\nfunc use(): int = value()\n";
		std::fs::write(&importer_path, importer_text).unwrap();
		std::fs::write(&dependency_path, "public func value(): int = true\n").unwrap();
		let importer_uri = crate::workspace::path_to_uri(&importer_path).unwrap();
		let dependency_uri = crate::workspace::path_to_uri(&dependency_path).unwrap();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let compiler = Arc::new(Mutex::new(CompilerState::new()));
		compiler
			.lock()
			.unwrap()
			.open(
				&mut docs.lock().unwrap(),
				importer_uri.clone(),
				importer_text.into(),
				1,
			)
			.unwrap();
		let (server, client) = Connection::memory();

		check_and_publish_state(&server, &docs, &compiler, &importer_uri).unwrap();
		for _ in 0..2 {
			client.receiver.recv().unwrap();
		}
		compiler
			.lock()
			.unwrap()
			.open(
				&mut docs.lock().unwrap(),
				dependency_uri.clone(),
				"public func value(): int = 1\n".into(),
				1,
			)
			.unwrap();
		compiler
			.lock()
			.unwrap()
			.change(
				&mut docs.lock().unwrap(),
				&importer_uri,
				importer_text.into(),
				2,
			)
			.unwrap();

		check_and_publish_state(&server, &docs, &compiler, &importer_uri).unwrap();

		let mut dependency_clear = None;
		for message in client.receiver.try_iter() {
			let Message::Notification(notification) = message else {
				panic!("expected diagnostics notification")
			};
			let params: PublishDiagnosticsParams = serde_json::from_value(notification.params).unwrap();
			if params.uri == dependency_uri {
				dependency_clear = Some(params.diagnostics);
			}
		}
		assert_eq!(dependency_clear, Some(Vec::new()));
	}

	#[test]
	fn live_buffer_edit_in_entry_module_is_checked() {
		let tmp = TempDir::new();
		std::fs::write(
			tmp.0.join("nymph.toml"),
			"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
		)
		.unwrap();
		std::fs::create_dir_all(tmp.0.join("src")).unwrap();
		let a_path = tmp.0.join("src/a.nym");
		// Clean on disk.
		std::fs::write(&a_path, "func main(): void = {}\n").unwrap();

		let a_uri = crate::workspace::path_to_uri(&a_path).unwrap();
		// Live buffer has a type error, unsaved.
		let live_text = "func main(): void = {\n  let x: int = true\n}\n";
		let docs = open_doc(&a_uri, live_text);
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &a_uri).unwrap();

		let msg = client.receiver.recv().unwrap();
		let Message::Notification(not) = msg else {
			panic!("expected a notification")
		};
		let params: PublishDiagnosticsParams = serde_json::from_value(not.params).unwrap();
		assert!(
			!params.diagnostics.is_empty(),
			"expected the live buffer's type error to be reported, got zero diagnostics"
		);
	}

	fn project_fixture(
		main: &str,
		files: &[(&str, &str)],
	) -> (TempDir, Uri, Arc<Mutex<DocumentStore>>) {
		let tmp = TempDir::new();
		std::fs::write(
			tmp.0.join("nymph.toml"),
			"[package]\nname = \"stdlib-diagnostics\"\nversion = \"0.1.0\"\n",
		)
		.unwrap();
		std::fs::create_dir_all(tmp.0.join("src")).unwrap();
		let main_path = tmp.0.join("src/main.nym");
		std::fs::write(&main_path, main).unwrap();
		for (module, source) in files {
			let path = tmp.0.join("src").join(format!("{module}.nym"));
			std::fs::create_dir_all(path.parent().unwrap()).unwrap();
			std::fs::write(path, source).unwrap();
		}
		let main_uri = crate::workspace::path_to_uri(&main_path).unwrap();
		let docs = open_doc(&main_uri, main);
		(tmp, main_uri, docs)
	}

	fn published(client: &Connection) -> Vec<PublishDiagnosticsParams> {
		client
			.receiver
			.try_iter()
			.map(|message| {
				let Message::Notification(notification) = message else {
					panic!("expected a diagnostics notification")
				};
				serde_json::from_value(notification.params).unwrap()
			})
			.collect()
	}

	#[test]
	fn project_diagnostics_resolve_available_and_transitive_embedded_std_without_synthetic_uris() {
		// Cover every compiler-owned importable std module directly, then reach a
		// nested provider path again through a transitive project import. None of
		// these std modules exists under this project's src root.
		let main = "import std/io as embedded_io\nimport std/collections/set as embedded_set\nimport std/collections/linked_list as embedded_linked_list\nimport std/collections/tree as embedded_tree\nimport std/math/complex as embedded_complex\nimport @/helper with (leaf)\nfunc use(): int = leaf()\n";
		let (_tmp, main_uri, docs) = project_fixture(
			main,
			&[(
				"helper",
				"import std/collections/tree with (Tree)\npublic func leaf(): int = match (Tree.Leaf(value = 1)) { Tree.Leaf(value) -> value, Tree.Node(...) -> 0 }\n",
			)],
		);
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &main_uri).unwrap();

		let notifications = published(&client);
		assert_eq!(
			notifications.len(),
			1,
			"provider-only modules were published"
		);
		assert_eq!(notifications[0].uri, main_uri);
		assert!(
			notifications
				.iter()
				.all(|params| params.diagnostics.is_empty()),
			"available embedded std import diagnosed: {:?}",
			notifications
		);
	}

	#[test]
	fn missing_embedded_std_is_diagnosed_on_the_importing_project_module() {
		let main = "import std/definitely_missing\nfunc helper(): int = 1\n";
		let (_tmp, main_uri, docs) = project_fixture(main, &[]);
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &main_uri).unwrap();

		let notifications = published(&client);
		assert_eq!(notifications.len(), 1);
		assert_eq!(notifications[0].uri, main_uri);
		assert!(notifications[0].diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("IMPORT-UNRESOLVED".into()))
		}));
	}

	#[test]
	fn semantic_errors_with_std_imports_are_published_on_the_project_source() {
		let main = "import std/collections/tree with (Tree)\nimport @/broken with (broken)\nfunc helper(): Tree<int> = Tree.Leaf(value = broken())\n";
		let (_tmp, main_uri, docs) =
			project_fixture(main, &[("broken", "public func broken(): int = true\n")]);
		let broken_uri = main_uri
			.as_str()
			.replace("main.nym", "broken.nym")
			.parse::<Uri>()
			.unwrap();
		let (server, client) = Connection::memory();

		check_and_publish(&server, &docs, &main_uri).unwrap();

		let notifications = published(&client);
		assert_eq!(notifications.len(), 2);
		let broken = notifications
			.iter()
			.find(|params| params.uri == broken_uri)
			.expect("semantic error must be attributed to broken.nym");
		assert!(broken.diagnostics.iter().any(|diagnostic| {
			diagnostic.severity == Some(DiagnosticSeverity::ERROR)
				&& diagnostic.code != Some(NumberOrString::String("IMPORT-UNRESOLVED".into()))
		}));
		assert!(
			notifications
				.iter()
				.find(|params| params.uri == main_uri)
				.unwrap()
				.diagnostics
				.is_empty()
		);
	}

	#[test]
	fn project_overlay_wins_for_colliding_project_path_without_shadowing_embedded_std() {
		let main = "import std/collections/tree with (Tree)\nimport @/std/collections/tree as project_tree\nfunc helper(): Tree<int> = Tree.Leaf(value = project_tree.project_value())\n";
		let (_tmp, main_uri, docs) = project_fixture(
			main,
			&[(
				"std/collections/tree",
				"public func project_value(): int = true\n",
			)],
		);
		let project_uri = main_uri
			.as_str()
			.replace("main.nym", "std/collections/tree.nym")
			.parse::<Uri>()
			.unwrap();
		let mut state = CompilerState::new();
		state
			.open(&mut docs.lock().unwrap(), main_uri.clone(), main.into(), 1)
			.unwrap();
		state
			.open(
				&mut docs.lock().unwrap(),
				project_uri,
				"public func project_value(): int = 1\n".into(),
				1,
			)
			.unwrap();

		let diagnostics = state
			.diagnostics_for_uri(&docs.lock().unwrap(), &main_uri)
			.unwrap();
		assert!(
			diagnostics.is_empty(),
			"project overlay and embedded std should resolve in separate domains: {diagnostics:?}"
		);
	}
}
