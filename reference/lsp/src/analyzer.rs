use ecow::EcoString;

use crate::document::Document;
use nymph_compiler::ast::Span;
use nymph_compiler::ast::declaration::Visibility;
use nymph_compiler::ast::declaration::{
	Declaration, EnumVariant, FuncDeclaration, ImplMember, ImportRoot, InterfaceElement,
	InterfaceMember, LetDeclaration, Module, StructField, StructInnerMember,
};
use nymph_compiler::ast::expr::{
	ClosureParam, Expr, ListPatternEntry, MapPatternEntry, MatchArm, Statement, StructPatternField,
};
use nymph_compiler::ast::ops::{BinaryOperator, PrefixOperator};
use nymph_compiler::ast::types::{GenericArg, GenericParam, Type};
use nymph_compiler::ast::{Ident, Spanned};
use nymph_compiler::types::{Context, ContextEntry, ContextValue, Type as CheckedType};

/// Information about a symbol at a specific location
#[derive(Debug, Clone)]
pub struct SymbolAtLocation {
	pub name: String,
	pub kind: SymbolKind,
	pub type_info: Option<String>,
	pub range: LocationRange,
	pub definition_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionTarget {
	pub uri: String,
	pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SymbolKind {
	Function,
	Variable,
	Type,
	Interface,
	Parameter,
	Field,
	Enum,
	Namespace,
	Struct,
	Module,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationRange {
	pub start_line: usize,
	pub start_char: usize,
	pub end_line: usize,
	pub end_char: usize,
}

/// Represents a symbol found in the AST
#[derive(Debug, Clone)]
pub struct Symbol {
	pub name: String,
	pub kind: SymbolKind,
	pub start_offset: usize,
	pub end_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionSuggestion {
	pub label: String,
	pub kind: SymbolKind,
	pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceOccurrence {
	pub uri: String,
	pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelpInfo {
	pub label: String,
	pub parameters: Vec<String>,
	pub active_parameter: usize,
}

/// Analyzer for getting type information and symbol details
pub struct SemanticAnalyzer;

impl SemanticAnalyzer {
	pub fn new() -> Self {
		Self
	}

	/// Analyze a document and build the symbol table
	pub fn analyze(&self, _document: &Document) {}

	/// Extract all top-level symbols from a module
	pub fn extract_symbols(&self, module: &Module) -> Vec<Symbol> {
		extract_module_symbols(module)
	}

	/// Get type information for a symbol at a specific position
	pub fn get_type_at_position(
		&self,
		line: usize,
		character: usize,
		document: &Document,
	) -> Option<String> {
		self
			.get_symbol_at_position(line, character, document)
			.and_then(|s| s.type_info)
	}

	/// Get the symbol at a specific position (byte offset)
	pub fn get_symbol_at_position(
		&self,
		line: usize,
		character: usize,
		document: &Document,
	) -> Option<SymbolAtLocation> {
		let offset = document.position_to_offset(line, character)?;
		let ast = document.ast.as_ref()?;
		find_symbol_at_offset(&ast.0, offset, document, document.type_context.as_ref())
	}

	pub fn get_symbol_at_lsp_position(
		&self,
		line: u32,
		character: u32,
		document: &Document,
	) -> Option<SymbolAtLocation> {
		let offset = document.lsp_position_to_offset(line, character)?;
		let ast = document.ast.as_ref()?;
		find_symbol_at_offset(&ast.0, offset, document, document.type_context.as_ref())
	}

	pub fn get_definition_at_position(
		&self,
		line: u32,
		character: u32,
		document: &Document,
	) -> Option<DefinitionTarget> {
		let offset = document.lsp_position_to_offset(line, character)?;
		let ast = document.ast.as_ref()?;
		let top_level = collect_top_level_definitions(&ast.0, document);
		resolve_definition_at_offset(&ast.0, offset, document, &top_level)
	}

	pub fn get_completion_suggestions(
		&self,
		line: u32,
		character: u32,
		document: &Document,
	) -> Vec<CompletionSuggestion> {
		let Some(offset) = document.lsp_position_to_offset(line, character) else {
			return default_keyword_suggestions();
		};
		let Some(ast) = document.ast.as_ref() else {
			return default_keyword_suggestions();
		};

		if let Some(member_suggestions) = get_member_completion_suggestions_at_offset(
			&ast.0,
			offset,
			document,
			document.type_context.as_ref(),
		) && !member_suggestions.is_empty()
		{
			return member_suggestions;
		}

		let mut suggestions = default_keyword_suggestions();
		suggestions.extend(collect_top_level_completion_suggestions(&ast.0, document));
		suggestions.extend(collect_local_completion_suggestions(
			&ast.0, offset, document,
		));
		suggestions.sort_by(|a, b| a.label.cmp(&b.label).then(a.kind.cmp(&b.kind)));
		suggestions.dedup_by(|a, b| a.label == b.label && a.kind == b.kind);
		suggestions
	}

	pub fn find_references(
		&self,
		line: u32,
		character: u32,
		document: &Document,
		search_documents: &[Document],
	) -> Option<(SymbolAtLocation, DefinitionTarget, Vec<ReferenceOccurrence>)> {
		let symbol = self.get_symbol_at_lsp_position(line, character, document)?;
		let target = self.get_definition_at_position(line, character, document)?;
		let mut references = Vec::new();

		for search_document in search_documents {
			references.extend(find_references_in_document(
				search_document,
				&symbol.name,
				&target,
			));
		}

		references.sort_by(|a, b| a.uri.cmp(&b.uri).then(a.span.start.cmp(&b.span.start)));
		references.dedup_by(|a, b| a.uri == b.uri && a.span == b.span);
		Some((symbol, target, references))
	}

	pub fn get_signature_help(
		&self,
		line: u32,
		character: u32,
		document: &Document,
	) -> Option<SignatureHelpInfo> {
		let offset = document.lsp_position_to_offset(line, character)?;
		let ast = document.ast.as_ref()?;
		let call_site =
			find_call_site_at_offset(&ast.0, offset, document, document.type_context.as_ref())?;
		let function_type = infer_checked_type(&call_site.func.0, document, Some(&call_site.ctx))
			.or_else(|| call_site_identifier_type(&call_site.func.0, &call_site.ctx))?;

		match function_type {
			CheckedType::Function {
				params,
				return_type,
				..
			} => {
				let parameters = params
					.iter()
					.map(|(name, type_)| match name {
						Some(name) => format!("{name}: {type_}"),
						None => type_.to_string(),
					})
					.collect::<Vec<_>>();
				Some(SignatureHelpInfo {
					label: format!("({}) -> {}", parameters.join(", "), return_type),
					parameters,
					active_parameter: call_site
						.active_parameter
						.min(params.len().saturating_sub(1)),
				})
			}
			_ => None,
		}
	}
}

impl Default for SemanticAnalyzer {
	fn default() -> Self {
		Self::new()
	}
}

/// Extract symbols from a module's declarations
fn extract_module_symbols(module: &Module) -> Vec<Symbol> {
	let mut symbols = Vec::new();

	for decl in &module.members {
		extract_declaration_symbols(decl, &mut symbols);
	}

	symbols
}

/// Extract symbols from a single declaration
fn extract_declaration_symbols(decl: &Declaration, symbols: &mut Vec<Symbol>) {
	match decl {
		Declaration::Let { meta, .. } => {
			// Extract variable name from pattern
			if let Some(name_ident) = meta.name.0.as_binding() {
				let name = &name_ident.0.clone();
				symbols.push(Symbol {
					name: name.to_string(),
					kind: SymbolKind::Variable,
					start_offset: name_ident.1.start,
					end_offset: name_ident.1.end,
				});
			}
		}
		Declaration::ExternalLet(_, _, meta) => {
			if let Some(name_ident) = meta.name.0.as_binding() {
				let name = &name_ident.0.clone();
				symbols.push(Symbol {
					name: name.to_string(),
					kind: SymbolKind::Variable,
					start_offset: name_ident.1.start,
					end_offset: name_ident.1.end,
				});
			}
		}
		Declaration::Func { meta, .. } => {
			symbols.push(Symbol {
				name: meta.name.0.to_string(),
				kind: SymbolKind::Function,
				start_offset: meta.name.1.start,
				end_offset: meta.name.1.end,
			});
		}
		Declaration::ExternalFunc(_, _, meta) => {
			symbols.push(Symbol {
				name: meta.name.0.to_string(),
				kind: SymbolKind::Function,
				start_offset: meta.name.1.start,
				end_offset: meta.name.1.end,
			});
		}
		Declaration::TypeAlias { meta, .. } => {
			symbols.push(Symbol {
				name: meta.name.0.to_string(),
				kind: SymbolKind::Type,
				start_offset: meta.name.1.start,
				end_offset: meta.name.1.end,
			});
		}
		Declaration::Struct { name, .. } => {
			symbols.push(Symbol {
				name: name.0.to_string(),
				kind: SymbolKind::Type,
				start_offset: name.1.start,
				end_offset: name.1.end,
			});
		}
		Declaration::Enum { name, .. } => {
			symbols.push(Symbol {
				name: name.0.to_string(),
				kind: SymbolKind::Enum,
				start_offset: name.1.start,
				end_offset: name.1.end,
			});
		}
		Declaration::Namespace { name, .. } => {
			symbols.push(Symbol {
				name: name.0.to_string(),
				kind: SymbolKind::Namespace,
				start_offset: name.1.start,
				end_offset: name.1.end,
			});
		}
		Declaration::Interface { name, .. } => {
			symbols.push(Symbol {
				name: name.0.to_string(),
				kind: SymbolKind::Interface,
				start_offset: name.1.start,
				end_offset: name.1.end,
			});
		}
		Declaration::Impl { type_: _, .. } => {
			// Impl blocks don't have direct names but we can extract the type being implemented
			// For now, skip them as they're handled by their implementations
		}
		Declaration::ImplFor { .. } => {
			// ImplFor blocks also don't have direct names
		}
		Declaration::Import { .. } => {
			// Import declarations are not typically shown as symbols in the outline
		}
	}
}

/// Find a symbol at a specific byte offset in the module
fn find_symbol_at_offset(
	module: &Module,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	for decl in &module.members {
		if let Some(sym) = find_symbol_in_declaration(decl, offset, doc, ctx) {
			return Some(sym);
		}
	}
	None
}

/// Check if offset falls within a span
fn offset_in_span(offset: usize, start: usize, end: usize) -> bool {
	offset >= start && offset < end
}

/// Look up a variable's type from the type context
fn lookup_type_in_context(name: &str, ctx: Option<&Context>) -> Option<String> {
	let ctx = ctx?;
	let name = EcoString::from(name);
	ctx.lookup_type(&name).map(|ty| format!("{}", ty))
}

fn lookup_checked_type_in_context(name: &str, ctx: Option<&Context>) -> Option<CheckedType> {
	let ctx = ctx?;
	let name = EcoString::from(name);
	ctx.lookup_type(&name)
}

fn infer_checked_type(expr: &Expr, doc: &Document, ctx: Option<&Context>) -> Option<CheckedType> {
	let mut checker = doc.type_checker.clone()?;
	let ctx = ctx?;
	checker
		.infer(&Spanned(expr.clone(), Span::new(0, 0)), ctx)
		.ok()
}

fn infer_hover_type(expr: &Expr, doc: &Document, ctx: Option<&Context>) -> Option<String> {
	infer_checked_type(expr, doc, ctx).map(|ty| ty.to_string())
}

fn pattern_binding_ident(pattern: &nymph_compiler::ast::expr::Pattern) -> Option<&Ident> {
	match pattern {
		nymph_compiler::ast::expr::Pattern::Binding { name, .. } => Some(name),
		nymph_compiler::ast::expr::Pattern::Struct { path, fields }
			if path.len() == 1 && fields.is_empty() =>
		{
			Some(&path[0])
		}
		_ => None,
	}
}

fn resolve_hover_ast_type(type_: &Type, ctx: Option<&Context>) -> Option<CheckedType> {
	match type_ {
		Type::Int => Some(CheckedType::Int),
		Type::UInt => Some(CheckedType::UInt),
		Type::Float => Some(CheckedType::Float),
		Type::Char => Some(CheckedType::Char),
		Type::String => Some(CheckedType::String),
		Type::Boolean => Some(CheckedType::Boolean),
		Type::Void => Some(CheckedType::Void),
		Type::Never => Some(CheckedType::Never),
		Type::Self_ => lookup_checked_type_in_context("self", ctx),
		Type::Infer => None,
		Type::List(item) => resolve_hover_ast_type(&item.0, ctx).map(|item| CheckedType::List {
			item: Box::new(item),
		}),
		Type::Tuple(items) => items
			.iter()
			.map(|item| resolve_hover_ast_type(&item.0, ctx))
			.collect::<Option<Vec<_>>>()
			.map(|items| CheckedType::Tuple { items }),
		Type::Map(key, value) => Some(CheckedType::Map {
			key: Box::new(resolve_hover_ast_type(&key.0, ctx)?),
			value: Box::new(resolve_hover_ast_type(&value.0, ctx)?),
		}),
		Type::Reference { name, .. } => lookup_checked_type_in_context(&name.0, ctx),
		Type::Grouped(inner) => resolve_hover_ast_type(&inner.0, ctx),
		Type::Intersection(a, b) => Some(CheckedType::Intersection {
			first: Box::new(resolve_hover_ast_type(&a.0, ctx)?),
			second: Box::new(resolve_hover_ast_type(&b.0, ctx)?),
		}),
		Type::Function {
			params,
			return_type,
		} => {
			let params = params
				.iter()
				.map(|(name, type_)| {
					Some((
						name.as_ref().map(|ident| ident.0.clone()),
						resolve_hover_ast_type(&type_.0, ctx)?,
					))
				})
				.collect::<Option<Vec<_>>>()?;
			Some(CheckedType::Function {
				generics: Default::default(),
				params,
				has_spread: false,
				return_type: Box::new(resolve_hover_ast_type(&return_type.0, ctx)?),
				constructor: false,
			})
		}
	}
}

fn function_body_context(meta: &FuncDeclaration, ctx: Option<&Context>) -> Context {
	let mut body_ctx = ctx.cloned().unwrap_or_default();

	for param_meta in &meta.params {
		let binding = match &param_meta.0.name.0 {
			nymph_compiler::ast::expr::Pattern::Binding { name, .. } => Some(name.0.clone()),
			nymph_compiler::ast::expr::Pattern::Struct { path, fields }
				if path.len() == 1 && fields.is_empty() =>
			{
				Some(path[0].0.clone())
			}
			_ => None,
		};

		if let Some(binding) = binding
			&& let Some(param_type) = resolve_hover_ast_type(&param_meta.0.type_.0, Some(&body_ctx))
		{
			body_ctx.insert_entry(
				binding,
				ContextEntry::Value(ContextValue {
					type_: param_type,
					mutable: param_meta.0.mutable,
					visibility: Visibility::Private,
				}),
			);
		}
	}

	body_ctx
}

fn function_param_type_info(meta: &FuncDeclaration, name: &str) -> Option<String> {
	meta.params.iter().find_map(|param| {
		let binding = pattern_binding_ident(&param.0.name.0)?;
		(binding.0 == name).then(|| format!("{}: {}", binding.0, type_to_string(&param.0.type_.0)))
	})
}

fn extend_context_with_let_binding(
	ctx: &mut Context,
	meta: &LetDeclaration,
	value: &Expr,
	doc: &Document,
) {
	let Some(binding) = pattern_binding_ident(&meta.name.0) else {
		return;
	};
	let Some(type_) = infer_checked_type(value, doc, Some(ctx)) else {
		return;
	};

	ctx.insert_entry(
		binding.0.clone(),
		ContextEntry::Value(ContextValue {
			type_,
			mutable: meta.mutable,
			visibility: Visibility::Private,
		}),
	);
}

/// Find a symbol in a declaration at the given offset
fn find_symbol_in_declaration(
	decl: &Declaration,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	match decl {
		Declaration::Let { meta, value, .. } => {
			find_symbol_in_let_with_value(meta, Some(&value.0), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(&value.0, offset, doc, ctx))
		}
		Declaration::ExternalLet(_, _, meta) => find_symbol_in_let(meta, offset, doc, ctx),
		Declaration::Func { meta, body, .. } => {
			find_symbol_in_func_with_body(meta, Some(&body.0), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(&body.0, offset, doc, ctx))
		}
		Declaration::ExternalFunc(_, _, meta) => find_symbol_in_func(meta, offset, doc, ctx),
		Declaration::TypeAlias { meta, value, .. } => {
			let name = &meta.name.0.clone();
			if offset_in_span(offset, meta.name.1.start, meta.name.1.end) {
				let generics_str = format_generics(&meta.generics);
				return Some(make_symbol_at_location(
					name,
					SymbolKind::Type,
					Some(format!(
						"type {}{} = {}",
						name,
						generics_str,
						type_to_string(&value.0)
					)),
					meta.name.1.start,
					meta.name.1.end,
					doc,
				));
			}
			// Check generics
			for generic in &meta.generics {
				if let Some(sym) = find_symbol_in_generic_param(&generic.0, offset, doc) {
					return Some(sym);
				}
			}
			None
		}
		Declaration::Struct {
			name,
			generics,
			fields,
			members,
			..
		} => {
			if offset_in_span(offset, name.1.start, name.1.end) {
				let generics_str = format_generics(generics);
				let fields_str = format_struct_fields(fields);
				return Some(make_symbol_at_location(
					&name.0,
					SymbolKind::Struct,
					Some(format!("struct {}{}{}", name.0, generics_str, fields_str)),
					name.1.start,
					name.1.end,
					doc,
				));
			}
			// Check generics
			for generic in generics {
				if let Some(sym) = find_symbol_in_generic_param(&generic.0, offset, doc) {
					return Some(sym);
				}
			}
			// Check fields
			for field in fields {
				if let Some(sym) = find_symbol_in_struct_field(&field.0, offset, doc) {
					return Some(sym);
				}
			}
			// Check inner members
			for member in members {
				if let Some(sym) = find_symbol_in_struct_inner_member(&member.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Declaration::Enum {
			name,
			generics,
			variants,
			members,
			..
		} => {
			if offset_in_span(offset, name.1.start, name.1.end) {
				let generics_str = format_generics(generics);
				let variants_str: Vec<_> = variants.iter().map(|v| v.0.name.0.to_string()).collect();
				return Some(make_symbol_at_location(
					&name.0,
					SymbolKind::Enum,
					Some(format!(
						"enum {}{} {{ {} }}",
						name.0,
						generics_str,
						variants_str.join(", ")
					)),
					name.1.start,
					name.1.end,
					doc,
				));
			}
			// Check generics
			for generic in generics {
				if let Some(sym) = find_symbol_in_generic_param(&generic.0, offset, doc) {
					return Some(sym);
				}
			}
			// Check variants and their fields
			for variant in variants {
				if let Some(sym) = find_symbol_in_enum_variant(&variant.0, &name.0, offset, doc) {
					return Some(sym);
				}
			}
			// Check inner members
			for member in members {
				if let Some(sym) = find_symbol_in_struct_inner_member(&member.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Declaration::Interface {
			name,
			generics,
			super_interfaces,
			members,
			..
		} => {
			if offset_in_span(offset, name.1.start, name.1.end) {
				let generics_str = format_generics(generics);
				let super_str = if super_interfaces.is_empty() {
					String::new()
				} else {
					let supers: Vec<_> = super_interfaces
						.iter()
						.map(|s| s.0.0.0.to_string())
						.collect();
					format!(": {}", supers.join(" + "))
				};
				return Some(make_symbol_at_location(
					&name.0,
					SymbolKind::Interface,
					Some(format!("interface {}{}{}", name.0, generics_str, super_str)),
					name.1.start,
					name.1.end,
					doc,
				));
			}
			// Check generics
			for generic in generics {
				if let Some(sym) = find_symbol_in_generic_param(&generic.0, offset, doc) {
					return Some(sym);
				}
			}
			// Check super interfaces
			for super_interface in super_interfaces {
				let (super_name, _) = &super_interface.0;
				if offset_in_span(offset, super_name.1.start, super_name.1.end) {
					return Some(make_symbol_at_location(
						&super_name.0,
						SymbolKind::Interface,
						Some(format!("super interface {}", super_name.0)),
						super_name.1.start,
						super_name.1.end,
						doc,
					));
				}
			}
			// Check members
			for member in members {
				if let Some(sym) = find_symbol_in_interface_member(&member.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Declaration::Namespace { name, members, .. } => {
			if offset_in_span(offset, name.1.start, name.1.end) {
				return Some(make_symbol_at_location(
					&name.0,
					SymbolKind::Namespace,
					Some(format!("namespace {}", name.0)),
					name.1.start,
					name.1.end,
					doc,
				));
			}
			for member in members {
				if let Some(sym) = find_symbol_in_impl_member(&member.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Declaration::Impl { members, .. } | Declaration::ImplFor { members, .. } => {
			for member in members {
				if let Some(sym) = find_symbol_in_impl_member(&member.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Declaration::Import { root, path, idents } => find_symbol_in_import(
			root,
			path,
			idents.as_ref().map(Vec::as_slice),
			offset,
			doc,
			ctx,
		),
	}
}

/// Find symbol in an import declaration
fn find_symbol_in_import(
	root: &ImportRoot,
	path: &[Ident],
	idents: Option<&[(Ident, Option<Ident>)]>,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	// Format the import root
	let root_prefix = match root {
		ImportRoot::Package(name) => format!("{}/", name.0),
		ImportRoot::Project => "@/".to_string(),
		ImportRoot::Current => "./".to_string(),
		ImportRoot::Parent => "../".to_string(),
	};

	// Build the full import path string
	let path_parts: Vec<&str> = path.iter().map(|p| p.0.as_str()).collect();
	let full_path = format!("{}{}", root_prefix, path_parts.join("/"));

	// Try to resolve the actual file path using the type checker
	let resolved_path = doc.type_checker.as_ref().and_then(|tc| {
		if path.is_empty() {
			return None;
		}
		let dummy_span = nymph_compiler::ast::Span::new(0, 1);
		tc.resolve_import_path(root, path, dummy_span).ok()
	});

	let definition_path = resolved_path.map(|p| p.display().to_string());

	// Check if offset is on one of the path segments
	for (i, segment) in path.iter().enumerate() {
		if offset_in_span(offset, segment.1.start, segment.1.end) {
			// Build partial path up to this segment
			let partial_path: Vec<&str> = path[..=i].iter().map(|p| p.0.as_str()).collect();
			let module_path = format!("{}{}", root_prefix, partial_path.join("/"));

			return Some(make_symbol_at_location_with_path(
				&segment.0,
				SymbolKind::Module,
				Some(format!("import {}", module_path)),
				segment.1.start,
				segment.1.end,
				doc,
				definition_path,
			));
		}
	}

	// Check the `with` clause items
	if let Some(import_idents) = idents {
		for (item_name, alias) in import_idents {
			// Check if on the original name
			if offset_in_span(offset, item_name.1.start, item_name.1.end) {
				let type_info = lookup_type_in_context(&item_name.0, ctx)
					.map(|t| format!("{}: {}", item_name.0, t))
					.unwrap_or_else(|| format!("{} (from {})", item_name.0, full_path));

				return Some(make_symbol_at_location_with_path(
					&item_name.0,
					SymbolKind::Variable,
					Some(type_info),
					item_name.1.start,
					item_name.1.end,
					doc,
					definition_path.clone(),
				));
			}

			// Check if on the alias
			if let Some(alias_ident) = alias
				&& offset_in_span(offset, alias_ident.1.start, alias_ident.1.end)
			{
				let type_info = lookup_type_in_context(&alias_ident.0, ctx)
					.map(|t| format!("{}: {}", alias_ident.0, t))
					.unwrap_or_else(|| {
						format!(
							"{} (alias for {} from {})",
							alias_ident.0, item_name.0, full_path
						)
					});

				return Some(make_symbol_at_location_with_path(
					&alias_ident.0,
					SymbolKind::Variable,
					Some(type_info),
					alias_ident.1.start,
					alias_ident.1.end,
					doc,
					definition_path.clone(),
				));
			}
		}
	}

	None
}

fn find_symbol_in_let(
	meta: &LetDeclaration,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	find_symbol_in_let_with_value(meta, None, offset, doc, ctx)
}

fn find_symbol_in_let_with_value(
	meta: &LetDeclaration,
	value: Option<&Expr>,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	let name_ident = pattern_binding_ident(&meta.name.0)?;
	if !offset_in_span(offset, name_ident.1.start, name_ident.1.end) {
		return None;
	}
	let type_str = lookup_type_in_context(&name_ident.0, ctx)
		.or_else(|| meta.type_.as_ref().map(|t| type_to_string(&t.0)))
		.or_else(|| value.and_then(infer_expr_type))
		.unwrap_or_else(|| "_".to_string());
	let mut_str = if meta.mutable { "mut " } else { "" };
	Some(make_symbol_at_location(
		&name_ident.0,
		SymbolKind::Variable,
		Some(format!("let {}{}: {}", mut_str, name_ident.0, type_str)),
		name_ident.1.start,
		name_ident.1.end,
		doc,
	))
}

fn find_symbol_in_func_with_body(
	meta: &FuncDeclaration,
	body: Option<&Expr>,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	// Check function name
	if offset_in_span(offset, meta.name.1.start, meta.name.1.end) {
		let sig = lookup_type_in_context(&meta.name.0, ctx)
			.map(|t| format!("func {}: {}", meta.name.0, t))
			.unwrap_or_else(|| format_func_signature_with_body(meta, body));
		return Some(make_symbol_at_location(
			&meta.name.0,
			SymbolKind::Function,
			Some(sig),
			meta.name.1.start,
			meta.name.1.end,
			doc,
		));
	}
	// Check generics
	for generic in &meta.generics {
		if let Some(sym) = find_symbol_in_generic_param(&generic.0, offset, doc) {
			return Some(sym);
		}
	}
	// Check parameters
	for param in &meta.params {
		let p = &param.0.clone();
		let Some(name_ident) = pattern_binding_ident(&p.name.0) else {
			continue;
		};
		if offset_in_span(offset, name_ident.1.start, name_ident.1.end) {
			let mut_str = if p.mutable { "mut " } else { "" };
			return Some(make_symbol_at_location(
				&name_ident.0,
				SymbolKind::Parameter,
				Some(format!(
					"{mut_str}{}: {}",
					name_ident.0,
					type_to_string(&p.type_.0)
				)),
				name_ident.1.start,
				name_ident.1.end,
				doc,
			));
		}
	}

	if let Some(body) = body {
		let body_ctx = function_body_context(meta, ctx);
		if let Some(mut sym) = find_symbol_in_expr(body, offset, doc, Some(&body_ctx)) {
			let needs_param_fallback = sym.kind == SymbolKind::Variable
				&& sym
					.type_info
					.as_ref()
					.is_none_or(|info| info == &sym.name || info.contains('_'));
			if needs_param_fallback && let Some(type_info) = function_param_type_info(meta, &sym.name) {
				sym.kind = SymbolKind::Parameter;
				sym.type_info = Some(type_info);
			}
			return Some(sym);
		}
	}

	None
}

#[allow(dead_code)]
fn find_symbol_in_func(
	meta: &FuncDeclaration,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	find_symbol_in_func_with_body(meta, None, offset, doc, ctx)
}

fn find_symbol_in_struct_field(
	field: &StructField,
	offset: usize,
	doc: &Document,
) -> Option<SymbolAtLocation> {
	if offset_in_span(offset, field.name.1.start, field.name.1.end) {
		return Some(make_symbol_at_location(
			&field.name.0,
			SymbolKind::Field,
			Some(format!(
				"field {}: {}",
				field.name.0,
				type_to_string(&field.type_.0)
			)),
			field.name.1.start,
			field.name.1.end,
			doc,
		));
	}
	None
}

fn find_symbol_in_enum_variant(
	variant: &EnumVariant,
	enum_name: &str,
	offset: usize,
	doc: &Document,
) -> Option<SymbolAtLocation> {
	// Check variant name
	if offset_in_span(offset, variant.name.1.start, variant.name.1.end) {
		let fields_str = format_struct_fields(&variant.fields);
		return Some(make_symbol_at_location(
			&variant.name.0,
			SymbolKind::Field,
			Some(format!(
				"variant {}.{}{}",
				enum_name, variant.name.0, fields_str
			)),
			variant.name.1.start,
			variant.name.1.end,
			doc,
		));
	}
	// Check variant fields
	for field in &variant.fields {
		if let Some(sym) = find_symbol_in_struct_field(&field.0, offset, doc) {
			return Some(sym);
		}
	}
	None
}

fn find_symbol_in_generic_param(
	param: &GenericParam,
	offset: usize,
	doc: &Document,
) -> Option<SymbolAtLocation> {
	if offset_in_span(offset, param.name.1.start, param.name.1.end) {
		let mut info = format!("type parameter {}", param.name.0);
		if let Some(constraint) = &param.constraint {
			info.push_str(&format!(": {}", type_to_string(&constraint.0)));
		}
		if let Some(default) = &param.default {
			info.push_str(&format!(" = {}", type_to_string(&default.0)));
		}
		return Some(make_symbol_at_location(
			&param.name.0,
			SymbolKind::Type,
			Some(info),
			param.name.1.start,
			param.name.1.end,
			doc,
		));
	}
	None
}

fn find_symbol_in_struct_inner_member(
	member: &StructInnerMember,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	match member {
		StructInnerMember::Member(m) => find_symbol_in_impl_member(&m.0, offset, doc, ctx),
		StructInnerMember::Namespace(members) => {
			for m in members {
				if let Some(sym) = find_symbol_in_impl_member(&m.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		StructInnerMember::Impl { members, .. } | StructInnerMember::ImplMut(members) => {
			for m in members {
				if let Some(sym) = find_symbol_in_impl_member(&m.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
	}
}

fn find_symbol_in_impl_member(
	member: &ImplMember,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	match member {
		ImplMember::Let { meta, value, .. } => {
			find_symbol_in_let_with_value(meta, Some(&value.0), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(&value.0, offset, doc, ctx))
		}
		ImplMember::ExternalLet(_, _, meta) => find_symbol_in_let(meta, offset, doc, ctx),
		ImplMember::Func { meta, body, .. } => {
			find_symbol_in_func_with_body(meta, Some(&body.0), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(&body.0, offset, doc, ctx))
		}
		ImplMember::ExternalFunc(_, _, meta) => find_symbol_in_func(meta, offset, doc, ctx),
	}
}

fn find_symbol_in_interface_member(
	member: &InterfaceMember,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	match member {
		InterfaceMember::Element(elem) => find_symbol_in_interface_element(&elem.0, offset, doc, ctx),
		InterfaceMember::Namespace(members) => {
			for m in members {
				if let Some(sym) = find_symbol_in_impl_member(&m.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		InterfaceMember::ImplMut(elements) => {
			for elem in elements {
				if let Some(sym) = find_symbol_in_interface_element(&elem.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		InterfaceMember::Impl { members, .. } => {
			for m in members {
				if let Some(sym) = find_symbol_in_impl_member(&m.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
	}
}

fn find_symbol_in_interface_element(
	elem: &InterfaceElement,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	match elem {
		InterfaceElement::Let { meta, value } => find_symbol_in_let_with_value(
			meta,
			value.as_ref().map(|v| v.0.clone()).as_ref(),
			offset,
			doc,
			ctx,
		)
		.or_else(|| {
			value
				.as_ref()
				.and_then(|v| find_symbol_in_expr(&v.0, offset, doc, ctx))
		}),
		InterfaceElement::Func { meta, body } => find_symbol_in_func_with_body(
			meta,
			body.as_ref().map(|b| b.0.clone()).as_ref(),
			offset,
			doc,
			ctx,
		)
		.or_else(|| {
			body
				.as_ref()
				.and_then(|b| find_symbol_in_expr(&b.0, offset, doc, ctx))
		}),
	}
}

/// Find a symbol in an expression at the given offset
fn find_symbol_in_expr(
	expr: &Expr,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	match expr {
		// Block - check let statements and nested expressions
		Expr::Block { body, .. } => {
			let mut block_ctx = ctx.cloned().unwrap_or_default();
			for stmt in body {
				if let Some(sym) = find_symbol_in_statement(&stmt.0, offset, doc, Some(&block_ctx)) {
					return Some(sym);
				}
				if let Statement::Let { meta, value } = &stmt.0 {
					extend_context_with_let_binding(&mut block_ctx, meta, &value.0, doc);
				}
			}
			None
		}

		// Closure - check parameters
		Expr::Closure { params, body, .. } => {
			for param in params {
				if let Some(sym) = find_symbol_in_closure_param(&param.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			find_symbol_in_expr(&body.0, offset, doc, ctx)
		}
		Expr::AnonymousParam(_) => None,

		// Match - check pattern bindings in arms
		Expr::Match { value, arms } => {
			if let Some(sym) = find_symbol_in_expr(&value.0, offset, doc, ctx) {
				return Some(sym);
			}
			for arm in arms {
				if let Some(sym) = find_symbol_in_match_arm(arm, &value.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}

		// If/While - traverse sub-expressions
		Expr::If {
			condition,
			then,
			otherwise,
			..
		} => find_symbol_in_expr(&condition.0, offset, doc, ctx)
			.or_else(|| find_symbol_in_expr(&then.0, offset, doc, ctx))
			.or_else(|| {
				otherwise
					.as_ref()
					.and_then(|e| find_symbol_in_expr(&e.0, offset, doc, ctx))
			}),

		Expr::While {
			condition, body, ..
		} => find_symbol_in_expr(&condition.0, offset, doc, ctx)
			.or_else(|| find_symbol_in_expr(&body.0, offset, doc, ctx)),

		Expr::For {
			variable,
			iterable,
			body,
			..
		} => {
			if let Some(binding) = variable.0.as_binding()
				&& offset_in_span(offset, binding.1.start, binding.1.end)
			{
				let type_info = infer_expr_type(&iterable.0).map(|t| format!("element of {}", t));
				return Some(make_symbol_at_location(
					&binding.0,
					SymbolKind::Variable,
					type_info,
					binding.1.start,
					binding.1.end,
					doc,
				));
			}
			find_symbol_in_expr(&iterable.0, offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(&body.0, offset, doc, ctx))
		}

		// Binary/Prefix/Postfix ops
		Expr::BinaryOp { lhs, rhs, .. } => find_symbol_in_expr(&lhs.0, offset, doc, ctx)
			.or_else(|| find_symbol_in_expr(&rhs.0, offset, doc, ctx)),
		Expr::PrefixOp { value, .. } | Expr::PostfixOp { value, .. } => {
			find_symbol_in_expr(&value.0, offset, doc, ctx)
		}
		Expr::TypeOp { lhs, .. } | Expr::PatternOp { lhs, .. } => {
			find_symbol_in_expr(&lhs.0, offset, doc, ctx)
		}
		Expr::AssignOp { lhs, rhs, .. } => find_symbol_in_expr(&lhs.0, offset, doc, ctx)
			.or_else(|| find_symbol_in_expr(&rhs.0, offset, doc, ctx)),

		// Call expressions
		Expr::Call { func, args, .. } => {
			if let Some(sym) = find_symbol_in_expr(&func.0, offset, doc, ctx) {
				return Some(sym);
			}
			for arg in args {
				if let Some(sym) = find_symbol_in_expr(&arg.0.value.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}

		// Member/Index access
		Expr::MemberAccess { parent, member, .. } => {
			// Check if we're hovering over the member name
			if offset_in_span(offset, member.1.start, member.1.end) {
				let type_info = infer_hover_type(expr, doc, ctx)
					.map(|ty| format!("{}: {}", member.0, ty))
					.or_else(|| {
						infer_hover_type(&parent.0, doc, ctx)
							.map(|parent_ty| format!("{}.{}", parent_ty, member.0))
					})
					.unwrap_or_else(|| member.0.to_string());

				return Some(make_symbol_at_location(
					&member.0,
					SymbolKind::Field,
					Some(type_info),
					member.1.start,
					member.1.end,
					doc,
				));
			}
			find_symbol_in_expr(&parent.0, offset, doc, ctx)
		}
		Expr::IndexAccess { parent, index, .. } => find_symbol_in_expr(&parent.0, offset, doc, ctx)
			.or_else(|| find_symbol_in_expr(&index.0, offset, doc, ctx)),

		// Grouped
		Expr::Grouped(inner) => find_symbol_in_expr(&inner.0, offset, doc, ctx),

		// Return/Break with values
		Expr::Return { value, .. } | Expr::Break { value, .. } => value
			.as_ref()
			.and_then(|v| find_symbol_in_expr(&v.0, offset, doc, ctx)),

		// Collections - traverse elements
		Expr::List(items) | Expr::Tuple(items) => {
			for item in items {
				let expr = match &item.0 {
					nymph_compiler::ast::expr::ListItem::Expr(e) => e,
					nymph_compiler::ast::expr::ListItem::Spread(e) => e,
				};
				if let Some(sym) = find_symbol_in_expr(&expr.0, offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Expr::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					nymph_compiler::ast::expr::MapEntry::Expr(k, v) => {
						if let Some(sym) = find_symbol_in_expr(&k.0, offset, doc, ctx) {
							return Some(sym);
						}
						if let Some(sym) = find_symbol_in_expr(&v.0, offset, doc, ctx) {
							return Some(sym);
						}
					}
					nymph_compiler::ast::expr::MapEntry::Spread(e) => {
						if let Some(sym) = find_symbol_in_expr(&e.0, offset, doc, ctx) {
							return Some(sym);
						}
					}
				}
			}
			None
		}

		// String interpolations
		Expr::String(parts) => {
			for part in parts {
				if let nymph_compiler::ast::expr::StringPart::InterpolatedExpr(e) = &part.0
					&& let Some(sym) = find_symbol_in_expr(&e.0, offset, doc, ctx)
				{
					return Some(sym);
				}
			}
			None
		}

		// Identifier - look up type in context
		Expr::Identifier(ident) => {
			let name = &ident.0.clone();
			if offset_in_span(offset, ident.1.start, ident.1.end) {
				let type_info = infer_hover_type(expr, doc, ctx)
					.map(|ty| format!("{}: {}", name, ty))
					.or_else(|| lookup_type_in_context(name, ctx).map(|ty| format!("{}: {}", name, ty)))
					.unwrap_or_else(|| name.to_string());
				return Some(make_symbol_at_location(
					name,
					SymbolKind::Variable,
					Some(type_info),
					ident.1.start,
					ident.1.end,
					doc,
				));
			}
			None
		}

		// `this` keyword - look up type in context
		Expr::This => None,

		// Literals and simple expressions - no nested symbols
		Expr::Int(_)
		| Expr::UInt(_)
		| Expr::Float(_)
		| Expr::Char(_)
		| Expr::Boolean(_)
		| Expr::Range(_)
		| Expr::Continue { .. }
		| Expr::Placeholder => None,
	}
}

fn find_symbol_in_statement(
	stmt: &Statement,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	match stmt {
		Statement::Expr(e) => find_symbol_in_expr(&e.0, offset, doc, ctx),
		Statement::Let { meta, value } => {
			find_symbol_in_let_with_value(meta, Some(&value.0), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(&value.0, offset, doc, ctx))
		}
	}
}

fn find_symbol_in_closure_param(
	param: &ClosureParam,
	offset: usize,
	doc: &Document,
	_ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	let name_ident = pattern_binding_ident(&param.name.0)?;
	if !offset_in_span(offset, name_ident.1.start, name_ident.1.end) {
		return None;
	}
	let type_str = param
		.type_
		.as_ref()
		.map(|t| type_to_string(&t.0))
		.unwrap_or_else(|| "_".to_string());
	let mut_str = if param.mutable { "mut " } else { "" };
	Some(make_symbol_at_location(
		&name_ident.0,
		SymbolKind::Parameter,
		Some(format!("{mut_str}{}: {type_str}", name_ident.0)),
		name_ident.1.start,
		name_ident.1.end,
		doc,
	))
}

fn find_symbol_in_match_arm(
	arm: &MatchArm,
	scrutinee: &Expr,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	// Check for bindings in the pattern
	if let Some(sym) = find_symbol_in_pattern(&arm.pattern.0, scrutinee, offset, doc, ctx) {
		return Some(sym);
	}
	// Check guard expression
	if let Some(guard) = &arm.guard
		&& let Some(sym) = find_symbol_in_expr(&guard.0, offset, doc, ctx)
	{
		return Some(sym);
	}
	// Check body
	find_symbol_in_expr(&arm.body.0, offset, doc, ctx)
}

fn find_symbol_in_pattern(
	pattern: &nymph_compiler::ast::expr::Pattern,
	scrutinee: &Expr,
	offset: usize,
	doc: &Document,
	_ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	use nymph_compiler::ast::expr::Pattern;

	match pattern {
		Pattern::Binding { name, inner } => {
			if offset_in_span(offset, name.1.start, name.1.end) {
				let type_str = infer_expr_type(scrutinee).unwrap_or_else(|| "_".to_string());
				return Some(make_symbol_at_location(
					&name.0,
					SymbolKind::Variable,
					Some(format!("binding {}: {}", name.0, type_str)),
					name.1.start,
					name.1.end,
					doc,
				));
			}
			find_symbol_in_pattern(&inner.0, scrutinee, offset, doc, _ctx)
		}
		Pattern::List(items) | Pattern::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListPatternEntry::Item(p) => {
						if let Some(sym) = find_symbol_in_pattern(&p.0, scrutinee, offset, doc, _ctx) {
							return Some(sym);
						}
					}
					ListPatternEntry::Rest(Some(name)) => {
						if offset_in_span(offset, name.1.start, name.1.end) {
							return Some(make_symbol_at_location(
								&name.0,
								SymbolKind::Variable,
								Some(format!("rest binding {}", name.0)),
								name.1.start,
								name.1.end,
								doc,
							));
						}
					}
					ListPatternEntry::Rest(None) => {}
				}
			}
			None
		}
		Pattern::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapPatternEntry::Entry(_, value) => {
						if let Some(sym) = find_symbol_in_pattern(&value.0, scrutinee, offset, doc, _ctx) {
							return Some(sym);
						}
					}
					MapPatternEntry::Rest(Some(name)) => {
						if offset_in_span(offset, name.1.start, name.1.end) {
							return Some(make_symbol_at_location(
								&name.0,
								SymbolKind::Variable,
								Some(format!("rest binding {}", name.0)),
								name.1.start,
								name.1.end,
								doc,
							));
						}
					}
					MapPatternEntry::Rest(None) => {}
				}
			}
			None
		}
		Pattern::Struct { fields, .. } => {
			for field in fields {
				match &field.0 {
					StructPatternField::Value { value, .. } => {
						if let Some(sym) = find_symbol_in_pattern(&value.0, scrutinee, offset, doc, _ctx) {
							return Some(sym);
						}
					}
					StructPatternField::Named(name) => {
						if offset_in_span(offset, name.1.start, name.1.end) {
							return Some(make_symbol_at_location(
								&name.0,
								SymbolKind::Variable,
								Some(name.0.to_string()),
								name.1.start,
								name.1.end,
								doc,
							));
						}
					}
					StructPatternField::Rest => {}
				}
			}
			None
		}
		Pattern::Union(a, b) => find_symbol_in_pattern(&a.0, scrutinee, offset, doc, _ctx)
			.or_else(|| find_symbol_in_pattern(&b.0, scrutinee, offset, doc, _ctx)),
		Pattern::Grouped(inner) => find_symbol_in_pattern(&inner.0, scrutinee, offset, doc, _ctx),
		// Literals don't have bindings
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Range(_)
		| Pattern::Placeholder => None,
	}
}

/// Create a SymbolAtLocation from components
fn make_symbol_at_location(
	name: &str,
	kind: SymbolKind,
	type_info: Option<String>,
	start_offset: usize,
	end_offset: usize,
	doc: &Document,
) -> SymbolAtLocation {
	make_symbol_at_location_with_path(name, kind, type_info, start_offset, end_offset, doc, None)
}

fn make_symbol_at_location_with_path(
	name: &str,
	kind: SymbolKind,
	type_info: Option<String>,
	start_offset: usize,
	end_offset: usize,
	doc: &Document,
	definition_path: Option<String>,
) -> SymbolAtLocation {
	let (start_line, start_char) = doc.position_to_line_col(start_offset);
	let (end_line, end_char) = doc.position_to_line_col(end_offset);
	SymbolAtLocation {
		name: name.to_string(),
		kind,
		type_info,
		range: LocationRange {
			start_line,
			start_char,
			end_line,
			end_char,
		},
		definition_path,
	}
}

/// Convert a Type AST node to a display string
pub fn type_to_string(ty: &Type) -> String {
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
				let generic_strs: Vec<_> = generics.iter().map(|g| format_generic_arg(&g.0)).collect();
				format!("{}<{}>", name.0, generic_strs.join(", "))
			}
		}
		Type::List(inner) => format!("#[{}]", type_to_string(&inner.0)),
		Type::Tuple(elems) => {
			let elem_strs: Vec<_> = elems.iter().map(|e| type_to_string(&e.0)).collect();
			format!("#({})", elem_strs.join(", "))
		}
		Type::Map(k, v) => format!("#{{ {}: {} }}", type_to_string(&k.0), type_to_string(&v.0)),
		Type::Function {
			params,
			return_type,
		} => {
			let param_strs: Vec<_> = params
				.iter()
				.map(|(name, ty)| {
					if let Some(n) = name {
						format!("{}: {}", n.0, type_to_string(&ty.0))
					} else {
						type_to_string(&ty.0)
					}
				})
				.collect();
			format!(
				"({}) -> {}",
				param_strs.join(", "),
				type_to_string(&return_type.0)
			)
		}
		Type::Intersection(a, b) => format!("{} + {}", type_to_string(&a.0), type_to_string(&b.0)),
		Type::Grouped(inner) => format!("({})", type_to_string(&inner.0)),
	}
}

fn format_generic_arg(arg: &GenericArg) -> String {
	if let Some(name) = &arg.name {
		format!("{}: {}", name.0, type_to_string(&arg.value.0))
	} else {
		type_to_string(&arg.value.0)
	}
}

fn format_generics(generics: &[Spanned<GenericParam>]) -> String {
	if generics.is_empty() {
		String::new()
	} else {
		let strs: Vec<_> = generics
			.iter()
			.map(|g| {
				let p = &g.0.clone();
				let mut s = p.name.0.to_string();
				if let Some(c) = &p.constraint {
					s.push_str(&format!(": {}", type_to_string(&c.0)));
				}
				if let Some(d) = &p.default {
					s.push_str(&format!(" = {}", type_to_string(&d.0)));
				}
				s
			})
			.collect();
		format!("<{}>", strs.join(", "))
	}
}

fn format_struct_fields(fields: &[Spanned<StructField>]) -> String {
	if fields.is_empty() {
		String::new()
	} else {
		let strs: Vec<_> = fields
			.iter()
			.map(|f| {
				let field = &f.0.clone();
				format!("{}: {}", field.name.0, type_to_string(&field.type_.0))
			})
			.collect();
		format!("({})", strs.join(", "))
	}
}

/// Infer the type of an expression and return a display string
fn infer_expr_type(expr: &Expr) -> Option<String> {
	match expr {
		// Literal types
		Expr::Int(_) => Some("int".to_string()),
		Expr::UInt(_) => Some("uint".to_string()),
		Expr::Float(_) => Some("float".to_string()),
		Expr::Char(_) => Some("char".to_string()),
		Expr::String(_) => Some("string".to_string()),
		Expr::Boolean(_) => Some("boolean".to_string()),
		Expr::AnonymousParam(_) => None,

		// Collection types - infer element types where possible
		Expr::List(items) => {
			if items.is_empty() {
				Some("#[_]".to_string())
			} else if let Some(first) = items.first() {
				match &first.0 {
					nymph_compiler::ast::expr::ListItem::Expr(e) => {
						let elem_type = infer_expr_type(&e.0).unwrap_or_else(|| "_".to_string());
						Some(format!("#[{elem_type}]"))
					}
					nymph_compiler::ast::expr::ListItem::Spread(_) => Some("#[_]".to_string()),
				}
			} else {
				Some("#[_]".to_string())
			}
		}
		Expr::Tuple(items) => {
			let elem_types: Vec<String> = items
				.iter()
				.map(|item| match &item.0 {
					nymph_compiler::ast::expr::ListItem::Expr(e) => {
						infer_expr_type(&e.0).unwrap_or_else(|| "_".to_string())
					}
					nymph_compiler::ast::expr::ListItem::Spread(_) => "_".to_string(),
				})
				.collect();
			Some(format!("#({})", elem_types.join(", ")))
		}
		Expr::Map(entries) => {
			if entries.is_empty() {
				Some("#{{_: _}}".to_string())
			} else if let Some(first) = entries.first() {
				match &first.0 {
					nymph_compiler::ast::expr::MapEntry::Expr(k, v) => {
						let key_type = infer_expr_type(&k.0).unwrap_or_else(|| "_".to_string());
						let val_type = infer_expr_type(&v.0).unwrap_or_else(|| "_".to_string());
						Some(format!("#{{{key_type}: {val_type}}}"))
					}
					nymph_compiler::ast::expr::MapEntry::Spread(_) => Some("#{{_: _}}".to_string()),
				}
			} else {
				Some("#{{_: _}}".to_string())
			}
		}

		// Operators that produce specific types
		Expr::BinaryOp { lhs, op, rhs } => match op {
			// Comparison operators always return boolean
			BinaryOperator::Equals
			| BinaryOperator::NotEquals
			| BinaryOperator::LessThan
			| BinaryOperator::LessThanEquals
			| BinaryOperator::GreaterThan
			| BinaryOperator::GreaterThanEquals
			| BinaryOperator::In
			| BinaryOperator::NotIn
			| BinaryOperator::BoolAnd
			| BinaryOperator::BoolOr => Some("boolean".to_string()),

			// Arithmetic operators - type depends on operands
			BinaryOperator::Plus
			| BinaryOperator::Minus
			| BinaryOperator::Times
			| BinaryOperator::Divide
			| BinaryOperator::Remainder
			| BinaryOperator::Power => {
				let lhs_type = infer_expr_type(&lhs.0);
				let rhs_type = infer_expr_type(&rhs.0);
				match (&lhs_type, &rhs_type) {
					(Some(l), Some(r)) if l == "float" || r == "float" => Some("float".to_string()),
					(Some(l), _) if l == "int" => Some("int".to_string()),
					(_, Some(r)) if r == "int" => Some("int".to_string()),
					_ => lhs_type.or(rhs_type),
				}
			}

			// Bitwise operators return int
			BinaryOperator::BitAnd
			| BinaryOperator::BitOr
			| BinaryOperator::BitXor
			| BinaryOperator::LeftShift
			| BinaryOperator::RightShift => Some("int".to_string()),

			// Pipe operator returns type of RHS (function call result)
			BinaryOperator::Pipe => infer_expr_type(&rhs.0),

			// Unwrap operator - returns the unwrapped type
			BinaryOperator::Unwrap => infer_expr_type(&lhs.0),
		},

		Expr::PrefixOp { op, value } => match op {
			PrefixOperator::BoolNot => Some("boolean".to_string()),
			PrefixOperator::Negate => infer_expr_type(&value.0),
			PrefixOperator::BitNot => Some("int".to_string()),
		},

		// Type cast - return the target type
		Expr::TypeOp { rhs, .. } => Some(type_to_string(&rhs.0)),

		// Pattern match - returns boolean
		Expr::PatternOp { .. } => Some("boolean".to_string()),

		// Control flow expressions
		Expr::If {
			then, otherwise, ..
		} => {
			if let Some(else_branch) = otherwise {
				// If both branches have the same type, return it
				let then_type = infer_expr_type(&then.0);
				let else_type = infer_expr_type(&else_branch.0);
				match (&then_type, &else_type) {
					(Some(t), Some(e)) if t == e => Some(t.clone()),
					_ => then_type.or(else_type),
				}
			} else {
				Some("void".to_string())
			}
		}

		Expr::Match { arms, .. } => {
			// Return type of first arm body if available
			arms.first().and_then(|arm| infer_expr_type(&arm.body.0))
		}

		// Block - type is the type of the last expression
		Expr::Block { body, .. } => body.last().and_then(|stmt| match &stmt.0 {
			Statement::Expr(e) => infer_expr_type(&e.0),
			Statement::Let { .. } => Some("void".to_string()),
		}),

		// Grouped expression - unwrap
		Expr::Grouped(inner) => infer_expr_type(&inner.0),

		// Control flow that doesn't produce values
		Expr::Return { value, .. } => value.as_ref().and_then(|v| infer_expr_type(&v.0)),
		Expr::Break { value, .. } => value.as_ref().and_then(|v| infer_expr_type(&v.0)),
		Expr::Continue { .. } => Some("never".to_string()),

		// Loops return void unless broken with a value
		Expr::While { .. } | Expr::For { .. } => Some("void".to_string()),

		// Assignment returns void
		Expr::AssignOp { .. } => Some("void".to_string()),

		// Range returns a range type (simplified)
		Expr::Range(_) => Some("Range".to_string()),

		// Closures - return their declared return type or infer from body
		Expr::Closure {
			return_type, body, ..
		} => {
			if let Some(ret) = return_type {
				Some(type_to_string(&ret.0))
			} else {
				infer_expr_type(&body.0)
			}
		}

		// These need context we don't have
		Expr::Identifier(_)
		| Expr::Call { .. }
		| Expr::MemberAccess { .. }
		| Expr::IndexAccess { .. }
		| Expr::PostfixOp { .. }
		| Expr::This
		| Expr::Placeholder => None,
	}
}

fn default_keyword_suggestions() -> Vec<CompletionSuggestion> {
	[
		("let", SymbolKind::Variable),
		("func", SymbolKind::Function),
		("if", SymbolKind::Variable),
		("else", SymbolKind::Variable),
		("struct", SymbolKind::Type),
		("enum", SymbolKind::Enum),
		("interface", SymbolKind::Interface),
		("namespace", SymbolKind::Namespace),
		("match", SymbolKind::Variable),
		("return", SymbolKind::Variable),
		("true", SymbolKind::Variable),
		("false", SymbolKind::Variable),
	]
	.into_iter()
	.map(|(label, kind)| CompletionSuggestion {
		label: label.to_string(),
		kind,
		detail: Some("keyword".to_string()),
	})
	.collect()
}

#[derive(Debug, Clone)]
struct CallSite {
	func: Spanned<Expr>,
	active_parameter: usize,
	ctx: Context,
}

#[derive(Debug, Clone)]
struct MemberAccessSite {
	parent: Spanned<Expr>,
	ctx: Context,
}

fn collect_top_level_completion_suggestions(
	module: &Module,
	document: &Document,
) -> Vec<CompletionSuggestion> {
	let mut suggestions = Vec::new();

	for decl in &module.members {
		match decl {
			Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
				if let Some(binding) = pattern_binding_ident(&meta.name.0) {
					suggestions.push(CompletionSuggestion {
						label: binding.0.to_string(),
						kind: SymbolKind::Variable,
						detail: lookup_type_in_context(&binding.0, document.type_context.as_ref()),
					});
				}
			}
			Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
				suggestions.push(CompletionSuggestion {
					label: meta.name.0.to_string(),
					kind: SymbolKind::Function,
					detail: Some(format_func_signature(meta)),
				});
			}
			Declaration::TypeAlias { meta, value, .. } => suggestions.push(CompletionSuggestion {
				label: meta.name.0.to_string(),
				kind: SymbolKind::Type,
				detail: Some(format!(
					"type {} = {}",
					meta.name.0,
					type_to_string(&value.0)
				)),
			}),
			Declaration::Struct { name, .. } => suggestions.push(CompletionSuggestion {
				label: name.0.to_string(),
				kind: SymbolKind::Struct,
				detail: Some("struct".to_string()),
			}),
			Declaration::Enum { name, .. } => suggestions.push(CompletionSuggestion {
				label: name.0.to_string(),
				kind: SymbolKind::Enum,
				detail: Some("enum".to_string()),
			}),
			Declaration::Namespace { name, .. } => suggestions.push(CompletionSuggestion {
				label: name.0.to_string(),
				kind: SymbolKind::Namespace,
				detail: Some("namespace".to_string()),
			}),
			Declaration::Interface { name, .. } => suggestions.push(CompletionSuggestion {
				label: name.0.to_string(),
				kind: SymbolKind::Interface,
				detail: Some("interface".to_string()),
			}),
			Declaration::Import { path, idents, .. } => {
				if let Some(last) = path.last() {
					suggestions.push(CompletionSuggestion {
						label: last.0.to_string(),
						kind: SymbolKind::Module,
						detail: Some("module".to_string()),
					});
				}
				for (name, alias) in idents.as_deref().unwrap_or(&[]) {
					let local_name = alias.as_ref().unwrap_or(name);
					suggestions.push(CompletionSuggestion {
						label: local_name.0.to_string(),
						kind: SymbolKind::Variable,
						detail: Some("import".to_string()),
					});
				}
			}
			Declaration::Impl { .. } | Declaration::ImplFor { .. } => {}
		}
	}

	suggestions
}

fn collect_local_completion_suggestions(
	module: &Module,
	offset: usize,
	_document: &Document,
) -> Vec<CompletionSuggestion> {
	let mut suggestions = Vec::new();

	for decl in &module.members {
		match decl {
			Declaration::Func { meta, body, .. } => {
				if body.1.start <= offset && offset <= body.1.end {
					for param in &meta.params {
						push_pattern_completion_suggestions(
							&param.0.name.0,
							&mut suggestions,
							SymbolKind::Parameter,
						);
					}
					collect_completion_suggestions_in_expr(&body.0, offset, &mut suggestions);
				}
			}
			Declaration::Let { value, .. } => {
				if value.1.start <= offset && offset <= value.1.end {
					collect_completion_suggestions_in_expr(&value.0, offset, &mut suggestions);
				}
			}
			_ => {}
		}
	}

	suggestions
}

fn get_member_completion_suggestions_at_offset(
	module: &Module,
	offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<Vec<CompletionSuggestion>> {
	let site = find_member_access_site_at_offset(module, offset, document, ctx)?;
	let parent_type = infer_checked_type(&site.parent.0, document, Some(&site.ctx))
		.or_else(|| call_site_identifier_type(&site.parent.0, &site.ctx))?;
	let mut suggestions = completion_suggestions_from_type(&parent_type);
	suggestions.sort_by(|a, b| a.label.cmp(&b.label).then(a.kind.cmp(&b.kind)));
	suggestions.dedup_by(|a, b| a.label == b.label && a.kind == b.kind);
	Some(suggestions)
}

fn completion_suggestions_from_type(type_: &CheckedType) -> Vec<CompletionSuggestion> {
	match type_ {
		CheckedType::Struct {
			fields,
			members,
			impls,
			..
		} => {
			let mut suggestions = fields
				.iter()
				.map(|(name, type_)| CompletionSuggestion {
					label: name.to_string(),
					kind: SymbolKind::Field,
					detail: Some(type_.to_string()),
				})
				.collect::<Vec<_>>();
			suggestions.extend(completion_suggestions_from_struct_members(members));
			suggestions.extend(
				impls
					.iter()
					.map(|(name, type_)| CompletionSuggestion {
						label: name.to_string(),
						kind: symbol_kind_from_checked_type(type_),
						detail: Some(type_.to_string()),
					})
					.collect::<Vec<_>>(),
			);
			suggestions
		}
		CheckedType::Enum { members, impls, .. } | CheckedType::Interface { members, impls, .. } => {
			let mut suggestions = completion_suggestions_from_struct_members(members);
			suggestions.extend(
				impls
					.iter()
					.map(|(name, type_)| CompletionSuggestion {
						label: name.to_string(),
						kind: symbol_kind_from_checked_type(type_),
						detail: Some(type_.to_string()),
					})
					.collect::<Vec<_>>(),
			);
			suggestions
		}
		CheckedType::EnumVariant { fields, impls, .. } => {
			let mut suggestions = fields
				.iter()
				.map(|(name, type_)| CompletionSuggestion {
					label: name.to_string(),
					kind: SymbolKind::Field,
					detail: Some(type_.to_string()),
				})
				.collect::<Vec<_>>();
			suggestions.extend(
				impls
					.iter()
					.map(|(name, type_)| CompletionSuggestion {
						label: name.to_string(),
						kind: symbol_kind_from_checked_type(type_),
						detail: Some(type_.to_string()),
					})
					.collect::<Vec<_>>(),
			);
			suggestions
		}
		CheckedType::Module { members, .. } => members
			.iter()
			.map(|(name, entry)| match entry {
				ContextEntry::Value(value) => CompletionSuggestion {
					label: name.to_string(),
					kind: symbol_kind_from_checked_type(&value.type_),
					detail: Some(value.type_.to_string()),
				},
				ContextEntry::Impl { parent, .. } => CompletionSuggestion {
					label: name.to_string(),
					kind: symbol_kind_from_checked_type(&parent.type_),
					detail: Some(parent.type_.to_string()),
				},
			})
			.collect(),
		_ => Vec::new(),
	}
}

fn completion_suggestions_from_struct_members(
	members: &std::sync::Arc<
		std::collections::BTreeMap<EcoString, nymph_compiler::types::StructMember>,
	>,
) -> Vec<CompletionSuggestion> {
	members
		.iter()
		.map(|(name, member)| CompletionSuggestion {
			label: name.to_string(),
			kind: symbol_kind_from_checked_type(&member.type_),
			detail: Some(member.type_.to_string()),
		})
		.collect()
}

fn symbol_kind_from_checked_type(type_: &CheckedType) -> SymbolKind {
	match type_ {
		CheckedType::Function { .. } => SymbolKind::Function,
		CheckedType::Struct { .. } => SymbolKind::Struct,
		CheckedType::Enum { .. } => SymbolKind::Enum,
		CheckedType::Interface { .. } => SymbolKind::Interface,
		CheckedType::Module { .. } => SymbolKind::Module,
		_ => SymbolKind::Variable,
	}
}

fn collect_completion_suggestions_in_expr(
	expr: &Expr,
	offset: usize,
	suggestions: &mut Vec<CompletionSuggestion>,
) {
	match expr {
		Expr::Block { body, .. } => {
			for statement in body {
				match &statement.0 {
					Statement::Let { meta, value } => {
						if statement.1.start <= offset {
							push_pattern_completion_suggestions(&meta.name.0, suggestions, SymbolKind::Variable);
						}
						if value.1.start <= offset && offset <= value.1.end {
							collect_completion_suggestions_in_expr(&value.0, offset, suggestions);
						}
					}
					Statement::Expr(expr) => {
						if expr.1.start <= offset && offset <= expr.1.end {
							collect_completion_suggestions_in_expr(&expr.0, offset, suggestions);
						}
					}
				}
			}
		}
		Expr::Closure { params, body, .. } => {
			if body.1.start <= offset && offset <= body.1.end {
				for param in params {
					push_pattern_completion_suggestions(&param.0.name.0, suggestions, SymbolKind::Parameter);
				}
				collect_completion_suggestions_in_expr(&body.0, offset, suggestions);
			}
		}
		Expr::For {
			variable,
			iterable,
			body,
			..
		} => {
			if iterable.1.start <= offset && offset <= iterable.1.end {
				collect_completion_suggestions_in_expr(&iterable.0, offset, suggestions);
			}
			if body.1.start <= offset && offset <= body.1.end {
				push_pattern_completion_suggestions(&variable.0, suggestions, SymbolKind::Variable);
				collect_completion_suggestions_in_expr(&body.0, offset, suggestions);
			}
		}
		Expr::Match { value, arms } => {
			if value.1.start <= offset && offset <= value.1.end {
				collect_completion_suggestions_in_expr(&value.0, offset, suggestions);
			}
			for arm in arms {
				if arm.body.1.start <= offset && offset <= arm.body.1.end {
					push_pattern_completion_suggestions(&arm.pattern.0, suggestions, SymbolKind::Variable);
					collect_completion_suggestions_in_expr(&arm.body.0, offset, suggestions);
				}
			}
		}
		Expr::If {
			condition,
			then,
			otherwise,
		} => {
			if condition.1.start <= offset && offset <= condition.1.end {
				collect_completion_suggestions_in_expr(&condition.0, offset, suggestions);
			}
			if then.1.start <= offset && offset <= then.1.end {
				collect_completion_suggestions_in_expr(&then.0, offset, suggestions);
			}
			if let Some(otherwise) = otherwise
				&& otherwise.1.start <= offset
				&& offset <= otherwise.1.end
			{
				collect_completion_suggestions_in_expr(&otherwise.0, offset, suggestions);
			}
		}
		Expr::While {
			condition, body, ..
		} => {
			if condition.1.start <= offset && offset <= condition.1.end {
				collect_completion_suggestions_in_expr(&condition.0, offset, suggestions);
			}
			if body.1.start <= offset && offset <= body.1.end {
				collect_completion_suggestions_in_expr(&body.0, offset, suggestions);
			}
		}
		Expr::Call { func, args, .. } => {
			if func.1.start <= offset && offset <= func.1.end {
				collect_completion_suggestions_in_expr(&func.0, offset, suggestions);
			}
			for arg in args {
				if arg.0.value.1.start <= offset && offset <= arg.0.value.1.end {
					collect_completion_suggestions_in_expr(&arg.0.value.0, offset, suggestions);
				}
			}
		}
		Expr::BinaryOp { lhs, rhs, .. } | Expr::AssignOp { lhs, rhs, .. } => {
			if lhs.1.start <= offset && offset <= lhs.1.end {
				collect_completion_suggestions_in_expr(&lhs.0, offset, suggestions);
			}
			if rhs.1.start <= offset && offset <= rhs.1.end {
				collect_completion_suggestions_in_expr(&rhs.0, offset, suggestions);
			}
		}
		Expr::PrefixOp { value, .. } | Expr::PostfixOp { value, .. } | Expr::Grouped(value) => {
			if value.1.start <= offset && offset <= value.1.end {
				collect_completion_suggestions_in_expr(&value.0, offset, suggestions);
			}
		}
		Expr::TypeOp { lhs, .. } | Expr::PatternOp { lhs, .. } => {
			if lhs.1.start <= offset && offset <= lhs.1.end {
				collect_completion_suggestions_in_expr(&lhs.0, offset, suggestions);
			}
		}
		Expr::MemberAccess { parent, .. } => {
			if parent.1.start <= offset && offset <= parent.1.end {
				collect_completion_suggestions_in_expr(&parent.0, offset, suggestions);
			}
		}
		Expr::IndexAccess { parent, index, .. } => {
			if parent.1.start <= offset && offset <= parent.1.end {
				collect_completion_suggestions_in_expr(&parent.0, offset, suggestions);
			}
			if index.1.start <= offset && offset <= index.1.end {
				collect_completion_suggestions_in_expr(&index.0, offset, suggestions);
			}
		}
		Expr::List(items) | Expr::Tuple(items) => {
			for item in items {
				let expr = match &item.0 {
					nymph_compiler::ast::expr::ListItem::Expr(expr)
					| nymph_compiler::ast::expr::ListItem::Spread(expr) => expr,
				};
				if expr.1.start <= offset && offset <= expr.1.end {
					collect_completion_suggestions_in_expr(&expr.0, offset, suggestions);
				}
			}
		}
		Expr::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					nymph_compiler::ast::expr::MapEntry::Expr(key, value) => {
						if key.1.start <= offset && offset <= key.1.end {
							collect_completion_suggestions_in_expr(&key.0, offset, suggestions);
						}
						if value.1.start <= offset && offset <= value.1.end {
							collect_completion_suggestions_in_expr(&value.0, offset, suggestions);
						}
					}
					nymph_compiler::ast::expr::MapEntry::Spread(expr) => {
						if expr.1.start <= offset && offset <= expr.1.end {
							collect_completion_suggestions_in_expr(&expr.0, offset, suggestions);
						}
					}
				}
			}
		}
		Expr::String(parts) => {
			for part in parts {
				if let nymph_compiler::ast::expr::StringPart::InterpolatedExpr(expr) = &part.0
					&& expr.1.start <= offset
					&& offset <= expr.1.end
				{
					collect_completion_suggestions_in_expr(&expr.0, offset, suggestions);
				}
			}
		}
		Expr::Return { value, .. } | Expr::Break { value, .. } => {
			if let Some(value) = value
				&& value.1.start <= offset
				&& offset <= value.1.end
			{
				collect_completion_suggestions_in_expr(&value.0, offset, suggestions);
			}
		}
		Expr::Range(range) => match range {
			nymph_compiler::ast::expr::RangeKind::From(expr)
			| nymph_compiler::ast::expr::RangeKind::To(expr)
			| nymph_compiler::ast::expr::RangeKind::ToInclusive(expr) => {
				if expr.1.start <= offset && offset <= expr.1.end {
					collect_completion_suggestions_in_expr(&expr.0, offset, suggestions);
				}
			}
			nymph_compiler::ast::expr::RangeKind::Exclusive { min, max }
			| nymph_compiler::ast::expr::RangeKind::Inclusive { min, max } => {
				if min.1.start <= offset && offset <= min.1.end {
					collect_completion_suggestions_in_expr(&min.0, offset, suggestions);
				}
				if max.1.start <= offset && offset <= max.1.end {
					collect_completion_suggestions_in_expr(&max.0, offset, suggestions);
				}
			}
		},
		Expr::Int(_)
		| Expr::UInt(_)
		| Expr::Float(_)
		| Expr::Char(_)
		| Expr::Boolean(_)
		| Expr::AnonymousParam(_)
		| Expr::Identifier(_)
		| Expr::This
		| Expr::Placeholder
		| Expr::Continue { .. } => {}
	}
}

fn push_pattern_completion_suggestions(
	pattern: &nymph_compiler::ast::expr::Pattern,
	suggestions: &mut Vec<CompletionSuggestion>,
	kind: SymbolKind,
) {
	use nymph_compiler::ast::expr::Pattern;

	match pattern {
		Pattern::Binding { name, inner } => {
			suggestions.push(CompletionSuggestion {
				label: name.0.to_string(),
				kind,
				detail: None,
			});
			push_pattern_completion_suggestions(&inner.0, suggestions, kind);
		}
		Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
			suggestions.push(CompletionSuggestion {
				label: path[0].0.to_string(),
				kind,
				detail: None,
			});
		}
		Pattern::List(items) | Pattern::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListPatternEntry::Item(pattern) => {
						push_pattern_completion_suggestions(&pattern.0, suggestions, kind);
					}
					ListPatternEntry::Rest(Some(name)) => suggestions.push(CompletionSuggestion {
						label: name.0.to_string(),
						kind,
						detail: None,
					}),
					ListPatternEntry::Rest(None) => {}
				}
			}
		}
		Pattern::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapPatternEntry::Entry(_, value) => {
						push_pattern_completion_suggestions(&value.0, suggestions, kind);
					}
					MapPatternEntry::Rest(Some(name)) => suggestions.push(CompletionSuggestion {
						label: name.0.to_string(),
						kind,
						detail: None,
					}),
					MapPatternEntry::Rest(None) => {}
				}
			}
		}
		Pattern::Struct { fields, .. } => {
			for field in fields {
				match &field.0 {
					StructPatternField::Named(name) => suggestions.push(CompletionSuggestion {
						label: name.0.to_string(),
						kind,
						detail: None,
					}),
					StructPatternField::Value { value, .. } => {
						push_pattern_completion_suggestions(&value.0, suggestions, kind);
					}
					StructPatternField::Rest => {}
				}
			}
		}
		Pattern::Union(left, right) => {
			push_pattern_completion_suggestions(&left.0, suggestions, kind);
			push_pattern_completion_suggestions(&right.0, suggestions, kind);
		}
		Pattern::Grouped(inner) => push_pattern_completion_suggestions(&inner.0, suggestions, kind),
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Range(_)
		| Pattern::Placeholder => {}
	}
}

#[derive(Debug, Clone)]
struct LocalBinding {
	name: EcoString,
	target: DefinitionTarget,
}

fn collect_top_level_definitions(
	module: &Module,
	doc: &Document,
) -> std::collections::HashMap<EcoString, DefinitionTarget> {
	let mut definitions = std::collections::HashMap::new();

	for decl in &module.members {
		match decl {
			Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
				if let Some(binding) = pattern_binding_ident(&meta.name.0) {
					definitions.insert(binding.0.clone(), target_for_ident(doc, binding));
				}
			}
			Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
				definitions.insert(meta.name.0.clone(), target_for_ident(doc, &meta.name));
			}
			Declaration::TypeAlias { meta, .. } => {
				definitions.insert(meta.name.0.clone(), target_for_ident(doc, &meta.name));
			}
			Declaration::Struct { name, .. }
			| Declaration::Enum { name, .. }
			| Declaration::Namespace { name, .. }
			| Declaration::Interface { name, .. } => {
				definitions.insert(name.0.clone(), target_for_ident(doc, name));
			}
			Declaration::Import { root, path, idents } => {
				if let Some(imported) = idents {
					for (name, alias) in imported {
						let local_name = alias.as_ref().unwrap_or(name);
						if let Some(target) = resolve_imported_item_target(doc, root, path, &name.0) {
							definitions.insert(local_name.0.clone(), target);
						}
					}
				}
			}
			Declaration::Impl { .. } | Declaration::ImplFor { .. } => {}
		}
	}

	definitions
}

fn resolve_definition_at_offset(
	module: &Module,
	offset: usize,
	doc: &Document,
	top_level: &std::collections::HashMap<EcoString, DefinitionTarget>,
) -> Option<DefinitionTarget> {
	for decl in &module.members {
		if let Some(target) = resolve_definition_in_declaration(decl, offset, doc, top_level) {
			return Some(target);
		}
	}
	None
}

fn resolve_definition_in_declaration(
	decl: &Declaration,
	offset: usize,
	doc: &Document,
	top_level: &std::collections::HashMap<EcoString, DefinitionTarget>,
) -> Option<DefinitionTarget> {
	match decl {
		Declaration::Import { root, path, idents } => {
			resolve_definition_in_import(root, path, idents.as_deref(), offset, doc)
		}
		Declaration::Let { meta, value, .. } => {
			resolve_definition_in_let(meta, Some(&value.0), offset, doc, top_level, &[])
		}
		Declaration::ExternalLet(_, _, meta) => {
			resolve_definition_in_let(meta, None, offset, doc, top_level, &[])
		}
		Declaration::Func { meta, body, .. } => {
			if offset_in_span(offset, meta.name.1.start, meta.name.1.end) {
				return Some(target_for_ident(doc, &meta.name));
			}
			for param in &meta.params {
				if let Some(target) =
					resolve_definition_in_pattern(&param.0.name.0, offset, doc, &[], top_level)
				{
					return Some(target);
				}
				if let Some(target) = resolve_definition_in_type(&param.0.type_.0, offset, doc, top_level) {
					return Some(target);
				}
			}
			if let Some(return_type) = &meta.return_type
				&& let Some(target) = resolve_definition_in_type(&return_type.0, offset, doc, top_level)
			{
				return Some(target);
			}
			let mut scope = Vec::new();
			for param in &meta.params {
				collect_pattern_bindings(&param.0.name.0, doc, &mut scope);
			}
			resolve_definition_in_expr(&body.0, offset, doc, top_level, &scope)
		}
		Declaration::ExternalFunc(_, _, meta) => {
			if offset_in_span(offset, meta.name.1.start, meta.name.1.end) {
				return Some(target_for_ident(doc, &meta.name));
			}
			for param in &meta.params {
				if let Some(target) =
					resolve_definition_in_pattern(&param.0.name.0, offset, doc, &[], top_level)
				{
					return Some(target);
				}
				if let Some(target) = resolve_definition_in_type(&param.0.type_.0, offset, doc, top_level) {
					return Some(target);
				}
			}
			meta
				.return_type
				.as_ref()
				.and_then(|return_type| resolve_definition_in_type(&return_type.0, offset, doc, top_level))
		}
		Declaration::TypeAlias { meta, value, .. } => {
			if offset_in_span(offset, meta.name.1.start, meta.name.1.end) {
				return Some(target_for_ident(doc, &meta.name));
			}
			resolve_definition_in_type(&value.0, offset, doc, top_level)
		}
		Declaration::Struct {
			name,
			generics: _,
			fields,
			members: _,
			..
		} => {
			if offset_in_span(offset, name.1.start, name.1.end) {
				return Some(target_for_ident(doc, name));
			}
			for field in fields {
				if offset_in_span(offset, field.0.name.1.start, field.0.name.1.end) {
					return Some(target_for_ident(doc, &field.0.name));
				}
				if let Some(target) = resolve_definition_in_type(&field.0.type_.0, offset, doc, top_level) {
					return Some(target);
				}
			}
			None
		}
		Declaration::Enum {
			name,
			variants,
			members: _,
			..
		} => {
			if offset_in_span(offset, name.1.start, name.1.end) {
				return Some(target_for_ident(doc, name));
			}
			for variant in variants {
				if offset_in_span(offset, variant.0.name.1.start, variant.0.name.1.end) {
					return Some(target_for_ident(doc, &variant.0.name));
				}
				for field in &variant.0.fields {
					if offset_in_span(offset, field.0.name.1.start, field.0.name.1.end) {
						return Some(target_for_ident(doc, &field.0.name));
					}
					if let Some(target) = resolve_definition_in_type(&field.0.type_.0, offset, doc, top_level)
					{
						return Some(target);
					}
				}
			}
			None
		}
		Declaration::Namespace { name, .. } | Declaration::Interface { name, .. } => {
			offset_in_span(offset, name.1.start, name.1.end).then(|| target_for_ident(doc, name))
		}
		Declaration::Impl { type_, .. } | Declaration::ImplFor { type_, .. } => {
			resolve_definition_in_type(&type_.0, offset, doc, top_level)
		}
	}
}

fn resolve_definition_in_import(
	root: &ImportRoot,
	path: &[Ident],
	idents: Option<&[(Ident, Option<Ident>)]>,
	offset: usize,
	doc: &Document,
) -> Option<DefinitionTarget> {
	for segment in path {
		if offset_in_span(offset, segment.1.start, segment.1.end) {
			return resolve_import_module_target(doc, root, path);
		}
	}

	for (name, alias) in idents.unwrap_or(&[]) {
		if offset_in_span(offset, name.1.start, name.1.end)
			|| alias
				.as_ref()
				.is_some_and(|alias| offset_in_span(offset, alias.1.start, alias.1.end))
		{
			return resolve_imported_item_target(doc, root, path, &name.0);
		}
	}

	None
}

fn resolve_definition_in_let(
	meta: &LetDeclaration,
	value: Option<&Expr>,
	offset: usize,
	doc: &Document,
	top_level: &std::collections::HashMap<EcoString, DefinitionTarget>,
	scope: &[LocalBinding],
) -> Option<DefinitionTarget> {
	if let Some(binding) = pattern_binding_ident(&meta.name.0)
		&& offset_in_span(offset, binding.1.start, binding.1.end)
	{
		return Some(target_for_ident(doc, binding));
	}
	if let Some(type_) = &meta.type_
		&& let Some(target) = resolve_definition_in_type(&type_.0, offset, doc, top_level)
	{
		return Some(target);
	}
	if let Some(target) = resolve_definition_in_pattern(&meta.name.0, offset, doc, scope, top_level) {
		return Some(target);
	}
	value.and_then(|value| resolve_definition_in_expr(value, offset, doc, top_level, scope))
}

fn resolve_definition_in_expr(
	expr: &Expr,
	offset: usize,
	doc: &Document,
	top_level: &std::collections::HashMap<EcoString, DefinitionTarget>,
	scope: &[LocalBinding],
) -> Option<DefinitionTarget> {
	match expr {
		Expr::Identifier(ident) => {
			if !offset_in_span(offset, ident.1.start, ident.1.end) {
				return None;
			}
			scope
				.iter()
				.rev()
				.find(|binding| binding.name == ident.0)
				.map(|binding| binding.target.clone())
				.or_else(|| top_level.get(&ident.0).cloned())
		}
		Expr::AnonymousParam(_) => None,
		Expr::Call {
			func,
			args,
			generics,
		} => {
			if let Some(target) = resolve_definition_in_expr(&func.0, offset, doc, top_level, scope) {
				return Some(target);
			}
			for generic in generics {
				if let Some(target) = resolve_definition_in_type(&generic.0.value.0, offset, doc, top_level)
				{
					return Some(target);
				}
			}
			for arg in args {
				if let Some(target) =
					resolve_definition_in_expr(&arg.0.value.0, offset, doc, top_level, scope)
				{
					return Some(target);
				}
			}
			None
		}
		Expr::MemberAccess { parent, .. } => {
			resolve_definition_in_expr(&parent.0, offset, doc, top_level, scope)
		}
		Expr::IndexAccess { parent, index, .. } => {
			resolve_definition_in_expr(&parent.0, offset, doc, top_level, scope)
				.or_else(|| resolve_definition_in_expr(&index.0, offset, doc, top_level, scope))
		}
		Expr::Closure {
			params,
			return_type,
			body,
			..
		} => {
			let mut nested_scope = scope.to_vec();
			for param in params {
				if let Some(target) =
					resolve_definition_in_pattern(&param.0.name.0, offset, doc, scope, top_level)
				{
					return Some(target);
				}
				if let Some(type_) = &param.0.type_
					&& let Some(target) = resolve_definition_in_type(&type_.0, offset, doc, top_level)
				{
					return Some(target);
				}
				collect_pattern_bindings(&param.0.name.0, doc, &mut nested_scope);
			}
			if let Some(return_type) = return_type
				&& let Some(target) = resolve_definition_in_type(&return_type.0, offset, doc, top_level)
			{
				return Some(target);
			}
			resolve_definition_in_expr(&body.0, offset, doc, top_level, &nested_scope)
		}
		Expr::PrefixOp { value, .. } | Expr::PostfixOp { value, .. } | Expr::Grouped(value) => {
			resolve_definition_in_expr(&value.0, offset, doc, top_level, scope)
		}
		Expr::BinaryOp { lhs, rhs, .. } | Expr::AssignOp { lhs, rhs, .. } => {
			resolve_definition_in_expr(&lhs.0, offset, doc, top_level, scope)
				.or_else(|| resolve_definition_in_expr(&rhs.0, offset, doc, top_level, scope))
		}
		Expr::TypeOp { lhs, rhs, .. } => {
			resolve_definition_in_expr(&lhs.0, offset, doc, top_level, scope)
				.or_else(|| resolve_definition_in_type(&rhs.0, offset, doc, top_level))
		}
		Expr::PatternOp { lhs, rhs, .. } => {
			resolve_definition_in_expr(&lhs.0, offset, doc, top_level, scope)
				.or_else(|| resolve_definition_in_pattern(&rhs.0, offset, doc, scope, top_level))
		}
		Expr::Return { value, .. } | Expr::Break { value, .. } => value
			.as_ref()
			.and_then(|value| resolve_definition_in_expr(&value.0, offset, doc, top_level, scope)),
		Expr::While {
			condition, body, ..
		} => resolve_definition_in_expr(&condition.0, offset, doc, top_level, scope)
			.or_else(|| resolve_definition_in_expr(&body.0, offset, doc, top_level, scope)),
		Expr::For {
			variable,
			iterable,
			body,
			..
		} => {
			if let Some(target) =
				resolve_definition_in_pattern(&variable.0, offset, doc, scope, top_level)
			{
				return Some(target);
			}
			if let Some(target) = resolve_definition_in_expr(&iterable.0, offset, doc, top_level, scope) {
				return Some(target);
			}
			let mut nested_scope = scope.to_vec();
			collect_pattern_bindings(&variable.0, doc, &mut nested_scope);
			resolve_definition_in_expr(&body.0, offset, doc, top_level, &nested_scope)
		}
		Expr::If {
			condition,
			then,
			otherwise,
		} => resolve_definition_in_expr(&condition.0, offset, doc, top_level, scope)
			.or_else(|| resolve_definition_in_expr(&then.0, offset, doc, top_level, scope))
			.or_else(|| {
				otherwise.as_ref().and_then(|otherwise| {
					resolve_definition_in_expr(&otherwise.0, offset, doc, top_level, scope)
				})
			}),
		Expr::Match { value, arms } => {
			if let Some(target) = resolve_definition_in_expr(&value.0, offset, doc, top_level, scope) {
				return Some(target);
			}
			for arm in arms {
				if let Some(target) =
					resolve_definition_in_pattern(&arm.pattern.0, offset, doc, scope, top_level)
				{
					return Some(target);
				}
				let mut arm_scope = scope.to_vec();
				collect_pattern_bindings(&arm.pattern.0, doc, &mut arm_scope);
				if let Some(guard) = &arm.guard
					&& let Some(target) =
						resolve_definition_in_expr(&guard.0, offset, doc, top_level, &arm_scope)
				{
					return Some(target);
				}
				if let Some(target) =
					resolve_definition_in_expr(&arm.body.0, offset, doc, top_level, &arm_scope)
				{
					return Some(target);
				}
			}
			None
		}
		Expr::Block { body, .. } => {
			let mut block_scope = scope.to_vec();
			for statement in body {
				if let Some(target) =
					resolve_definition_in_statement(&statement.0, offset, doc, top_level, &block_scope)
				{
					return Some(target);
				}
				if let Statement::Let { meta, .. } = &statement.0 {
					collect_pattern_bindings(&meta.name.0, doc, &mut block_scope);
				}
			}
			None
		}
		Expr::List(items) | Expr::Tuple(items) => {
			for item in items {
				let expr = match &item.0 {
					nymph_compiler::ast::expr::ListItem::Expr(expr)
					| nymph_compiler::ast::expr::ListItem::Spread(expr) => &expr.0,
				};
				if let Some(target) = resolve_definition_in_expr(expr, offset, doc, top_level, scope) {
					return Some(target);
				}
			}
			None
		}
		Expr::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					nymph_compiler::ast::expr::MapEntry::Expr(key, value) => {
						if let Some(target) = resolve_definition_in_expr(&key.0, offset, doc, top_level, scope)
						{
							return Some(target);
						}
						if let Some(target) =
							resolve_definition_in_expr(&value.0, offset, doc, top_level, scope)
						{
							return Some(target);
						}
					}
					nymph_compiler::ast::expr::MapEntry::Spread(expr) => {
						if let Some(target) = resolve_definition_in_expr(&expr.0, offset, doc, top_level, scope)
						{
							return Some(target);
						}
					}
				}
			}
			None
		}
		Expr::String(parts) => {
			for part in parts {
				if let nymph_compiler::ast::expr::StringPart::InterpolatedExpr(expr) = &part.0
					&& let Some(target) = resolve_definition_in_expr(&expr.0, offset, doc, top_level, scope)
				{
					return Some(target);
				}
			}
			None
		}
		Expr::Range(range) => match range {
			nymph_compiler::ast::expr::RangeKind::From(expr)
			| nymph_compiler::ast::expr::RangeKind::To(expr)
			| nymph_compiler::ast::expr::RangeKind::ToInclusive(expr) => {
				resolve_definition_in_expr(&expr.0, offset, doc, top_level, scope)
			}
			nymph_compiler::ast::expr::RangeKind::Exclusive { min, max }
			| nymph_compiler::ast::expr::RangeKind::Inclusive { min, max } => {
				resolve_definition_in_expr(&min.0, offset, doc, top_level, scope)
					.or_else(|| resolve_definition_in_expr(&max.0, offset, doc, top_level, scope))
			}
		},
		Expr::Int(_)
		| Expr::UInt(_)
		| Expr::Float(_)
		| Expr::Char(_)
		| Expr::Boolean(_)
		| Expr::Continue { .. }
		| Expr::This
		| Expr::Placeholder => None,
	}
}

fn resolve_definition_in_statement(
	stmt: &Statement,
	offset: usize,
	doc: &Document,
	top_level: &std::collections::HashMap<EcoString, DefinitionTarget>,
	scope: &[LocalBinding],
) -> Option<DefinitionTarget> {
	match stmt {
		Statement::Expr(expr) => resolve_definition_in_expr(&expr.0, offset, doc, top_level, scope),
		Statement::Let { meta, value } => {
			resolve_definition_in_let(meta, Some(&value.0), offset, doc, top_level, scope)
		}
	}
}

fn resolve_definition_in_pattern(
	pattern: &nymph_compiler::ast::expr::Pattern,
	offset: usize,
	doc: &Document,
	scope: &[LocalBinding],
	top_level: &std::collections::HashMap<EcoString, DefinitionTarget>,
) -> Option<DefinitionTarget> {
	use nymph_compiler::ast::expr::Pattern;

	match pattern {
		Pattern::Binding { name, inner } => {
			if offset_in_span(offset, name.1.start, name.1.end) {
				return Some(target_for_ident(doc, name));
			}
			resolve_definition_in_pattern(&inner.0, offset, doc, scope, top_level)
		}
		Pattern::Struct { path, fields } => {
			if path.len() == 1 && fields.is_empty() {
				let ident = &path[0];
				if offset_in_span(offset, ident.1.start, ident.1.end) {
					return Some(target_for_ident(doc, ident));
				}
				return None;
			}
			for segment in path {
				if offset_in_span(offset, segment.1.start, segment.1.end) {
					return top_level.get(&segment.0).cloned();
				}
			}
			for field in fields {
				match &field.0 {
					StructPatternField::Value { value, .. } => {
						if let Some(target) =
							resolve_definition_in_pattern(&value.0, offset, doc, scope, top_level)
						{
							return Some(target);
						}
					}
					StructPatternField::Named(name) => {
						if offset_in_span(offset, name.1.start, name.1.end) {
							return Some(target_for_ident(doc, name));
						}
					}
					StructPatternField::Rest => {}
				}
			}
			None
		}
		Pattern::List(items) | Pattern::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListPatternEntry::Item(pattern) => {
						if let Some(target) =
							resolve_definition_in_pattern(&pattern.0, offset, doc, scope, top_level)
						{
							return Some(target);
						}
					}
					ListPatternEntry::Rest(Some(name)) => {
						if offset_in_span(offset, name.1.start, name.1.end) {
							return Some(target_for_ident(doc, name));
						}
					}
					ListPatternEntry::Rest(None) => {}
				}
			}
			None
		}
		Pattern::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapPatternEntry::Entry(key, value) => {
						if let Some(target) =
							resolve_definition_in_pattern(&key.0, offset, doc, scope, top_level)
						{
							return Some(target);
						}
						if let Some(target) =
							resolve_definition_in_pattern(&value.0, offset, doc, scope, top_level)
						{
							return Some(target);
						}
					}
					MapPatternEntry::Rest(Some(name)) => {
						if offset_in_span(offset, name.1.start, name.1.end) {
							return Some(target_for_ident(doc, name));
						}
					}
					MapPatternEntry::Rest(None) => {}
				}
			}
			None
		}
		Pattern::Union(left, right) => {
			resolve_definition_in_pattern(&left.0, offset, doc, scope, top_level)
				.or_else(|| resolve_definition_in_pattern(&right.0, offset, doc, scope, top_level))
		}
		Pattern::Grouped(inner) => {
			resolve_definition_in_pattern(&inner.0, offset, doc, scope, top_level)
		}
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Range(_)
		| Pattern::Placeholder => None,
	}
}

fn resolve_definition_in_type(
	type_: &Type,
	offset: usize,
	_doc: &Document,
	top_level: &std::collections::HashMap<EcoString, DefinitionTarget>,
) -> Option<DefinitionTarget> {
	match type_ {
		Type::Reference { name, generics } => {
			if offset_in_span(offset, name.1.start, name.1.end) {
				return top_level.get(&name.0).cloned();
			}
			for generic in generics {
				if let Some(target) =
					resolve_definition_in_type(&generic.0.value.0, offset, _doc, top_level)
				{
					return Some(target);
				}
			}
			None
		}
		Type::List(item) | Type::Grouped(item) => {
			resolve_definition_in_type(&item.0, offset, _doc, top_level)
		}
		Type::Tuple(items) => items
			.iter()
			.find_map(|item| resolve_definition_in_type(&item.0, offset, _doc, top_level)),
		Type::Map(key, value) => resolve_definition_in_type(&key.0, offset, _doc, top_level)
			.or_else(|| resolve_definition_in_type(&value.0, offset, _doc, top_level)),
		Type::Function {
			params,
			return_type,
		} => params
			.iter()
			.find_map(|(_, param_type)| {
				resolve_definition_in_type(&param_type.0, offset, _doc, top_level)
			})
			.or_else(|| resolve_definition_in_type(&return_type.0, offset, _doc, top_level)),
		Type::Intersection(left, right) => resolve_definition_in_type(&left.0, offset, _doc, top_level)
			.or_else(|| resolve_definition_in_type(&right.0, offset, _doc, top_level)),
		Type::Int
		| Type::UInt
		| Type::Float
		| Type::Char
		| Type::String
		| Type::Boolean
		| Type::Void
		| Type::Never
		| Type::Self_
		| Type::Infer => None,
	}
}

fn collect_pattern_bindings(
	pattern: &nymph_compiler::ast::expr::Pattern,
	doc: &Document,
	bindings: &mut Vec<LocalBinding>,
) {
	use nymph_compiler::ast::expr::Pattern;

	match pattern {
		Pattern::Binding { name, inner } => {
			bindings.push(LocalBinding {
				name: name.0.clone(),
				target: target_for_ident(doc, name),
			});
			collect_pattern_bindings(&inner.0, doc, bindings);
		}
		Pattern::List(items) | Pattern::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListPatternEntry::Item(pattern) => collect_pattern_bindings(&pattern.0, doc, bindings),
					ListPatternEntry::Rest(Some(name)) => bindings.push(LocalBinding {
						name: name.0.clone(),
						target: target_for_ident(doc, name),
					}),
					ListPatternEntry::Rest(None) => {}
				}
			}
		}
		Pattern::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapPatternEntry::Entry(_, value) => collect_pattern_bindings(&value.0, doc, bindings),
					MapPatternEntry::Rest(Some(name)) => bindings.push(LocalBinding {
						name: name.0.clone(),
						target: target_for_ident(doc, name),
					}),
					MapPatternEntry::Rest(None) => {}
				}
			}
		}
		Pattern::Struct { fields, .. } => {
			if let Some(binding) = pattern_binding_ident(pattern) {
				bindings.push(LocalBinding {
					name: binding.0.clone(),
					target: target_for_ident(doc, binding),
				});
				return;
			}
			for field in fields {
				match &field.0 {
					StructPatternField::Value { value, .. } => {
						collect_pattern_bindings(&value.0, doc, bindings);
					}
					StructPatternField::Named(name) => bindings.push(LocalBinding {
						name: name.0.clone(),
						target: target_for_ident(doc, name),
					}),
					StructPatternField::Rest => {}
				}
			}
		}
		Pattern::Union(left, right) => {
			collect_pattern_bindings(&left.0, doc, bindings);
			collect_pattern_bindings(&right.0, doc, bindings);
		}
		Pattern::Grouped(inner) => collect_pattern_bindings(&inner.0, doc, bindings),
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Range(_)
		| Pattern::Placeholder => {}
	}
}

fn find_references_in_document(
	document: &Document,
	name: &str,
	target: &DefinitionTarget,
) -> Vec<ReferenceOccurrence> {
	let Some(ast) = document.ast.as_ref() else {
		return Vec::new();
	};
	let top_level = collect_top_level_definitions(&ast.0, document);
	let mut references = Vec::new();
	let mut search_start = 0usize;

	while let Some(relative) = document.content[search_start..].find(name) {
		let start = search_start + relative;
		let end = start + name.len();
		if !document.content.is_char_boundary(start) || !document.content.is_char_boundary(end) {
			search_start = end;
			continue;
		}
		let before_ok = start == 0 || {
			let before = document.content[..start].chars().next_back();
			before.is_none() || !is_identifier_char(before.expect("checked above"))
		};
		let after_ok = end == document.content.len() || {
			let after = document.content[end..].chars().next();
			after.is_none() || !is_identifier_char(after.expect("checked above"))
		};
		if before_ok
			&& after_ok
			&& let Some(resolved) = resolve_definition_at_offset(&ast.0, start, document, &top_level)
			&& &resolved == target
		{
			references.push(ReferenceOccurrence {
				uri: document.uri.clone(),
				span: Span::new(start, end),
			});
		}
		search_start = end;
	}

	references
}

fn is_identifier_char(ch: char) -> bool {
	ch.is_alphanumeric() || ch == '_'
}

fn call_site_identifier_type(expr: &Expr, ctx: &Context) -> Option<CheckedType> {
	match expr {
		Expr::Identifier(ident) => ctx.lookup_type(&ident.0),
		Expr::Grouped(inner) => call_site_identifier_type(&inner.0, ctx),
		_ => None,
	}
}

fn find_call_site_at_offset(
	module: &Module,
	offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<CallSite> {
	for decl in &module.members {
		if let Some(call_site) = find_call_site_in_declaration(decl, offset, document, ctx) {
			return Some(call_site);
		}
	}
	None
}

fn find_call_site_in_declaration(
	decl: &Declaration,
	offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<CallSite> {
	match decl {
		Declaration::Let { value, .. } => find_call_site_in_spanned_expr(value, offset, document, ctx),
		Declaration::Func { meta, body, .. } => {
			let body_ctx = function_body_context(meta, ctx);
			find_call_site_in_spanned_expr(body, offset, document, Some(&body_ctx))
		}
		Declaration::TypeAlias { .. }
		| Declaration::Struct { .. }
		| Declaration::Enum { .. }
		| Declaration::Namespace { .. }
		| Declaration::Interface { .. }
		| Declaration::Impl { .. }
		| Declaration::ImplFor { .. }
		| Declaration::Import { .. }
		| Declaration::ExternalLet(_, _, _)
		| Declaration::ExternalFunc(_, _, _) => None,
	}
}

fn find_call_site_in_spanned_expr(
	expr: &Spanned<Expr>,
	offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<CallSite> {
	if offset < expr.1.start || offset > expr.1.end {
		return None;
	}
	find_call_site_in_expr(&expr.0, expr.1, offset, document, ctx)
}

fn find_call_site_in_expr(
	expr: &Expr,
	span: Span,
	offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<CallSite> {
	match expr {
		Expr::Call { func, args, .. } => {
			if let Some(call_site) = find_call_site_in_spanned_expr(func, offset, document, ctx) {
				return Some(call_site);
			}
			for arg in args {
				if let Some(call_site) = find_call_site_in_spanned_expr(&arg.0.value, offset, document, ctx)
				{
					return Some(call_site);
				}
			}
			if span.start <= offset && offset <= span.end {
				return Some(CallSite {
					func: (**func).clone(),
					active_parameter: active_parameter_for_call(args, offset),
					ctx: ctx.cloned().unwrap_or_default(),
				});
			}
			None
		}
		Expr::Block { body, .. } => {
			let mut block_ctx = ctx.cloned().unwrap_or_default();
			for statement in body {
				if offset < statement.1.start || offset > statement.1.end {
					if let Statement::Let { meta, value } = &statement.0
						&& statement.1.start < offset
					{
						extend_context_with_let_binding(&mut block_ctx, meta, &value.0, document);
					}
					continue;
				}
				if let Some(call_site) = find_call_site_in_statement(
					&statement.0,
					statement.1,
					offset,
					document,
					Some(&block_ctx),
				) {
					return Some(call_site);
				}
				if let Statement::Let { meta, value } = &statement.0 {
					extend_context_with_let_binding(&mut block_ctx, meta, &value.0, document);
				}
			}
			None
		}
		Expr::Closure {
			params,
			body,
			return_type: _,
			..
		} => {
			let mut closure_ctx = ctx.cloned().unwrap_or_default();
			for param in params {
				if let Some(type_) = param
					.0
					.type_
					.as_ref()
					.and_then(|type_| resolve_hover_ast_type(&type_.0, Some(&closure_ctx)))
					&& let Some(binding) = pattern_binding_ident(&param.0.name.0)
				{
					closure_ctx.insert_entry(
						binding.0.clone(),
						ContextEntry::Value(ContextValue {
							type_,
							mutable: param.0.mutable,
							visibility: Visibility::Private,
						}),
					);
				}
			}
			find_call_site_in_spanned_expr(body, offset, document, Some(&closure_ctx))
		}
		Expr::If {
			condition,
			then,
			otherwise,
		} => find_call_site_in_spanned_expr(condition, offset, document, ctx)
			.or_else(|| find_call_site_in_spanned_expr(then, offset, document, ctx))
			.or_else(|| {
				otherwise
					.as_ref()
					.and_then(|otherwise| find_call_site_in_spanned_expr(otherwise, offset, document, ctx))
			}),
		Expr::While {
			condition, body, ..
		} => find_call_site_in_spanned_expr(condition, offset, document, ctx)
			.or_else(|| find_call_site_in_spanned_expr(body, offset, document, ctx)),
		Expr::For {
			variable,
			iterable,
			body,
			..
		} => {
			if let Some(call_site) = find_call_site_in_spanned_expr(iterable, offset, document, ctx) {
				return Some(call_site);
			}
			let mut loop_ctx = ctx.cloned().unwrap_or_default();
			if let Some(binding) = pattern_binding_ident(&variable.0) {
				loop_ctx.insert_entry(
					binding.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: CheckedType::Never,
						mutable: false,
						visibility: Visibility::Private,
					}),
				);
			}
			find_call_site_in_spanned_expr(body, offset, document, Some(&loop_ctx))
		}
		Expr::Match { value, arms } => {
			if let Some(call_site) = find_call_site_in_spanned_expr(value, offset, document, ctx) {
				return Some(call_site);
			}
			for arm in arms {
				if let Some(call_site) = find_call_site_in_spanned_expr(&arm.body, offset, document, ctx) {
					return Some(call_site);
				}
			}
			None
		}
		Expr::BinaryOp { lhs, rhs, .. } | Expr::AssignOp { lhs, rhs, .. } => {
			find_call_site_in_spanned_expr(lhs, offset, document, ctx)
				.or_else(|| find_call_site_in_spanned_expr(rhs, offset, document, ctx))
		}
		Expr::PrefixOp { value, .. } | Expr::PostfixOp { value, .. } | Expr::Grouped(value) => {
			find_call_site_in_spanned_expr(value, offset, document, ctx)
		}
		Expr::TypeOp { lhs, .. } | Expr::PatternOp { lhs, .. } => {
			find_call_site_in_spanned_expr(lhs, offset, document, ctx)
		}
		Expr::MemberAccess { parent, .. } => {
			find_call_site_in_spanned_expr(parent, offset, document, ctx)
		}
		Expr::IndexAccess { parent, index, .. } => {
			find_call_site_in_spanned_expr(parent, offset, document, ctx)
				.or_else(|| find_call_site_in_spanned_expr(index, offset, document, ctx))
		}
		Expr::List(items) | Expr::Tuple(items) => items.iter().find_map(|item| match &item.0 {
			nymph_compiler::ast::expr::ListItem::Expr(expr)
			| nymph_compiler::ast::expr::ListItem::Spread(expr) => {
				find_call_site_in_spanned_expr(expr, offset, document, ctx)
			}
		}),
		Expr::Map(entries) => entries.iter().find_map(|entry| match &entry.0 {
			nymph_compiler::ast::expr::MapEntry::Expr(key, value) => {
				find_call_site_in_spanned_expr(key, offset, document, ctx)
					.or_else(|| find_call_site_in_spanned_expr(value, offset, document, ctx))
			}
			nymph_compiler::ast::expr::MapEntry::Spread(expr) => {
				find_call_site_in_spanned_expr(expr, offset, document, ctx)
			}
		}),
		Expr::String(parts) => parts.iter().find_map(|part| match &part.0 {
			nymph_compiler::ast::expr::StringPart::InterpolatedExpr(expr) => {
				find_call_site_in_spanned_expr(expr, offset, document, ctx)
			}
			_ => None,
		}),
		Expr::Return { value, .. } | Expr::Break { value, .. } => value
			.as_ref()
			.and_then(|value| find_call_site_in_spanned_expr(value, offset, document, ctx)),
		Expr::Range(range) => match range {
			nymph_compiler::ast::expr::RangeKind::From(expr)
			| nymph_compiler::ast::expr::RangeKind::To(expr)
			| nymph_compiler::ast::expr::RangeKind::ToInclusive(expr) => {
				find_call_site_in_spanned_expr(expr, offset, document, ctx)
			}
			nymph_compiler::ast::expr::RangeKind::Exclusive { min, max }
			| nymph_compiler::ast::expr::RangeKind::Inclusive { min, max } => {
				find_call_site_in_spanned_expr(min, offset, document, ctx)
					.or_else(|| find_call_site_in_spanned_expr(max, offset, document, ctx))
			}
		},
		Expr::Int(_)
		| Expr::UInt(_)
		| Expr::Float(_)
		| Expr::Char(_)
		| Expr::Boolean(_)
		| Expr::AnonymousParam(_)
		| Expr::Identifier(_)
		| Expr::This
		| Expr::Placeholder
		| Expr::Continue { .. } => None,
	}
}

fn find_call_site_in_statement(
	statement: &Statement,
	span: Span,
	offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<CallSite> {
	if offset < span.start || offset > span.end {
		return None;
	}
	match statement {
		Statement::Expr(expr) => find_call_site_in_spanned_expr(expr, offset, document, ctx),
		Statement::Let { value, .. } => find_call_site_in_spanned_expr(value, offset, document, ctx),
	}
}

fn active_parameter_for_call(
	args: &[Spanned<nymph_compiler::ast::expr::CallArg>],
	offset: usize,
) -> usize {
	if args.is_empty() {
		return 0;
	}

	for (index, arg) in args.iter().enumerate() {
		if offset <= arg.1.end {
			return index;
		}
	}

	args.len().saturating_sub(1)
}

fn find_member_access_site_at_offset(
	module: &Module,
	offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<MemberAccessSite> {
	for decl in &module.members {
		if let Some(site) = find_member_access_site_in_declaration(decl, offset, document, ctx) {
			return Some(site);
		}
	}
	None
}

fn find_member_access_site_in_declaration(
	decl: &Declaration,
	offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<MemberAccessSite> {
	match decl {
		Declaration::Let { value, .. } => {
			find_member_access_site_in_spanned_expr(value, offset, document, ctx)
		}
		Declaration::Func { meta, body, .. } => {
			let body_ctx = function_body_context(meta, ctx);
			find_member_access_site_in_spanned_expr(body, offset, document, Some(&body_ctx))
		}
		_ => None,
	}
}

fn find_member_access_site_in_spanned_expr(
	expr: &Spanned<Expr>,
	offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<MemberAccessSite> {
	if offset < expr.1.start || offset > expr.1.end {
		return None;
	}
	find_member_access_site_in_expr(&expr.0, expr.1, offset, document, ctx)
}

fn find_member_access_site_in_expr(
	expr: &Expr,
	span: Span,
	document_offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<MemberAccessSite> {
	match expr {
		Expr::MemberAccess { parent, member, .. } => {
			if let Some(site) =
				find_member_access_site_in_spanned_expr(parent, document_offset, document, ctx)
			{
				return Some(site);
			}
			if document_offset >= member.1.start && document_offset <= member.1.end {
				return Some(MemberAccessSite {
					parent: (**parent).clone(),
					ctx: ctx.cloned().unwrap_or_default(),
				});
			}
			None
		}
		Expr::Block { body, .. } => {
			let mut block_ctx = ctx.cloned().unwrap_or_default();
			for statement in body {
				if document_offset < statement.1.start || document_offset > statement.1.end {
					if let Statement::Let { meta, value } = &statement.0
						&& statement.1.start < document_offset
					{
						extend_context_with_let_binding(&mut block_ctx, meta, &value.0, document);
					}
					continue;
				}
				if let Some(site) = find_member_access_site_in_statement(
					&statement.0,
					statement.1,
					document_offset,
					document,
					Some(&block_ctx),
				) {
					return Some(site);
				}
				if let Statement::Let { meta, value } = &statement.0 {
					extend_context_with_let_binding(&mut block_ctx, meta, &value.0, document);
				}
			}
			None
		}
		Expr::Closure { params, body, .. } => {
			let mut closure_ctx = ctx.cloned().unwrap_or_default();
			for param in params {
				if let Some(type_) = param
					.0
					.type_
					.as_ref()
					.and_then(|type_| resolve_hover_ast_type(&type_.0, Some(&closure_ctx)))
					&& let Some(binding) = pattern_binding_ident(&param.0.name.0)
				{
					closure_ctx.insert_entry(
						binding.0.clone(),
						ContextEntry::Value(ContextValue {
							type_,
							mutable: param.0.mutable,
							visibility: Visibility::Private,
						}),
					);
				}
			}
			find_member_access_site_in_spanned_expr(body, document_offset, document, Some(&closure_ctx))
		}
		Expr::Call { func, args, .. } => {
			find_member_access_site_in_spanned_expr(func, document_offset, document, ctx).or_else(|| {
				args.iter().find_map(|arg| {
					find_member_access_site_in_spanned_expr(&arg.0.value, document_offset, document, ctx)
				})
			})
		}
		Expr::If {
			condition,
			then,
			otherwise,
		} => find_member_access_site_in_spanned_expr(condition, document_offset, document, ctx)
			.or_else(|| find_member_access_site_in_spanned_expr(then, document_offset, document, ctx))
			.or_else(|| {
				otherwise.as_ref().and_then(|otherwise| {
					find_member_access_site_in_spanned_expr(otherwise, document_offset, document, ctx)
				})
			}),
		Expr::While {
			condition, body, ..
		} => find_member_access_site_in_spanned_expr(condition, document_offset, document, ctx)
			.or_else(|| find_member_access_site_in_spanned_expr(body, document_offset, document, ctx)),
		Expr::For {
			variable,
			iterable,
			body,
			..
		} => {
			if let Some(site) =
				find_member_access_site_in_spanned_expr(iterable, document_offset, document, ctx)
			{
				return Some(site);
			}
			let mut loop_ctx = ctx.cloned().unwrap_or_default();
			if let Some(binding) = pattern_binding_ident(&variable.0) {
				loop_ctx.insert_entry(
					binding.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: CheckedType::Never,
						mutable: false,
						visibility: Visibility::Private,
					}),
				);
			}
			find_member_access_site_in_spanned_expr(body, document_offset, document, Some(&loop_ctx))
		}
		Expr::Match { value, arms } => {
			if let Some(site) =
				find_member_access_site_in_spanned_expr(value, document_offset, document, ctx)
			{
				return Some(site);
			}
			for arm in arms {
				if let Some(site) =
					find_member_access_site_in_spanned_expr(&arm.body, document_offset, document, ctx)
				{
					return Some(site);
				}
			}
			None
		}
		Expr::BinaryOp { lhs, rhs, .. } | Expr::AssignOp { lhs, rhs, .. } => {
			find_member_access_site_in_spanned_expr(lhs, document_offset, document, ctx)
				.or_else(|| find_member_access_site_in_spanned_expr(rhs, document_offset, document, ctx))
		}
		Expr::PrefixOp { value, .. } | Expr::PostfixOp { value, .. } | Expr::Grouped(value) => {
			find_member_access_site_in_spanned_expr(value, document_offset, document, ctx)
		}
		Expr::TypeOp { lhs, .. } | Expr::PatternOp { lhs, .. } => {
			find_member_access_site_in_spanned_expr(lhs, document_offset, document, ctx)
		}
		Expr::IndexAccess { parent, index, .. } => {
			find_member_access_site_in_spanned_expr(parent, document_offset, document, ctx)
				.or_else(|| find_member_access_site_in_spanned_expr(index, document_offset, document, ctx))
		}
		Expr::List(items) | Expr::Tuple(items) => items.iter().find_map(|item| match &item.0 {
			nymph_compiler::ast::expr::ListItem::Expr(expr)
			| nymph_compiler::ast::expr::ListItem::Spread(expr) => {
				find_member_access_site_in_spanned_expr(expr, document_offset, document, ctx)
			}
		}),
		Expr::Map(entries) => entries.iter().find_map(|entry| match &entry.0 {
			nymph_compiler::ast::expr::MapEntry::Expr(key, value) => {
				find_member_access_site_in_spanned_expr(key, document_offset, document, ctx).or_else(|| {
					find_member_access_site_in_spanned_expr(value, document_offset, document, ctx)
				})
			}
			nymph_compiler::ast::expr::MapEntry::Spread(expr) => {
				find_member_access_site_in_spanned_expr(expr, document_offset, document, ctx)
			}
		}),
		Expr::String(parts) => parts.iter().find_map(|part| match &part.0 {
			nymph_compiler::ast::expr::StringPart::InterpolatedExpr(expr) => {
				find_member_access_site_in_spanned_expr(expr, document_offset, document, ctx)
			}
			_ => None,
		}),
		Expr::Return { value, .. } | Expr::Break { value, .. } => value.as_ref().and_then(|value| {
			find_member_access_site_in_spanned_expr(value, document_offset, document, ctx)
		}),
		Expr::Range(range) => match range {
			nymph_compiler::ast::expr::RangeKind::From(expr)
			| nymph_compiler::ast::expr::RangeKind::To(expr)
			| nymph_compiler::ast::expr::RangeKind::ToInclusive(expr) => {
				find_member_access_site_in_spanned_expr(expr, document_offset, document, ctx)
			}
			nymph_compiler::ast::expr::RangeKind::Exclusive { min, max }
			| nymph_compiler::ast::expr::RangeKind::Inclusive { min, max } => {
				find_member_access_site_in_spanned_expr(min, document_offset, document, ctx)
					.or_else(|| find_member_access_site_in_spanned_expr(max, document_offset, document, ctx))
			}
		},
		Expr::Int(_)
		| Expr::UInt(_)
		| Expr::Float(_)
		| Expr::Char(_)
		| Expr::Boolean(_)
		| Expr::AnonymousParam(_)
		| Expr::Identifier(_)
		| Expr::This
		| Expr::Placeholder
		| Expr::Continue { .. } => {
			let _ = span;
			None
		}
	}
}

fn find_member_access_site_in_statement(
	statement: &Statement,
	span: Span,
	document_offset: usize,
	document: &Document,
	ctx: Option<&Context>,
) -> Option<MemberAccessSite> {
	if document_offset < span.start || document_offset > span.end {
		return None;
	}
	match statement {
		Statement::Expr(expr) => {
			find_member_access_site_in_spanned_expr(expr, document_offset, document, ctx)
		}
		Statement::Let { value, .. } => {
			find_member_access_site_in_spanned_expr(value, document_offset, document, ctx)
		}
	}
}

fn target_for_ident(doc: &Document, ident: &Ident) -> DefinitionTarget {
	DefinitionTarget {
		uri: doc.uri.clone(),
		span: ident.1,
	}
}

fn resolve_import_module_target(
	doc: &Document,
	root: &ImportRoot,
	path: &[Ident],
) -> Option<DefinitionTarget> {
	let resolved = doc
		.type_checker
		.as_ref()?
		.resolve_import_path(root, path, Span::new(0, 0))
		.ok()?;
	let module = Document::load_from_path(&resolved).ok()?;
	let span = module
		.ast
		.as_ref()
		.map(|module| module.1)
		.unwrap_or(Span::new(0, 0));
	Some(DefinitionTarget {
		uri: module.uri,
		span,
	})
}

fn resolve_imported_item_target(
	doc: &Document,
	root: &ImportRoot,
	path: &[Ident],
	item_name: &str,
) -> Option<DefinitionTarget> {
	let resolved = doc
		.type_checker
		.as_ref()?
		.resolve_import_path(root, path, Span::new(0, 0))
		.ok()?;
	let module = Document::load_from_path(&resolved).ok()?;
	find_top_level_definition_by_name(&module, item_name)
}

fn find_top_level_definition_by_name(doc: &Document, name: &str) -> Option<DefinitionTarget> {
	let module = doc.ast.as_ref()?;
	for decl in &module.0.members {
		match decl {
			Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
				if let Some(binding) = pattern_binding_ident(&meta.name.0)
					&& binding.0 == name
				{
					return Some(target_for_ident(doc, binding));
				}
			}
			Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
				if meta.name.0 == name {
					return Some(target_for_ident(doc, &meta.name));
				}
			}
			Declaration::TypeAlias { meta, .. } => {
				if meta.name.0 == name {
					return Some(target_for_ident(doc, &meta.name));
				}
			}
			Declaration::Struct { name: ident, .. }
			| Declaration::Enum { name: ident, .. }
			| Declaration::Namespace { name: ident, .. }
			| Declaration::Interface { name: ident, .. } => {
				if ident.0 == name {
					return Some(target_for_ident(doc, ident));
				}
			}
			Declaration::Import { .. } | Declaration::Impl { .. } | Declaration::ImplFor { .. } => {}
		}
	}
	None
}

fn format_func_signature_with_body(meta: &FuncDeclaration, body: Option<&Expr>) -> String {
	let generics = format_generics(&meta.generics);
	let params: Vec<_> = meta
		.params
		.iter()
		.map(|p| {
			let param = &p.0.clone();
			let mut_str = if param.mutable { "mut " } else { "" };
			let name =
				pattern_binding_ident(&param.name.0).map_or_else(|| "_".to_string(), |i| i.0.to_string());
			format!("{mut_str}{name}: {}", type_to_string(&param.type_.0))
		})
		.collect();
	let ret = meta
		.return_type
		.as_ref()
		.map(|t| type_to_string(&t.0))
		.or_else(|| body.and_then(infer_expr_type))
		.unwrap_or_else(|| "void".to_string());
	format!(
		"func {}{generics}({}) -> {ret}",
		meta.name.0,
		params.join(", "),
	)
}

#[allow(dead_code)]
fn format_func_signature(meta: &FuncDeclaration) -> String {
	format_func_signature_with_body(meta, None)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::document::Document;
	use std::fs;
	use tempfile::tempdir;

	#[test]
	fn test_symbol_kind_equality() {
		assert_eq!(SymbolKind::Function, SymbolKind::Function);
		assert_ne!(SymbolKind::Function, SymbolKind::Variable);
	}

	#[test]
	fn test_location_range_equality() {
		let range1 = LocationRange {
			start_line: 1,
			start_char: 1,
			end_line: 1,
			end_char: 5,
		};
		let range2 = LocationRange {
			start_line: 1,
			start_char: 1,
			end_line: 1,
			end_char: 5,
		};
		assert_eq!(range1, range2);
	}

	#[test]
	fn test_hover_prefers_call_argument_over_receiver_this() {
		let source = r#"
struct Counter(value: int) {
	func add(delta: int) -> this.value.plus(delta)
}
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();
		analyzer.analyze(&document);

		let delta_offset = document
			.content
			.rfind("delta")
			.expect("expected delta identifier in method body");
		let (line, column) = document.position_to_line_col(delta_offset);

		let symbol = analyzer
			.get_symbol_at_position(line, column - 1, &document)
			.expect("expected hover symbol at argument position");

		assert_eq!(symbol.name, "delta");
		assert_ne!(symbol.name, "this");
	}

	#[test]
	fn test_hover_uses_type_checker_for_function_body_bindings() {
		let source = r#"
func add_one(value: int) -> {
	let next = value
	next
}
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();
		analyzer.analyze(&document);

		let next_offset = document
			.content
			.rfind("next")
			.expect("expected next identifier in function body");
		let (line, column) = document.position_to_line_col(next_offset);

		let symbol = analyzer
			.get_symbol_at_position(line, column - 1, &document)
			.expect("expected hover symbol for local binding");

		assert_eq!(symbol.name, "next");
		assert_eq!(symbol.type_info.as_deref(), Some("next: int"));
	}

	#[test]
	fn test_function_hover_preserves_parameter_names_in_signature() {
		let source = r#"
func identity(value: int) -> value
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();
		analyzer.analyze(&document);

		let func_offset = document
			.content
			.find("identity")
			.expect("expected function name");
		let (line, column) = document.position_to_line_col(func_offset);

		let symbol = analyzer
			.get_symbol_at_position(line, column - 1, &document)
			.expect("expected hover symbol for function");

		assert_eq!(symbol.name, "identity");
		assert_eq!(
			symbol.type_info.as_deref(),
			Some("func identity: (value: int) -> int")
		);
	}

	#[test]
	fn test_hover_falls_back_to_parameter_annotation_in_generic_interface_body() {
		let source = r#"
interface Iterator<Item> {
	func any(predicate: (Item) -> boolean) -> {
		predicate
	}
}
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();
		analyzer.analyze(&document);

		let predicate_offset = document
			.content
			.rfind("predicate")
			.expect("expected predicate reference in function body");
		let (line, column) = document.position_to_line_col(predicate_offset);

		let symbol = analyzer
			.get_symbol_at_position(line, column - 1, &document)
			.expect("expected hover symbol for parameter reference");

		assert_eq!(symbol.name, "predicate");
		assert_eq!(
			symbol.type_info.as_deref(),
			Some("predicate: (Item) -> boolean")
		);
	}

	#[test]
	fn test_definition_resolves_local_let_binding() {
		let source = r#"
func add_one(value: int) -> {
	let next = value
	next
}
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();

		let usage_offset = document
			.content
			.rfind("next")
			.expect("expected local binding reference");
		let position = document
			.offset_to_lsp_position(usage_offset)
			.expect("expected LSP position");

		let target = analyzer
			.get_definition_at_position(position.line, position.character, &document)
			.expect("expected definition target");

		let definition_offset = document
			.content
			.find("next =")
			.expect("expected local binding definition");
		assert_eq!(target.uri, document.uri);
		assert_eq!(target.span.start, definition_offset);
	}

	#[test]
	fn test_definition_resolves_imported_item() {
		let temp_dir = tempdir().expect("expected temp dir");
		let root = temp_dir.path();
		let src_dir = root.join("src");
		fs::create_dir_all(&src_dir).expect("expected src dir");
		fs::write(
			root.join("nymph.toml"),
			"name = 'tmp'\nversion = '0.1.0'\nnymph_version = '*'\nauthor = ['x']\n",
		)
		.expect("expected config file");
		fs::write(src_dir.join("foo.nym"), "let answer = 42\n").expect("expected module");
		fs::write(
			src_dir.join("main.nym"),
			"import ./foo with (answer)\nfunc main() -> answer\n",
		)
		.expect("expected source file");

		let document = Document::load_from_path(&src_dir.join("main.nym")).expect("expected document");
		let analyzer = SemanticAnalyzer::new();
		let usage_offset = document
			.content
			.rfind("answer")
			.expect("expected imported item usage");
		let position = document
			.offset_to_lsp_position(usage_offset)
			.expect("expected LSP position");

		let target = analyzer
			.get_definition_at_position(position.line, position.character, &document)
			.expect("expected imported definition target");

		assert!(target.uri.ends_with("/foo.nym"));
		assert_eq!(target.span.start, 4);
	}

	#[test]
	fn test_completion_includes_local_bindings_and_parameters() {
		let source = r#"
func add_one(value: int) -> {
	let next = value
	ne
}
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();
		let offset = document
			.content
			.rfind("ne")
			.expect("expected completion site");
		let position = document
			.offset_to_lsp_position(offset + 2)
			.expect("expected LSP position");

		let suggestions =
			analyzer.get_completion_suggestions(position.line, position.character, &document);

		assert!(suggestions.iter().any(|item| item.label == "next"));
		assert!(suggestions.iter().any(|item| item.label == "value"));
	}

	#[test]
	fn test_completion_includes_imported_items() {
		let source = r#"
import ./math with (sum)
func main() -> su
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();
		let offset = document
			.content
			.rfind("su")
			.expect("expected completion site");
		let position = document
			.offset_to_lsp_position(offset + 2)
			.expect("expected LSP position");

		let suggestions =
			analyzer.get_completion_suggestions(position.line, position.character, &document);

		assert!(suggestions.iter().any(|item| item.label == "sum"));
	}

	#[test]
	fn test_references_find_local_binding_declaration_and_use() {
		let source = r#"
func add_one(value: int) -> {
	let next = value
	next
}
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();
		let offset = document
			.content
			.rfind("next")
			.expect("expected local binding reference");
		let position = document
			.offset_to_lsp_position(offset)
			.expect("expected LSP position");

		let (_, _, references) = analyzer
			.find_references(
				position.line,
				position.character,
				&document,
				std::slice::from_ref(&document),
			)
			.expect("expected references");

		assert_eq!(references.len(), 2);
		assert!(
			references
				.iter()
				.all(|reference| reference.uri == document.uri)
		);
	}

	#[test]
	fn test_references_include_import_definition_and_usage() {
		let temp_dir = tempdir().expect("expected temp dir");
		let root = temp_dir.path();
		let src_dir = root.join("src");
		fs::create_dir_all(&src_dir).expect("expected src dir");
		fs::write(
			root.join("nymph.toml"),
			"name = 'tmp'\nversion = '0.1.0'\nnymph_version = '*'\nauthor = ['x']\n",
		)
		.expect("expected config file");
		fs::write(src_dir.join("foo.nym"), "let answer = 42\n").expect("expected module");
		fs::write(
			src_dir.join("main.nym"),
			"import ./foo with (answer)\nfunc main() -> answer\n",
		)
		.expect("expected source file");

		let main_document =
			Document::load_from_path(&src_dir.join("main.nym")).expect("expected document");
		let foo_document =
			Document::load_from_path(&src_dir.join("foo.nym")).expect("expected module document");
		let analyzer = SemanticAnalyzer::new();
		let offset = main_document
			.content
			.rfind("answer")
			.expect("expected imported item usage");
		let position = main_document
			.offset_to_lsp_position(offset)
			.expect("expected LSP position");

		let (_, _, references) = analyzer
			.find_references(
				position.line,
				position.character,
				&main_document,
				&[main_document.clone(), foo_document.clone()],
			)
			.expect("expected references");

		assert_eq!(references.len(), 3);
		assert!(
			references
				.iter()
				.any(|reference| reference.uri == main_document.uri)
		);
		assert!(
			references
				.iter()
				.any(|reference| reference.uri == foo_document.uri)
		);
	}

	#[test]
	fn test_signature_help_reports_function_parameters() {
		let source = r#"
func add(left: int, right: int) -> left
func main() -> add(1, 2)
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();
		let offset = document
			.content
			.find("2)")
			.expect("expected second argument position");
		let position = document
			.offset_to_lsp_position(offset)
			.expect("expected LSP position");

		let help = analyzer
			.get_signature_help(position.line, position.character, &document)
			.expect("expected signature help");

		assert_eq!(help.parameters, vec!["left: int", "right: int"]);
		assert_eq!(help.active_parameter, 1);
		assert!(help.label.contains("(left: int, right: int) -> int"));
	}

	#[test]
	fn test_completion_includes_struct_members_after_dot() {
		let source = r#"
struct Point(x: int, y: int) {}
func main() -> {
	let point = Point(1, 2)
	point.x
}
"#;
		let document = Document::new("file:///test.nym".to_string(), source.to_string());
		let analyzer = SemanticAnalyzer::new();
		let offset = document
			.content
			.rfind("x")
			.expect("expected member completion site");
		let position = document
			.offset_to_lsp_position(offset)
			.expect("expected LSP position");

		let suggestions =
			analyzer.get_completion_suggestions(position.line, position.character, &document);

		assert!(suggestions.iter().any(|item| item.label == "x"));
		assert!(suggestions.iter().any(|item| item.label == "y"));
	}
}
