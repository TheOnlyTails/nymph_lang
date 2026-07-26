//! Re-check a document and republish its diagnostics.
//!
//! Loose mode (Task 2): no `nymph.toml` project is found for the document's
//! file, so it's checked standalone via [`nymph_compiler::check`] — full
//! parse + check diagnostics (errors and warnings), no `main` required.
//! Project mode (Task 3) is layered in on top for documents that do sit
//! inside a discovered project.

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
/// [`workspace::detect`]), the WHOLE project graph reachable from `uri`'s
/// own module (it plus its transitive `import` closure — see
/// [`nymph_compiler::check_project_library`]'s doc comment on that limit) is
/// checked, and every touched module's diagnostics are republished against
/// its own file. Otherwise `uri` is checked standalone (loose mode).
pub fn check_and_publish_state(
	connection: &Connection,
	docs: &Arc<Mutex<DocumentStore>>,
	compiler: &Arc<Mutex<CompilerState>>,
	uri: &Uri,
) -> anyhow::Result<()> {
	// Copy the document table first, then release its mutex before entering the
	// compiler. Publication below holds neither mutex.
	let docs_snapshot = docs.lock().unwrap().clone();
	let snapshot = compiler
		.lock()
		.unwrap()
		.diagnostics_snapshot(&docs_snapshot, uri);
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
}
