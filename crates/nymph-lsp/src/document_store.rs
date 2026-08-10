//! An in-memory table of every document the client currently has open,
//! keyed by its LSP [`Uri`]. Updated on `textDocument/didOpen`,
//! `textDocument/didChange` (full-document sync — see the server's
//! advertised `textDocumentSync: FULL` capability), and
//! `textDocument/didClose`.

use lsp_types::Uri;
use std::collections::HashMap;

/// One open document: its full current text and the client's version
/// counter. The version orders notifications and guards response publication;
/// it is not an analysis-cache key. Compiler/Salsa reuse follows effective
/// source content and dependency revisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
	pub text: String,
	pub version: i32,
}

/// Monotonic identity for one complete state of the open-document overlays.
/// Unlike an LSP document version, this spans URIs and open/close lifecycles,
/// so snapshots that depend on imports can be rejected after any overlay
/// changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentStoreRevision(u64);

#[derive(Clone, Default)]
pub struct DocumentStore {
	docs: HashMap<Uri, Document>,
	revision: DocumentStoreRevision,
}

impl DocumentStore {
	/// Record a newly opened document (`textDocument/didOpen`).
	pub fn open(&mut self, uri: Uri, text: String, version: i32) {
		self.advance_revision();
		self.docs.insert(uri, Document { text, version });
	}

	/// Replace an open document's full text (`textDocument/didChange` under
	/// FULL sync — every change event carries the whole new text, so only the
	/// last one in a batch matters). Returns `false` for a stale notification
	/// received after `didClose`; only `didOpen` starts a document lifecycle.
	pub fn change_full(&mut self, uri: &Uri, text: String, version: i32) -> bool {
		if !self.docs.contains_key(uri) {
			return false;
		}
		self.advance_revision();
		let doc = self.docs.get_mut(uri).expect("document checked above");
		doc.text = text;
		doc.version = version;
		true
	}

	/// Forget a closed document (`textDocument/didClose`).
	pub fn close(&mut self, uri: &Uri) {
		self.advance_revision();
		self.docs.remove(uri);
	}

	#[must_use]
	pub fn get(&self, uri: &Uri) -> Option<&Document> {
		self.docs.get(uri)
	}

	#[must_use]
	pub fn version(&self, uri: &Uri) -> Option<i32> {
		self.docs.get(uri).map(|document| document.version)
	}

	#[must_use]
	pub fn revision(&self) -> DocumentStoreRevision {
		self.revision
	}

	pub fn iter(&self) -> impl Iterator<Item = (&Uri, &Document)> {
		self.docs.iter()
	}

	/// Advance the shared publication revision for a filesystem event. Open
	/// document contents are unchanged, but project snapshots may now contain
	/// different disk-backed modules or manifest discovery results.
	pub fn filesystem_changed(&mut self) {
		self.advance_revision();
	}

	fn advance_revision(&mut self) {
		self.revision.0 = self
			.revision
			.0
			.checked_add(1)
			.expect("document store revision exhausted");
	}
}
