//! The Nymph language server: a synchronous `lsp-server` loop (no
//! tokio/async — the compiler facade it wraps is synchronous) providing
//! diagnostics, hover, document/workspace symbols, navigation, rename, and completion
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
//! `textDocument/references` searches compiler-resolved declaration identity
//! across every source in the current project snapshot (see [`references`]);
//! `textDocument/prepareRename` and `textDocument/rename` edit every occurrence
//! of a user-written semantic identity (see [`rename`]);
//! `textDocument/completion` offers lexical names, resolved project imports,
//! same-module declarations, and keywords from an immutable analysis snapshot
//! (see [`completion`] — member completion after a `.` is deferred, see its
//! module doc comment); `workspace/symbol` ranks visible declarations across
//! synchronized project modules (see [`workspace_symbols`]);
//! `textDocument/semanticTokens/full` classifies every
//! token from the compiler's own lexer + AST, so highlighting stays correct
//! independent of the TextMate grammar (see [`semantic_tokens`]).
//! Incremental sync is deliberately out of scope. Document and
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
pub mod references;
mod rename;
pub mod semantic_tokens;
pub mod workspace;
pub mod workspace_symbols;

use std::sync::{Arc, Mutex};

use document_store::DocumentStore;
use lsp_server::{
	Connection, Message, Notification as ServerNotification, Request as ServerRequest, RequestId,
	Response,
};
use lsp_types::{
	CompletionOptions, CompletionParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
	DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
	DocumentFormattingParams, DocumentRangeFormattingParams, DocumentSymbolParams, FileSystemWatcher,
	GlobPattern, GotoDefinitionParams, HoverParams, HoverProviderCapability, InitializeParams,
	InitializeResult, OneOf, ReferenceParams, Registration, RegistrationParams, RenameOptions,
	RenameParams, SemanticTokensFullOptions, SemanticTokensOptions, SemanticTokensParams,
	SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
	TextDocumentSyncKind, WorkspaceSymbolParams,
	notification::{
		DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
		Notification as _,
	},
	request::{
		Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest,
		PrepareRenameRequest, RangeFormatting, References, RegisterCapability, Rename, Request as _,
		SemanticTokensFullRequest, WorkspaceSymbolRequest,
	},
};

const WATCH_REGISTRATION_REQUEST_ID: &str = "nymph-watchers";
const WATCH_REGISTRATION_ID: &str = "nymph-project-files";

struct ClientState {
	supports_dynamic_watch_registration: bool,
	registration_request: Option<RequestId>,
	watchers_authoritative: bool,
}

impl ClientState {
	fn from_initialize(params: &InitializeParams) -> Self {
		let supports_dynamic_watch_registration = params
			.capabilities
			.workspace
			.as_ref()
			.and_then(|workspace| workspace.did_change_watched_files.as_ref())
			.and_then(|watched_files| watched_files.dynamic_registration)
			.unwrap_or(false);
		Self {
			supports_dynamic_watch_registration,
			registration_request: None,
			watchers_authoritative: false,
		}
	}

	fn register_watchers(&mut self, connection: &Connection) -> anyhow::Result<()> {
		if !self.supports_dynamic_watch_registration || self.registration_request.is_some() {
			return Ok(());
		}
		let id = RequestId::from(WATCH_REGISTRATION_REQUEST_ID.to_string());
		let options = DidChangeWatchedFilesRegistrationOptions {
			watchers: ["**/*.nym", "**/nymph.toml"]
				.into_iter()
				.map(|pattern| FileSystemWatcher {
					glob_pattern: GlobPattern::String(pattern.to_string()),
					kind: None,
				})
				.collect(),
		};
		let params = RegistrationParams {
			registrations: vec![Registration {
				id: WATCH_REGISTRATION_ID.to_string(),
				method: DidChangeWatchedFiles::METHOD.to_string(),
				register_options: Some(serde_json::to_value(options)?),
			}],
		};
		connection.sender.send(Message::Request(ServerRequest::new(
			id.clone(),
			RegisterCapability::METHOD.to_string(),
			params,
		)))?;
		self.registration_request = Some(id);
		Ok(())
	}

	fn handle_response(&mut self, response: &Response) {
		if self.registration_request.as_ref() == Some(&response.id) {
			self.watchers_authoritative = response.response_result.is_ok();
		}
	}
}

/// The capabilities this server advertises during `initialize`: full-text
/// document sync, hover, document symbols, go-to-definition, references, completion
/// (triggered on typing and on `.`), and full-document semantic tokens.
/// Diagnostics are pushed (`textDocument/publishDiagnostics`), not pulled,
/// so they need no capability flag here.
#[must_use]
pub fn server_capabilities() -> ServerCapabilities {
	ServerCapabilities {
		text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
		hover_provider: Some(HoverProviderCapability::Simple(true)),
		document_symbol_provider: Some(OneOf::Left(true)),
		workspace_symbol_provider: Some(OneOf::Left(true)),
		document_formatting_provider: Some(OneOf::Left(true)),
		document_range_formatting_provider: Some(OneOf::Left(true)),
		definition_provider: Some(OneOf::Left(true)),
		references_provider: Some(OneOf::Left(true)),
		rename_provider: Some(OneOf::Right(RenameOptions {
			prepare_provider: Some(true),
			work_done_progress_options: Default::default(),
		})),
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
	let init_params: InitializeParams = serde_json::from_value(params)?;
	let mut client_state = ClientState::from_initialize(&init_params);

	let init_result = InitializeResult {
		capabilities: server_capabilities(),
		server_info: Some(ServerInfo {
			name: "nymph-lsp".to_string(),
			version: Some(env!("CARGO_PKG_VERSION").to_string()),
		}),
	};
	connection.initialize_finish(id, serde_json::to_value(init_result)?)?;
	client_state.register_watchers(&connection)?;

	main_loop(&connection, &docs, &compiler, &mut client_state)
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

fn prepare_references_response(
	docs: &Mutex<DocumentStore>,
	uri: &lsp_types::Uri,
	snapshot: &compiler_state::AnalysisSnapshot,
	value: Option<Vec<lsp_types::Location>>,
) -> Option<Option<Vec<lsp_types::Location>>> {
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
	client_state: &mut ClientState,
) -> anyhow::Result<()> {
	for msg in &connection.receiver {
		match msg {
			Message::Request(req) => {
				if connection.handle_shutdown(&req)? {
					return Ok(());
				}
				if req.method == WorkspaceSymbolRequest::METHOD {
					let (id, params) =
						req.extract::<WorkspaceSymbolParams>(WorkspaceSymbolRequest::METHOD)?;
					let snapshot = {
						let mut compiler = compiler.lock().unwrap();
						let docs = docs.lock().unwrap();
						compiler.refresh_workspace_symbols(&docs);
						compiler.workspace_symbol_snapshot(&docs)
					};
					let result = workspace_symbols::workspace_symbols(&snapshot, &params);
					connection
						.sender
						.send(Message::Response(Response::new_ok(id, result)))?;
				} else if req.method == HoverRequest::METHOD {
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
				} else if req.method == References::METHOD {
					let (id, params) = req.extract::<ReferenceParams>(References::METHOD)?;
					let uri = &params.text_document_position.text_document.uri;
					let mut compiler = compiler.lock().unwrap();
					let docs_guard = docs.lock().unwrap();
					let snapshot = compiler.references_analysis_for_uri(&docs_guard, uri);
					let response = match snapshot {
						Some(snapshot) => {
							let candidate = references::references_snapshot_candidate(
								&docs_guard,
								&compiler,
								&snapshot,
								&params,
							);
							drop(docs_guard);
							drop(compiler);
							let result =
								candidate.and_then(references::ReferencesResponseCandidate::validate_disk_sources);
							prepare_references_response(docs, uri, &snapshot, result)
						}
						None => Some(None),
					};
					if let Some(result) = response {
						connection
							.sender
							.send(Message::Response(Response::new_ok(id, result)))?;
					}
				} else if req.method == PrepareRenameRequest::METHOD {
					let (id, params) =
						req.extract::<lsp_types::TextDocumentPositionParams>(PrepareRenameRequest::METHOD)?;
					let uri = &params.text_document.uri;
					let mut compiler = compiler.lock().unwrap();
					let docs_guard = docs.lock().unwrap();
					let snapshot = compiler.references_analysis_for_uri(&docs_guard, uri);
					let response = match snapshot {
						Some(snapshot) => {
							let candidate =
								rename::rename_candidate(&docs_guard, &compiler, &snapshot, params.position, "");
							drop(docs_guard);
							drop(compiler);
							let result = candidate.and_then(rename::RenameCandidate::validate_prepare);
							prepare_if_current(docs, uri, &snapshot, result)
						}
						None => Some(None),
					};
					if let Some(result) = response {
						connection
							.sender
							.send(Message::Response(Response::new_ok(id, result)))?;
					}
				} else if req.method == Rename::METHOD {
					let (id, params) = req.extract::<RenameParams>(Rename::METHOD)?;
					if !rename::valid_new_name(&params.new_name) {
						connection.sender.send(Message::Response(Response::new_err(
							id,
							lsp_server::ErrorCode::InvalidParams as i32,
							"new name must be exactly one Nymph identifier".into(),
						)))?;
						continue;
					}
					let uri = &params.text_document_position.text_document.uri;
					let mut compiler = compiler.lock().unwrap();
					let docs_guard = docs.lock().unwrap();
					let snapshot = compiler.references_analysis_for_uri(&docs_guard, uri);
					let Some(snapshot) = snapshot else {
						drop(docs_guard);
						drop(compiler);
						connection.sender.send(Message::Response(Response::new_err(
							id,
							lsp_server::ErrorCode::InvalidParams as i32,
							"target is not renameable".into(),
						)))?;
						continue;
					};
					let candidate = rename::rename_candidate(
						&docs_guard,
						&compiler,
						&snapshot,
						params.text_document_position.position,
						&params.new_name,
					);
					drop(docs_guard);
					drop(compiler);
					let result = candidate.and_then(rename::RenameCandidate::validate_disk_sources);
					if let Some(edit) = result {
						if let Some(edit) = prepare_if_current(docs, uri, &snapshot, edit) {
							connection
								.sender
								.send(Message::Response(Response::new_ok(id, edit)))?;
						}
					} else {
						connection.sender.send(Message::Response(Response::new_err(
							id,
							lsp_server::ErrorCode::InvalidParams as i32,
							"target is not renameable or project sources changed".into(),
						)))?;
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
			Message::Notification(not) => {
				if not.method == DidChangeWatchedFiles::METHOD && !client_state.watchers_authoritative {
					continue;
				}
				handle_notification(connection, docs, compiler, not)?;
			}
			Message::Response(response) => client_state.handle_response(&response),
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
		m if m == DidChangeWatchedFiles::METHOD => {
			let params: DidChangeWatchedFilesParams = serde_json::from_value(not.params)?;
			let uris: Vec<_> = params
				.changes
				.into_iter()
				.map(|change| change.uri)
				.collect();
			let refreshes = compiler
				.lock()
				.unwrap()
				.watched_files_changed(&mut docs.lock().unwrap(), &uris)?;
			for refresh in refreshes {
				diagnostics::check_and_publish_affected(
					connection,
					docs,
					compiler,
					&refresh.origin,
					&refresh.affected,
				)?;
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

	fn handshake_with_watchers(client: &Connection) -> Request {
		let mut params = InitializeParams::default();
		params.capabilities.workspace = Some(lsp_types::WorkspaceClientCapabilities {
			did_change_watched_files: Some(lsp_types::DidChangeWatchedFilesClientCapabilities {
				dynamic_registration: Some(true),
				relative_pattern_support: None,
			}),
			..Default::default()
		});
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(1),
				"initialize".to_string(),
				serde_json::to_value(params).unwrap(),
			)))
			.unwrap();
		assert!(matches!(
			client.receiver.recv().unwrap(),
			Message::Response(_)
		));
		assert!(
			client
				.receiver
				.recv_timeout(std::time::Duration::from_millis(100))
				.is_err(),
			"dynamic registration must wait for the initialized notification"
		);
		client
			.sender
			.send(Message::Notification(Notification::new(
				lsp_types::notification::Initialized::METHOD.to_string(),
				serde_json::json!({}),
			)))
			.unwrap();
		match client.receiver.recv().unwrap() {
			Message::Request(request) => request,
			other => panic!("expected dynamic registration request, got {other:?}"),
		}
	}

	fn send_watched_file(client: &Connection, uri: Uri) {
		send_watched_files(
			client,
			vec![lsp_types::FileEvent::new(
				uri,
				lsp_types::FileChangeType::CHANGED,
			)],
		);
	}

	fn send_watched_files(client: &Connection, changes: Vec<lsp_types::FileEvent>) {
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidChangeWatchedFiles::METHOD.into(),
				serde_json::to_value(DidChangeWatchedFilesParams { changes }).unwrap(),
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

	fn barrier_diagnostics(client: &Connection, id: i32) -> Vec<PublishDiagnosticsParams> {
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(id),
				"test/barrier".into(),
				serde_json::Value::Null,
			)))
			.unwrap();
		let mut publications = Vec::new();
		loop {
			match client.receiver.recv().unwrap() {
				Message::Notification(notification)
					if notification.method == lsp_types::notification::PublishDiagnostics::METHOD =>
				{
					publications
						.push(serde_json::from_value::<PublishDiagnosticsParams>(notification.params).unwrap());
				}
				Message::Response(response) => {
					assert_eq!(response.id, RequestId::from(id));
					return publications;
				}
				other => panic!("expected diagnostics or barrier response, got {other:?}"),
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

	fn send_change(client: &Connection, uri: Uri, version: i32, text: &str) {
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidChangeTextDocument::METHOD.into(),
				serde_json::to_value(DidChangeTextDocumentParams {
					text_document: VersionedTextDocumentIdentifier { uri, version },
					content_changes: vec![TextDocumentContentChangeEvent {
						range: None,
						range_length: None,
						text: text.into(),
					}],
				})
				.unwrap(),
			)))
			.unwrap();
	}

	fn request_value(
		client: &Connection,
		id: i32,
		method: &str,
		params: serde_json::Value,
	) -> serde_json::Value {
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(id),
				method.into(),
				params,
			)))
			.unwrap();
		recv_response(client, id).response_result.unwrap()
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
	fn untitled_wire_supports_every_advertised_same_buffer_capability_with_unresolved_imports() {
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);
		let uri: Uri = "untitled:capabilities-52".parse().unwrap();
		let text = "import @/missing\nimport @/document as cycle\nfunc helper(value: int): int = value.abs()\nfunc main(): int = {\n  let local=helper(1)\n  local\n}\nfunc bad(): int = true";
		send_open(&client, uri.clone(), 52, text);
		let diagnostics = recv_diagnostics_for(&client, &uri);
		assert_eq!(diagnostics.version, Some(52));
		assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
			diagnostic.code
				== Some(lsp_types::NumberOrString::String(
					"IMPORT-UNRESOLVED".into(),
				))
		}));
		assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
			diagnostic.code
				!= Some(lsp_types::NumberOrString::String(
					"IMPORT-UNRESOLVED".into(),
				)) && diagnostic.range.start.line == 7
		}));

		let position = |line, character| {
			serde_json::json!({
				"textDocument": { "uri": uri.as_str() },
				"position": { "line": line, "character": character }
			})
		};
		let hover = request_value(&client, 10, HoverRequest::METHOD, position(4, 19));
		assert_ne!(hover, serde_json::Value::Null);

		let symbols = request_value(
			&client,
			11,
			DocumentSymbolRequest::METHOD,
			serde_json::json!({ "textDocument": { "uri": uri.as_str() } }),
		);
		assert_eq!(symbols.as_array().unwrap().len(), 3);

		let definition = request_value(&client, 12, GotoDefinition::METHOD, position(4, 14));
		assert_eq!(definition["uri"], uri.as_str());
		assert_eq!(definition["range"]["start"]["line"], 2);

		let mut references_params = position(4, 14);
		references_params["context"] = serde_json::json!({ "includeDeclaration": true });
		let references = request_value(&client, 13, References::METHOD, references_params);
		let references = references.as_array().unwrap();
		assert_eq!(references.len(), 2);
		assert!(
			references
				.iter()
				.all(|location| location["uri"] == uri.as_str())
		);

		let completion = request_value(&client, 14, Completion::METHOD, position(5, 5));
		let completion_items = completion
			.as_array()
			.or_else(|| completion["items"].as_array())
			.unwrap();
		assert!(completion_items.iter().any(|item| item["label"] == "local"));
		assert!(completion_items.iter().all(|item| item["label"] != "cycle"));

		let tokens = request_value(
			&client,
			15,
			SemanticTokensFullRequest::METHOD,
			serde_json::json!({ "textDocument": { "uri": uri.as_str() } }),
		);
		assert!(!tokens["data"].as_array().unwrap().is_empty());

		let formatting = request_value(
			&client,
			16,
			Formatting::METHOD,
			serde_json::json!({
				"textDocument": { "uri": uri.as_str() },
				"options": { "tabSize": 2, "insertSpaces": true }
			}),
		);
		assert!(formatting.as_array().is_some_and(|edits| !edits.is_empty()));

		let range_formatting = request_value(
			&client,
			17,
			RangeFormatting::METHOD,
			serde_json::json!({
				"textDocument": { "uri": uri.as_str() },
				"range": {
					"start": { "line": 3, "character": 2 },
					"end": { "line": 3, "character": 21 }
				},
				"options": { "tabSize": 2, "insertSpaces": true }
			}),
		);
		assert!(
			range_formatting
				.as_array()
				.is_some_and(|edits| !edits.is_empty())
		);

		send_close(&client, uri.clone());
		let clear = recv_diagnostics_for(&client, &uri);
		assert_eq!(clear.version, None);
		assert!(clear.diagnostics.is_empty());
		shutdown(&client, handle);
	}

	#[test]
	fn untitled_wire_versions_malformed_input_and_close_preserve_buffer_authority() {
		let (server, client) = Connection::memory();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let observed_docs = docs.clone();
		let handle =
			std::thread::spawn(move || serve(server, docs, Arc::new(Mutex::new(CompilerState::new()))));
		handshake(&client);
		let uri: Uri = "untitled:lifecycle-52".parse().unwrap();
		send_open(&client, uri.clone(), 7, "func value(): int = true");
		assert_eq!(recv_diagnostics_for(&client, &uri).version, Some(7));

		let malformed = "func value(: = { definitely malformed";
		send_change(&client, uri.clone(), 8, malformed);
		let changed = recv_diagnostics_for(&client, &uri);
		assert_eq!(changed.version, Some(8));
		assert!(!changed.diagnostics.is_empty());
		let malformed_tokens = request_value(
			&client,
			20,
			SemanticTokensFullRequest::METHOD,
			serde_json::json!({ "textDocument": { "uri": uri.as_str() } }),
		);
		assert!(!malformed_tokens["data"].as_array().unwrap().is_empty());

		send_change(&client, uri.clone(), 7, "func stale(): int = 1");
		assert!(barrier_diagnostics(&client, 21).is_empty());
		{
			let docs = observed_docs.lock().unwrap();
			assert_eq!(docs.version(&uri), Some(8));
			assert_eq!(docs.get(&uri).unwrap().text, malformed);
		}

		send_close(&client, uri.clone());
		let clear = recv_diagnostics_for(&client, &uri);
		assert_eq!(clear.version, None);
		assert!(clear.diagnostics.is_empty());
		send_change(&client, uri.clone(), 9, "func resurrected(): int = 1");
		assert!(barrier_diagnostics(&client, 22).is_empty());
		assert!(observed_docs.lock().unwrap().get(&uri).is_none());

		// A reopened lifecycle may restart its client version counter.
		send_open(&client, uri.clone(), 1, "func reopened(): int = 1");
		let reopened = recv_diagnostics_for(&client, &uri);
		assert_eq!(reopened.version, Some(1));
		assert!(reopened.diagnostics.is_empty());
		send_close(&client, uri.clone());
		recv_diagnostics_for(&client, &uri);
		shutdown(&client, handle);
	}

	#[test]
	fn save_reopen_transition_keeps_untitled_and_file_lifecycles_independent() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='untitled-save-transition'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		let source = "import @/dependency with (disk_value)\nfunc main(): int = disk_value()";
		std::fs::write(&main_path, source).unwrap();
		std::fs::write(
			temp.path().join("src/dependency.nym"),
			"public func disk_value(): int = 52",
		)
		.unwrap();
		let file_uri = workspace::path_to_uri(&main_path).unwrap();
		let untitled_uri: Uri = "untitled:save-transition-52".parse().unwrap();
		let (server, client) = Connection::memory();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let observed_docs = docs.clone();
		let handle =
			std::thread::spawn(move || serve(server, docs, Arc::new(Mutex::new(CompilerState::new()))));
		handshake(&client);

		send_open(&client, untitled_uri.clone(), 7, source);
		let isolated = recv_diagnostics_for(&client, &untitled_uri);
		assert!(isolated.diagnostics.iter().any(|diagnostic| {
			diagnostic.code
				== Some(lsp_types::NumberOrString::String(
					"IMPORT-UNRESOLVED".into(),
				))
		}));

		// VS Code may open the saved file URI before closing the old untitled
		// URI. They must remain independent during this overlap.
		send_open(&client, file_uri.clone(), 1, source);
		let file_diagnostics = recv_diagnostics_for(&client, &file_uri);
		assert_eq!(file_diagnostics.version, Some(1));
		assert!(file_diagnostics.diagnostics.is_empty());
		{
			let docs = observed_docs.lock().unwrap();
			assert!(docs.get(&untitled_uri).is_some());
			assert!(docs.get(&file_uri).is_some());
		}

		send_close(&client, untitled_uri.clone());
		let clear = recv_diagnostics_for(&client, &untitled_uri);
		assert!(clear.diagnostics.is_empty());
		assert_eq!(clear.version, None);
		assert!(observed_docs.lock().unwrap().get(&file_uri).is_some());
		let hover = request_value(
			&client,
			30,
			HoverRequest::METHOD,
			serde_json::json!({
				"textDocument": { "uri": file_uri.as_str() },
				"position": { "line": 1, "character": 20 }
			}),
		);
		assert_ne!(hover, serde_json::Value::Null);

		send_close(&client, file_uri.clone());
		recv_diagnostics_for(&client, &file_uri);
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
			result.capabilities.workspace_symbol_provider,
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
		assert_eq!(
			result.capabilities.references_provider,
			Some(OneOf::Left(true))
		);
		assert_eq!(
			result.capabilities.rename_provider,
			Some(OneOf::Right(RenameOptions {
				prepare_provider: Some(true),
				work_done_progress_options: Default::default(),
			}))
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
	fn supported_client_receives_exactly_one_project_file_watcher_registration() {
		let (server, client) = Connection::memory();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let observed_docs = docs.clone();
		let handle =
			std::thread::spawn(move || serve(server, docs, Arc::new(Mutex::new(CompilerState::new()))));
		let request = handshake_with_watchers(&client);
		let registration_request_id = request.id.clone();
		assert_eq!(request.method, RegisterCapability::METHOD);
		let params: RegistrationParams = serde_json::from_value(request.params).unwrap();
		assert_eq!(params.registrations.len(), 1);
		let registration = &params.registrations[0];
		assert_eq!(registration.id, WATCH_REGISTRATION_ID);
		assert_eq!(registration.method, DidChangeWatchedFiles::METHOD);
		let options: DidChangeWatchedFilesRegistrationOptions =
			serde_json::from_value(registration.register_options.clone().unwrap()).unwrap();
		assert_eq!(
			options
				.watchers
				.iter()
				.map(|watcher| &watcher.glob_pattern)
				.collect::<Vec<_>>(),
			[
				&GlobPattern::String("**/*.nym".into()),
				&GlobPattern::String("**/nymph.toml".into())
			]
		);
		assert!(
			options
				.watchers
				.iter()
				.all(|watcher| watcher.kind.is_none())
		);

		client
			.sender
			.send(Message::Response(Response::new_ok(
				RequestId::from("unrelated".to_string()),
				serde_json::Value::Null,
			)))
			.unwrap();
		send_watched_file(
			&client,
			"file:///ignored-before-registration.nym".parse().unwrap(),
		);
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(89),
				"test/barrier".into(),
				serde_json::Value::Null,
			)))
			.unwrap();
		assert_eq!(recv_response(&client, 89).id, RequestId::from(89));
		assert_eq!(
			observed_docs.lock().unwrap().revision(),
			DocumentStore::default().revision(),
			"an unrelated response must not activate watched-file handling"
		);

		client
			.sender
			.send(Message::Response(Response::new_ok(
				registration_request_id,
				serde_json::Value::Null,
			)))
			.unwrap();
		client
			.sender
			.send(Message::Notification(Notification::new(
				lsp_types::notification::Initialized::METHOD.into(),
				serde_json::json!({}),
			)))
			.unwrap();
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(90),
				"test/barrier".into(),
				serde_json::Value::Null,
			)))
			.unwrap();
		match client.receiver.recv().unwrap() {
			Message::Response(response) => assert_eq!(response.id, RequestId::from(90)),
			other => panic!("duplicate initialized triggered an extra message: {other:?}"),
		}
		assert!(client.receiver.try_recv().is_err());
		shutdown(&client, handle);
	}

	#[test]
	fn unsupported_client_skips_registration_and_continues_serving_requests() {
		let (server, client) = Connection::memory();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let observed_docs = docs.clone();
		let handle =
			std::thread::spawn(move || serve(server, docs, Arc::new(Mutex::new(CompilerState::new()))));
		handshake(&client);
		send_watched_file(&client, "file:///unsupported-client.nym".parse().unwrap());
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(91),
				"test/barrier".into(),
				serde_json::Value::Null,
			)))
			.unwrap();
		match client.receiver.recv().unwrap() {
			Message::Response(response) => assert_eq!(response.id, RequestId::from(91)),
			other => panic!("unsupported client unexpectedly received {other:?}"),
		}
		assert_eq!(
			observed_docs.lock().unwrap().revision(),
			DocumentStore::default().revision(),
			"an unsupported client's watcher notification must be ignored"
		);
		shutdown(&client, handle);
	}

	#[test]
	fn rejected_watcher_registration_does_not_stop_the_server() {
		let (server, client) = Connection::memory();
		let docs = Arc::new(Mutex::new(DocumentStore::default()));
		let observed_docs = docs.clone();
		let handle =
			std::thread::spawn(move || serve(server, docs, Arc::new(Mutex::new(CompilerState::new()))));
		let registration = handshake_with_watchers(&client);
		client
			.sender
			.send(Message::Response(Response::new_err(
				registration.id,
				lsp_server::ErrorCode::InternalError as i32,
				"watching unavailable".into(),
			)))
			.unwrap();
		send_watched_file(
			&client,
			"file:///rejected-registration.nym".parse().unwrap(),
		);
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(93),
				"test/barrier".into(),
				serde_json::Value::Null,
			)))
			.unwrap();
		assert_eq!(recv_response(&client, 93).id, RequestId::from(93));
		assert_eq!(
			observed_docs.lock().unwrap().revision(),
			DocumentStore::default().revision(),
			"a rejected registration must not authorize watcher notifications"
		);
		shutdown(&client, handle);
	}

	#[test]
	fn watcher_notification_republishes_disk_dependency_and_importer_over_wire() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-watch'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		let dep_path = temp.path().join("src/dep.nym");
		let main_source = "import @/dep with (value)\nfunc use(): int = value()";
		std::fs::write(&main_path, main_source).unwrap();
		std::fs::write(&dep_path, "public func value(): int = 1").unwrap();
		let main_uri = workspace::path_to_uri(&main_path).unwrap();
		let dep_uri = workspace::path_to_uri(&dep_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		let registration = handshake_with_watchers(&client);
		client
			.sender
			.send(Message::Response(Response::new_ok(
				registration.id,
				serde_json::Value::Null,
			)))
			.unwrap();
		send_open(&client, main_uri.clone(), 1, main_source);
		assert!(
			recv_diagnostics_for(&client, &main_uri)
				.diagnostics
				.is_empty()
		);

		std::fs::write(&dep_path, "public func value(): int = true").unwrap();
		send_watched_files(
			&client,
			vec![
				lsp_types::FileEvent::new(dep_uri.clone(), lsp_types::FileChangeType::CREATED),
				lsp_types::FileEvent::new(main_uri.clone(), lsp_types::FileChangeType::CHANGED),
				lsp_types::FileEvent::new(dep_uri.clone(), lsp_types::FileChangeType::CHANGED),
				lsp_types::FileEvent::new(dep_uri.clone(), lsp_types::FileChangeType::DELETED),
			],
		);
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(92),
				"test/barrier".into(),
				serde_json::Value::Null,
			)))
			.unwrap();
		let mut publications = Vec::new();
		loop {
			match client.receiver.recv().unwrap() {
				Message::Notification(notification)
					if notification.method == lsp_types::notification::PublishDiagnostics::METHOD =>
				{
					publications
						.push(serde_json::from_value::<PublishDiagnosticsParams>(notification.params).unwrap());
				}
				Message::Response(response) => {
					assert_eq!(response.id, RequestId::from(92));
					break;
				}
				other => panic!("expected diagnostics or barrier response, got {other:?}"),
			}
		}
		assert_eq!(
			publications
				.iter()
				.map(|params| &params.uri)
				.collect::<Vec<_>>(),
			[&dep_uri, &main_uri]
		);
		assert!(!publications[0].diagnostics.is_empty());
		assert!(publications[1].diagnostics.is_empty());
		shutdown(&client, handle);
	}

	#[test]
	fn watcher_notification_publishes_an_authoritative_equivalent_overlay_once() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-watch-alias'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		let dep_path = temp.path().join("src/dep.nym");
		let main_source = "import @/dep with (value)\nfunc use(): int = value()";
		std::fs::write(&main_path, main_source).unwrap();
		std::fs::write(&dep_path, "public func value(): int = 1").unwrap();
		let main_uri = workspace::path_to_uri(&main_path).unwrap();
		let dep_uri = workspace::path_to_uri(&dep_path).unwrap();
		let equivalent_uri: Uri = dep_uri
			.as_str()
			.replace("dep.nym", "%64ep.nym")
			.parse()
			.unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		let registration = handshake_with_watchers(&client);
		client
			.sender
			.send(Message::Response(Response::new_ok(
				registration.id,
				serde_json::Value::Null,
			)))
			.unwrap();
		send_open(&client, main_uri.clone(), 1, main_source);
		recv_diagnostics_for(&client, &main_uri);
		send_open(
			&client,
			equivalent_uri.clone(),
			7,
			"public func value(): int = true",
		);
		recv_diagnostics_for(&client, &equivalent_uri);
		recv_diagnostics_for(&client, &main_uri);

		std::fs::write(&dep_path, "public func value(): int = 2").unwrap();
		send_watched_files(
			&client,
			vec![
				lsp_types::FileEvent::new(dep_uri.clone(), lsp_types::FileChangeType::CHANGED),
				lsp_types::FileEvent::new(equivalent_uri.clone(), lsp_types::FileChangeType::CHANGED),
			],
		);
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(94),
				"test/barrier".into(),
				serde_json::Value::Null,
			)))
			.unwrap();
		let mut publications = Vec::new();
		loop {
			match client.receiver.recv().unwrap() {
				Message::Notification(notification)
					if notification.method == lsp_types::notification::PublishDiagnostics::METHOD =>
				{
					publications
						.push(serde_json::from_value::<PublishDiagnosticsParams>(notification.params).unwrap());
				}
				Message::Response(response) => {
					assert_eq!(response.id, RequestId::from(94));
					break;
				}
				other => panic!("expected diagnostics or barrier response, got {other:?}"),
			}
		}
		assert_eq!(
			publications
				.iter()
				.map(|params| (&params.uri, params.version))
				.collect::<Vec<_>>(),
			[(&equivalent_uri, Some(7)), (&main_uri, Some(1))]
		);
		assert!(publications.iter().all(|params| params.uri != dep_uri));
		shutdown(&client, handle);
	}

	#[test]
	fn watcher_read_race_removes_the_source_without_stopping_the_server() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-watch-race'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		let dep_path = temp.path().join("src/dep.nym");
		let main_source = "import @/dep with (value)\nfunc use(): int = value()";
		std::fs::write(&main_path, main_source).unwrap();
		std::fs::write(&dep_path, "public func value(): int = 1").unwrap();
		let main_uri = workspace::path_to_uri(&main_path).unwrap();
		let dep_uri = workspace::path_to_uri(&dep_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		let registration = handshake_with_watchers(&client);
		client
			.sender
			.send(Message::Response(Response::new_ok(
				registration.id,
				serde_json::Value::Null,
			)))
			.unwrap();
		send_open(&client, main_uri.clone(), 1, main_source);
		recv_diagnostics_for(&client, &main_uri);

		std::fs::remove_file(&dep_path).unwrap();
		std::fs::create_dir(&dep_path).unwrap();
		send_watched_file(&client, dep_uri.clone());
		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(95),
				"test/barrier".into(),
				serde_json::Value::Null,
			)))
			.unwrap();
		let mut publications = Vec::new();
		loop {
			match client.receiver.recv().unwrap() {
				Message::Notification(notification)
					if notification.method == lsp_types::notification::PublishDiagnostics::METHOD =>
				{
					publications
						.push(serde_json::from_value::<PublishDiagnosticsParams>(notification.params).unwrap());
				}
				Message::Response(response) => {
					assert_eq!(response.id, RequestId::from(95));
					break;
				}
				other => panic!("expected diagnostics or barrier response, got {other:?}"),
			}
		}
		assert_eq!(
			publications
				.iter()
				.map(|params| &params.uri)
				.collect::<Vec<_>>(),
			[&dep_uri, &main_uri]
		);
		assert!(publications[0].diagnostics.is_empty());
		assert!(publications[1].diagnostics.iter().any(|diagnostic| {
			diagnostic.code
				== Some(lsp_types::NumberOrString::String(
					"IMPORT-UNRESOLVED".into(),
				))
		}));
		shutdown(&client, handle);
	}

	#[test]
	fn stale_reverse_importer_owner_does_not_clear_newer_diagnostics() {
		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-watch-owner'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		let a_path = temp.path().join("src/a.nym");
		let b_path = temp.path().join("src/b.nym");
		let main_source = "import @/a with (middle)\nfunc use(): int = middle()";
		std::fs::write(&main_path, main_source).unwrap();
		std::fs::write(
			&a_path,
			"import @/b with (value)\npublic func middle(): int = value()",
		)
		.unwrap();
		std::fs::write(&b_path, "public func value(): int = 1").unwrap();
		let main_uri = workspace::path_to_uri(&main_path).unwrap();
		let a_uri = workspace::path_to_uri(&a_path).unwrap();
		let b_uri = workspace::path_to_uri(&b_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		let registration = handshake_with_watchers(&client);
		client
			.sender
			.send(Message::Response(Response::new_ok(
				registration.id,
				serde_json::Value::Null,
			)))
			.unwrap();
		send_open(&client, main_uri.clone(), 1, main_source);
		recv_diagnostics_for(&client, &main_uri);

		std::fs::write(&b_path, "public func value(): float = 1.0").unwrap();
		send_watched_file(&client, b_uri.clone());
		let from_b = barrier_diagnostics(&client, 96);
		assert!(
			from_b
				.iter()
				.find(|params| params.uri == a_uri)
				.is_some_and(|params| !params.diagnostics.is_empty())
		);

		std::fs::write(&a_path, "public func middle(): int = true").unwrap();
		send_watched_file(&client, a_uri.clone());
		let from_a = barrier_diagnostics(&client, 97);
		assert!(
			from_a
				.iter()
				.find(|params| params.uri == a_uri)
				.is_some_and(|params| !params.diagnostics.is_empty())
		);

		std::fs::write(&b_path, "public func value(): int = 2").unwrap();
		send_watched_file(&client, b_uri.clone());
		let final_publications = barrier_diagnostics(&client, 98);
		assert_eq!(
			final_publications
				.iter()
				.map(|params| &params.uri)
				.collect::<Vec<_>>(),
			[&b_uri],
			"the old b refresh owner cleared diagnostics more recently published by a"
		);
		shutdown(&client, handle);
	}

	#[test]
	fn watched_manifest_removal_clears_diagnostics_owned_by_a_retired_closed_module() {
		let temp = tempfile::tempdir().unwrap();
		let manifest_path = temp.path().join("nymph.toml");
		std::fs::write(
			&manifest_path,
			"[package]\nname='wire-watch-retired'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		let dep_path = temp.path().join("src/dep.nym");
		let main_source = "import @/dep with (value)\nfunc use(): int = value()";
		std::fs::write(&main_path, main_source).unwrap();
		std::fs::write(&dep_path, "public func value(): int = 1").unwrap();
		let manifest_uri = workspace::path_to_uri(&manifest_path).unwrap();
		let main_uri = workspace::path_to_uri(&main_path).unwrap();
		let dep_uri = workspace::path_to_uri(&dep_path).unwrap();
		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		let registration = handshake_with_watchers(&client);
		client
			.sender
			.send(Message::Response(Response::new_ok(
				registration.id,
				serde_json::Value::Null,
			)))
			.unwrap();
		send_open(&client, main_uri.clone(), 1, main_source);
		recv_diagnostics_for(&client, &main_uri);

		std::fs::write(&dep_path, "public func value(): float = 1.0").unwrap();
		send_watched_file(&client, dep_uri.clone());
		let from_dep = barrier_diagnostics(&client, 99);
		assert!(
			from_dep
				.iter()
				.find(|params| params.uri == main_uri)
				.is_some_and(|params| !params.diagnostics.is_empty())
		);

		std::fs::remove_file(&manifest_path).unwrap();
		send_watched_files(
			&client,
			vec![lsp_types::FileEvent::new(
				manifest_uri,
				lsp_types::FileChangeType::DELETED,
			)],
		);
		let transition = barrier_diagnostics(&client, 100);
		assert_eq!(
			transition
				.iter()
				.filter(|params| params.uri == dep_uri)
				.count(),
			1,
			"the retired dependency must be cleared exactly once"
		);
		assert!(
			transition
				.iter()
				.find(|params| params.uri == dep_uri)
				.is_some_and(|params| params.diagnostics.is_empty()),
			"the retired dependency kept diagnostics from its former project"
		);
		assert_eq!(
			transition
				.iter()
				.filter(|params| params.uri == main_uri)
				.count(),
			1,
			"the transitioned open document must be republished exactly once"
		);
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
		assert!(prepare_references_response(&docs, &uri, &snapshot, None).is_none());
		assert!(
			prepare_if_current(&docs, &uri, &snapshot, lsp_types::WorkspaceEdit::default(),).is_none(),
			"rename edits from a prior close/reopen lifecycle must not be published"
		);
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

	#[test]
	fn references_request_round_trips_success_and_no_symbol_through_the_wire() {
		use lsp_types::{
			Position, ReferenceContext, ReferenceParams, TextDocumentIdentifier,
			TextDocumentPositionParams, WorkDoneProgressParams,
			request::{References, Request as _},
		};

		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);

		let uri: Uri = "file:///wire_references.nym".parse().unwrap();
		let text = "func main(): int = {\n  let value = 1\n  value\n}";
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

		let request = |id, position| {
			Message::Request(Request::new(
				RequestId::from(id),
				References::METHOD.to_string(),
				serde_json::to_value(ReferenceParams {
					text_document_position: TextDocumentPositionParams {
						text_document: TextDocumentIdentifier { uri: uri.clone() },
						position,
					},
					work_done_progress_params: WorkDoneProgressParams::default(),
					partial_result_params: Default::default(),
					context: ReferenceContext {
						include_declaration: true,
					},
				})
				.unwrap(),
			))
		};
		client.sender.send(request(3, Position::new(2, 3))).unwrap();
		let found: Option<Vec<lsp_types::Location>> = loop {
			match client.receiver.recv().unwrap() {
				Message::Response(response) => {
					break serde_json::from_value(response.response_result.unwrap()).unwrap();
				}
				_ => continue,
			}
		};
		let found = found.expect("references result");
		assert_eq!(found.len(), 2);
		assert_eq!(found[0].range.start, Position::new(1, 6));
		assert_eq!(found[1].range.start, Position::new(2, 2));

		client.sender.send(request(4, Position::new(0, 4))).unwrap();
		let missing: Option<Vec<lsp_types::Location>> = loop {
			match client.receiver.recv().unwrap() {
				Message::Response(response) => {
					break serde_json::from_value(response.response_result.unwrap()).unwrap();
				}
				_ => continue,
			}
		};
		assert!(missing.is_none());

		shutdown(&client, handle);
	}

	#[test]
	fn references_request_discovers_unopened_modules_added_after_initial_sync() {
		use lsp_types::{
			Position, ReferenceContext, ReferenceParams, TextDocumentIdentifier,
			TextDocumentPositionParams, WorkDoneProgressParams,
			request::{References, Request as _},
		};

		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='late-references'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let target_path = temp.path().join("src/target.nym");
		let target_source = "public func answer(): int = 1";
		std::fs::write(&target_path, target_source).unwrap();
		let target_uri = workspace::path_to_uri(&target_path).unwrap();

		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);
		client
			.sender
			.send(Message::Notification(Notification::new(
				DidOpenTextDocument::METHOD.to_string(),
				serde_json::to_value(DidOpenTextDocumentParams {
					text_document: TextDocumentItem {
						uri: target_uri.clone(),
						language_id: "nymph".to_string(),
						version: 1,
						text: target_source.to_string(),
					},
				})
				.unwrap(),
			)))
			.unwrap();
		let _ = recv_diagnostics_for(&client, &target_uri);

		let importer_path = temp.path().join("src/late_importer.nym");
		std::fs::write(
			&importer_path,
			"import @/target with (answer)\nfunc use(): int = answer()",
		)
		.unwrap();
		std::fs::write(
			temp.path().join("src/malformed.nym"),
			"func broken(: = answer answer",
		)
		.unwrap();

		let request = |id| {
			Message::Request(Request::new(
				RequestId::from(id),
				References::METHOD.to_string(),
				serde_json::to_value(ReferenceParams {
					text_document_position: TextDocumentPositionParams {
						text_document: TextDocumentIdentifier {
							uri: target_uri.clone(),
						},
						position: Position::new(0, 13),
					},
					work_done_progress_params: WorkDoneProgressParams::default(),
					partial_result_params: Default::default(),
					context: ReferenceContext {
						include_declaration: true,
					},
				})
				.unwrap(),
			))
		};
		let receive = || -> Option<Vec<lsp_types::Location>> {
			loop {
				match client.receiver.recv().unwrap() {
					Message::Response(response) => {
						break serde_json::from_value(response.response_result.unwrap()).unwrap();
					}
					_ => continue,
				}
			}
		};

		client.sender.send(request(3)).unwrap();
		let found = receive().expect("references result");
		assert_eq!(found.len(), 3, "declaration, late import, and late use");
		let importer_uri = workspace::path_to_uri(&importer_path).unwrap();
		assert_eq!(
			found
				.iter()
				.filter(|location| location.uri == importer_uri)
				.map(|location| location.range.start)
				.collect::<Vec<_>>(),
			vec![Position::new(0, 22), Position::new(1, 18)]
		);

		std::fs::write(
			&importer_path,
			"\nimport @/target with (answer)\nfunc use(): int = answer()\nfunc again(): int = answer()",
		)
		.unwrap();
		client.sender.send(request(4)).unwrap();
		let refreshed = receive().expect("references after unopened disk edit");
		assert_eq!(refreshed.len(), 4);
		assert_eq!(
			refreshed
				.iter()
				.filter(|location| location.uri == importer_uri)
				.map(|location| location.range.start)
				.collect::<Vec<_>>(),
			vec![
				Position::new(1, 22),
				Position::new(2, 18),
				Position::new(3, 20),
			]
		);

		std::fs::remove_file(importer_path).unwrap();
		client.sender.send(request(5)).unwrap();
		let after_delete = receive().expect("references after unopened module deletion");
		assert_eq!(after_delete.len(), 1);
		assert_eq!(after_delete[0].uri, target_uri);

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

	#[test]
	fn workspace_symbol_request_round_trips_unopened_project_declarations() {
		use lsp_types::{
			WorkspaceSymbolResponse,
			request::{Request as _, WorkspaceSymbolRequest},
		};

		let temp = tempfile::tempdir().unwrap();
		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='wire-symbols'\nversion='0.1.0'\n",
		)
		.unwrap();
		std::fs::create_dir(temp.path().join("src")).unwrap();
		let main_path = temp.path().join("src/main.nym");
		std::fs::write(&main_path, "public func main(): void = {}").unwrap();
		std::fs::write(
			temp.path().join("src/unopened.nym"),
			"public struct WireResult()",
		)
		.unwrap();
		let uri = workspace::path_to_uri(&main_path).unwrap();

		let (server, client) = Connection::memory();
		let handle = std::thread::spawn(move || run(server));
		handshake(&client);
		send_open(&client, uri.clone(), 1, "public func main(): void = {}");
		recv_diagnostics_for(&client, &uri);

		client
			.sender
			.send(Message::Request(Request::new(
				RequestId::from(46),
				WorkspaceSymbolRequest::METHOD.to_string(),
				serde_json::json!({ "query": "WireResult" }),
			)))
			.unwrap();
		let response = recv_response(&client, 46);
		let result: Option<WorkspaceSymbolResponse> =
			serde_json::from_value(response.response_result.unwrap()).unwrap();
		let WorkspaceSymbolResponse::Flat(symbols) = result.unwrap() else {
			panic!("expected flat workspace symbols");
		};
		assert_eq!(symbols.len(), 1);
		assert_eq!(symbols[0].name, "WireResult");
		assert_eq!(symbols[0].kind, lsp_types::SymbolKind::STRUCT);
		assert_eq!(symbols[0].container_name.as_deref(), Some("unopened"));

		shutdown(&client, handle);
	}
}
