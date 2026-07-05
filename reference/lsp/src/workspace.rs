use crate::document::Document;
use smol::lock::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tower_lsp::lsp_types::Range;

/// The workspace manages all open documents
pub struct Workspace {
	documents: Arc<RwLock<HashMap<String, Document>>>,
}

impl Workspace {
	#[must_use]
	pub fn new() -> Self {
		Self {
			documents: Arc::new(RwLock::new(HashMap::new())),
		}
	}

	/// Open a document
	pub async fn open_document(&self, uri: String, content: String) {
		let doc_uri = uri.clone();
		let doc = smol::unblock(move || Document::new(doc_uri, content)).await;
		self.documents.write().await.insert(uri, doc);
	}

	/// Update a document
	pub async fn update_document(&self, uri: String, content: String) {
		let exists = self.documents.read().await.contains_key(&uri);
		if exists {
			let doc_uri = uri.clone();
			let doc = smol::unblock(move || Document::new(doc_uri, content)).await;
			self.documents.write().await.insert(uri, doc);
		}
	}

	pub async fn apply_document_change(&self, uri: &str, range: Range, text: String) {
		let Some(doc) = self.get_document(uri, Clone::clone).await else {
			return;
		};

		let updated = smol::unblock(move || {
			let mut doc = doc;
			doc.apply_lsp_change(&range, &text)?;
			Ok::<_, String>(doc)
		})
		.await;

		if let Ok(doc) = updated {
			self.documents.write().await.insert(uri.to_string(), doc);
		}
	}

	/// Close a document
	pub async fn close_document(&self, uri: &str) {
		self.documents.write().await.remove(uri);
	}

	/// Get a document (read-only)
	pub async fn get_document<F, R>(&self, uri: &str, f: F) -> Option<R>
	where
		F: FnOnce(&Document) -> R,
	{
		let docs = self.documents.read().await;
		docs.get(uri).map(f)
	}

	/// Get a mutable document reference
	pub async fn get_document_mut<F, R>(&self, uri: &str, f: F) -> Option<R>
	where
		F: FnOnce(&mut Document) -> R,
	{
		let mut docs = self.documents.write().await;
		docs.get_mut(uri).map(f)
	}

	/// List all open documents
	pub async fn list_documents(&self) -> Vec<String> {
		self.documents.read().await.keys().cloned().collect()
	}

	/// Snapshot all open documents
	pub async fn documents(&self) -> Vec<Document> {
		self.documents.read().await.values().cloned().collect()
	}

	/// Get document count
	pub async fn document_count(&self) -> usize {
		self.documents.read().await.len()
	}
}

impl Default for Workspace {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_workspace_open_document() {
		smol::block_on(async {
			let ws = Workspace::new();
			ws.open_document("file:///test.nymph".to_string(), "let x = 5".to_string())
				.await;

			let count = ws.document_count().await;
			assert_eq!(count, 1);
		});
	}

	#[test]
	fn test_workspace_get_document() {
		smol::block_on(async {
			let ws = Workspace::new();
			ws.open_document("file:///test.nymph".to_string(), "let x = 5".to_string())
				.await;

			let content = ws
				.get_document("file:///test.nymph", |doc| doc.content.clone())
				.await;
			assert_eq!(content, Some("let x = 5".to_string()));
		});
	}

	#[test]
	fn test_workspace_update_document() {
		smol::block_on(async {
			let ws = Workspace::new();
			ws.open_document("file:///test.nymph".to_string(), "let x = 5".to_string())
				.await;
			ws.update_document("file:///test.nymph".to_string(), "let x = 10".to_string())
				.await;

			let content = ws
				.get_document("file:///test.nymph", |doc| doc.content.clone())
				.await;
			assert_eq!(content, Some("let x = 10".to_string()));
		});
	}

	#[test]
	fn test_workspace_close_document() {
		smol::block_on(async {
			let ws = Workspace::new();
			ws.open_document("file:///test.nymph".to_string(), "let x = 5".to_string())
				.await;
			ws.close_document("file:///test.nymph").await;

			let count = ws.document_count().await;
			assert_eq!(count, 0);
		});
	}

	#[test]
	fn test_workspace_documents_snapshot() {
		smol::block_on(async {
			let ws = Workspace::new();
			ws.open_document("file:///one.nymph".to_string(), "let x = 1".to_string())
				.await;
			ws.open_document("file:///two.nymph".to_string(), "let y = 2".to_string())
				.await;

			let docs = ws.documents().await;
			assert_eq!(docs.len(), 2);
		});
	}
}
