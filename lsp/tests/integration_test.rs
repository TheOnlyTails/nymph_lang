#[cfg(test)]
mod tests {

	#[test]
	fn test_lsp_crate_loads() {
		// Basic test that the LSP crate loads
		// More complex integration tests would require a real LSP client
	}

	#[test]
	fn test_document_parsing() {
		use nymph_lsp::document::Document;

		let doc = Document::new("file:///test.nymph".to_string(), "let x = 5".to_string());

		assert_eq!(doc.uri, "file:///test.nymph");
		assert_eq!(doc.content, "let x = 5");
	}

	#[test]
	fn test_document_update() {
		use nymph_lsp::document::Document;

		let mut doc = Document::new("file:///test.nymph".to_string(), "let x = 5".to_string());

		doc.update("let y = 10".to_string());
		assert_eq!(doc.content, "let y = 10");
	}

	#[test]
	fn test_semantic_tokenizer() {
		use nymph_lsp::semantic_tokens::{SemanticTokenizer, TokenType};

		let mut tokenizer = SemanticTokenizer::new();
		let tokens = tokenizer.tokenize("let x = 5\nfn foo() {}");

		// Should find 'let' and 'fn' keywords
		let keyword_tokens: Vec<_> = tokens
			.iter()
			.filter(|t| t.token_type == TokenType::Keyword)
			.collect();

		assert!(!keyword_tokens.is_empty());
	}

	#[tokio::test]
	async fn test_workspace() {
		use nymph_lsp::workspace::Workspace;

		let ws = Workspace::new();
		ws.open_document("file:///test.nymph".to_string(), "let x = 5".to_string())
			.await;

		let count = ws.document_count().await;
		assert_eq!(count, 1);

		let content = ws
			.get_document("file:///test.nymph", |doc| doc.content.clone())
			.await;
		assert_eq!(content, Some("let x = 5".to_string()));

		ws.close_document("file:///test.nymph").await;
		let count = ws.document_count().await;
		assert_eq!(count, 0);
	}
}
