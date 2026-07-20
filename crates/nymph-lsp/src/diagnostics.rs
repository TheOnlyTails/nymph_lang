//! Re-check a document and republish its diagnostics.
//!
//! Loose mode (Task 2): no `nymph.toml` project is found for the document's
//! file, so it's checked standalone via [`nymph_compiler::check`] — full
//! parse + check diagnostics (errors and warnings), no `main` required.
//! Project mode (Task 3) is layered in on top for documents that do sit
//! inside a discovered project.

use std::{
	collections::BTreeMap,
	sync::{Arc, Mutex},
};

use lsp_server::Connection;
use lsp_types::{
	Diagnostic as LspDiagnostic, DiagnosticSeverity, NumberOrString, PublishDiagnosticsParams, Uri,
	notification::{Notification as _, PublishDiagnostics},
};
use nymph_diagnostics::{Diagnostic, Severity};

use crate::{document_store::DocumentStore, line_index::LineIndex, workspace};

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
pub fn check_and_publish(
	connection: &Connection,
	docs: &Arc<Mutex<DocumentStore>>,
	uri: &Uri,
) -> anyhow::Result<()> {
	let text = {
		let docs = docs.lock().unwrap();
		match docs.get(uri) {
			Some(doc) => doc.text.clone(),
			None => return Ok(()),
		}
	};

	let file_path = workspace::uri_to_path(uri);

	// A stdlib SOURCE file (e.g. `stdlib/src/ops/mod.nym`) must never be checked
	// against the ambient `core` prelude: `check` always injects a fresh parse of
	// `core_prelude()` ahead of the checked module, and for a file that IS part of
	// that very prelude, that injects a second copy of itself right next to the
	// real one — every declaration collides with its own ambient copy. Short-
	// circuit BEFORE `workspace::detect` (rather than only in the loose-mode `None`
	// arm below) so the fix holds regardless of whether the file resolves loose or
	// project mode; a normal user file never matches this and is unaffected.
	if nymph_compiler::is_stdlib_source_path(&file_path) {
		let diags = nymph_compiler::check_without_prelude(&text, uri.path().as_str());
		return publish(connection, uri, &text, &diags);
	}

	match workspace::detect(&file_path) {
		Some(project) => check_and_publish_project(connection, &project, &text, docs),
		None => {
			let diags = nymph_compiler::check(&text, uri.path().as_str());
			publish(connection, uri, &text, &diags)
		}
	}
}

/// Project-mode branch of [`check_and_publish`]: whole-graph check rooted
/// at `project.entry_key`, diagnostics grouped by module and republished
/// per file. The check runs against a BUFFER-AWARE loader: the entry module
/// (the file the client is editing) and any other OPEN module are checked
/// against their live editor buffers — not the on-disk copies — so unsaved
/// edits produce diagnostics immediately; modules that are not open fall back
/// to disk. Diagnostics are then rendered against the same buffer text.
fn check_and_publish_project(
	connection: &Connection,
	project: &workspace::Project,
	text: &str,
	docs: &Arc<Mutex<DocumentStore>>,
) -> anyhow::Result<()> {
	let disk = workspace::fs_loader(project.src_root.clone());
	let loader = |key: &str| -> Option<String> {
		// The entry is special-cased against `text` directly (rather than via
		// `key_to_uri` → store lookup) so a URI-canonicalization mismatch can
		// never fall the file the client is editing back to its stale disk copy.
		if key == project.entry_key {
			return Some(text.to_string());
		}
		if let Some(uri) = workspace::key_to_uri(&project.src_root, key)
			&& let Some(doc) = docs.lock().unwrap().get(&uri)
		{
			return Some(doc.text.clone());
		}
		disk(key)
	};
	let project_diags = nymph_compiler::check_project_library(&project.entry_key, &loader);

	let mut by_module: BTreeMap<String, Vec<Diagnostic>> = BTreeMap::new();
	// Ensure the entry module (the file the client just edited) is always
	// republished, even with zero diagnostics, so a fix clears stale marks.
	by_module.entry(project.entry_key.clone()).or_default();
	for d in project_diags {
		by_module.entry(d.module).or_default().push(d.diag);
	}

	for (module, diags) in by_module {
		let Some(module_uri) = workspace::key_to_uri(&project.src_root, &module) else {
			continue;
		};
		let source = if module == project.entry_key {
			text.to_string()
		} else {
			loader(&module).unwrap_or_default()
		};
		publish(connection, &module_uri, &source, &diags)?;
	}
	Ok(())
}

/// Publish `diags` (already anchored against `text`) for `uri`.
fn publish(
	connection: &Connection,
	uri: &Uri,
	text: &str,
	diags: &[Diagnostic],
) -> anyhow::Result<()> {
	let index = LineIndex::new(text);
	let lsp_diags: Vec<LspDiagnostic> = diags.iter().map(|d| to_lsp(d, text, &index)).collect();
	let params = PublishDiagnosticsParams {
		uri: uri.clone(),
		diagnostics: lsp_diags,
		version: None,
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
