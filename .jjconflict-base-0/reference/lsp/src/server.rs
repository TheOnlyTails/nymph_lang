use crate::analyzer::SemanticAnalyzer;
use crate::document::{self, Document};
use crate::semantic_tokens::{SemanticToken, SemanticTokenizer, TokenType};
use crate::symbols::symbol_kind_to_lsp_enum;
use crate::workspace::Workspace;
use nymph_compiler::ast;
use std::sync::Arc;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{self, *};
use tower_lsp::{Client, LanguageServer};

pub struct NymphLanguageServer {
	client: Client,
	workspace: Arc<Workspace>,
	analyzer: SemanticAnalyzer,
}

impl NymphLanguageServer {
	pub fn new(client: Client) -> Self {
		Self {
			client,
			workspace: Arc::new(Workspace::new()),
			analyzer: SemanticAnalyzer::new(),
		}
	}

	/// Publish diagnostics for a document
	async fn publish_diagnostics(&self, uri: &str, doc: &Document) {
		let diagnostics = doc
			.diagnostics
			.iter()
			.filter_map(|diag| {
				Some(Diagnostic {
					range: doc.span_to_lsp_range(diag.span)?,
					severity: Some(DiagnosticSeverity::ERROR),
					source: Some(diag.source.clone()),
					message: diag.message.clone(),
					..Default::default()
				})
			})
			.collect();

		if let Ok(parsed_uri) = uri.parse::<Url>() {
			self
				.client
				.publish_diagnostics(parsed_uri, diagnostics, None)
				.await;
		}
	}
}

#[tower_lsp::async_trait]
impl LanguageServer for NymphLanguageServer {
	async fn initialize(&self, _: InitializeParams) -> LspResult<InitializeResult> {
		let capabilities = ServerCapabilities {
			text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
			hover_provider: Some(HoverProviderCapability::Simple(true)),
			document_symbol_provider: Some(OneOf::Left(true)),
			semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
				SemanticTokensOptions {
					legend: SemanticTokensLegend {
						token_types: vec![
							"keyword".into(),
							"type".into(),
							"function".into(),
							"variable".into(),
							"parameter".into(),
							"number".into(),
							"string".into(),
							"comment".into(),
							"operator".into(),
							"interface".into(),
							"member".into(),
						],
						token_modifiers: vec![
							"declaration".into(),
							"definition".into(),
							"builtin".into(),
							"mutable".into(),
						],
					},
					range: Some(true),
					full: Some(SemanticTokensFullOptions::Bool(true)),
					work_done_progress_options: WorkDoneProgressOptions::default(),
				},
			)),
			signature_help_provider: Some(SignatureHelpOptions {
				trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
				retrigger_characters: Some(vec![",".to_string()]),
				work_done_progress_options: WorkDoneProgressOptions::default(),
			}),
			completion_provider: Some(CompletionOptions::default()),
			definition_provider: Some(OneOf::Left(true)),
			references_provider: Some(OneOf::Left(true)),
			rename_provider: Some(OneOf::Right(RenameOptions {
				prepare_provider: Some(true),
				work_done_progress_options: WorkDoneProgressOptions::default(),
			})),
			workspace_symbol_provider: Some(OneOf::Left(true)),
			workspace: Some(WorkspaceServerCapabilities {
				workspace_folders: Some(WorkspaceFoldersServerCapabilities {
					supported: Some(true),
					change_notifications: Some(OneOf::Left(true)),
				}),
				file_operations: None,
			}),
			..Default::default()
		};

		Ok(InitializeResult {
			capabilities,
			server_info: Some(ServerInfo {
				name: "Nymph Language Server".to_string(),
				version: Some(env!("CARGO_PKG_VERSION").to_string()),
			}),
		})
	}

	async fn initialized(&self, _: InitializedParams) {
		self
			.client
			.log_message(MessageType::INFO, "Nymph Language Server initialized")
			.await;
	}

	async fn shutdown(&self) -> LspResult<()> {
		Ok(())
	}

	async fn did_open(&self, params: DidOpenTextDocumentParams) {
		let uri = params.text_document.uri.to_string();
		let content = params.text_document.text.clone();

		self.workspace.open_document(uri.clone(), content).await;

		if let Some(doc) = self.workspace.get_document(&uri, Clone::clone).await {
			self.analyzer.analyze(&doc);
		}

		// Publish diagnostics
		if let Some(doc) = self.workspace.get_document(&uri, Clone::clone).await {
			self.publish_diagnostics(&uri, &doc).await;
		}

		self
			.client
			.log_message(MessageType::INFO, format!("Opened document: {uri}"))
			.await;
	}

	async fn did_change(&self, params: DidChangeTextDocumentParams) {
		let uri = params.text_document.uri.to_string();

		for change in params.content_changes {
			match change {
				TextDocumentContentChangeEvent {
					range: None, text, ..
				} => {
					self.workspace.update_document(uri.clone(), text).await;
				}
				TextDocumentContentChangeEvent {
					range: Some(range),
					text,
					..
				} => {
					self
						.workspace
						.apply_document_change(&uri, range, text)
						.await;
				}
			}
		}

		if let Some(doc) = self.workspace.get_document(&uri, Clone::clone).await {
			self.analyzer.analyze(&doc);
		}

		// Publish diagnostics after changes
		if let Some(doc) = self.workspace.get_document(&uri, Clone::clone).await {
			self.publish_diagnostics(&uri, &doc).await;
		}
	}

	async fn did_close(&self, params: DidCloseTextDocumentParams) {
		let uri = params.text_document.uri.to_string();
		self.workspace.close_document(&uri).await;
		self
			.client
			.publish_diagnostics(params.text_document.uri.clone(), Vec::new(), None)
			.await;

		self
			.client
			.log_message(MessageType::INFO, format!("Closed document: {uri}"))
			.await;
	}

	async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
		let uri = params
			.text_document_position_params
			.text_document
			.uri
			.to_string();

		let analyzer = &self.analyzer;

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				if let Some(symbol) = analyzer.get_symbol_at_lsp_position(
					params.text_document_position_params.position.line,
					params.text_document_position_params.position.character,
					doc,
				) {
					let type_info = symbol.type_info.unwrap_or_else(|| symbol.name.clone());
					let contents = format!("```nymph\n{type_info}\n```");
					let start = doc.position_to_offset(
						symbol.range.start_line,
						symbol.range.start_char.saturating_sub(1),
					)?;
					let end = doc.position_to_offset(
						symbol.range.end_line,
						symbol.range.end_char.saturating_sub(1),
					)?;
					let range = doc.span_to_lsp_range(ast::Span::new(start, end));
					Some(Hover {
						contents: HoverContents::Markup(MarkupContent {
							kind: MarkupKind::Markdown,
							value: contents,
						}),
						range,
					})
				} else {
					None
				}
			})
			.await
			.flatten();

		Ok(result)
	}

	async fn goto_definition(
		&self,
		params: GotoDefinitionParams,
	) -> LspResult<Option<GotoDefinitionResponse>> {
		let uri = params
			.text_document_position_params
			.text_document
			.uri
			.to_string();

		let analyzer = &self.analyzer;

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				let target = analyzer.get_definition_at_position(
					params.text_document_position_params.position.line,
					params.text_document_position_params.position.character,
					doc,
				)?;
				let target_uri = target.uri.parse::<Url>().ok()?;
				let range = if target.uri == doc.uri {
					doc.span_to_lsp_range(target.span)?
				} else {
					let target_path = target_uri.to_file_path().ok()?;
					let target_doc = Document::load_from_path(&target_path).ok()?;
					target_doc.span_to_lsp_range(target.span)?
				};
				Some(GotoDefinitionResponse::Scalar(Location {
					uri: target_uri,
					range,
				}))
			})
			.await
			.flatten();

		Ok(result)
	}

	async fn document_symbol(
		&self,
		params: DocumentSymbolParams,
	) -> LspResult<Option<DocumentSymbolResponse>> {
		let uri = params.text_document.uri.to_string();

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				if let Some(spanned_module) = &doc.ast {
					let symbols = extract_ast_symbols_nested(&spanned_module.0, doc);
					if symbols.is_empty() {
						None
					} else {
						Some(DocumentSymbolResponse::Nested(symbols))
					}
				} else {
					None
				}
			})
			.await
			.flatten();

		Ok(result)
	}

	async fn semantic_tokens_full(
		&self,
		params: SemanticTokensParams,
	) -> LspResult<Option<SemanticTokensResult>> {
		let uri = params.text_document.uri.to_string();

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				if doc.ast.is_some() {
					let mut tokenizer = SemanticTokenizer::new();
					let tokens = tokenizer.tokenize_document(doc);
					let tokens = self.encode_semantic_tokens(&tokens);
					Some(tokens)
				} else {
					None
				}
			})
			.await
			.flatten();

		Ok(result)
	}

	async fn semantic_tokens_range(
		&self,
		params: SemanticTokensRangeParams,
	) -> LspResult<Option<SemanticTokensRangeResult>> {
		let uri = params.text_document.uri.to_string();

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				if doc.ast.is_some() {
					let mut tokenizer = SemanticTokenizer::new();
					let tokens = tokenizer.tokenize_document(doc);
					let tokens = self.encode_semantic_tokens(&tokens);
					match tokens {
						SemanticTokensResult::Tokens(t) => Some(SemanticTokensRangeResult::Tokens(t)),
						SemanticTokensResult::Partial(p) => Some(SemanticTokensRangeResult::Partial(p)),
					}
				} else {
					None
				}
			})
			.await
			.flatten();

		Ok(result)
	}

	async fn completion(&self, _params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
		let uri = _params.text_document_position.text_document.uri.to_string();
		let position = _params.text_document_position.position;
		let analyzer = &self.analyzer;

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				let prefix = completion_prefix(doc, position);
				let items = analyzer
					.get_completion_suggestions(position.line, position.character, doc)
					.into_iter()
					.filter(|suggestion| {
						prefix
							.as_ref()
							.is_none_or(|prefix| suggestion.label.starts_with(prefix))
					})
					.map(|suggestion| CompletionItem {
						label: suggestion.label,
						kind: Some(symbol_kind_to_completion_item_kind(suggestion.kind)),
						detail: suggestion.detail,
						..Default::default()
					})
					.collect::<Vec<_>>();
				Some(CompletionResponse::Array(items))
			})
			.await
			.flatten();

		Ok(result)
	}

	#[allow(deprecated)]
	async fn symbol(
		&self,
		params: WorkspaceSymbolParams,
	) -> LspResult<Option<Vec<SymbolInformation>>> {
		let docs = self.workspace.documents().await;
		let query = params.query.to_lowercase();
		let analyzer = &self.analyzer;
		let mut results = Vec::new();

		for doc in docs {
			let Some(ast) = &doc.ast else {
				continue;
			};
			for symbol in analyzer.extract_symbols(&ast.0) {
				if !query.is_empty() && !symbol.name.to_lowercase().contains(&query) {
					continue;
				}
				let Some(range) =
					doc.span_to_lsp_range(ast::Span::new(symbol.start_offset, symbol.end_offset))
				else {
					continue;
				};
				let Ok(uri) = doc.uri.parse::<Url>() else {
					continue;
				};
				results.push(SymbolInformation {
					name: symbol.name,
					kind: symbol_kind_to_lsp_enum(symbol.kind),
					tags: None,
					location: Location { uri, range },
					container_name: None,
					deprecated: None,
				});
			}
		}

		Ok(Some(results))
	}

	async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
		let uri = params.text_document_position.text_document.uri.to_string();
		let position = params.text_document_position.position;
		let analyzer = &self.analyzer;
		let docs = self.workspace.documents().await;

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				let mut search_docs = docs.clone();
				if !search_docs.iter().any(|candidate| candidate.uri == doc.uri) {
					search_docs.push(doc.clone());
				}

				let (_, _, references) =
					analyzer.find_references(position.line, position.character, doc, &search_docs)?;
				let locations = references
					.into_iter()
					.filter_map(|reference| {
						let reference_uri = reference.uri.parse::<Url>().ok()?;
						let range = if reference.uri == doc.uri {
							doc.span_to_lsp_range(reference.span)?
						} else {
							let path = reference_uri.to_file_path().ok()?;
							let target_doc = Document::load_from_path(&path).ok()?;
							target_doc.span_to_lsp_range(reference.span)?
						};
						Some(Location {
							uri: reference_uri,
							range,
						})
					})
					.collect::<Vec<_>>();
				Some(locations)
			})
			.await
			.flatten();

		Ok(result)
	}

	async fn prepare_rename(
		&self,
		params: TextDocumentPositionParams,
	) -> LspResult<Option<PrepareRenameResponse>> {
		let uri = params.text_document.uri.to_string();
		let position = params.position;
		let analyzer = &self.analyzer;
		let docs = self.workspace.documents().await;

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				let (symbol, target, _) =
					analyzer.find_references(position.line, position.character, doc, &docs)?;
				if target.uri != doc.uri || symbol.kind == crate::analyzer::SymbolKind::Module {
					return None;
				}
				let start = doc.position_to_offset(
					symbol.range.start_line,
					symbol.range.start_char.saturating_sub(1),
				)?;
				let end = doc.position_to_offset(
					symbol.range.end_line,
					symbol.range.end_char.saturating_sub(1),
				)?;
				let range = doc.span_to_lsp_range(ast::Span::new(start, end))?;
				Some(PrepareRenameResponse::RangeWithPlaceholder {
					range,
					placeholder: symbol.name,
				})
			})
			.await
			.flatten();

		Ok(result)
	}

	async fn rename(&self, params: RenameParams) -> LspResult<Option<WorkspaceEdit>> {
		let uri = params.text_document_position.text_document.uri.to_string();
		let position = params.text_document_position.position;
		let analyzer = &self.analyzer;
		let docs = self.workspace.documents().await;

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				let (symbol, target, references) =
					analyzer.find_references(position.line, position.character, doc, &docs)?;
				if target.uri != doc.uri || symbol.kind == crate::analyzer::SymbolKind::Module {
					return None;
				}

				let mut changes: std::collections::HashMap<Url, Vec<TextEdit>> =
					std::collections::HashMap::new();
				for reference in references {
					let reference_uri = reference.uri.parse::<Url>().ok()?;
					let range = if reference.uri == doc.uri {
						doc.span_to_lsp_range(reference.span)?
					} else {
						let path = reference_uri.to_file_path().ok()?;
						let target_doc = Document::load_from_path(&path).ok()?;
						target_doc.span_to_lsp_range(reference.span)?
					};
					changes.entry(reference_uri).or_default().push(TextEdit {
						range,
						new_text: params.new_name.clone(),
					});
				}

				Some(WorkspaceEdit {
					changes: Some(changes),
					document_changes: None,
					change_annotations: None,
				})
			})
			.await
			.flatten();

		Ok(result)
	}

	async fn signature_help(&self, params: SignatureHelpParams) -> LspResult<Option<SignatureHelp>> {
		let uri = params
			.text_document_position_params
			.text_document
			.uri
			.to_string();
		let position = params.text_document_position_params.position;
		let analyzer = &self.analyzer;

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				let help = analyzer.get_signature_help(position.line, position.character, doc)?;
				let parameters = help
					.parameters
					.iter()
					.map(|parameter| ParameterInformation {
						label: ParameterLabel::Simple(parameter.clone()),
						documentation: None,
					})
					.collect::<Vec<_>>();
				Some(SignatureHelp {
					signatures: vec![SignatureInformation {
						label: help.label,
						documentation: None,
						parameters: Some(parameters),
						active_parameter: Some(help.active_parameter as u32),
					}],
					active_signature: Some(0),
					active_parameter: Some(help.active_parameter as u32),
				})
			})
			.await
			.flatten();

		Ok(result)
	}
}

impl NymphLanguageServer {
	fn encode_semantic_tokens(&self, tokens: &[SemanticToken]) -> SemanticTokensResult {
		let mut data: Vec<lsp_types::SemanticToken> = Vec::new();
		let mut prev_line = 0;
		let mut prev_start_char = 0;

		for token in tokens {
			let line_delta = token.line.saturating_sub(prev_line) as u32;
			let start_char_delta = if prev_line == token.line {
				token.start_char.saturating_sub(prev_start_char) as u32
			} else {
				token.start_char as u32
			};

			let token_type_index = match token.token_type {
				TokenType::Keyword => 0u32,
				TokenType::Type => 1u32,
				TokenType::Function => 2u32,
				TokenType::Variable => 3u32,
				TokenType::Parameter => 4u32,
				TokenType::Number => 5u32,
				TokenType::String => 6u32,
				TokenType::Comment => 7u32,
				TokenType::Operator => 8u32,
				TokenType::Interface => 9u32,
				TokenType::Member => 10u32,
			};

			let modifier_mask: u32 = token
				.modifiers
				.iter()
				.map(|modifier| match modifier {
					crate::semantic_tokens::TokenModifier::Declaration => 1u32 << 0,
					crate::semantic_tokens::TokenModifier::Definition => 1u32 << 1,
					crate::semantic_tokens::TokenModifier::Builtin => 1u32 << 2,
					crate::semantic_tokens::TokenModifier::Mutable => 1u32 << 3,
				})
				.fold(0u32, |acc, x| acc | x);

			data.push(lsp_types::SemanticToken {
				delta_line: line_delta,
				delta_start: start_char_delta,
				length: token.length as u32,
				token_type: token_type_index,
				token_modifiers_bitset: modifier_mask,
			});

			prev_line = token.line;
			prev_start_char = token.start_char;
		}

		SemanticTokensResult::Tokens(SemanticTokens {
			result_id: None,
			data,
		})
	}
}

fn completion_prefix(doc: &Document, position: Position) -> Option<String> {
	let offset = doc.lsp_position_to_offset(position.line, position.character)?;
	let prefix = doc.content[..offset]
		.chars()
		.rev()
		.take_while(|ch| ch.is_alphanumeric() || *ch == '_')
		.collect::<Vec<_>>()
		.into_iter()
		.rev()
		.collect::<String>();
	(!prefix.is_empty()).then_some(prefix)
}

fn symbol_kind_to_completion_item_kind(kind: crate::analyzer::SymbolKind) -> CompletionItemKind {
	match kind {
		crate::analyzer::SymbolKind::Function => CompletionItemKind::FUNCTION,
		crate::analyzer::SymbolKind::Variable => CompletionItemKind::VARIABLE,
		crate::analyzer::SymbolKind::Type | crate::analyzer::SymbolKind::Struct => {
			CompletionItemKind::STRUCT
		}
		crate::analyzer::SymbolKind::Interface => CompletionItemKind::INTERFACE,
		crate::analyzer::SymbolKind::Parameter => CompletionItemKind::VARIABLE,
		crate::analyzer::SymbolKind::Field => CompletionItemKind::FIELD,
		crate::analyzer::SymbolKind::Enum => CompletionItemKind::ENUM,
		crate::analyzer::SymbolKind::Namespace | crate::analyzer::SymbolKind::Module => {
			CompletionItemKind::MODULE
		}
	}
}

/// Extract document symbols from the AST
fn extract_ast_symbols_nested(
	module: &ast::declaration::Module,
	doc: &document::Document,
) -> Vec<DocumentSymbol> {
	let mut symbols = Vec::new();

	for decl in &module.members {
		if let Some(symbol) = declaration_to_document_symbol(decl, doc) {
			symbols.push(symbol);
		}
	}

	symbols
}

/// Convert a declaration to a DocumentSymbol with children
#[allow(deprecated)]
fn declaration_to_document_symbol(
	decl: &ast::declaration::Declaration,
	doc: &document::Document,
) -> Option<DocumentSymbol> {
	use nymph_compiler::ast::declaration::Declaration;

	match decl {
		Declaration::Let { meta, value, .. } => {
			if let Some(name_ident) = meta.name.0.as_binding() {
				let name = &name_ident.0.clone();
				let declaration_span = let_declaration_span(meta, Some(value.1));
				make_document_symbol(
					name,
					SymbolKind::VARIABLE,
					declaration_span,
					name_ident.1,
					doc,
					vec![],
				)
			} else {
				None
			}
		}
		Declaration::ExternalLet(_, _, meta) => {
			if let Some(name_ident) = meta.name.0.as_binding() {
				let name = &name_ident.0.clone();
				let declaration_span = let_declaration_span(meta, None);
				make_document_symbol(
					name,
					SymbolKind::VARIABLE,
					declaration_span,
					name_ident.1,
					doc,
					vec![],
				)
			} else {
				None
			}
		}
		Declaration::Func { meta, body, .. } => {
			let name = &meta.name.0.clone();
			let declaration_span = func_declaration_span(meta, Some(body.1));
			make_document_symbol(
				name,
				SymbolKind::FUNCTION,
				declaration_span,
				meta.name.1,
				doc,
				vec![],
			)
		}
		Declaration::ExternalFunc(_, _, meta) => {
			let name = &meta.name.0.clone();
			let declaration_span = func_declaration_span(meta, None);
			make_document_symbol(
				name,
				SymbolKind::FUNCTION,
				declaration_span,
				meta.name.1,
				doc,
				vec![],
			)
		}
		Declaration::TypeAlias { meta, value, .. } => {
			let name = &meta.name.0.clone();
			let mut declaration_span = meta.name.1;
			declaration_span = extend_with_spanned_slice(declaration_span, &meta.generics);
			declaration_span = merge_spans(declaration_span, value.1);
			make_document_symbol(
				name,
				SymbolKind::TYPE_PARAMETER,
				declaration_span,
				meta.name.1,
				doc,
				vec![],
			)
		}
		Declaration::Struct {
			name,
			generics,
			members,
			fields,
			..
		} => {
			let name_str = &name.0.clone();
			let mut children = Vec::new();
			let mut declaration_span = name.1;
			declaration_span = extend_with_spanned_slice(declaration_span, generics);
			declaration_span = extend_with_spanned_slice(declaration_span, fields);
			declaration_span = extend_with_spanned_slice(declaration_span, members);

			// Add fields
			for field in fields {
				let field_inner = &field.0.clone();
				let field_name = &field_inner.name.0;
				if let Some(sym) = make_document_symbol(
					field_name,
					SymbolKind::FIELD,
					field.1,
					field_inner.name.1,
					doc,
					vec![],
				) {
					children.push(sym);
				}
			}

			// Add members (instance methods, etc.)
			children.extend(extract_struct_inner_members(members, doc));

			make_document_symbol(
				name_str,
				SymbolKind::STRUCT,
				declaration_span,
				name.1,
				doc,
				children,
			)
		}
		Declaration::Enum {
			name,
			generics,
			variants,
			members,
			..
		} => {
			let name_str = &name.0.clone();
			let mut children = Vec::new();
			let mut declaration_span = name.1;
			declaration_span = extend_with_spanned_slice(declaration_span, generics);
			declaration_span = extend_with_spanned_slice(declaration_span, variants);
			declaration_span = extend_with_spanned_slice(declaration_span, members);

			// Add enum variants
			for variant in variants {
				let variant_inner = &variant.0.clone();
				let variant_name = &variant_inner.name.0;
				if let Some(sym) = make_document_symbol(
					variant_name,
					SymbolKind::ENUM_MEMBER,
					variant.1,
					variant_inner.name.1,
					doc,
					vec![],
				) {
					children.push(sym);
				}
			}

			// Add members (instance methods, etc.)
			children.extend(extract_struct_inner_members(members, doc));

			make_document_symbol(
				name_str,
				SymbolKind::ENUM,
				declaration_span,
				name.1,
				doc,
				children,
			)
		}
		Declaration::Namespace { name, members, .. } => {
			let name_str = &name.0.clone();
			let children = extract_impl_members(members, doc);
			let declaration_span = extend_with_spanned_slice(name.1, members);
			make_document_symbol(
				name_str,
				SymbolKind::NAMESPACE,
				declaration_span,
				name.1,
				doc,
				children,
			)
		}
		Declaration::Interface {
			name,
			generics,
			super_interfaces,
			members,
			..
		} => {
			let name_str = &name.0.clone();
			let children = extract_interface_members(members, doc);
			let mut declaration_span = name.1;
			declaration_span = extend_with_spanned_slice(declaration_span, generics);
			declaration_span = extend_with_spanned_slice(declaration_span, super_interfaces);
			declaration_span = extend_with_spanned_slice(declaration_span, members);
			make_document_symbol(
				name_str,
				SymbolKind::INTERFACE,
				declaration_span,
				name.1,
				doc,
				children,
			)
		}
		Declaration::Impl {
			type_,
			generics,
			members,
			mutable,
			..
		} => {
			let type_name = type_to_display_name(&type_.0);
			let prefix = if *mutable { "impl mut " } else { "impl " };
			let declaration_span = impl_block_span(type_.1, generics, members);
			make_document_symbol(
				&format!("{prefix}{type_name}"),
				SymbolKind::OBJECT,
				declaration_span,
				type_.1,
				doc,
				extract_impl_members(members, doc),
			)
		}
		Declaration::ImplFor {
			type_,
			generics,
			for_interface,
			members,
			mutable,
			..
		} => {
			let type_name = type_to_display_name(&type_.0);
			let interface_name = &for_interface.0.0.clone();
			let prefix = if *mutable { "impl mut " } else { "impl " };
			let children = extract_impl_members(members, doc);
			let mut declaration_span = impl_block_span(type_.1, generics, members);
			declaration_span = merge_spans(declaration_span, for_interface.0.1);
			declaration_span = extend_with_spanned_slice(declaration_span, &for_interface.1);
			make_document_symbol(
				&format!("{prefix}{interface_name} for {type_name}"),
				SymbolKind::OBJECT,
				declaration_span,
				for_interface.0.1,
				doc,
				children,
			)
		}
		Declaration::Import { .. } => None,
	}
}

/// Get a displayable name for a Type
fn type_to_display_name(ty: &ast::types::Type) -> String {
	use nymph_compiler::ast::types::Type;

	match ty {
		Type::Int => "int".to_string(),
		Type::UInt => "uint".to_string(),
		Type::Float => "float".to_string(),
		Type::Char => "char".to_string(),
		Type::String => "string".to_string(),
		Type::Boolean => "boolean".to_string(),
		Type::Void => "void".to_string(),
		Type::Never => "never".to_string(),
		Type::Self_ => "self".to_string(),
		Type::Infer => "_".to_string(),
		Type::Reference { name, generics } => {
			if generics.is_empty() {
				name.0.to_string()
			} else {
				format!("{}<...>", name.0)
			}
		}
		Type::List(el) => format!("#[{}]", type_to_display_name(&el.0)),
		Type::Tuple(elements) => format!(
			"#({})",
			elements
				.iter()
				.map(|e| type_to_display_name(&e.0))
				.collect::<Vec<_>>()
				.join(", ")
		),
		Type::Map(key, val) => format!(
			"#{}: {}",
			type_to_display_name(&key.0),
			type_to_display_name(&val.0)
		),
		Type::Function {
			params,
			return_type,
		} => {
			let params = params
				.iter()
				.map(|(name, ty)| {
					if let Some(name) = name {
						format!("{}: {}", name.0, type_to_display_name(&ty.0))
					} else {
						type_to_display_name(&ty.0)
					}
				})
				.collect::<Vec<_>>()
				.join(", ");
			format!("({params}) -> {}", type_to_display_name(&return_type.0))
		}
		Type::Intersection(lhs, rhs) => format!(
			"{} + {}",
			type_to_display_name(&lhs.0),
			type_to_display_name(&rhs.0)
		),
		Type::Grouped(inner) => format!("({})", type_to_display_name(&inner.0)),
	}
}

/// Extract children from StructInnerMember list
fn extract_struct_inner_members(
	members: &[ast::Spanned<ast::declaration::StructInnerMember>],
	doc: &document::Document,
) -> Vec<DocumentSymbol> {
	use nymph_compiler::ast::declaration::StructInnerMember;

	let mut children = Vec::new();

	for member in members {
		match &member.0 {
			StructInnerMember::Member(impl_member) => {
				if let Some(sym) = impl_member_to_symbol(&impl_member.0, doc) {
					children.push(sym);
				}
			}
			StructInnerMember::Namespace(impl_members) => {
				let ns_children = extract_impl_members_from_spanned(impl_members, doc);
				let selection_span = impl_members.first().map_or(member.1, |first| first.1);
				if let Some(sym) = make_document_symbol(
					"namespace",
					SymbolKind::NAMESPACE,
					member.1,
					selection_span,
					doc,
					ns_children,
				) {
					children.push(sym);
				}
			}
			StructInnerMember::Impl {
				interface,
				members: impl_members,
				..
			} => {
				let interface_name = &interface.0.0;
				let impl_children = extract_impl_members_from_spanned(impl_members, doc);
				if let Some(sym) = make_document_symbol(
					&format!("impl {interface_name}"),
					SymbolKind::OBJECT,
					member.1,
					interface.0.1,
					doc,
					impl_children,
				) {
					children.push(sym);
				}
			}
			StructInnerMember::ImplMut(impl_members) => {
				let impl_children = extract_impl_members_from_spanned(impl_members, doc);
				let selection_span = impl_members.first().map_or(member.1, |first| first.1);
				if let Some(sym) = make_document_symbol(
					"impl mut",
					SymbolKind::OBJECT,
					member.1,
					selection_span,
					doc,
					impl_children,
				) {
					children.push(sym);
				}
			}
		}
	}

	children
}

/// Extract children from ImplMember list (non-spanned)
fn extract_impl_members(
	members: &[ast::Spanned<ast::declaration::ImplMember>],
	doc: &document::Document,
) -> Vec<DocumentSymbol> {
	let mut children = Vec::new();
	for member in members {
		if let Some(sym) = impl_member_to_symbol(&member.0, doc) {
			children.push(sym);
		}
	}
	children
}

/// Extract children from spanned ImplMember list
fn extract_impl_members_from_spanned(
	members: &[ast::Spanned<ast::declaration::ImplMember>],
	doc: &document::Document,
) -> Vec<DocumentSymbol> {
	extract_impl_members(members, doc)
}

/// Convert an ImplMember to a DocumentSymbol
#[allow(deprecated)]
fn impl_member_to_symbol(
	member: &ast::declaration::ImplMember,
	doc: &document::Document,
) -> Option<DocumentSymbol> {
	use nymph_compiler::ast::declaration::ImplMember;

	match member {
		ImplMember::Let { meta, value, .. } => {
			if let Some(name_ident) = meta.name.0.as_binding() {
				let name = &name_ident.0.clone();
				let declaration_span = let_declaration_span(meta, Some(value.1));
				make_document_symbol(
					name,
					SymbolKind::VARIABLE,
					declaration_span,
					name_ident.1,
					doc,
					vec![],
				)
			} else {
				None
			}
		}
		ImplMember::ExternalLet(_, _, meta) => {
			if let Some(name_ident) = meta.name.0.as_binding() {
				let name = &name_ident.0.clone();
				let declaration_span = let_declaration_span(meta, None);
				make_document_symbol(
					name,
					SymbolKind::VARIABLE,
					declaration_span,
					name_ident.1,
					doc,
					vec![],
				)
			} else {
				None
			}
		}
		ImplMember::Func { meta, body, .. } => {
			let name = &meta.name.0.clone();
			let declaration_span = func_declaration_span(meta, Some(body.1));
			make_document_symbol(
				name,
				SymbolKind::METHOD,
				declaration_span,
				meta.name.1,
				doc,
				vec![],
			)
		}
		ImplMember::ExternalFunc(_, _, meta) => {
			let name = &meta.name.0.clone();
			let declaration_span = func_declaration_span(meta, None);
			make_document_symbol(
				name,
				SymbolKind::METHOD,
				declaration_span,
				meta.name.1,
				doc,
				vec![],
			)
		}
	}
}

/// Extract children from InterfaceMember list
fn extract_interface_members(
	members: &[ast::Spanned<ast::declaration::InterfaceMember>],
	doc: &document::Document,
) -> Vec<DocumentSymbol> {
	use nymph_compiler::ast::declaration::InterfaceMember;

	let mut children = Vec::new();

	for member in members {
		match &member.0 {
			InterfaceMember::Element(elem) => {
				if let Some(sym) = interface_element_to_symbol(&elem.0, doc) {
					children.push(sym);
				}
			}
			InterfaceMember::Namespace(impl_members) => {
				let namespace_children = extract_impl_members(impl_members, doc);
				let selection_span = impl_members.first().map_or(member.1, |first| first.1);
				if let Some(symbol) = make_document_symbol(
					"namespace",
					SymbolKind::NAMESPACE,
					member.1,
					selection_span,
					doc,
					namespace_children,
				) {
					children.push(symbol);
				}
			}
			InterfaceMember::ImplMut(elements) => {
				let impl_children = extract_interface_elements(elements, doc);
				let selection_span = elements.first().map_or(member.1, |first| first.1);
				if let Some(symbol) = make_document_symbol(
					"impl mut",
					SymbolKind::OBJECT,
					member.1,
					selection_span,
					doc,
					impl_children,
				) {
					children.push(symbol);
				}
			}
			InterfaceMember::Impl {
				interface,
				members: impl_members,
				..
			} => {
				let impl_children = extract_impl_members_from_spanned(impl_members, doc);
				if let Some(symbol) = make_document_symbol(
					&format!("impl {}", interface.0.0),
					SymbolKind::OBJECT,
					member.1,
					interface.0.1,
					doc,
					impl_children,
				) {
					children.push(symbol);
				}
			}
		}
	}

	children
}

fn extract_interface_elements(
	elements: &[ast::Spanned<ast::declaration::InterfaceElement>],
	doc: &document::Document,
) -> Vec<DocumentSymbol> {
	let mut children = Vec::new();
	for element in elements {
		if let Some(symbol) = interface_element_to_symbol(&element.0, doc) {
			children.push(symbol);
		}
	}
	children
}

/// Convert an InterfaceElement to a DocumentSymbol
// #[allow(deprecated)]
fn interface_element_to_symbol(
	elem: &ast::declaration::InterfaceElement,
	doc: &document::Document,
) -> Option<DocumentSymbol> {
	use nymph_compiler::ast::declaration::InterfaceElement;

	match elem {
		InterfaceElement::Let { meta, value } => {
			if let Some(name_ident) = meta.name.0.as_binding() {
				let name = &name_ident.0.clone();
				let declaration_span = let_declaration_span(meta, value.as_ref().map(|expr| expr.1));
				make_document_symbol(
					name,
					SymbolKind::VARIABLE,
					declaration_span,
					name_ident.1,
					doc,
					vec![],
				)
			} else {
				None
			}
		}
		InterfaceElement::Func { meta, body } => {
			let name = &meta.name.0.clone();
			let declaration_span = func_declaration_span(meta, body.as_ref().map(|expr| expr.1));
			make_document_symbol(
				name,
				SymbolKind::METHOD,
				declaration_span,
				meta.name.1,
				doc,
				vec![],
			)
		}
	}
}

fn merge_spans(left: ast::Span, right: ast::Span) -> ast::Span {
	ast::Span::new(left.start.min(right.start), left.end.max(right.end))
}

fn extend_with_spanned_slice<T>(base: ast::Span, items: &[ast::Spanned<T>]) -> ast::Span {
	items
		.iter()
		.fold(base, |span, item| merge_spans(span, item.1))
}

fn let_declaration_span(
	meta: &ast::declaration::LetDeclaration,
	value_span: Option<ast::Span>,
) -> ast::Span {
	let mut span = meta.name.1;
	if let Some(type_) = &meta.type_ {
		span = merge_spans(span, type_.1);
	}
	if let Some(value_span) = value_span {
		span = merge_spans(span, value_span);
	}
	span
}

fn func_declaration_span(
	meta: &ast::declaration::FuncDeclaration,
	body_span: Option<ast::Span>,
) -> ast::Span {
	let mut span = meta.name.1;
	span = extend_with_spanned_slice(span, &meta.generics);
	span = extend_with_spanned_slice(span, &meta.params);
	if let Some(return_type) = &meta.return_type {
		span = merge_spans(span, return_type.1);
	}
	if let Some(body_span) = body_span {
		span = merge_spans(span, body_span);
	}
	span
}

fn impl_block_span(
	base_span: ast::Span,
	generics: &[ast::Spanned<ast::types::GenericParam>],
	members: &[ast::Spanned<ast::declaration::ImplMember>],
) -> ast::Span {
	let mut span = extend_with_spanned_slice(base_span, generics);
	span = extend_with_spanned_slice(span, members);
	span
}

/// Helper to create DocumentSymbol from symbol data
#[allow(deprecated)]
fn make_document_symbol(
	name: &str,
	kind: SymbolKind,
	range_span: ast::Span,
	selection_span: ast::Span,
	doc: &document::Document,
	children: Vec<DocumentSymbol>,
) -> Option<DocumentSymbol> {
	let range = doc.span_to_lsp_range(range_span)?;
	let selection_range = doc.span_to_lsp_range(selection_span)?;

	Some(DocumentSymbol {
		name: name.to_string(),
		detail: None,
		kind,
		tags: None,
		deprecated: None,
		range,
		selection_range,
		children: if children.is_empty() {
			None
		} else {
			Some(children)
		},
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::document::Document;
	use nymph_compiler::ast::declaration::Declaration;

	fn parse_document(source: &str) -> Document {
		Document::new("file:///test.nym".to_string(), source.to_string())
	}

	fn position_le(left: Position, right: Position) -> bool {
		(left.line, left.character) <= (right.line, right.character)
	}

	#[test]
	fn test_document_symbol_function_uses_declaration_and_name_spans() {
		let doc = parse_document(
			r#"
func add(value: int) -> {
	value
}
"#,
		);
		let module = doc.ast.as_ref().expect("expected AST");
		let Declaration::Func { meta, body, .. } = &module.0.members[0] else {
			panic!("expected function declaration")
		};

		let symbols = extract_ast_symbols_nested(&module.0, &doc);
		let symbol = symbols
			.iter()
			.find(|symbol| symbol.name == "add")
			.expect("expected function symbol");

		let expected_range = doc
			.span_to_lsp_range(func_declaration_span(meta, Some(body.1)))
			.expect("expected declaration range");
		let expected_selection = doc
			.span_to_lsp_range(meta.name.1)
			.expect("expected name range");

		assert_eq!(symbol.range, expected_range);
		assert_eq!(symbol.selection_range, expected_selection);
		assert!(position_le(
			symbol.range.start,
			symbol.selection_range.start
		));
		assert!(position_le(symbol.selection_range.end, symbol.range.end));
	}

	#[test]
	fn test_document_symbol_field_selection_is_name_only() {
		let doc = parse_document("struct Point(x: int, y: int) {}\n");
		let module = doc.ast.as_ref().expect("expected AST");
		let symbols = extract_ast_symbols_nested(&module.0, &doc);
		let struct_symbol = symbols
			.iter()
			.find(|symbol| symbol.name == "Point")
			.expect("expected struct symbol");
		let fields = struct_symbol
			.children
			.as_ref()
			.expect("expected field symbols");
		let x_field = fields
			.iter()
			.find(|symbol| symbol.name == "x")
			.expect("expected x field symbol");

		assert!(position_le(
			x_field.range.start,
			x_field.selection_range.start
		));
		assert!(position_le(x_field.selection_range.end, x_field.range.end));
		assert_ne!(x_field.range, x_field.selection_range);
	}

	#[test]
	fn test_document_symbol_interface_namespace_is_container() {
		let doc = parse_document(
			r#"
interface HasValue {
	namespace {
		func value() -> 1
	}
}
"#,
		);
		let module = doc.ast.as_ref().expect("expected AST");
		let symbols = extract_ast_symbols_nested(&module.0, &doc);
		let interface_symbol = symbols
			.iter()
			.find(|symbol| symbol.name == "HasValue")
			.expect("expected interface symbol");
		let namespace_symbol = interface_symbol
			.children
			.as_ref()
			.expect("expected interface children")
			.iter()
			.find(|symbol| symbol.name == "namespace")
			.expect("expected namespace symbol");

		assert!(position_le(
			namespace_symbol.range.start,
			namespace_symbol.selection_range.start,
		));
		assert!(position_le(
			namespace_symbol.selection_range.end,
			namespace_symbol.range.end,
		));
		assert!(
			namespace_symbol
				.children
				.as_ref()
				.is_some_and(|children| children.iter().any(|child| child.name == "value"))
		);
	}
}
