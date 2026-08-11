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
	/// Owner revision of the notification that last opened or changed this
	/// URI. Worker-local compiler sessions use it to replay equivalent URI
	/// overlays in the same authoritative order as the protocol owner.
	pub(crate) update_revision: DocumentStoreRevision,
	/// Owner revision that began this URI's current open lifecycle.
	pub(crate) lifecycle_revision: DocumentStoreRevision,
}

/// Monotonic identity for one complete state of the open-document overlays.
/// Unlike an LSP document version, this spans URIs and open/close lifecycles,
/// so snapshots that depend on imports can be rejected after any overlay
/// changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentStoreRevision(u64);

#[derive(Clone, Default)]
pub struct DocumentStore {
	docs: HashMap<Uri, Document>,
	revision: DocumentStoreRevision,
	filesystem_revision: u64,
}

impl DocumentStore {
	/// Record a newly opened document (`textDocument/didOpen`).
	pub fn open(&mut self, uri: Uri, text: String, version: i32) {
		self.advance_revision();
		self.docs.insert(
			uri,
			Document {
				text,
				version,
				update_revision: self.revision,
				lifecycle_revision: self.revision,
			},
		);
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
		doc.update_revision = self.revision;
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

	/// Open documents in protocol-authoritative update order. The URI tie-break
	/// is defensive (revisions are unique) and keeps reconstruction deterministic.
	pub(crate) fn documents_in_update_order(&self) -> Vec<(&Uri, &Document)> {
		let mut documents = self.docs.iter().collect::<Vec<_>>();
		documents.sort_by(|(left_uri, left), (right_uri, right)| {
			left
				.update_revision
				.cmp(&right.update_revision)
				.then_with(|| left_uri.as_str().cmp(right_uri.as_str()))
		});
		documents
	}

	/// Advance the shared publication revision for a filesystem event. Open
	/// document contents are unchanged, but project snapshots may now contain
	/// different disk-backed modules or manifest discovery results.
	pub fn filesystem_changed(&mut self) {
		self.advance_revision();
		self.filesystem_revision = self
			.filesystem_revision
			.checked_add(1)
			.expect("document store filesystem revision exhausted");
	}

	#[must_use]
	pub(crate) fn filesystem_revision(&self) -> u64 {
		self.filesystem_revision
	}

	fn advance_revision(&mut self) {
		self.revision.0 = self
			.revision
			.0
			.checked_add(1)
			.expect("document store revision exhausted");
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn untitled_lifecycle_is_uri_keyed_and_never_resurrected_by_change() {
		let uri: Uri = "untitled:Untitled-52".parse().unwrap();
		let mut store = DocumentStore::default();

		store.open(uri.clone(), "let value = 1".into(), 7);
		assert_eq!(store.get(&uri).unwrap().text, "let value = 1");
		assert_eq!(store.version(&uri), Some(7));
		let open_revision = store.revision();

		assert!(store.change_full(&uri, "let value = 2".into(), 8));
		assert_eq!(store.get(&uri).unwrap().text, "let value = 2");
		assert_eq!(store.version(&uri), Some(8));
		assert_ne!(store.revision(), open_revision);

		store.close(&uri);
		let close_revision = store.revision();
		assert!(store.get(&uri).is_none());
		assert!(!store.change_full(&uri, "let stale = true".into(), 9));
		assert_eq!(store.revision(), close_revision);
		assert!(store.get(&uri).is_none());
	}
}
