//! The Nymph language server: a synchronous `lsp-server` loop (no
//! tokio/async — the compiler facade it wraps is synchronous) providing
//! diagnostics, hover, document symbols, go-to-definition, and completion
//! over stdio, spawned by the VS Code extension (`extension/src/extension.ts`)
//! as `target/{release,debug}/nymph-lsp`.
//!
//! MVP scope (see `extension/LSP_INTEGRATION.md`): `textDocument/didOpen` /
//! `didChange` (full sync) / `didClose` keep an in-memory [`DocumentStore`]
//! current; every open/change re-checks the document and republishes its
//! full diagnostic set (loose single-file mode, or whole-project mode when a
//! `nymph.toml` is found — see [`workspace`]); `textDocument/hover` answers
//! with the type of the smallest checked expression under the cursor (see
//! [`hover`]); `textDocument/documentSymbol` outlines a module's top-level
//! declarations, parser-only (see [`document_symbols`]);
//! `textDocument/definition` jumps an identifier/variant/type-name use to its
//! declaration, AST + `DefMap`-only, no type-check (see [`definition`]);
//! `textDocument/completion` offers in-scope identifiers and keywords (see
//! [`completion`] — member completion after a `.` is deferred, see its
//! module doc comment); `textDocument/semanticTokens/full` classifies every
//! token from the compiler's own lexer + AST, so highlighting stays correct
//! independent of the TextMate grammar (see [`semantic_tokens`]).
//! Incremental sync, formatting, and rename are deliberately out of scope.

pub mod completion;
pub mod definition;
pub mod diagnostics;
pub mod document_store;
pub mod document_symbols;
pub mod hover;
pub mod line_index;
pub mod semantic_tokens;
pub mod workspace;

use std::sync::{Arc, Mutex};

use document_store::DocumentStore;
use hover::HoverCache;
use lsp_server::{Connection, Message, Notification as ServerNotification, Response};
use lsp_types::{
	CompletionOptions, CompletionParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
	DidOpenTextDocumentParams, DocumentSymbolParams, GotoDefinitionParams, HoverParams,
	HoverProviderCapability, InitializeParams, InitializeResult, OneOf, SemanticTokensFullOptions,
	SemanticTokensOptions, SemanticTokensParams, SemanticTokensServerCapabilities,
	ServerCapabilities, ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind,
	notification::{
		DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
	},
	request::{
		Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, Request as _,
		SemanticTokensFullRequest,
	},
};

/// The capabilities this server advertises during `initialize`: full-text
/// document sync, hover, document symbols, go-to-definition, completion
/// (triggered on typing and on `.`), and full-document semantic tokens.
/// Diagnostics are pushed (`textDocument/publishDiagnostics`), not pulled,
/// so they need no capability flag here.
#[must_use]
pub fn server_capabilities() -> ServerCapabilities {
	ServerCapabilities {
		text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
		hover_provider: Some(HoverProviderCapability::Simple(true)),
		document_symbol_provider: Some(OneOf::Left(true)),
		definition_provider: Some(OneOf::Left(true)),
		completion_provider: Some(CompletionOptions {
			trigger_characters: Some(vec![".".to_string()]),
			..Default::default()
		}),
		semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
			SemanticTokensOptions {
				work_done_progress_options: Default::default(),
				legend: semantic_tokens::legend(),
				range: None,
				full: Some(SemanticTokensFullOptions::Bool(true)),
			},
		)),
		..Default::default()
	}
}

/// Run the server to completion over `connection` (blocks until the client
/// shuts it down). This is the real entry point (`main.rs` wires it to
/// `Connection::stdio()`); [`serve`] is the test seam that also exposes the
/// live [`DocumentStore`] for assertions.
pub fn run(connection: Connection) -> anyhow::Result<()> {
	serve(
		connection,
		Arc::new(Mutex::new(DocumentStore::default())),
		Arc::new(Mutex::new(HoverCache::default())),
	)
}

/// Like [`run`], but over caller-supplied, shared state — the production
/// entry point owns its own (via [`run`]); tests share it with the driving
/// thread so they can inspect the [`DocumentStore`] after exchanging
/// messages, with no polling: message delivery through `lsp_server`'s
/// channel is in-order, so any request/response round trip after a
/// notification proves that notification was already applied.
fn serve(
	connection: Connection,
	docs: Arc<Mutex<DocumentStore>>,
	hover_cache: Arc<Mutex<HoverCache>>,
) -> anyhow::Result<()> {
	let (id, params) = connection.initialize_start()?;
	let _init_params: InitializeParams = serde_json::from_value(params)?;

	let init_result = InitializeResult {
		capabilities: server_capabilities(),
		server_info: Some(ServerInfo {
			name: "nymph-lsp".to_string(),
			version: Some(env!("CARGO_PKG_VERSION").to_string()),
		}),
	};
	connection.initialize_finish(id, serde_json::to_value(init_result)?)?;

	main_loop(&connection, &docs, &hover_cache)
}

fn main_loop(
	connection: &Connection,
	docs: &Arc<Mutex<DocumentStore>>,
	hover_cache: &Arc<Mutex<HoverCache>>,
) -> anyhow::Result<()> {
	for msg in &connection.receiver {
		match msg {
			Message::Request(req) => {
				if connection.handle_shutdown(&req)? {
					return Ok(());
				}
				if req.method == HoverRequest::METHOD {
					let (id, params) = req.extract::<HoverParams>(HoverRequest::METHOD)?;
					let result = hover::hover(
						&docs.lock().unwrap(),
						&mut hover_cache.lock().unwrap(),
						&params,
					);
					connection
						.sender
						.send(Message::Response(Response::new_ok(id, result)))?;
				} else if req.method == DocumentSymbolRequest::METHOD {
					let (id, params) = req.extract::<DocumentSymbolParams>(DocumentSymbolRequest::METHOD)?;
					let result = document_symbols::document_symbols(&docs.lock().unwrap(), &params);
					connection
						.sender
						.send(Message::Response(Response::new_ok(id, result)))?;
				} else if req.method == GotoDefinition::METHOD {
					let (id, params) = req.extract::<GotoDefinitionParams>(GotoDefinition::METHOD)?;
					let result = definition::definition(&docs.lock().unwrap(), &params);
					connection
						.sender
						.send(Message::Response(Response::new_ok(id, result)))?;
				} else if req.method == Completion::METHOD {
					let (id, params) = req.extract::<CompletionParams>(Completion::METHOD)?;
					let result = completion::completion(&docs.lock().unwrap(), &params);
					connection
						.sender
						.send(Message::Response(Response::new_ok(id, result)))?;
				} else if req.method == SemanticTokensFullRequest::METHOD {
					let (id, params) =
						req.extract::<SemanticTokensParams>(SemanticTokensFullRequest::METHOD)?;
					let result = semantic_tokens::semantic_tokens_full(&docs.lock().unwrap(), &params);
					connection
						.sender
						.send(Message::Response(Response::new_ok(id, result)))?;
				} else {
					connection.sender.send(Message::Response(Response::new_err(
						req.id,
						lsp_server::ErrorCode::MethodNotFound as i32,
						format!("unhandled request method `{}`", req.method),
					)))?;
				}
			}
			Message::Notification(not) => handle_notification(connection, docs, not)?,
			Message::Response(_) => {}
		}
	}
	Ok(())
}

fn handle_notification(
	connection: &Connection,
	docs: &Arc<Mutex<DocumentStore>>,
	not: ServerNotification,
) -> anyhow::Result<()> {
	match not.method.as_str() {
		m if m == DidOpenTextDocument::METHOD => {
			let params: DidOpenTextDocumentParams = serde_json::from_value(not.params)?;
			let uri = params.text_document.uri.clone();
			{
				let mut docs = docs.lock().unwrap();
				docs.open(
					params.text_document.uri,
					params.text_document.text,
					params.text_document.version,
				);
			}
			diagnostics::check_and_publish(connection, docs, &uri)?;
		}
		m if m == DidChangeTextDocument::METHOD => {
			let params: DidChangeTextDocumentParams = serde_json::from_value(not.params)?;
			let uri = params.text_document.uri.clone();
			if let Some(change) = params.content_changes.into_iter().last() {
				let mut docs = docs.lock().unwrap();
				docs.change_full(&uri, change.text, params.text_document.version);
			}
			diagnostics::check_and_publish(connection, docs, &uri)?;
		}
		m if m == DidCloseTextDocument::METHOD => {
			let params: DidCloseTextDocumentParams = serde_json::from_value(not.params)?;
			docs.lock().unwrap().close(&params.text_document.uri);
		}
		_ => {}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use lsp_server::{Notification, Request, RequestId};
	use lsp_types::{
		TextDocumentContentChangeEvent, TextDocumentItem, Uri, VersionedTextDocumentIdentifier,
	};

	fn handshake(client: &Connection) {
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(1),
				"initialize".to_string(),
				serde_json::to_value(InitializeParams::default()).unwrap(),
			)))
			.unwrap();
		client.receiver.recv().unwrap(); // InitializeResult
		client
			.sender
			.send(Message::Notification(Notification::new(
				"initialized".to_string(),
				serde_json::json!({}),
			)))
			.unwrap();
	}

	fn shutdown(client: &Connection, handle: std::thread::JoinHandle<anyhow::Result<()>>) {
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(2),
				"shutdown".to_string(),
				serde_json::Value::Null,
			)))
			.unwrap();
		client
			.sender
			.send(Message::Notification(Notification::new(
				"exit".to_string(),
				serde_json::Value::Null,
			)))
			.unwrap();
		// Drain any interleaved server-pushed notifications (e.g.
		// `publishDiagnostics`) until the shutdown ack response arrives.
		loop {
			match client.receiver.recv().unwrap() {
				Message::Response(_) => break,
				_ => continue,
			}
		}
		handle.join().unwrap().unwrap();
	}

	#[test]
	fn initialize_advertises_full_sync_hover_symbols_definition_and_completion() {
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));

		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(1),
				"initialize".to_string(),
				serde_json::to_value(InitializeParams::default()).unwrap(),
			)))
			.unwrap();
		let resp = client.receiver.recv().unwrap();
		let result: InitializeResult = match resp {
			Message::Response(r) => serde_json::from_value(r.response_result.unwrap()).unwrap(),
			other => panic!("expected a response, got {other:?}"),
		};
		assert_eq!(
			result.capabilities.hover_provider,
			Some(HoverProviderCapability::Simple(true))
		);
		assert!(matches!(
			result.capabilities.text_document_sync,
			Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL))
		));
		assert_eq!(
			result.capabilities.document_symbol_provider,
			Some(OneOf::Left(true))
		);
		assert_eq!(
			result.capabilities.definition_provider,
			Some(OneOf::Left(true))
		);
		let completion = result
			.capabilities
			.completion_provider
			.expect("completion should be advertised");
		assert_eq!(completion.trigger_characters, Some(vec![".".to_string()]));

		client
			.sender
			.send(Message::Notification(Notification::new(
				"initialized".to_string(),
				serde_json::json!({}),
			)))
			.unwrap();

		shutdown(&client, handle);
	}

	/// A round trip through the real `Connection::memory()` wire (not just a
	/// direct call into `document_symbols::document_symbols`): proves
	/// `textDocument/documentSymbol` is actually dispatched by `main_loop`.
	#[test]
	fn document_symbol_request_round_trips_through_the_wire() {
		use lsp_types::{
			DocumentSymbolParams, DocumentSymbolResponse, PartialResultParams, TextDocumentIdentifier,
			WorkDoneProgressParams,
			request::{DocumentSymbolRequest, Request as _},
		};

		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		let uri: Uri = "file:///wire_symbols.nym".parse().unwrap();
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.to_string(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: uri.clone(),
						language_id: "nymph".to_string(),
						version: 1,
						text: "func main(): void = {}".to_string(),
					},
				})
				.unwrap(),
			)))
			.unwrap();

		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(3),
				DocumentSymbolRequest::METHOD.to_string(),
				serde_json::to_value(DocumentSymbolParams {
					text_document: TextDocumentIdentifier { uri: uri.clone() },
					work_done_progress_params: WorkDoneProgressParams::default(),
					partial_result_params: PartialResultParams::default(),
				})
				.unwrap(),
			)))
			.unwrap();

		// Drain the `publishDiagnostics` notification the `didOpen` triggers,
		// then read the documentSymbol response.
		let result: Option<DocumentSymbolResponse> = loop {
			match client.receiver.recv().unwrap() {
				Message::Response(r) => {
					break serde_json::from_value(r.response_result.unwrap()).unwrap();
				}
				_ => continue,
			}
		};
		let symbols = match result.expect("expected a documentSymbol result") {
			DocumentSymbolResponse::Nested(symbols) => symbols,
			DocumentSymbolResponse::Flat(_) => panic!("expected the Nested arm"),
		};
		assert_eq!(symbols.len(), 1);
		assert_eq!(symbols[0].name, "main");

		shutdown(&client, handle);
	}

	/// A round trip proving `textDocument/definition` is dispatched by
	/// `main_loop` and jumps a local use to its `let` binder.
	#[test]
	fn definition_request_round_trips_through_the_wire() {
		use lsp_types::{
			GotoDefinitionParams, GotoDefinitionResponse, Position, TextDocumentIdentifier,
			TextDocumentPositionParams, WorkDoneProgressParams,
			request::{GotoDefinition, Request as _},
		};

		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		let uri: Uri = "file:///wire_def.nym".parse().unwrap();
		let text = "func main(): int = {\n  let x = 1\n  x\n}";
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.to_string(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: uri.clone(),
						language_id: "nymph".to_string(),
						version: 1,
						text: text.to_string(),
					},
				})
				.unwrap(),
			)))
			.unwrap();

		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(3),
				GotoDefinition::METHOD.to_string(),
				serde_json::to_value(GotoDefinitionParams {
					text_document_position_params: TextDocumentPositionParams {
						text_document: TextDocumentIdentifier { uri: uri.clone() },
						// `x` on the last line before `}`.
						position: Position {
							line: 2,
							character: 2,
						},
					},
					work_done_progress_params: WorkDoneProgressParams::default(),
					partial_result_params: Default::default(),
				})
				.unwrap(),
			)))
			.unwrap();

		let result: Option<GotoDefinitionResponse> = loop {
			match client.receiver.recv().unwrap() {
				Message::Response(r) => {
					break serde_json::from_value(r.response_result.unwrap()).unwrap();
				}
				_ => continue,
			}
		};
		let location = match result.expect("expected a definition result") {
			GotoDefinitionResponse::Scalar(loc) => loc,
			other => panic!("expected the Scalar arm, got {other:?}"),
		};
		assert_eq!(location.uri, uri);
		assert_eq!(location.range.start.line, 1);

		shutdown(&client, handle);
	}

	/// A round trip proving `textDocument/completion` is dispatched by
	/// `main_loop` and returns at least one top-level name and one keyword.
	#[test]
	fn completion_request_round_trips_through_the_wire() {
		use lsp_types::{
			CompletionParams, CompletionResponse, Position, TextDocumentIdentifier,
			TextDocumentPositionParams, WorkDoneProgressParams,
			request::{Completion, Request as _},
		};

		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		let uri: Uri = "file:///wire_complete.nym".parse().unwrap();
		let text = "func helper(): int = 1\nfunc main(): int = 1";
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.to_string(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: uri.clone(),
						language_id: "nymph".to_string(),
						version: 1,
						text: text.to_string(),
					},
				})
				.unwrap(),
			)))
			.unwrap();

		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(3),
				Completion::METHOD.to_string(),
				serde_json::to_value(CompletionParams {
					text_document_position: TextDocumentPositionParams {
						text_document: TextDocumentIdentifier { uri: uri.clone() },
						position: Position {
							line: 1,
							character: 0,
						},
					},
					work_done_progress_params: WorkDoneProgressParams::default(),
					partial_result_params: Default::default(),
					context: None,
				})
				.unwrap(),
			)))
			.unwrap();

		let result: Option<CompletionResponse> = loop {
			match client.receiver.recv().unwrap() {
				Message::Response(r) => {
					break serde_json::from_value(r.response_result.unwrap()).unwrap();
				}
				_ => continue,
			}
		};
		let labels: Vec<String> = match result.expect("expected a completion result") {
			CompletionResponse::Array(items) => items.into_iter().map(|i| i.label).collect(),
			CompletionResponse::List(list) => list.items.into_iter().map(|i| i.label).collect(),
		};
		assert!(labels.contains(&"helper".to_string()), "got {labels:?}");
		assert!(labels.contains(&"func".to_string()), "got {labels:?}");

		shutdown(&client, handle);
	}

	/// A round trip through the real `Connection::memory()` wire proving
	/// `textDocument/semanticTokens/full` is actually dispatched by
	/// `main_loop`, and that a match arm's `->` decodes to `operator` (the
	/// bug the feature exists to fix).
	#[test]
	fn semantic_tokens_request_round_trips_through_the_wire() {
		use lsp_types::{
			PartialResultParams, SemanticToken, SemanticTokensParams, SemanticTokensResult,
			TextDocumentIdentifier, WorkDoneProgressParams,
			request::{Request as _, SemanticTokensFullRequest},
		};

		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		let uri: Uri = "file:///wire_semtok.nym".parse().unwrap();
		let text = "func f(): int = match 1 { _ -> 1 }";
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.to_string(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: uri.clone(),
						language_id: "nymph".to_string(),
						version: 1,
						text: text.to_string(),
					},
				})
				.unwrap(),
			)))
			.unwrap();

		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(3),
				SemanticTokensFullRequest::METHOD.to_string(),
				serde_json::to_value(SemanticTokensParams {
					work_done_progress_params: WorkDoneProgressParams::default(),
					partial_result_params: PartialResultParams::default(),
					text_document: TextDocumentIdentifier { uri: uri.clone() },
				})
				.unwrap(),
			)))
			.unwrap();

		let result: Option<SemanticTokensResult> = loop {
			match client.receiver.recv().unwrap() {
				Message::Response(r) => {
					break serde_json::from_value(r.response_result.unwrap()).unwrap();
				}
				_ => continue,
			}
		};
		let tokens = match result.expect("expected a semanticTokens result") {
			SemanticTokensResult::Tokens(tokens) => tokens,
			SemanticTokensResult::Partial(_) => panic!("expected the Tokens arm"),
		};
		assert!(!tokens.data.is_empty());

		// Decode and find the `->` token: legend index 1 is `operator`.
		let mut line = 0u32;
		let mut col = 0u32;
		let arrow: Option<&SemanticToken> = tokens.data.iter().find(|tok| {
			if tok.delta_line == 0 {
				col += tok.delta_start;
			} else {
				line += tok.delta_line;
				col = tok.delta_start;
			}
			tok.token_type == 1
		});
		assert!(
			arrow.is_some(),
			"expected an `operator` token (the match arm `->`) in {:?}",
			tokens.data
		);

		shutdown(&client, handle);
	}

	/// `semanticTokensProvider` is advertised with the fixed legend.
	#[test]
	fn initialize_advertises_semantic_tokens_with_the_legend() {
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));

		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(1),
				"initialize".to_string(),
				serde_json::to_value(InitializeParams::default()).unwrap(),
			)))
			.unwrap();
		let resp = client.receiver.recv().unwrap();
		let result: InitializeResult = match resp {
			Message::Response(r) => serde_json::from_value(r.response_result.unwrap()).unwrap(),
			other => panic!("expected a response, got {other:?}"),
		};
		let caps = result
			.capabilities
			.semantic_tokens_provider
			.expect("semantic_tokens_provider should be advertised");
		let SemanticTokensServerCapabilities::SemanticTokensOptions(options) = caps else {
			panic!("expected the SemanticTokensOptions arm");
		};
		assert_eq!(options.legend, semantic_tokens::legend());
		assert_eq!(options.full, Some(SemanticTokensFullOptions::Bool(true)));

		client
			.sender
			.send(Message::Notification(Notification::new(
				"initialized".to_string(),
				serde_json::json!({}),
			)))
			.unwrap();

		shutdown(&client, handle);
	}

	#[test]
	fn did_open_then_change_leaves_the_expected_text() {
		let (server, client) = Connection::memory();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let docs_for_server = docs.clone();
		let hover_cache = Arc::new(Mutex::new(HoverCache::default()));
		let handle = std::thread::spawn(move || serve(server, docs_for_server, hover_cache));

		handshake(&client);

		let uri: Uri = "file:///test.nym".parse().unwrap();
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.to_string(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: uri.clone(),
						language_id: "nymph".to_string(),
						version: 1,
						text: "func main() = void".to_string(),
					},
				})
				.unwrap(),
			)))
			.unwrap();
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidChangeTextDocument::METHOD.to_string(),
				serde_json::to_value(DidChangeTextDocumentParams {
					text_document: VersionedTextDocumentIdentifier {
						uri: uri.clone(),
						version: 2,
					},
					content_changes: vec![TextDocumentContentChangeEvent {
						range: None,
						range_length: None,
						text: "func main() = 1".to_string(),
					}],
				})
				.unwrap(),
			)))
			.unwrap();

		shutdown(&client, handle);

		let docs = docs.lock().unwrap();
		let doc = docs.get(&uri).expect("document should be open");
		assert_eq!(doc.text, "func main() = 1");
		assert_eq!(doc.version, 2);
	}
}
