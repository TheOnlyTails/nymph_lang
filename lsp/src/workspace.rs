use crate::document::Document;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

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
		let doc = Document::new(uri.clone(), content);
		self.documents.write().await.insert(uri, doc);
	}

	/// Update a document
	pub async fn update_document(&self, uri: String, content: String) {
		let mut docs = self.documents.write().await;
		if let Some(doc) = docs.get_mut(&uri) {
			doc.update(content);
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
	use tokio;

	#[tokio::test]
	async fn test_workspace_open_document() {
		let ws = Workspace::new();
		ws.open_document("file:///test.nymph".to_string(), "let x = 5".to_string())
			.await;

		let count = ws.document_count().await;
		assert_eq!(count, 1);
	}

	#[tokio::test]
	async fn test_workspace_get_document() {
		let ws = Workspace::new();
		ws.open_document("file:///test.nymph".to_string(), "let x = 5".to_string())
			.await;

		let content = ws
			.get_document("file:///test.nymph", |doc| doc.content.clone())
			.await;
		assert_eq!(content, Some("let x = 5".to_string()));
	}

	#[tokio::test]
	async fn test_workspace_update_document() {
		let ws = Workspace::new();
		ws.open_document("file:///test.nymph".to_string(), "let x = 5".to_string())
			.await;
		ws.update_document("file:///test.nymph".to_string(), "let x = 10".to_string())
			.await;

		let content = ws
			.get_document("file:///test.nymph", |doc| doc.content.clone())
			.await;
		assert_eq!(content, Some("let x = 10".to_string()));
	}

	#[tokio::test]
	async fn test_workspace_close_document() {
		let ws = Workspace::new();
		ws.open_document("file:///test.nymph".to_string(), "let x = 5".to_string())
			.await;
		ws.close_document("file:///test.nymph").await;

		let count = ws.document_count().await;
		assert_eq!(count, 0);
	}
}
