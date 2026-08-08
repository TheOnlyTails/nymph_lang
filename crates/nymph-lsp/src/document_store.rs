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

#[derive(Clone, Default)]
pub struct DocumentStore {
	docs: HashMap<Uri, Document>,
}

impl DocumentStore {
	/// Record a newly opened document (`textDocument/didOpen`).
	pub fn open(&mut self, uri: Uri, text: String, version: i32) {
		self.docs.insert(uri, Document { text, version });
	}

	/// Replace a document's full text (`textDocument/didChange` under FULL
	/// sync — every change event carries the whole new text, so only the
	/// last one in a batch matters). Inserts the document if it was somehow
	/// not already open, rather than silently dropping the edit.
	pub fn change_full(&mut self, uri: &Uri, text: String, version: i32) {
		if let Some(doc) = self.docs.get_mut(uri) {
			doc.text = text;
			doc.version = version;
		} else {
			self.docs.insert(uri.clone(), Document { text, version });
		}
	}

	/// Forget a closed document (`textDocument/didClose`).
	pub fn close(&mut self, uri: &Uri) {
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

	pub fn iter(&self) -> impl Iterator<Item = (&Uri, &Document)> {
		self.docs.iter()
	}
}
