//! The Nymph language server: a synchronous `lsp-server` loop (no
//! tokio/async — the compiler facade it wraps is synchronous) providing
//! diagnostics, hover, document symbols, go-to-definition, and completion
//! over stdio, spawned by the VS Code extension (`extension/src/extension.ts`)
//! from its target-specific packaged payload.
//!
//! MVP scope (see `extension/README.md`): `textDocument/didOpen` /
//! `didChange` (full sync) / `didClose` keep an in-memory [`DocumentStore`]
//! current; project source changes and overlay closes re-check and republish
//! the changed module plus its transitive reverse importers in stable
//! dependency order (loose files remain single-document checks; see
//! [`workspace`]); `textDocument/hover` answers
//! with the type of the smallest checked expression under the cursor (see
//! [`hover`]); `textDocument/documentSymbol` outlines a module's top-level
//! declarations, parser-only (see [`document_symbols`]);
//! `textDocument/definition` jumps an identifier/variant/type-name use to its
//! declaration, AST + `DefMap`-only, no type-check (see [`definition`]);
//! `textDocument/completion` offers lexical names, resolved project imports,
//! same-module declarations, and keywords from an immutable analysis snapshot
//! (see [`completion`] — member completion after a `.` is deferred, see its
//! module doc comment); `textDocument/semanticTokens/full` classifies every
//! token from the compiler's own lexer + AST, so highlighting stays correct
//! independent of the TextMate grammar (see [`semantic_tokens`]).
//! Incremental sync and rename are deliberately out of scope. Document and
//! range formatting use the canonical formatter against the open buffer.

pub mod compiler_state;
pub mod completion;
pub mod definition;
pub mod diagnostics;
pub mod document_store;
pub mod document_symbols;
pub mod formatting;
pub mod hover;
pub mod line_index;
mod position;
pub mod semantic_tokens;
pub mod workspace;

use std::sync::{Arc, Mutex};

use document_store::DocumentStore;
use lsp_server::{Connection, Message, Notification as ServerNotification, Response};
use lsp_types::{
	CompletionOptions, CompletionParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
	DidOpenTextDocumentParams, DocumentFormattingParams, DocumentRangeFormattingParams,
	DocumentSymbolParams, GotoDefinitionParams, HoverParams, HoverProviderCapability,
	InitializeParams, InitializeResult, OneOf, SemanticTokensFullOptions, SemanticTokensOptions,
	SemanticTokensParams, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
	TextDocumentSyncCapability, TextDocumentSyncKind,
	notification::{
		DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
	},
	request::{
		Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest, RangeFormatting,
		Request as _, SemanticTokensFullRequest,
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
		document_formatting_provider: Some(OneOf::Left(true)),
		document_range_formatting_provider: Some(OneOf::Left(true)),
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
		Arc::new(Mutex::new(compiler_state::CompilerState::new())),
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
	compiler: Arc<Mutex<compiler_state::CompilerState>>,
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

	main_loop(&connection, &docs, &compiler)
}

fn prepare_if_current<T>(
	docs: &Mutex<DocumentStore>,
	uri: &lsp_types::Uri,
	snapshot: &compiler_state::AnalysisSnapshot,
	value: T,
) -> Option<T> {
	let mut prepared = None;
	{
		let docs = docs.lock().unwrap();
		compiler_state::publish_if_current(&docs, uri, snapshot, value, |value| {
			prepared = Some(value);
		});
	}
	prepared
}

fn prepare_hover_response(
	docs: &Mutex<DocumentStore>,
	uri: &lsp_types::Uri,
	snapshot: &compiler_state::AnalysisSnapshot,
	value: Option<lsp_types::Hover>,
) -> Option<Option<lsp_types::Hover>> {
	prepare_if_current(docs, uri, snapshot, value)
}

fn prepare_semantic_tokens_response(
	docs: &Mutex<DocumentStore>,
	uri: &lsp_types::Uri,
	snapshot: &compiler_state::AnalysisSnapshot,
	value: Option<lsp_types::SemanticTokensResult>,
) -> Option<Option<lsp_types::SemanticTokensResult>> {
	prepare_if_current(docs, uri, snapshot, value)
}

fn prepare_definition_response(
	docs: &Mutex<DocumentStore>,
	uri: &lsp_types::Uri,
	snapshot: &compiler_state::AnalysisSnapshot,
	value: Option<lsp_types::GotoDefinitionResponse>,
) -> Option<Option<lsp_types::GotoDefinitionResponse>> {
	prepare_if_current(docs, uri, snapshot, value)
}

fn prepare_completion_response(
	docs: &Mutex<DocumentStore>,
	uri: &lsp_types::Uri,
	snapshot: &compiler_state::CompletionSnapshot,
	value: lsp_types::CompletionResponse,
) -> Option<Option<lsp_types::CompletionResponse>> {
	let mut prepared = None;
	{
		let docs = docs.lock().unwrap();
		compiler_state::publish_completion_if_current(&docs, uri, snapshot, Some(value), |value| {
			prepared = Some(value);
		});
	}
	prepared
}

fn main_loop(
	connection: &Connection,
	docs: &Arc<Mutex<DocumentStore>>,
	compiler: &Arc<Mutex<compiler_state::CompilerState>>,
) -> anyhow::Result<()> {
	for msg in &connection.receiver {
		match msg {
			Message::Request(req) => {
				if connection.handle_shutdown(&req)? {
					return Ok(());
				}
				if req.method == HoverRequest::METHOD {
					let (id, params) = req.extract::<HoverParams>(HoverRequest::METHOD)?;
					let uri = &params.text_document_position_params.text_document.uri;
					let snapshot = compiler
						.lock()
						.unwrap()
						.analysis_for_uri(&docs.lock().unwrap(), uri);
					let response = match snapshot {
						Some(snapshot) => {
							let result = hover::hover_snapshot(&snapshot, &params);
							prepare_hover_response(docs, uri, &snapshot, result)
						}
						None => Some(None),
					};
					// This synchronous loop is the sole server-state mutator, so no
					// document change can interleave between the final guard above and
					// this send. The docs/compiler locks are both released before I/O.
					if let Some(result) = response {
						connection
							.sender
							.send(Message::Response(Response::new_ok(id, result)))?;
					}
				} else if req.method == Formatting::METHOD {
					let (id, params) = req.extract::<DocumentFormattingParams>(Formatting::METHOD)?;
					let result = formatting::document_formatting(&docs.lock().unwrap(), &params);
					connection
						.sender
						.send(Message::Response(Response::new_ok(id, result)))?;
				} else if req.method == RangeFormatting::METHOD {
					let (id, params) =
						req.extract::<DocumentRangeFormattingParams>(RangeFormatting::METHOD)?;
					let result = formatting::document_range_formatting(&docs.lock().unwrap(), &params);
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
					let uri = &params.text_document_position_params.text_document.uri;
					let compiler = compiler.lock().unwrap();
					let docs_guard = docs.lock().unwrap();
					let snapshot = compiler.analysis_for_uri(&docs_guard, uri);
					let response = match snapshot {
						Some(snapshot) => {
							let candidate = definition::definition_snapshot_candidate(
								&docs_guard,
								&compiler,
								&snapshot,
								&params,
							);
							drop(docs_guard);
							drop(compiler);
							let result = candidate.and_then(|candidate| candidate.validate_disk_source());
							prepare_definition_response(docs, uri, &snapshot, result)
						}
						None => {
							let result = definition::definition(&docs_guard, &params);
							drop(docs_guard);
							drop(compiler);
							Some(result)
						}
					};
					if let Some(result) = response {
						connection
							.sender
							.send(Message::Response(Response::new_ok(id, result)))?;
					}
				} else if req.method == Completion::METHOD {
					let (id, params) = req.extract::<CompletionParams>(Completion::METHOD)?;
					let uri = &params.text_document_position.text_document.uri;
					let snapshot = compiler
						.lock()
						.unwrap()
						.completion_for_uri(&docs.lock().unwrap(), uri);
					let response = match snapshot {
						Some(snapshot) => {
							let result = completion::completion_snapshot(&snapshot, &params);
							prepare_completion_response(docs, uri, &snapshot, result)
						}
						None => Some(completion::completion(&docs.lock().unwrap(), &params)),
					};
					if let Some(result) = response {
						connection
							.sender
							.send(Message::Response(Response::new_ok(id, result)))?;
					}
				} else if req.method == SemanticTokensFullRequest::METHOD {
					let (id, params) =
						req.extract::<SemanticTokensParams>(SemanticTokensFullRequest::METHOD)?;
					let uri = &params.text_document.uri;
					let snapshot = compiler
						.lock()
						.unwrap()
						.analysis_for_uri(&docs.lock().unwrap(), uri);
					let response = match snapshot {
						Some(snapshot) => {
							let result = semantic_tokens::semantic_tokens_snapshot(&snapshot, &params);
							prepare_semantic_tokens_response(docs, uri, &snapshot, result)
						}
						None => Some(semantic_tokens::semantic_tokens_for_open_document(
							&docs.lock().unwrap(),
							&params,
						)),
					};
					if let Some(result) = response {
						connection
							.sender
							.send(Message::Response(Response::new_ok(id, result)))?;
					}
				} else {
					connection.sender.send(Message::Response(Response::new_err(
						req.id,
						lsp_server::ErrorCode::MethodNotFound as i32,
						format!("unhandled request method `{}`", req.method),
					)))?;
				}
			}
			Message::Notification(not) => handle_notification(connection, docs, compiler, not)?,
			Message::Response(_) => {}
		}
	}
	Ok(())
}

fn handle_notification(
	connection: &Connection,
	docs: &Arc<Mutex<DocumentStore>>,
	compiler: &Arc<Mutex<compiler_state::CompilerState>>,
	not: ServerNotification,
) -> anyhow::Result<()> {
	match not.method.as_str() {
		m if m == DidOpenTextDocument::METHOD => {
			let params: DidOpenTextDocumentParams = serde_json::from_value(not.params)?;
			let uri = params.text_document.uri;
			let affected = compiler.lock().unwrap().open(
				&mut docs.lock().unwrap(),
				uri.clone(),
				params.text_document.text,
				params.text_document.version,
			)?;
			diagnostics::check_and_publish_affected(connection, docs, compiler, &uri, &affected)?;
		}
		m if m == DidChangeTextDocument::METHOD => {
			let params: DidChangeTextDocumentParams = serde_json::from_value(not.params)?;
			let uri = params.text_document.uri.clone();
			if let Some(change) = params.content_changes.into_iter().last() {
				let affected = compiler.lock().unwrap().change(
					&mut docs.lock().unwrap(),
					&uri,
					change.text,
					params.text_document.version,
				)?;
				diagnostics::check_and_publish_affected(connection, docs, compiler, &uri, &affected)?;
			}
		}
		m if m == DidCloseTextDocument::METHOD => {
			let params: DidCloseTextDocumentParams = serde_json::from_value(not.params)?;
			let action = compiler
				.lock()
				.unwrap()
				.close(&mut docs.lock().unwrap(), &params.text_document.uri)?;
			match action {
				compiler_state::CloseAction::PublishProject(affected) => {
					diagnostics::check_and_publish_affected(
						connection,
						docs,
						compiler,
						&params.text_document.uri,
						&affected,
					)?;
				}
				compiler_state::CloseAction::Clear => {
					diagnostics::clear(connection, &params.text_document.uri)?;
				}
			}
		}
		_ => {}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use compiler_state::CompilerState;
	use lsp_server::{Notification, Request, RequestId};
	use lsp_types::{
		Position, PublishDiagnosticsParams, SemanticTokensParams, TextDocumentContentChangeEvent,
		TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Uri,
		VersionedTextDocumentIdentifier, WorkDoneProgressParams,
	};
	use std::sync::atomic::{AtomicUsize, Ordering};

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

	fn recv_response(client: &Connection, expected_id: i32) -> Response {
		loop {
			if let Message::Response(response) = client.receiver.recv().unwrap() {
				assert_eq!(response.id, RequestId::from(expected_id));
				return response;
			}
		}
	}

	fn recv_diagnostics(client: &Connection) -> PublishDiagnosticsParams {
		loop {
			if let Message::Notification(notification) = client.receiver.recv().unwrap()
				&& notification.method == lsp_types::notification::PublishDiagnostics::METHOD
			{
				return serde_json::from_value(notification.params).unwrap();
			}
		}
	}

	fn recv_diagnostics_for(client: &Connection, uri: &Uri) -> PublishDiagnosticsParams {
		loop {
			let diagnostics = recv_diagnostics(client);
			if diagnostics.uri == *uri {
				return diagnostics;
			}
		}
	}

	fn send_open(client: &Connection, uri: Uri, version: i32, text: &str) {
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.into(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri,
						language_id: "nymph".into(),
						version,
						text: text.into(),
					},
				})
				.unwrap(),
			)))
			.unwrap();
	}

	fn send_close(client: &Connection, uri: Uri) {
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidCloseTextDocument::METHOD.into(),
				serde_json::to_value(DidCloseTextDocumentParams {
					text_document: TextDocumentIdentifier { uri },
				})
				.unwrap(),
			)))
			.unwrap();
	}

	#[test]
	fn closing_loose_and_non_file_documents_publishes_one_unversioned_clear_each() {
		let temp = tempfile::tempdir().unwrap();
		let loose_path = temp.path().join("loose.nym");
		std::fs::write(&loose_path, "func disk(): missing = 1").unwrap();
		let loose_uri = workspace::path_to_uri(&loose_path).unwrap();
		let untitled_uri: Uri = "untitled:Untitled-1".parse().unwrap();
		let (server, client) = Connection::memory();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let observed_docs = docs.clone();
		let handle =
			std::thread::spawn(move || serve(server, docs, Arc::new(Mutex::new(CompilerState::new()))));
		handshake(&client);

		for uri in [&loose_uri, &untitled_uri] {
			send_open(&client, uri.clone(), 7, "func overlay(): missing = 1");
			let opened = recv_diagnostics(&client);
			assert_eq!(opened.uri, *uri);
			assert_eq!(opened.version, Some(7));
			assert!(!opened.diagnostics.is_empty());

			send_close(&client, uri.clone());
			let closed = recv_diagnostics(&client);
			assert_eq!(closed.uri, *uri);
			assert_eq!(closed.version, None);
			assert!(closed.diagnostics.is_empty());
			assert!(observed_docs.lock().unwrap().get(uri).is_none());
			assert!(client.receiver.try_recv().is_err());
		}

		shutdown(&client, handle);
	}

	#[test]
	fn closing_an_equivalent_project_uri_publishes_only_the_closed_spelling() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-equivalent-close'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let importer_path = temp.path().join("src/importer.nym");
		let dependency_path = temp.path().join("src/dependency.nym");
		let importer_text = "import @/dependency with (value)\nfunc use(): int = value()";
		std::fs::write(&importer_path, importer_text).unwrap();
		std::fs::write(&dependency_path, "public func value(): boolean = true").unwrap();
		let importer_uri = workspace::path_to_uri(&importer_path).unwrap();
		let dependency_uri = workspace::path_to_uri(&dependency_path).unwrap();
		let alternate_uri: Uri = dependency_uri
			.as_str()
			.replace("dependency.nym", "%64ependency.nym")
			.parse()
			.unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		send_open(&client, importer_uri.clone(), 1, importer_text);
		recv_diagnostics_for(&client, &importer_uri);
		send_open(
			&client,
			dependency_uri.clone(),
			2,
			"public func value(): float = 1.0",
		);
		recv_diagnostics_for(&client, &dependency_uri);
		recv_diagnostics_for(&client, &importer_uri);
		send_open(
			&client,
			alternate_uri.clone(),
			3,
			"public func value(): int = 1",
		);
		recv_diagnostics_for(&client, &alternate_uri);
		assert!(
			recv_diagnostics_for(&client, &importer_uri)
				.diagnostics
				.is_empty()
		);

		send_close(&client, dependency_uri.clone());
		let closed = [recv_diagnostics(&client), recv_diagnostics(&client)];
		assert_eq!(
			closed.iter().map(|params| &params.uri).collect::<Vec<_>>(),
			[&dependency_uri, &importer_uri]
		);
		assert_eq!(
			closed
				.iter()
				.map(|params| params.version)
				.collect::<Vec<_>>(),
			[None, Some(1)]
		);
		assert!(closed.iter().all(|params| params.diagnostics.is_empty()));
		assert!(closed.iter().all(|params| params.uri != alternate_uri));
		assert!(client.receiver.try_recv().is_err());

		shutdown(&client, handle);
	}

	#[test]
	fn reverse_importer_refresh_uses_its_open_equivalent_uri_and_version() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-equivalent-importer'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let importer_path = temp.path().join("src/importer.nym");
		let dependency_path = temp.path().join("src/dependency.nym");
		let disk_importer_text = "import @/dependency with (value)\nfunc use(): int = value()";
		let importer_text = format!("\n{disk_importer_text}");
		std::fs::write(&importer_path, disk_importer_text).unwrap();
		std::fs::write(&dependency_path, "public func value(): int = 1").unwrap();
		let canonical_importer_uri = workspace::path_to_uri(&importer_path).unwrap();
		let importer_uri: Uri = canonical_importer_uri
			.as_str()
			.replace("importer.nym", "%69mporter.nym")
			.parse()
			.unwrap();
		let dependency_uri = workspace::path_to_uri(&dependency_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		send_open(&client, importer_uri.clone(), 7, &importer_text);
		let opened = recv_diagnostics(&client);
		assert_eq!(opened.uri, importer_uri);
		assert_eq!(opened.version, Some(7));
		assert!(opened.diagnostics.is_empty());

		send_open(
			&client,
			dependency_uri.clone(),
			2,
			"public func value(): float = 1.0",
		);
		let changed = [recv_diagnostics(&client), recv_diagnostics(&client)];
		assert_eq!(
			changed.iter().map(|params| &params.uri).collect::<Vec<_>>(),
			[&dependency_uri, &importer_uri]
		);
		assert_eq!(
			changed
				.iter()
				.map(|params| params.version)
				.collect::<Vec<_>>(),
			[Some(2), Some(7)]
		);
		assert!(!changed[1].diagnostics.is_empty());
		assert!(
			changed[1]
				.diagnostics
				.iter()
				.all(|diagnostic| diagnostic.range.start.line == 2),
			"reverse-importer ranges must use the open alias text, not canonical disk text"
		);
		assert!(
			changed
				.iter()
				.all(|params| params.uri != canonical_importer_uri)
		);
		assert!(client.receiver.try_recv().is_err());

		shutdown(&client, handle);
	}

	#[test]
	fn closing_the_last_equivalent_uri_clears_prior_closed_alias_diagnostics() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-equivalent-last-close'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let importer_path = temp.path().join("src/importer.nym");
		let dependency_path = temp.path().join("src/dependency.nym");
		let importer_text = "import @/dependency with (value)\nfunc use(): int = value()";
		std::fs::write(&importer_path, importer_text).unwrap();
		std::fs::write(&dependency_path, "public func value(): int = 1").unwrap();
		let importer_uri = workspace::path_to_uri(&importer_path).unwrap();
		let dependency_uri = workspace::path_to_uri(&dependency_path).unwrap();
		let alternate_uri: Uri = dependency_uri
			.as_str()
			.replace("dependency.nym", "%64ependency.nym")
			.parse()
			.unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		send_open(&client, importer_uri.clone(), 1, importer_text);
		recv_diagnostics(&client);
		send_open(
			&client,
			dependency_uri.clone(),
			2,
			"public func value(): int = 2",
		);
		for _ in 0..2 {
			recv_diagnostics(&client);
		}
		send_open(
			&client,
			alternate_uri.clone(),
			3,
			"public func value(): int = true",
		);
		for _ in 0..2 {
			recv_diagnostics(&client);
		}

		send_close(&client, dependency_uri.clone());
		let first_close = [recv_diagnostics(&client), recv_diagnostics(&client)];
		assert_eq!(first_close[0].uri, dependency_uri);
		assert!(!first_close[0].diagnostics.is_empty());
		assert_eq!(first_close[1].uri, importer_uri);
		assert!(client.receiver.try_recv().is_err());

		send_close(&client, alternate_uri.clone());
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(99),
				HoverRequest::METHOD.into(),
				serde_json::to_value(HoverParams {
					text_document_position_params: TextDocumentPositionParams {
						text_document: TextDocumentIdentifier {
							uri: importer_uri.clone(),
						},
						position: Position::new(1, 18),
					},
					work_done_progress_params: WorkDoneProgressParams::default(),
				})
				.unwrap(),
			)))
			.unwrap();
		let mut last_close = Vec::new();
		loop {
			match client.receiver.recv().unwrap() {
				Message::Notification(notification)
					if notification.method == lsp_types::notification::PublishDiagnostics::METHOD =>
				{
					last_close
						.push(serde_json::from_value::<PublishDiagnosticsParams>(notification.params).unwrap());
				}
				Message::Response(response) => {
					assert_eq!(response.id, RequestId::from(99));
					break;
				}
				other => panic!("expected diagnostics or hover response, got {other:?}"),
			}
		}
		assert_eq!(
			last_close
				.iter()
				.map(|params| &params.uri)
				.collect::<Vec<_>>(),
			[&alternate_uri, &dependency_uri, &importer_uri]
		);
		assert_eq!(
			last_close
				.iter()
				.map(|params| params.version)
				.collect::<Vec<_>>(),
			[None, None, Some(1)]
		);
		assert!(
			last_close
				.iter()
				.all(|params| params.diagnostics.is_empty())
		);

		send_open(
			&client,
			alternate_uri.clone(),
			4,
			"public func value(): int = 4",
		);
		let reopened = [recv_diagnostics(&client), recv_diagnostics(&client)];
		assert_eq!(
			reopened
				.iter()
				.map(|params| &params.uri)
				.collect::<Vec<_>>(),
			[&alternate_uri, &importer_uri]
		);
		assert_eq!(
			reopened
				.iter()
				.map(|params| params.version)
				.collect::<Vec<_>>(),
			[Some(4), Some(1)]
		);
		assert!(reopened.iter().all(|params| params.diagnostics.is_empty()));
		assert!(client.receiver.try_recv().is_err());

		shutdown(&client, handle);
	}

	#[test]
	fn production_handlers_reuse_analysis_until_effective_source_changes() {
		let parse = Arc::new(AtomicUsize::new(0));
		let analysis = Arc::new(AtomicUsize::new(0));
		let parse_events = parse.clone();
		let analysis_events = analysis.clone();
		let compiler = compiler_state::CompilerState::with_event_callback(move |event| match event {
			"parse" => _ = parse_events.fetch_add(1, Ordering::Relaxed),
			"interface_module_analysis" => _ = analysis_events.fetch_add(1, Ordering::Relaxed),
			_ => {}
		});
		let (server, client) = Connection::memory();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let compiler = Arc::new(Mutex::new(compiler));
		let handle = std::thread::spawn(move || serve(server, docs, compiler));
		handshake(&client);
		let uri: Uri = "file:///wire_incremental.nym".parse().unwrap();
		let send_open_or_change = |version, open: bool, text: &str| {
			let notification = if open {
				Notification::new(
					DidOpenTextDocument::METHOD.to_string(),
					serde_json::to_value(DidOpenTextDocumentParams {
						text_document: TextDocumentItem {
							uri: uri.clone(),
							language_id: "nymph".into(),
							version,
							text: text.into(),
						},
					})
					.unwrap(),
				)
			} else {
				Notification::new(
					DidChangeTextDocument::METHOD.to_string(),
					serde_json::to_value(DidChangeTextDocumentParams {
						text_document: VersionedTextDocumentIdentifier {
							uri: uri.clone(),
							version,
						},
						content_changes: vec![TextDocumentContentChangeEvent {
							range: None,
							range_length: None,
							text: text.into(),
						}],
					})
					.unwrap(),
				)
			};
			client
				.sender
				.send(Message::Notification(notification))
				.unwrap();
		};
		send_open_or_change(1, true, "func value(): int = 1");
		assert_eq!(recv_diagnostics(&client).version, Some(1));

		for id in [10, 11] {
			client
				.sender
				.send(Message::Request(Request::new(
					RequestId::from(id),
					HoverRequest::METHOD.into(),
					serde_json::to_value(HoverParams {
						text_document_position_params: TextDocumentPositionParams {
							text_document: TextDocumentIdentifier { uri: uri.clone() },
							position: Position {
								line: 0,
								character: 20,
							},
						},
						work_done_progress_params: WorkDoneProgressParams::default(),
					})
					.unwrap(),
				)))
				.unwrap();
			recv_response(&client, id);
		}
		let semantic_request = |id| {
			client
				.sender
				.send(Message::Request(Request::new(
					RequestId::from(id),
					SemanticTokensFullRequest::METHOD.into(),
					serde_json::to_value(SemanticTokensParams {
						text_document: TextDocumentIdentifier { uri: uri.clone() },
						work_done_progress_params: WorkDoneProgressParams::default(),
						partial_result_params: Default::default(),
					})
					.unwrap(),
				)))
				.unwrap();
			recv_response(&client, id);
		};
		semantic_request(12);
		assert_eq!(
			(
				parse.load(Ordering::Relaxed),
				analysis.load(Ordering::Relaxed)
			),
			(13, 1)
		);

		send_open_or_change(2, false, "func value(): int = 2");
		assert_eq!(recv_diagnostics(&client).version, Some(2));
		semantic_request(13);
		assert_eq!(
			(
				parse.load(Ordering::Relaxed),
				analysis.load(Ordering::Relaxed)
			),
			(14, 2)
		);
		shutdown(&client, handle);
	}

	#[test]
	fn wire_project_diagnostics_resolve_transitive_std_without_provider_uri_publication() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-stdlib'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		let helper_path = temp.path().join("src/helper.nym");
		let source = "import @/helper with (leaf)\nfunc use(): int = leaf()";
		let helper = "import std/collections/tree with (Tree)\npublic func leaf(): int = match (Tree.Leaf(value = 1)) { Tree.Leaf(value) -> value, Tree.Node(...) -> 0 }";
		std::fs::write(&main_path, source).unwrap();
		std::fs::write(&helper_path, helper).unwrap();
		let uri = workspace::path_to_uri(&main_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.into(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: uri.clone(),
						language_id: "nymph".into(),
						version: 1,
						text: source.into(),
					},
				})
				.unwrap(),
			)))
			.unwrap();
		// A request/response round trip is an ordering barrier: any fabricated
		// provider publication from didOpen would have arrived before this response.
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(20),
				HoverRequest::METHOD.into(),
				serde_json::to_value(HoverParams {
					text_document_position_params: TextDocumentPositionParams {
						text_document: TextDocumentIdentifier { uri: uri.clone() },
						position: Position {
							line: 1,
							character: 5,
						},
					},
					work_done_progress_params: WorkDoneProgressParams::default(),
				})
				.unwrap(),
			)))
			.unwrap();
		let mut diagnostic_uris = Vec::new();
		loop {
			match client.receiver.recv().unwrap() {
				Message::Response(response) => {
					assert_eq!(response.id, RequestId::from(20));
					break;
				}
				Message::Notification(notification)
					if notification.method == lsp_types::notification::PublishDiagnostics::METHOD =>
				{
					let params: PublishDiagnosticsParams =
						serde_json::from_value(notification.params).unwrap();
					assert!(params.diagnostics.is_empty());
					diagnostic_uris.push(params.uri);
				}
				other => panic!("expected diagnostics or hover response, got {other:?}"),
			}
		}
		assert_eq!(diagnostic_uris, [uri]);
		shutdown(&client, handle);
	}

	#[test]
	fn closing_dirty_dependency_republishes_open_importer_after_restore_and_delete() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-close'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let importer_path = temp.path().join("src/importer.nym");
		let dependency_path = temp.path().join("src/dependency.nym");
		let importer_text = "import @/dependency with (value)\nfunc use(): int = value()";
		let disk_dependency = "public func value(): int = 1";
		std::fs::write(&importer_path, importer_text).unwrap();
		std::fs::write(&dependency_path, disk_dependency).unwrap();
		let importer_uri = workspace::path_to_uri(&importer_path).unwrap();
		let dependency_uri = workspace::path_to_uri(&dependency_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);
		let open = |uri: Uri, version, text: &str| {
			client
				.sender
				.send(Message::Notification(Notification::new(
					DidOpenTextDocument::METHOD.into(),
					serde_json::to_value(DidOpenTextDocumentParams {
						text_document: TextDocumentItem {
							uri,
							language_id: "nymph".into(),
							version,
							text: text.into(),
						},
					})
					.unwrap(),
				)))
				.unwrap();
		};
		let close = |uri: Uri| {
			client
				.sender
				.send(Message::Notification(Notification::new(
					DidCloseTextDocument::METHOD.into(),
					serde_json::to_value(DidCloseTextDocumentParams {
						text_document: TextDocumentIdentifier { uri },
					})
					.unwrap(),
				)))
				.unwrap();
		};

		open(importer_uri.clone(), 1, importer_text);
		recv_diagnostics_for(&client, &importer_uri);
		open(dependency_uri.clone(), 1, "public func value(): int = true");
		recv_diagnostics_for(&client, &dependency_uri);
		recv_diagnostics_for(&client, &importer_uri);
		close(dependency_uri.clone());
		let restored = [recv_diagnostics(&client), recv_diagnostics(&client)];
		assert_eq!(
			restored
				.iter()
				.map(|params| &params.uri)
				.collect::<Vec<_>>(),
			[&dependency_uri, &importer_uri]
		);
		assert_eq!(
			restored
				.iter()
				.map(|params| params.version)
				.collect::<Vec<_>>(),
			[None, Some(1)]
		);
		assert!(restored.iter().all(|params| params.diagnostics.is_empty()));
		assert!(client.receiver.try_recv().is_err());

		open(dependency_uri.clone(), 2, "public func value(): int = true");
		recv_diagnostics_for(&client, &dependency_uri);
		recv_diagnostics_for(&client, &importer_uri);
		std::fs::remove_file(&dependency_path).unwrap();
		close(dependency_uri.clone());
		let removed = [recv_diagnostics(&client), recv_diagnostics(&client)];
		assert_eq!(
			removed.iter().map(|params| &params.uri).collect::<Vec<_>>(),
			[&dependency_uri, &importer_uri]
		);
		assert_eq!(
			removed
				.iter()
				.map(|params| params.version)
				.collect::<Vec<_>>(),
			[None, Some(1)]
		);
		assert!(removed[0].diagnostics.is_empty());
		let importer = &removed[1];
		assert_eq!(importer.diagnostics.len(), 1);
		assert_eq!(
			importer.diagnostics[0].code,
			Some(lsp_types::NumberOrString::String(
				"IMPORT-UNRESOLVED".into()
			))
		);
		shutdown(&client, handle);
	}

	#[test]
	fn dependency_overlay_notifications_republish_transitive_importer_diagnostics() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-transitive'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		let middle_path = temp.path().join("src/middle.nym");
		let leaf_path = temp.path().join("src/leaf.nym");
		let unrelated_path = temp.path().join("src/unrelated.nym");
		let main = "import @/middle with (middle)\nfunc use(): int = middle()";
		let middle = "import @/leaf with (value)\npublic func middle(): int = value()";
		let leaf = "public func value(): int = 1";
		std::fs::write(&main_path, main).unwrap();
		std::fs::write(&middle_path, middle).unwrap();
		std::fs::write(&leaf_path, leaf).unwrap();
		std::fs::write(&unrelated_path, "func stable(): int = 0").unwrap();
		let main_uri = workspace::path_to_uri(&main_path).unwrap();
		let middle_uri = workspace::path_to_uri(&middle_path).unwrap();
		let leaf_uri = workspace::path_to_uri(&leaf_path).unwrap();
		let unrelated_uri = workspace::path_to_uri(&unrelated_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		let open = |uri: Uri, version, text: &str| {
			client
				.sender
				.send(Message::Notification(Notification::new(
					DidOpenTextDocument::METHOD.into(),
					serde_json::to_value(DidOpenTextDocumentParams {
						text_document: TextDocumentItem {
							uri,
							language_id: "nymph".into(),
							version,
							text: text.into(),
						},
					})
					.unwrap(),
				)))
				.unwrap();
		};
		open(main_uri.clone(), 1, main);
		assert!(
			recv_diagnostics_for(&client, &main_uri)
				.diagnostics
				.is_empty()
		);

		open(leaf_uri.clone(), 1, leaf);
		for _ in 0..3 {
			recv_diagnostics(&client);
		}
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidChangeTextDocument::METHOD.into(),
				serde_json::to_value(DidChangeTextDocumentParams {
					text_document: VersionedTextDocumentIdentifier {
						uri: leaf_uri.clone(),
						version: 2,
					},
					content_changes: vec![TextDocumentContentChangeEvent {
						range: None,
						range_length: None,
						text: "public func value(): float = 1.0".into(),
					}],
				})
				.unwrap(),
			)))
			.unwrap();
		let changed = [
			recv_diagnostics(&client),
			recv_diagnostics(&client),
			recv_diagnostics(&client),
		];
		assert_eq!(
			changed.iter().map(|params| &params.uri).collect::<Vec<_>>(),
			[&leaf_uri, &middle_uri, &main_uri]
		);
		assert_eq!(
			changed
				.iter()
				.map(|params| params.version)
				.collect::<Vec<_>>(),
			[Some(2), None, Some(1)]
		);
		assert!(
			!changed[1].diagnostics.is_empty(),
			"opening a dependency overlay did not republish its transitive importer diagnostics"
		);
		assert!(changed.iter().all(|params| params.uri != unrelated_uri));
		assert!(client.receiver.try_recv().is_err());

		client
			.sender
			.send(Message::Notification(Notification::new(
				DidCloseTextDocument::METHOD.into(),
				serde_json::to_value(DidCloseTextDocumentParams {
					text_document: TextDocumentIdentifier {
						uri: leaf_uri.clone(),
					},
				})
				.unwrap(),
			)))
			.unwrap();
		let restored = [
			recv_diagnostics(&client),
			recv_diagnostics(&client),
			recv_diagnostics(&client),
		];
		assert_eq!(
			restored
				.iter()
				.map(|params| &params.uri)
				.collect::<Vec<_>>(),
			[&leaf_uri, &middle_uri, &main_uri]
		);
		assert!(
			restored[1].diagnostics.is_empty(),
			"closing the overlay did not republish diagnostics from restored disk source"
		);
		assert_eq!(restored[0].version, None);
		assert!(restored.iter().all(|params| params.uri != unrelated_uri));
		assert!(client.receiver.try_recv().is_err());
		shutdown(&client, handle);
	}

	#[test]
	fn cyclic_reverse_importers_publish_once_each_without_losing_cycle_diagnostics() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-cycle'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let a_path = temp.path().join("src/a.nym");
		let b_path = temp.path().join("src/b.nym");
		std::fs::write(&a_path, "import @/b").unwrap();
		std::fs::write(&b_path, "import @/a").unwrap();
		let a_uri = workspace::path_to_uri(&a_path).unwrap();
		let b_uri = workspace::path_to_uri(&b_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.into(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: a_uri.clone(),
						language_id: "nymph".into(),
						version: 1,
						text: "import @/b".into(),
					},
				})
				.unwrap(),
			)))
			.unwrap();

		let cycle = [recv_diagnostics(&client), recv_diagnostics(&client)];
		assert_eq!(
			cycle.iter().map(|params| &params.uri).collect::<Vec<_>>(),
			[&a_uri, &b_uri]
		);
		assert!(
			cycle
				.iter()
				.all(|params| params.diagnostics.iter().any(|diagnostic| {
					diagnostic.code == Some(lsp_types::NumberOrString::String("IMPORT-CYCLE".into()))
				}))
		);
		assert!(client.receiver.try_recv().is_err());
		shutdown(&client, handle);
	}

	#[test]
	fn manifest_transition_clears_previously_published_reverse_importers() {
		let temp = tempfile::tempdir().unwrap();
		let manifest_path = temp.path().join("nymph.toml");
		std::fs::write(
			&manifest_path,
			"[package]\nname='wire-manifest-transition'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		let dependency_path = temp.path().join("src/dependency.nym");
		let main = "import @/dependency with (value)\nfunc use(): int = value()";
		let dependency = "public func value(): int = 1";
		std::fs::write(&main_path, main).unwrap();
		std::fs::write(&dependency_path, dependency).unwrap();
		let main_uri = workspace::path_to_uri(&main_path).unwrap();
		let dependency_uri = workspace::path_to_uri(&dependency_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		let open = |uri: Uri, text: &str| {
			client
				.sender
				.send(Message::Notification(Notification::new(
					DidOpenTextDocument::METHOD.into(),
					serde_json::to_value(DidOpenTextDocumentParams {
						text_document: TextDocumentItem {
							uri,
							language_id: "nymph".into(),
							version: 1,
							text: text.into(),
						},
					})
					.unwrap(),
				)))
				.unwrap();
		};
		open(main_uri.clone(), main);
		assert!(
			recv_diagnostics_for(&client, &main_uri)
				.diagnostics
				.is_empty()
		);
		open(dependency_uri.clone(), "public func value(): float = 1.0");
		let overlay = [recv_diagnostics(&client), recv_diagnostics(&client)];
		assert_eq!(
			overlay.iter().map(|params| &params.uri).collect::<Vec<_>>(),
			[&dependency_uri, &main_uri]
		);
		assert!(!overlay[1].diagnostics.is_empty());

		std::fs::write(&manifest_path, "not = [toml").unwrap();
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidChangeTextDocument::METHOD.into(),
				serde_json::to_value(DidChangeTextDocumentParams {
					text_document: VersionedTextDocumentIdentifier {
						uri: dependency_uri.clone(),
						version: 2,
					},
					content_changes: vec![TextDocumentContentChangeEvent {
						range: None,
						range_length: None,
						text: "public func value(): float = 2.0".into(),
					}],
				})
				.unwrap(),
			)))
			.unwrap();
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(20),
				HoverRequest::METHOD.into(),
				serde_json::to_value(HoverParams {
					text_document_position_params: TextDocumentPositionParams {
						text_document: TextDocumentIdentifier {
							uri: dependency_uri.clone(),
						},
						position: Position {
							line: 0,
							character: 12,
						},
					},
					work_done_progress_params: WorkDoneProgressParams::default(),
				})
				.unwrap(),
			)))
			.unwrap();
		let mut publications = Vec::new();
		loop {
			match client.receiver.recv().unwrap() {
				Message::Response(response) => {
					assert_eq!(response.id, RequestId::from(20));
					break;
				}
				Message::Notification(notification)
					if notification.method == lsp_types::notification::PublishDiagnostics::METHOD =>
				{
					publications
						.push(serde_json::from_value::<PublishDiagnosticsParams>(notification.params).unwrap());
				}
				other => panic!("expected diagnostics or hover response, got {other:?}"),
			}
		}
		assert_eq!(
			publications
				.iter()
				.map(|params| &params.uri)
				.collect::<Vec<_>>(),
			[&dependency_uri, &main_uri]
		);
		assert_eq!(publications[0].version, Some(2));
		assert_eq!(publications[1].version, Some(1));
		assert_eq!(
			publications[0].diagnostics[0].code,
			Some(lsp_types::NumberOrString::String("MANIFEST".into()))
		);
		assert!(publications[1].diagnostics.is_empty());

		send_close(&client, dependency_uri.clone());
		let closed = [recv_diagnostics(&client), recv_diagnostics(&client)];
		assert_eq!(
			closed.iter().map(|params| &params.uri).collect::<Vec<_>>(),
			[&dependency_uri, &main_uri]
		);
		assert_eq!(
			closed
				.iter()
				.map(|params| params.version)
				.collect::<Vec<_>>(),
			[None, Some(1)]
		);
		assert!(closed.iter().all(|params| params.diagnostics.is_empty()));
		assert!(client.receiver.try_recv().is_err());
		shutdown(&client, handle);
	}

	#[test]
	fn closing_a_document_with_a_manifest_error_clears_its_diagnostic() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(temp.path().join("nymph.toml"), "not = [toml").unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let source_path = temp.path().join("src/main.nym");
		let uri = workspace::path_to_uri(&source_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.into(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: uri.clone(),
						language_id: "nymph".into(),
						version: 1,
						text: "func main(): void = {}".into(),
					},
				})
				.unwrap(),
			)))
			.unwrap();
		assert_eq!(recv_diagnostics_for(&client, &uri).diagnostics.len(), 1);
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidCloseTextDocument::METHOD.into(),
				serde_json::to_value(DidCloseTextDocumentParams {
					text_document: TextDocumentIdentifier { uri: uri.clone() },
				})
				.unwrap(),
			)))
			.unwrap();
		let cleared = recv_diagnostics_for(&client, &uri);
		assert!(cleared.diagnostics.is_empty());
		assert_eq!(cleared.version, None);
		shutdown(&client, handle);
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
			result.capabilities.document_formatting_provider,
			Some(OneOf::Left(true))
		);
		assert_eq!(
			result.capabilities.document_range_formatting_provider,
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

	#[test]
	fn previous_lifecycle_responses_are_not_prepared_after_same_version_reopen() {
		let uri: lsp_types::Uri = "file:///wire_stale_analysis.nym".parse().unwrap();
		let mut docs = DocumentStore::default();
		let mut compiler = CompilerState::new();
		compiler
			.open(&mut docs, uri.clone(), "func f(): int = 1".into(), 1)
			.unwrap();
		let snapshot = compiler.analysis_for_uri(&docs, &uri).unwrap();
		let completion_snapshot = compiler.completion_for_uri(&docs, &uri).unwrap();
		compiler.close(&mut docs, &uri).unwrap();
		compiler
			.open(&mut docs, uri.clone(), "func f(): boolean = true".into(), 1)
			.unwrap();
		let docs = Mutex::new(docs);

		assert!(prepare_hover_response(&docs, &uri, &snapshot, None).is_none());
		assert!(prepare_semantic_tokens_response(&docs, &uri, &snapshot, None).is_none());
		assert!(prepare_definition_response(&docs, &uri, &snapshot, None).is_none());
		assert!(
			prepare_completion_response(
				&docs,
				&uri,
				&completion_snapshot,
				lsp_types::CompletionResponse::Array(Vec::new()),
			)
			.is_none()
		);
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
	/// `main_loop` through project analysis and returns imported, same-module,
	/// and keyword tiers.
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

		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-completion'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		std::fs::write(
			temp.path().join("src/dep.nym"),
			"public func imported(): int = 1",
		)
		.unwrap();
		let text = "import @/dep with (imported as imported_alias)\nfunc helper(): int = 1\nfunc main(): int = 1";
		std::fs::write(&main_path, text).unwrap();
		let uri = workspace::path_to_uri(&main_path).unwrap();
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
							line: 2,
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
		assert!(
			labels.contains(&"imported_alias".to_string()),
			"got {labels:?}"
		);
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
		let text = "func f(): int = match (1) { _ -> 1 }";
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

	#[test]
	fn malformed_project_semantic_tokens_fall_back_over_the_wire() {
		use lsp_types::{
			PartialResultParams, SemanticTokensParams, SemanticTokensResult, TextDocumentIdentifier,
			WorkDoneProgressParams,
			request::{Request as _, SemanticTokensFullRequest},
		};

		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='malformed-semantic-tokens'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let path = temp.path().join("src/main.nym");
		let text = "import @/missing with (\n// keep me\nfunc broken(): string = \"x ${1 + 2}\"";
		std::fs::write(&path, text).unwrap();
		let uri = crate::workspace::path_to_uri(&path).unwrap();

		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);
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
				RequestId::from(4),
				SemanticTokensFullRequest::METHOD.to_string(),
				serde_json::to_value(SemanticTokensParams {
					work_done_progress_params: WorkDoneProgressParams::default(),
					partial_result_params: PartialResultParams::default(),
					text_document: TextDocumentIdentifier { uri },
				})
				.unwrap(),
			)))
			.unwrap();

		let result: Option<SemanticTokensResult> = loop {
			if let Message::Response(response) = client.receiver.recv().unwrap() {
				break serde_json::from_value(response.response_result.unwrap()).unwrap();
			}
		};
		let SemanticTokensResult::Tokens(tokens) = result.expect("expected fallback tokens") else {
			panic!("expected full tokens");
		};
		assert!(!tokens.data.is_empty());
		for expected in [0, 1, 9, 10, 11] {
			assert!(
				tokens.data.iter().any(|token| token.token_type == expected),
				"expected fixed-legend token type {expected} in {:?}",
				tokens.data
			);
		}

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
		let compiler = Arc::new(Mutex::new(compiler_state::CompilerState::new()));
		let handle = std::thread::spawn(move || serve(server, docs_for_server, compiler));

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

	#[test]
	fn stale_did_change_after_close_does_not_resurrect_formatting_buffer() {
		use lsp_types::request::{Formatting, Request as _};

		let (server, client) = Connection::memory();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let docs_for_server = docs.clone();
		let compiler = Arc::new(Mutex::new(compiler_state::CompilerState::new()));
		let handle = std::thread::spawn(move || serve(server, docs_for_server, compiler));
		handshake(&client);

		let uri: Uri = "file:///stale-change-after-close.nym".parse().unwrap();
		send_open(&client, uri.clone(), 1, "let value=1\n");
		recv_diagnostics_for(&client, &uri);
		send_close(&client, uri.clone());
		recv_diagnostics_for(&client, &uri);
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
						text: "let value=2\n".to_string(),
					}],
				})
				.unwrap(),
			)))
			.unwrap();
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(40),
				Formatting::METHOD.into(),
				serde_json::json!({
					"textDocument": { "uri": uri.as_str() },
					"options": { "tabSize": 2, "insertSpaces": false }
				}),
			)))
			.unwrap();
		let response = recv_response(&client, 40);
		let edits: Option<Vec<lsp_types::TextEdit>> =
			serde_json::from_value(response.response_result.unwrap()).unwrap();
		assert!(edits.is_none());

		shutdown(&client, handle);
		assert!(docs.lock().unwrap().get(&uri).is_none());
	}

	#[test]
	fn document_and_range_formatting_requests_round_trip_through_the_wire() {
		use lsp_types::{
			TextEdit,
			request::{Formatting, RangeFormatting},
		};

		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);
		let uri: Uri = "file:///wire-format.nym".parse().unwrap();
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.to_string(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: uri.clone(),
						language_id: "nymph".into(),
						version: 1,
						text: "let λ=alpha+beta\n".into(),
					},
				})
				.unwrap(),
			)))
			.unwrap();
		let _ = recv_diagnostics_for(&client, &uri);

		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(41),
				Formatting::METHOD.into(),
				serde_json::json!({
					"textDocument": { "uri": uri.as_str() },
					"options": { "tabSize": 8, "insertSpaces": true }
				}),
			)))
			.unwrap();
		let response = recv_response(&client, 41);
		let edits: Option<Vec<TextEdit>> =
			serde_json::from_value(response.response_result.unwrap()).unwrap();
		assert_eq!(edits.unwrap()[0].new_text, "let λ = alpha + beta\n");

		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(42),
				RangeFormatting::METHOD.into(),
				serde_json::json!({
					"textDocument": { "uri": uri.as_str() },
					"range": {
						"start": { "line": 0, "character": 11 },
						"end": { "line": 0, "character": 12 }
					},
					"options": { "tabSize": 4, "insertSpaces": true }
				}),
			)))
			.unwrap();
		let response = recv_response(&client, 42);
		let edits: Option<Vec<TextEdit>> =
			serde_json::from_value(response.response_result.unwrap()).unwrap();
		let edits = edits.unwrap();
		let edit = &edits[0];
		assert_eq!(edit.new_text, "alpha + beta");
		assert_eq!(edit.range.start.character, 6);

		shutdown(&client, handle);
	}
}
