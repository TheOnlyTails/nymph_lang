use crate::analyzer::SemanticAnalyzer;
use crate::document::{self, Document};
use crate::semantic_tokens::{SemanticToken, TokenType};
use crate::workspace::Workspace;
use nymph_compiler::ast;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{self, *};
use tower_lsp::{Client, LanguageServer};

pub struct NymphLanguageServer {
	client: Client,
	workspace: Arc<Workspace>,
	analyzer: Arc<Mutex<SemanticAnalyzer>>,
}

impl NymphLanguageServer {
	pub fn new(client: Client) -> Self {
		Self {
			client,
			workspace: Arc::new(Workspace::new()),
			analyzer: Arc::new(Mutex::new(SemanticAnalyzer::new())),
		}
	}

	fn position_in_source(source: &str, offset: usize) -> Position {
		let mut line = 0u32;
		let mut col = 0u32;
		for (i, ch) in source.char_indices() {
			if i >= offset {
				break;
			}
			if ch == '\n' {
				line += 1;
				col = 0;
			} else {
				col += 1;
			}
		}
		Position {
			line,
			character: col,
		}
	}

	/// Publish diagnostics for a document
	async fn publish_diagnostics(&self, uri: &str, doc: &Document) {
		let mut diagnostics_by_uri: std::collections::BTreeMap<String, Vec<Diagnostic>> =
			std::collections::BTreeMap::new();

		// Add parse errors to the current document
		for error_msg in &doc.parse_errors {
			diagnostics_by_uri
				.entry(uri.to_string())
				.or_default()
				.push(Diagnostic {
					range: Range {
						start: Position {
							line: 0,
							character: 0,
						},
						end: Position {
							line: 0,
							character: 1,
						},
					},
					severity: Some(DiagnosticSeverity::ERROR),
					source: Some("nymph".to_string()),
					message: error_msg.clone(),
					..Default::default()
				});
		}

		// Add type errors
		for error in &doc.type_errors {
			if let Some(module_path) = error.file_path() {
				let source =
					std::fs::read_to_string(module_path.as_str()).unwrap_or_default();
				let span = error.span();
				let start = Self::position_in_source(&source, span.start);
				let end = Self::position_in_source(&source, span.end);

				let module_uri =
					Url::from_file_path(module_path.as_str())
						.map(|u| u.to_string())
						.unwrap_or_else(|_| uri.to_string());

				diagnostics_by_uri
					.entry(module_uri)
					.or_default()
					.push(Diagnostic {
						range: Range { start, end },
						severity: Some(DiagnosticSeverity::ERROR),
						source: Some("nymph-typecheck".to_string()),
						message: error.to_string(),
						..Default::default()
					});
			} else {
				let span = error.span();
				let (start_line, start_char) = doc.position_to_line_col(span.start);
				let (end_line, end_char) = doc.position_to_line_col(span.end);

				diagnostics_by_uri
					.entry(uri.to_string())
					.or_default()
					.push(Diagnostic {
						range: Range {
							start: Position {
								line: (start_line.saturating_sub(1)) as u32,
								character: (start_char.saturating_sub(1)) as u32,
							},
							end: Position {
								line: (end_line.saturating_sub(1)) as u32,
								character: (end_char.saturating_sub(1)) as u32,
							},
						},
						severity: Some(DiagnosticSeverity::ERROR),
						source: Some("nymph-typecheck".to_string()),
						message: error.to_string(),
						..Default::default()
					});
			}
		}

		// Publish diagnostics for each file
		for (diag_uri, diagnostics) in diagnostics_by_uri {
			if let Ok(parsed_uri) = diag_uri.parse::<Url>() {
				self
					.client
					.publish_diagnostics(parsed_uri, diagnostics, None)
					.await;
			}
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
			completion_provider: Some(CompletionOptions::default()),
			definition_provider: Some(OneOf::Left(true)),
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
					if let Some(doc) = self.workspace.get_document(&uri, Clone::clone).await {
						let mut lines: Vec<String> = doc.content.lines().map(ToString::to_string).collect();
						let start_line = range.start.line as usize;
						let start_char = range.start.character as usize;

						if start_line < lines.len() {
							let line = &mut lines[start_line];
							line.replace_range(start_char..start_char.min(line.len()), &text);
						}

						let new_content = lines.join("\n");
						self
							.workspace
							.update_document(uri.clone(), new_content)
							.await;
					}
				}
			}
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
			.log_message(MessageType::INFO, format!("Closed document: {uri}"))
			.await;
	}

	async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
		let uri = params
			.text_document_position_params
			.text_document
			.uri
			.to_string();
		let line = params.text_document_position_params.position.line as usize + 1;
		let character = params.text_document_position_params.position.character as usize;

		let analyzer = self.analyzer.lock().await;

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				if !doc.parse_errors.is_empty() {
					return Some(Hover {
						contents: HoverContents::Scalar(MarkedString::String(format!(
							"**Parse Error**: {}",
							doc.parse_errors.join(", ")
						))),
						range: None,
					});
				}

				if let Some(symbol) = analyzer.get_symbol_at_position(line, character, doc) {
					let type_info = symbol.type_info.unwrap_or_else(|| symbol.name.clone());
					let contents = format!("```nymph\n{type_info}\n```");
					let range = Range {
						start: Position {
							line: (symbol.range.start_line - 1) as u32,
							character: (symbol.range.start_char - 1) as u32,
						},
						end: Position {
							line: (symbol.range.end_line - 1) as u32,
							character: (symbol.range.end_char - 1) as u32,
						},
					};
					Some(Hover {
						contents: HoverContents::Markup(MarkupContent {
							kind: MarkupKind::Markdown,
							value: contents,
						}),
						range: Some(range),
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
		let line = params.text_document_position_params.position.line as usize + 1;
		let character = params.text_document_position_params.position.character as usize;

		let analyzer = self.analyzer.lock().await;

		let result = self
			.workspace
			.get_document(&uri, |doc| {
				if !doc.parse_errors.is_empty() {
					return None;
				}

				if let Some(symbol) = analyzer.get_symbol_at_position(line, character, doc) {
					if let Some(def_path) = symbol.definition_path {
						// Convert file path to URI
						let def_uri = Url::from_file_path(&def_path).ok()?;
						Some(GotoDefinitionResponse::Scalar(Location {
							uri: def_uri,
							range: Range {
								start: Position {
									line: 0,
									character: 0,
								},
								end: Position {
									line: 0,
									character: 0,
								},
							},
						}))
					} else {
						None
					}
				} else {
					None
				}
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
					let symbols = extract_ast_symbols_nested(spanned_module.inner(), doc);
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
					let tokens = self.encode_semantic_tokens(&[]);
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
					let tokens = self.encode_semantic_tokens(&[]);
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
		let keywords = vec![
			"let",
			"func",
			"if",
			"else",
			"struct",
			"interface",
			"return",
			"true",
			"false",
		];

		let items: Vec<CompletionItem> = keywords
			.iter()
			.map(|kw| CompletionItem {
				label: kw.to_string(),
				kind: Some(CompletionItemKind::KEYWORD),
				..Default::default()
			})
			.collect();

		Ok(Some(CompletionResponse::Array(items)))
	}
}

impl NymphLanguageServer {
	fn encode_semantic_tokens(&self, tokens: &[SemanticToken]) -> SemanticTokensResult {
		let mut data: Vec<u32> = Vec::new();
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
				.enumerate()
				.map(|(idx, _)| 1u32 << idx)
				.fold(0u32, |acc, x| acc | x);

			// LSP semantic tokens format: [line_delta, start_char_delta, length, token_type, token_modifiers]
			data.push(line_delta);
			data.push(start_char_delta);
			data.push(token.length as u32);
			data.push(token_type_index);
			data.push(modifier_mask);

			prev_line = token.line;
			prev_start_char = token.start_char;
		}

		let semantic_tokens: Vec<lsp_types::SemanticToken> = Vec::new();

		SemanticTokensResult::Tokens(SemanticTokens {
			result_id: None,
			data: semantic_tokens,
		})
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
		Declaration::Let { meta, .. } => {
			if let Some(name_ident) = meta.name.inner().as_binding() {
				let name = name_ident.inner();
				make_document_symbol(
					name,
					SymbolKind::VARIABLE,
					name_ident.start(),
					name_ident.end(),
					doc,
					vec![],
				)
			} else {
				None
			}
		}
		Declaration::ExternalLet(_, meta) => {
			if let Some(name_ident) = meta.name.inner().as_binding() {
				let name = name_ident.inner();
				make_document_symbol(
					name,
					SymbolKind::VARIABLE,
					name_ident.start(),
					name_ident.end(),
					doc,
					vec![],
				)
			} else {
				None
			}
		}
		Declaration::Func { meta, .. } => {
			let name = meta.name.inner();
			make_document_symbol(
				name,
				SymbolKind::FUNCTION,
				meta.name.start(),
				meta.name.end(),
				doc,
				vec![],
			)
		}
		Declaration::ExternalFunc(_, meta) => {
			let name = meta.name.inner();
			make_document_symbol(
				name,
				SymbolKind::FUNCTION,
				meta.name.start(),
				meta.name.end(),
				doc,
				vec![],
			)
		}
		Declaration::TypeAlias { meta, .. } => {
			let name = meta.name.inner();
			make_document_symbol(
				name,
				SymbolKind::TYPE_PARAMETER,
				meta.name.start(),
				meta.name.end(),
				doc,
				vec![],
			)
		}
		Declaration::Struct {
			name,
			members,
			fields,
			..
		} => {
			let name_str = name.inner();
			let mut children = Vec::new();

			// Add fields
			for field in fields {
				let field_inner = field.inner();
				let field_name = field_inner.name.inner();
				if let Some(sym) = make_document_symbol(
					field_name,
					SymbolKind::FIELD,
					field_inner.name.start(),
					field_inner.name.end(),
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
				name.start(),
				name.end(),
				doc,
				children,
			)
		}
		Declaration::Enum {
			name,
			variants,
			members,
			..
		} => {
			let name_str = name.inner();
			let mut children = Vec::new();

			// Add enum variants
			for variant in variants {
				let variant_inner = variant.inner();
				let variant_name = variant_inner.name.inner();
				if let Some(sym) = make_document_symbol(
					variant_name,
					SymbolKind::ENUM_MEMBER,
					variant_inner.name.start(),
					variant_inner.name.end(),
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
				name.start(),
				name.end(),
				doc,
				children,
			)
		}
		Declaration::Namespace { name, members, .. } => {
			let name_str = name.inner();
			let children = extract_impl_members(members, doc);
			make_document_symbol(
				name_str,
				SymbolKind::NAMESPACE,
				name.start(),
				name.end(),
				doc,
				children,
			)
		}
		Declaration::Interface { name, members, .. } => {
			let name_str = name.inner();
			let children = extract_interface_members(members, doc);
			make_document_symbol(
				name_str,
				SymbolKind::INTERFACE,
				name.start(),
				name.end(),
				doc,
				children,
			)
		}
		Declaration::Impl {
			type_,
			members,
			mutable,
			..
		} => {
			let type_name = type_to_display_name(type_.inner());
			let prefix = if *mutable { "impl mut " } else { "impl " };
			make_document_symbol(
				&format!("{prefix}{type_name}"),
				SymbolKind::OBJECT,
				type_.start(),
				type_.end(),
				doc,
				extract_impl_members(members, doc),
			)
		}
		Declaration::ImplFor {
			type_,
			for_interface,
			members,
			mutable,
			..
		} => {
			let type_name = type_to_display_name(type_.inner());
			let interface_name = for_interface.0.inner();
			let prefix = if *mutable { "impl mut " } else { "impl " };
			let children = extract_impl_members(members, doc);
			make_document_symbol(
				&format!("{prefix}{interface_name} for {type_name}"),
				SymbolKind::OBJECT,
				type_.start(),
				type_.end(),
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
				name.inner().to_string()
			} else {
				format!("{}<...>", name.inner())
			}
		}
		Type::List(el) => format!("#[{}]", type_to_display_name(el.inner())),
		Type::Tuple(elements) => format!(
			"#({})",
			elements
				.iter()
				.map(|e| type_to_display_name(e.inner()))
				.collect::<Vec<_>>()
				.join(", ")
		),
		Type::Map(key, val) => format!(
			"#{}: {}",
			type_to_display_name(key.inner()),
			type_to_display_name(val.inner())
		),
		Type::Function {
			params,
			return_type,
		} => {
			let params = params
				.iter()
				.map(|(name, ty)| {
					if let Some(name) = name {
						format!("{}: {}", name.inner(), type_to_display_name(ty.inner()))
					} else {
						type_to_display_name(ty.inner())
					}
				})
				.collect::<Vec<_>>()
				.join(", ");
			format!(
				"({params}) -> {}",
				type_to_display_name(return_type.inner())
			)
		}
		Type::Intersection(lhs, rhs) => format!(
			"{} + {}",
			type_to_display_name(lhs.inner()),
			type_to_display_name(rhs.inner())
		),
		Type::Grouped(inner) => format!("({})", type_to_display_name(inner.inner())),
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
		match member.inner() {
			StructInnerMember::Member(impl_member) => {
				if let Some(sym) = impl_member_to_symbol(impl_member.inner(), doc) {
					children.push(sym);
				}
			}
			StructInnerMember::Namespace(impl_members) => {
				let ns_children = extract_impl_members_from_spanned(impl_members, doc);
				if let Some(first) = impl_members.first()
					&& let Some(sym) = make_document_symbol(
						"namespace",
						SymbolKind::NAMESPACE,
						first.start(),
						first.end(),
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
				let interface_name = interface.0.inner();
				let impl_children = extract_impl_members_from_spanned(impl_members, doc);
				if let Some(sym) = make_document_symbol(
					&format!("impl {interface_name}"),
					SymbolKind::OBJECT,
					interface.0.start(),
					interface.0.end(),
					doc,
					impl_children,
				) {
					children.push(sym);
				}
			}
			StructInnerMember::ImplMut(impl_members) => {
				let impl_children = extract_impl_members_from_spanned(impl_members, doc);
				if let Some(first) = impl_members.first()
					&& let Some(sym) = make_document_symbol(
						"impl mut",
						SymbolKind::OBJECT,
						first.start(),
						first.end(),
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
		if let Some(sym) = impl_member_to_symbol(member.inner(), doc) {
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
		ImplMember::Let { meta, .. } | ImplMember::ExternalLet(_, meta) => {
			if let Some(name_ident) = meta.name.inner().as_binding() {
				let name = name_ident.inner();
				make_document_symbol(
					name,
					SymbolKind::VARIABLE,
					name_ident.start(),
					name_ident.end(),
					doc,
					vec![],
				)
			} else {
				None
			}
		}
		ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, meta) => {
			let name = meta.name.inner();
			make_document_symbol(
				name,
				SymbolKind::METHOD,
				meta.name.start(),
				meta.name.end(),
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
		match member.inner() {
			InterfaceMember::Element(elem) => {
				if let Some(sym) = interface_element_to_symbol(elem.inner(), doc) {
					children.push(sym);
				}
			}
			InterfaceMember::Namespace(impl_members) => {
				children.extend(extract_impl_members(impl_members, doc));
			}
			InterfaceMember::ImplMut(elements) => {
				for elem in elements {
					if let Some(sym) = interface_element_to_symbol(elem.inner(), doc) {
						children.push(sym);
					}
				}
			}
			InterfaceMember::Impl {
				members: impl_members,
				..
			} => {
				children.extend(extract_impl_members_from_spanned(impl_members, doc));
			}
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
		InterfaceElement::Let { meta, .. } => {
			if let Some(name_ident) = meta.name.inner().as_binding() {
				let name = name_ident.inner();
				make_document_symbol(
					name,
					SymbolKind::VARIABLE,
					name_ident.start(),
					name_ident.end(),
					doc,
					vec![],
				)
			} else {
				None
			}
		}
		InterfaceElement::Func { meta, .. } => {
			let name = meta.name.inner();
			make_document_symbol(
				name,
				SymbolKind::METHOD,
				meta.name.start(),
				meta.name.end(),
				doc,
				vec![],
			)
		}
	}
}

/// Helper to create DocumentSymbol from symbol data
#[allow(deprecated)]
fn make_document_symbol(
	name: &str,
	kind: SymbolKind,
	start_offset: usize,
	end_offset: usize,
	doc: &document::Document,
	children: Vec<DocumentSymbol>,
) -> Option<DocumentSymbol> {
	let (start_line, start_col) = doc.position_to_line_col(start_offset);
	let (end_line, end_col) = doc.position_to_line_col(end_offset);

	Some(DocumentSymbol {
		name: name.to_string(),
		detail: None,
		kind,
		tags: None,
		deprecated: None,
		range: Range {
			start: Position {
				line: (start_line - 1) as u32,
				character: (start_col - 1) as u32,
			},
			end: Position {
				line: (end_line - 1) as u32,
				character: (end_col - 1) as u32,
			},
		},
		selection_range: Range {
			start: Position {
				line: (start_line - 1) as u32,
				character: (start_col - 1) as u32,
			},
			end: Position {
				line: (end_line - 1) as u32,
				character: (end_col - 1) as u32,
			},
		},
		children: if children.is_empty() {
			None
		} else {
			Some(children)
		},
	})
}
