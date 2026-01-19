use std::collections::HashMap;

use ecow::EcoString;

use crate::document::Document;
use nymph_compiler::ast::declaration::{
	Declaration, EnumVariant, FuncDeclaration, ImplMember, ImportRoot, InterfaceElement,
	InterfaceMember, LetDeclaration, Module, StructField, StructInnerMember,
};
use nymph_compiler::ast::expr::{ClosureParam, Expr, MatchArm, Statement};
use nymph_compiler::ast::ops::{BinaryOperator, PrefixOperator};
use nymph_compiler::ast::types::{GenericArg, GenericParam, Type};
use nymph_compiler::ast::{Ident, Spanned};
use nymph_compiler::types::Context;

/// Information about a symbol at a specific location
#[derive(Debug, Clone)]
pub struct SymbolAtLocation {
	pub name: String,
	pub kind: SymbolKind,
	pub type_info: Option<String>,
	pub range: LocationRange,
	pub definition_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Analyzer for getting type information and symbol details
pub struct SemanticAnalyzer {
	symbol_table: HashMap<String, String>,
}

impl SemanticAnalyzer {
	pub fn new() -> Self {
		Self {
			symbol_table: HashMap::new(),
		}
	}

	/// Analyze a document and build the symbol table
	pub fn analyze(&mut self, document: &Document) {
		self.symbol_table.clear();

		// Extract symbols from the AST if available
		if let Some(spanned_module) = &document.ast {
			let symbols = extract_module_symbols(spanned_module.inner());
			for symbol in symbols {
				self.symbol_table.insert(symbol.name, "symbol".to_string());
			}
		} else {
			// Fallback: Extract variable and function definitions from the document text
			// This is a placeholder that will be enhanced when AST APIs are stable
			for line in document.content.lines() {
				if let Some(pos) = line.find("let ") {
					let after_let = &line[pos + 4..];
					if let Some(eq_pos) = after_let.find('=') {
						let var_name = after_let[..eq_pos].trim();
						self
							.symbol_table
							.insert(var_name.to_string(), "variable".to_string());
					}
				}
				if let Some(pos) = line.find("func ") {
					let after_fn = &line[pos + 3..];
					if let Some(paren_pos) = after_fn.find('(') {
						let fn_name = after_fn[..paren_pos].trim();
						self
							.symbol_table
							.insert(fn_name.to_string(), "function".to_string());
					}
				}
			}
		}
	}

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
		find_symbol_at_offset(
			ast.inner(),
			offset,
			document,
			document.type_context.as_ref(),
		)
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
			if let Some(name_ident) = meta.name.inner().as_binding() {
				let name = name_ident.inner();
				symbols.push(Symbol {
					name: name.to_string(),
					kind: SymbolKind::Variable,
					start_offset: name_ident.start(),
					end_offset: name_ident.end(),
				});
			}
		}
		Declaration::ExternalLet(_, meta) => {
			if let Some(name_ident) = meta.name.inner().as_binding() {
				let name = name_ident.inner();
				symbols.push(Symbol {
					name: name.to_string(),
					kind: SymbolKind::Variable,
					start_offset: name_ident.start(),
					end_offset: name_ident.end(),
				});
			}
		}
		Declaration::Func { meta, .. } => {
			symbols.push(Symbol {
				name: meta.name.inner().to_string(),
				kind: SymbolKind::Function,
				start_offset: meta.name.start(),
				end_offset: meta.name.end(),
			});
		}
		Declaration::ExternalFunc(_, meta) => {
			symbols.push(Symbol {
				name: meta.name.inner().to_string(),
				kind: SymbolKind::Function,
				start_offset: meta.name.start(),
				end_offset: meta.name.end(),
			});
		}
		Declaration::TypeAlias { meta, .. } => {
			symbols.push(Symbol {
				name: meta.name.inner().to_string(),
				kind: SymbolKind::Type,
				start_offset: meta.name.start(),
				end_offset: meta.name.end(),
			});
		}
		Declaration::Struct { name, .. } => {
			symbols.push(Symbol {
				name: name.inner().to_string(),
				kind: SymbolKind::Type,
				start_offset: name.start(),
				end_offset: name.end(),
			});
		}
		Declaration::Enum { name, .. } => {
			symbols.push(Symbol {
				name: name.inner().to_string(),
				kind: SymbolKind::Enum,
				start_offset: name.start(),
				end_offset: name.end(),
			});
		}
		Declaration::Namespace { name, .. } => {
			symbols.push(Symbol {
				name: name.inner().to_string(),
				kind: SymbolKind::Namespace,
				start_offset: name.start(),
				end_offset: name.end(),
			});
		}
		Declaration::Interface { name, .. } => {
			symbols.push(Symbol {
				name: name.inner().to_string(),
				kind: SymbolKind::Interface,
				start_offset: name.start(),
				end_offset: name.end(),
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

/// Find a symbol in a declaration at the given offset
fn find_symbol_in_declaration(
	decl: &Declaration,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	match decl {
		Declaration::Let { meta, value, .. } => {
			find_symbol_in_let_with_value(meta, Some(value.inner()), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(value.inner(), offset, doc, ctx))
		}
		Declaration::ExternalLet(_, meta) => find_symbol_in_let(meta, offset, doc, ctx),
		Declaration::Func { meta, body, .. } => {
			find_symbol_in_func_with_body(meta, Some(body.inner()), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(body.inner(), offset, doc, ctx))
		}
		Declaration::ExternalFunc(_, meta) => find_symbol_in_func(meta, offset, doc, ctx),
		Declaration::TypeAlias { meta, value, .. } => {
			let name = meta.name.inner();
			if offset_in_span(offset, meta.name.start(), meta.name.end()) {
				let generics_str = format_generics(&meta.generics);
				return Some(make_symbol_at_location(
					name,
					SymbolKind::Type,
					Some(format!(
						"type {}{} = {}",
						name,
						generics_str,
						type_to_string(value.inner())
					)),
					meta.name.start(),
					meta.name.end(),
					doc,
				));
			}
			// Check generics
			for generic in &meta.generics {
				if let Some(sym) = find_symbol_in_generic_param(generic.inner(), offset, doc) {
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
			if offset_in_span(offset, name.start(), name.end()) {
				let generics_str = format_generics(generics);
				let fields_str = format_struct_fields(fields);
				return Some(make_symbol_at_location(
					name.inner(),
					SymbolKind::Struct,
					Some(format!(
						"struct {}{}{}",
						name.inner(),
						generics_str,
						fields_str
					)),
					name.start(),
					name.end(),
					doc,
				));
			}
			// Check generics
			for generic in generics {
				if let Some(sym) = find_symbol_in_generic_param(generic.inner(), offset, doc) {
					return Some(sym);
				}
			}
			// Check fields
			for field in fields {
				if let Some(sym) = find_symbol_in_struct_field(field.inner(), offset, doc) {
					return Some(sym);
				}
			}
			// Check inner members
			for member in members {
				if let Some(sym) = find_symbol_in_struct_inner_member(member.inner(), offset, doc, ctx) {
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
			if offset_in_span(offset, name.start(), name.end()) {
				let generics_str = format_generics(generics);
				let variants_str: Vec<_> = variants
					.iter()
					.map(|v| v.inner().name.inner().to_string())
					.collect();
				return Some(make_symbol_at_location(
					name.inner(),
					SymbolKind::Enum,
					Some(format!(
						"enum {}{} {{ {} }}",
						name.inner(),
						generics_str,
						variants_str.join(", ")
					)),
					name.start(),
					name.end(),
					doc,
				));
			}
			// Check generics
			for generic in generics {
				if let Some(sym) = find_symbol_in_generic_param(generic.inner(), offset, doc) {
					return Some(sym);
				}
			}
			// Check variants and their fields
			for variant in variants {
				if let Some(sym) = find_symbol_in_enum_variant(variant.inner(), name.inner(), offset, doc) {
					return Some(sym);
				}
			}
			// Check inner members
			for member in members {
				if let Some(sym) = find_symbol_in_struct_inner_member(member.inner(), offset, doc, ctx) {
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
			if offset_in_span(offset, name.start(), name.end()) {
				let generics_str = format_generics(generics);
				let super_str = if super_interfaces.is_empty() {
					String::new()
				} else {
					let supers: Vec<_> = super_interfaces
						.iter()
						.map(|s| s.inner().0.inner().to_string())
						.collect();
					format!(": {}", supers.join(" + "))
				};
				return Some(make_symbol_at_location(
					name.inner(),
					SymbolKind::Interface,
					Some(format!(
						"interface {}{}{}",
						name.inner(),
						generics_str,
						super_str
					)),
					name.start(),
					name.end(),
					doc,
				));
			}
			// Check generics
			for generic in generics {
				if let Some(sym) = find_symbol_in_generic_param(generic.inner(), offset, doc) {
					return Some(sym);
				}
			}
			// Check super interfaces
			for super_interface in super_interfaces {
				let (super_name, _) = super_interface.inner();
				if offset_in_span(offset, super_name.start(), super_name.end()) {
					return Some(make_symbol_at_location(
						super_name.inner(),
						SymbolKind::Interface,
						Some(format!("super interface {}", super_name.inner())),
						super_name.start(),
						super_name.end(),
						doc,
					));
				}
			}
			// Check members
			for member in members {
				if let Some(sym) = find_symbol_in_interface_member(member.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Declaration::Namespace { name, members, .. } => {
			if offset_in_span(offset, name.start(), name.end()) {
				return Some(make_symbol_at_location(
					name.inner(),
					SymbolKind::Namespace,
					Some(format!("namespace {}", name.inner())),
					name.start(),
					name.end(),
					doc,
				));
			}
			for member in members {
				if let Some(sym) = find_symbol_in_impl_member(member.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Declaration::Impl { members, .. } | Declaration::ImplFor { members, .. } => {
			for member in members {
				if let Some(sym) = find_symbol_in_impl_member(member.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Declaration::Import { root, path, idents } => {
			find_symbol_in_import(root, path, idents.as_ref(), offset, doc, ctx)
		}
	}
}

/// Find symbol in an import declaration
fn find_symbol_in_import(
	root: &ImportRoot,
	path: &[Ident],
	idents: Option<&Vec<(Ident, Option<Ident>)>>,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	// Format the import root
	let root_prefix = match root {
		ImportRoot::Package(name) => format!("{}/", name.inner()),
		ImportRoot::Project => "@/".to_string(),
		ImportRoot::Current => "./".to_string(),
		ImportRoot::Parent => "../".to_string(),
	};

	// Build the full import path string
	let path_parts: Vec<&str> = path.iter().map(|p| p.inner().as_str()).collect();
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
		if offset_in_span(offset, segment.start(), segment.end()) {
			// Build partial path up to this segment
			let partial_path: Vec<&str> = path[..=i].iter().map(|p| p.inner().as_str()).collect();
			let module_path = format!("{}{}", root_prefix, partial_path.join("/"));

			return Some(make_symbol_at_location_with_path(
				segment.inner(),
				SymbolKind::Module,
				Some(format!("import {}", module_path)),
				segment.start(),
				segment.end(),
				doc,
				definition_path,
			));
		}
	}

	// Check the `with` clause items
	if let Some(import_idents) = idents {
		for (item_name, alias) in import_idents {
			// Check if on the original name
			if offset_in_span(offset, item_name.start(), item_name.end()) {
				let type_info = lookup_type_in_context(item_name.inner(), ctx)
					.map(|t| format!("{}: {}", item_name.inner(), t))
					.unwrap_or_else(|| format!("{} (from {})", item_name.inner(), full_path));

				return Some(make_symbol_at_location_with_path(
					item_name.inner(),
					SymbolKind::Variable,
					Some(type_info),
					item_name.start(),
					item_name.end(),
					doc,
					definition_path.clone(),
				));
			}

			// Check if on the alias
			if let Some(alias_ident) = alias
				&& offset_in_span(offset, alias_ident.start(), alias_ident.end())
			{
				let type_info = lookup_type_in_context(alias_ident.inner(), ctx)
					.map(|t| format!("{}: {}", alias_ident.inner(), t))
					.unwrap_or_else(|| {
						format!(
							"{} (alias for {} from {})",
							alias_ident.inner(),
							item_name.inner(),
							full_path
						)
					});

				return Some(make_symbol_at_location_with_path(
					alias_ident.inner(),
					SymbolKind::Variable,
					Some(type_info),
					alias_ident.start(),
					alias_ident.end(),
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
	let name_ident = meta.name.inner().as_binding()?;
	if !offset_in_span(offset, name_ident.start(), name_ident.end()) {
		return None;
	}
	let type_str = lookup_type_in_context(name_ident.inner(), ctx)
		.or_else(|| meta.type_.as_ref().map(|t| type_to_string(t.inner())))
		.or_else(|| value.and_then(infer_expr_type))
		.unwrap_or_else(|| "_".to_string());
	let mut_str = if meta.mutable { "mut " } else { "" };
	Some(make_symbol_at_location(
		name_ident.inner(),
		SymbolKind::Variable,
		Some(format!(
			"let {}{}: {}",
			mut_str,
			name_ident.inner(),
			type_str
		)),
		name_ident.start(),
		name_ident.end(),
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
	if offset_in_span(offset, meta.name.start(), meta.name.end()) {
		let sig = lookup_type_in_context(meta.name.inner(), ctx)
			.map(|t| format!("func {}: {}", meta.name.inner(), t))
			.unwrap_or_else(|| format_func_signature_with_body(meta, body));
		return Some(make_symbol_at_location(
			meta.name.inner(),
			SymbolKind::Function,
			Some(sig),
			meta.name.start(),
			meta.name.end(),
			doc,
		));
	}
	// Check generics
	for generic in &meta.generics {
		if let Some(sym) = find_symbol_in_generic_param(generic.inner(), offset, doc) {
			return Some(sym);
		}
	}
	// Check parameters
	for param in &meta.params {
		let p = param.inner();
		let Some(name_ident) = p.name.inner().as_binding() else {
			continue;
		};
		if offset_in_span(offset, name_ident.start(), name_ident.end()) {
			let mut_str = if p.mutable { "mut " } else { "" };
			return Some(make_symbol_at_location(
				name_ident.inner(),
				SymbolKind::Parameter,
				Some(format!(
					"parameter {}{}: {}",
					mut_str,
					name_ident.inner(),
					type_to_string(p.type_.inner())
				)),
				name_ident.start(),
				name_ident.end(),
				doc,
			));
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
	if offset_in_span(offset, field.name.start(), field.name.end()) {
		return Some(make_symbol_at_location(
			field.name.inner(),
			SymbolKind::Field,
			Some(format!(
				"field {}: {}",
				field.name.inner(),
				type_to_string(field.type_.inner())
			)),
			field.name.start(),
			field.name.end(),
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
	if offset_in_span(offset, variant.name.start(), variant.name.end()) {
		let fields_str = format_struct_fields(&variant.fields);
		return Some(make_symbol_at_location(
			variant.name.inner(),
			SymbolKind::Field,
			Some(format!(
				"variant {}.{}{}",
				enum_name,
				variant.name.inner(),
				fields_str
			)),
			variant.name.start(),
			variant.name.end(),
			doc,
		));
	}
	// Check variant fields
	for field in &variant.fields {
		if let Some(sym) = find_symbol_in_struct_field(field.inner(), offset, doc) {
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
	if offset_in_span(offset, param.name.start(), param.name.end()) {
		let mut info = format!("type parameter {}", param.name.inner());
		if let Some(constraint) = &param.constraint {
			info.push_str(&format!(": {}", type_to_string(constraint.inner())));
		}
		if let Some(default) = &param.default {
			info.push_str(&format!(" = {}", type_to_string(default.inner())));
		}
		return Some(make_symbol_at_location(
			param.name.inner(),
			SymbolKind::Type,
			Some(info),
			param.name.start(),
			param.name.end(),
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
		StructInnerMember::Member(m) => find_symbol_in_impl_member(m.inner(), offset, doc, ctx),
		StructInnerMember::Namespace(members) => {
			for m in members {
				if let Some(sym) = find_symbol_in_impl_member(m.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		StructInnerMember::Impl { members, .. } | StructInnerMember::ImplMut(members) => {
			for m in members {
				if let Some(sym) = find_symbol_in_impl_member(m.inner(), offset, doc, ctx) {
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
			find_symbol_in_let_with_value(meta, Some(value.inner()), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(value.inner(), offset, doc, ctx))
		}
		ImplMember::ExternalLet(_, meta) => find_symbol_in_let(meta, offset, doc, ctx),
		ImplMember::Func { meta, body, .. } => {
			find_symbol_in_func_with_body(meta, Some(body.inner()), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(body.inner(), offset, doc, ctx))
		}
		ImplMember::ExternalFunc(_, meta) => find_symbol_in_func(meta, offset, doc, ctx),
	}
}

fn find_symbol_in_interface_member(
	member: &InterfaceMember,
	offset: usize,
	doc: &Document,
	ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	match member {
		InterfaceMember::Element(elem) => {
			find_symbol_in_interface_element(elem.inner(), offset, doc, ctx)
		}
		InterfaceMember::Namespace(members) => {
			for m in members {
				if let Some(sym) = find_symbol_in_impl_member(m.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		InterfaceMember::ImplMut(elements) => {
			for elem in elements {
				if let Some(sym) = find_symbol_in_interface_element(elem.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		InterfaceMember::Impl { members, .. } => {
			for m in members {
				if let Some(sym) = find_symbol_in_impl_member(m.inner(), offset, doc, ctx) {
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
		InterfaceElement::Let { meta, value } => {
			find_symbol_in_let_with_value(meta, value.as_ref().map(|v| v.inner()), offset, doc, ctx)
				.or_else(|| {
					value
						.as_ref()
						.and_then(|v| find_symbol_in_expr(v.inner(), offset, doc, ctx))
				})
		}
		InterfaceElement::Func { meta, body } => {
			find_symbol_in_func_with_body(meta, body.as_ref().map(|b| b.inner()), offset, doc, ctx)
				.or_else(|| {
					body
						.as_ref()
						.and_then(|b| find_symbol_in_expr(b.inner(), offset, doc, ctx))
				})
		}
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
			for stmt in body {
				if let Some(sym) = find_symbol_in_statement(stmt.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}

		// Closure - check parameters
		Expr::Closure { params, body, .. } => {
			for param in params {
				if let Some(sym) = find_symbol_in_closure_param(param.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			find_symbol_in_expr(body.inner(), offset, doc, ctx)
		}

		// For loop - check loop variable
		Expr::For {
			variable,
			iterable,
			body,
			..
		} => {
			if let Some(name_ident) = variable.inner().as_binding()
				&& offset_in_span(offset, name_ident.start(), name_ident.end())
			{
				let iter_type = infer_expr_type(iterable.inner())
					.map(|t| {
						// Try to extract element type from list type
						if t.starts_with("#[") && t.ends_with("]") {
							t[2..t.len() - 1].to_string()
						} else {
							"_".to_string()
						}
					})
					.unwrap_or_else(|| "_".to_string());
				return Some(make_symbol_at_location(
					name_ident.inner(),
					SymbolKind::Variable,
					Some(format!("for {}: {}", name_ident.inner(), iter_type)),
					name_ident.start(),
					name_ident.end(),
					doc,
				));
			}
			find_symbol_in_expr(iterable.inner(), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(body.inner(), offset, doc, ctx))
		}

		// Match - check pattern bindings in arms
		Expr::Match { value, arms } => {
			if let Some(sym) = find_symbol_in_expr(value.inner(), offset, doc, ctx) {
				return Some(sym);
			}
			for arm in arms {
				if let Some(sym) = find_symbol_in_match_arm(arm, value.inner(), offset, doc, ctx) {
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
		} => find_symbol_in_expr(condition.inner(), offset, doc, ctx)
			.or_else(|| find_symbol_in_expr(then.inner(), offset, doc, ctx))
			.or_else(|| {
				otherwise
					.as_ref()
					.and_then(|e| find_symbol_in_expr(e.inner(), offset, doc, ctx))
			}),

		Expr::While {
			condition, body, ..
		} => find_symbol_in_expr(condition.inner(), offset, doc, ctx)
			.or_else(|| find_symbol_in_expr(body.inner(), offset, doc, ctx)),

		// Binary/Prefix/Postfix ops
		Expr::BinaryOp { lhs, rhs, .. } => find_symbol_in_expr(lhs.inner(), offset, doc, ctx)
			.or_else(|| find_symbol_in_expr(rhs.inner(), offset, doc, ctx)),
		Expr::PrefixOp { value, .. } | Expr::PostfixOp { value, .. } => {
			find_symbol_in_expr(value.inner(), offset, doc, ctx)
		}
		Expr::TypeOp { lhs, .. } | Expr::PatternOp { lhs, .. } => {
			find_symbol_in_expr(lhs.inner(), offset, doc, ctx)
		}
		Expr::AssignOp { lhs, rhs, .. } => find_symbol_in_expr(lhs.inner(), offset, doc, ctx)
			.or_else(|| find_symbol_in_expr(rhs.inner(), offset, doc, ctx)),

		// Call expressions
		Expr::Call { func, args, .. } => {
			if let Some(sym) = find_symbol_in_expr(func.inner(), offset, doc, ctx) {
				return Some(sym);
			}
			for arg in args {
				if let Some(sym) = find_symbol_in_expr(arg.inner().value.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}

		// Member/Index access
		Expr::MemberAccess { parent, member, .. } => {
			// Check if we're hovering over the member name
			if offset_in_span(offset, member.start(), member.end()) {
				// Try to get the parent type and look up the member
				let parent_type_name = match parent.inner() {
					Expr::Identifier(ident) => Some(ident.inner().to_string()),
					Expr::This => Some("this".to_string()),
					_ => None,
				};

				let type_info = parent_type_name
					.and_then(|name| lookup_type_in_context(&name, ctx))
					.map(|parent_ty| {
						// Extract field/member info from parent type if it's a struct
						format!("member {}.{}", parent_ty, member.inner())
					})
					.unwrap_or_else(|| format!("member .{}", member.inner()));

				return Some(make_symbol_at_location(
					member.inner(),
					SymbolKind::Field,
					Some(type_info),
					member.start(),
					member.end(),
					doc,
				));
			}
			find_symbol_in_expr(parent.inner(), offset, doc, ctx)
		}
		Expr::IndexAccess { parent, index, .. } => {
			find_symbol_in_expr(parent.inner(), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(index.inner(), offset, doc, ctx))
		}

		// Grouped
		Expr::Grouped(inner) => find_symbol_in_expr(inner.inner(), offset, doc, ctx),

		// Return/Break with values
		Expr::Return { value, .. } | Expr::Break { value, .. } => value
			.as_ref()
			.and_then(|v| find_symbol_in_expr(v.inner(), offset, doc, ctx)),

		// Collections - traverse elements
		Expr::List(items) | Expr::Tuple(items) => {
			for item in items {
				let expr = match item.inner() {
					nymph_compiler::ast::expr::ListItem::Expr(e) => e,
					nymph_compiler::ast::expr::ListItem::Spread(e) => e,
				};
				if let Some(sym) = find_symbol_in_expr(expr.inner(), offset, doc, ctx) {
					return Some(sym);
				}
			}
			None
		}
		Expr::Map(entries) => {
			for entry in entries {
				match entry.inner() {
					nymph_compiler::ast::expr::MapEntry::Expr(k, v) => {
						if let Some(sym) = find_symbol_in_expr(k.inner(), offset, doc, ctx) {
							return Some(sym);
						}
						if let Some(sym) = find_symbol_in_expr(v.inner(), offset, doc, ctx) {
							return Some(sym);
						}
					}
					nymph_compiler::ast::expr::MapEntry::Spread(e) => {
						if let Some(sym) = find_symbol_in_expr(e.inner(), offset, doc, ctx) {
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
				if let nymph_compiler::ast::expr::StringPart::InterpolatedExpr(e) = part.inner()
					&& let Some(sym) = find_symbol_in_expr(e.inner(), offset, doc, ctx)
				{
					return Some(sym);
				}
			}
			None
		}

		// Identifier - look up type in context
		Expr::Identifier(ident) => {
			let name = ident.inner();
			if offset_in_span(offset, ident.start(), ident.end()) {
				let type_info = lookup_type_in_context(name, ctx)
					.map(|ty| format!("let {}: {}", name, ty))
					.unwrap_or_else(|| name.to_string());
				return Some(make_symbol_at_location(
					name,
					SymbolKind::Variable,
					Some(type_info),
					ident.start(),
					ident.end(),
					doc,
				));
			}
			None
		}

		// `this` keyword - look up type in context
		Expr::This => {
			let type_info = lookup_type_in_context("this", ctx)
				.map(|ty| format!("this: {}", ty))
				.unwrap_or_else(|| "this".to_string());
			Some(make_symbol_at_location(
				"this",
				SymbolKind::Variable,
				Some(type_info),
				0, // We don't have span info for `this` keyword in this context
				0,
				doc,
			))
		}

		// Literals and simple expressions - no nested symbols
		Expr::Int(_)
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
		Statement::Expr(e) => find_symbol_in_expr(e.inner(), offset, doc, ctx),
		Statement::Let { meta, value } => {
			find_symbol_in_let_with_value(meta, Some(value.inner()), offset, doc, ctx)
				.or_else(|| find_symbol_in_expr(value.inner(), offset, doc, ctx))
		}
	}
}

fn find_symbol_in_closure_param(
	param: &ClosureParam,
	offset: usize,
	doc: &Document,
	_ctx: Option<&Context>,
) -> Option<SymbolAtLocation> {
	let name_ident = param.name.inner().as_binding()?;
	if !offset_in_span(offset, name_ident.start(), name_ident.end()) {
		return None;
	}
	let type_str = param
		.type_
		.as_ref()
		.map(|t| type_to_string(t.inner()))
		.unwrap_or_else(|| "_".to_string());
	let mut_str = if param.mutable { "mut " } else { "" };
	Some(make_symbol_at_location(
		name_ident.inner(),
		SymbolKind::Parameter,
		Some(format!(
			"closure param {}{}: {}",
			mut_str,
			name_ident.inner(),
			type_str
		)),
		name_ident.start(),
		name_ident.end(),
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
	if let Some(sym) = find_symbol_in_pattern(arm.pattern.inner(), scrutinee, offset, doc, ctx) {
		return Some(sym);
	}
	// Check guard expression
	if let Some(guard) = &arm.guard
		&& let Some(sym) = find_symbol_in_expr(guard.inner(), offset, doc, ctx)
	{
		return Some(sym);
	}
	// Check body
	find_symbol_in_expr(arm.body.inner(), offset, doc, ctx)
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
			if offset_in_span(offset, name.start(), name.end()) {
				let type_str = infer_expr_type(scrutinee).unwrap_or_else(|| "_".to_string());
				return Some(make_symbol_at_location(
					name.inner(),
					SymbolKind::Variable,
					Some(format!("binding {}: {}", name.inner(), type_str)),
					name.start(),
					name.end(),
					doc,
				));
			}
			find_symbol_in_pattern(inner.inner(), scrutinee, offset, doc, _ctx)
		}
		Pattern::List(items) | Pattern::Tuple(items) => {
			for item in items {
				match item.inner() {
					nymph_compiler::ast::expr::ListPatternEntry::Item(p) => {
						if let Some(sym) = find_symbol_in_pattern(p.inner(), scrutinee, offset, doc, _ctx) {
							return Some(sym);
						}
					}
					nymph_compiler::ast::expr::ListPatternEntry::Rest(Some(name)) => {
						if offset_in_span(offset, name.start(), name.end()) {
							return Some(make_symbol_at_location(
								name.inner(),
								SymbolKind::Variable,
								Some(format!("rest binding {}", name.inner())),
								name.start(),
								name.end(),
								doc,
							));
						}
					}
					nymph_compiler::ast::expr::ListPatternEntry::Rest(None) => {}
				}
			}
			None
		}
		Pattern::Map(entries) => {
			for entry in entries {
				match entry.inner() {
					nymph_compiler::ast::expr::MapPatternEntry::Entry(_, value) => {
						if let Some(sym) = find_symbol_in_pattern(value.inner(), scrutinee, offset, doc, _ctx) {
							return Some(sym);
						}
					}
					nymph_compiler::ast::expr::MapPatternEntry::Rest(Some(name)) => {
						if offset_in_span(offset, name.start(), name.end()) {
							return Some(make_symbol_at_location(
								name.inner(),
								SymbolKind::Variable,
								Some(format!("rest binding {}", name.inner())),
								name.start(),
								name.end(),
								doc,
							));
						}
					}
					nymph_compiler::ast::expr::MapPatternEntry::Rest(None) => {}
				}
			}
			None
		}
		Pattern::Struct { fields, .. } => {
			for field in fields {
				match field.inner() {
					nymph_compiler::ast::expr::StructPatternField::Value { value, .. } => {
						if let Some(sym) = find_symbol_in_pattern(value.inner(), scrutinee, offset, doc, _ctx) {
							return Some(sym);
						}
					}
					nymph_compiler::ast::expr::StructPatternField::Named(name) => {
						if offset_in_span(offset, name.start(), name.end()) {
							return Some(make_symbol_at_location(
								name.inner(),
								SymbolKind::Variable,
								Some(format!("field binding {}", name.inner())),
								name.start(),
								name.end(),
								doc,
							));
						}
					}
					nymph_compiler::ast::expr::StructPatternField::Rest => {}
				}
			}
			None
		}
		Pattern::Union(a, b) => find_symbol_in_pattern(a.inner(), scrutinee, offset, doc, _ctx)
			.or_else(|| find_symbol_in_pattern(b.inner(), scrutinee, offset, doc, _ctx)),
		Pattern::Grouped(inner) => find_symbol_in_pattern(inner.inner(), scrutinee, offset, doc, _ctx),
		// Literals don't have bindings
		Pattern::Int(_)
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
				let generic_strs: Vec<_> = generics
					.iter()
					.map(|g| format_generic_arg(g.inner()))
					.collect();
				format!("{}<{}>", name.inner(), generic_strs.join(", "))
			}
		}
		Type::List(inner) => format!("#[{}]", type_to_string(inner.inner())),
		Type::Tuple(elems) => {
			let elem_strs: Vec<_> = elems.iter().map(|e| type_to_string(e.inner())).collect();
			format!("#({})", elem_strs.join(", "))
		}
		Type::Map(k, v) => format!(
			"#{{ {}: {} }}",
			type_to_string(k.inner()),
			type_to_string(v.inner())
		),
		Type::Function {
			params,
			return_type,
		} => {
			let param_strs: Vec<_> = params
				.iter()
				.map(|(name, ty)| {
					if let Some(n) = name {
						format!("{}: {}", n.inner(), type_to_string(ty.inner()))
					} else {
						type_to_string(ty.inner())
					}
				})
				.collect();
			format!(
				"({}) -> {}",
				param_strs.join(", "),
				type_to_string(return_type.inner())
			)
		}
		Type::Intersection(a, b) => format!(
			"{} + {}",
			type_to_string(a.inner()),
			type_to_string(b.inner())
		),
		Type::Grouped(inner) => format!("({})", type_to_string(inner.inner())),
	}
}

fn format_generic_arg(arg: &GenericArg) -> String {
	if let Some(name) = &arg.name {
		format!("{}: {}", name.inner(), type_to_string(arg.value.inner()))
	} else {
		type_to_string(arg.value.inner())
	}
}

fn format_generics(generics: &[Spanned<GenericParam>]) -> String {
	if generics.is_empty() {
		String::new()
	} else {
		let strs: Vec<_> = generics
			.iter()
			.map(|g| {
				let p = g.inner();
				let mut s = p.name.inner().to_string();
				if let Some(c) = &p.constraint {
					s.push_str(&format!(": {}", type_to_string(c.inner())));
				}
				if let Some(d) = &p.default {
					s.push_str(&format!(" = {}", type_to_string(d.inner())));
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
				let field = f.inner();
				format!(
					"{}: {}",
					field.name.inner(),
					type_to_string(field.type_.inner())
				)
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
		Expr::Float(_) => Some("float".to_string()),
		Expr::Char(_) => Some("char".to_string()),
		Expr::String(_) => Some("string".to_string()),
		Expr::Boolean(_) => Some("boolean".to_string()),

		// Collection types - infer element types where possible
		Expr::List(items) => {
			if items.is_empty() {
				Some("#[_]".to_string())
			} else if let Some(first) = items.first() {
				match first.inner() {
					nymph_compiler::ast::expr::ListItem::Expr(e) => {
						let elem_type = infer_expr_type(e.inner()).unwrap_or_else(|| "_".to_string());
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
				.map(|item| match item.inner() {
					nymph_compiler::ast::expr::ListItem::Expr(e) => {
						infer_expr_type(e.inner()).unwrap_or_else(|| "_".to_string())
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
				match first.inner() {
					nymph_compiler::ast::expr::MapEntry::Expr(k, v) => {
						let key_type = infer_expr_type(k.inner()).unwrap_or_else(|| "_".to_string());
						let val_type = infer_expr_type(v.inner()).unwrap_or_else(|| "_".to_string());
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
				let lhs_type = infer_expr_type(lhs.inner());
				let rhs_type = infer_expr_type(rhs.inner());
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
			BinaryOperator::Pipe => infer_expr_type(rhs.inner()),

			// Unwrap operator - returns the unwrapped type
			BinaryOperator::Unwrap => infer_expr_type(lhs.inner()),
		},

		Expr::PrefixOp { op, value } => match op {
			PrefixOperator::BoolNot => Some("boolean".to_string()),
			PrefixOperator::Negate => infer_expr_type(value.inner()),
			PrefixOperator::BitNot => Some("int".to_string()),
		},

		// Type cast - return the target type
		Expr::TypeOp { rhs, .. } => Some(type_to_string(rhs.inner())),

		// Pattern match - returns boolean
		Expr::PatternOp { .. } => Some("boolean".to_string()),

		// Control flow expressions
		Expr::If {
			then, otherwise, ..
		} => {
			if let Some(else_branch) = otherwise {
				// If both branches have the same type, return it
				let then_type = infer_expr_type(then.inner());
				let else_type = infer_expr_type(else_branch.inner());
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
			arms
				.first()
				.and_then(|arm| infer_expr_type(arm.body.inner()))
		}

		// Block - type is the type of the last expression
		Expr::Block { body, .. } => body.last().and_then(|stmt| match stmt.inner() {
			Statement::Expr(e) => infer_expr_type(e.inner()),
			Statement::Let { .. } => Some("void".to_string()),
		}),

		// Grouped expression - unwrap
		Expr::Grouped(inner) => infer_expr_type(inner.inner()),

		// Control flow that doesn't produce values
		Expr::Return { value, .. } => value.as_ref().and_then(|v| infer_expr_type(v.inner())),
		Expr::Break { value, .. } => value.as_ref().and_then(|v| infer_expr_type(v.inner())),
		Expr::Continue { .. } => Some("never".to_string()),

		// Loops return void unless broken with a value
		Expr::For { .. } | Expr::While { .. } => Some("void".to_string()),

		// Assignment returns void
		Expr::AssignOp { .. } => Some("void".to_string()),

		// Range returns a range type (simplified)
		Expr::Range(_) => Some("Range".to_string()),

		// Closures - return their declared return type or infer from body
		Expr::Closure {
			return_type, body, ..
		} => {
			if let Some(ret) = return_type {
				Some(type_to_string(ret.inner()))
			} else {
				infer_expr_type(body.inner())
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

fn format_func_signature_with_body(meta: &FuncDeclaration, body: Option<&Expr>) -> String {
	let generics = format_generics(&meta.generics);
	let params: Vec<_> = meta
		.params
		.iter()
		.map(|p| {
			let param = p.inner();
			let mut_str = if param.mutable { "mut " } else { "" };
			let name = param
				.name
				.inner()
				.as_binding()
				.map_or_else(|| "_".to_string(), |i| i.inner().to_string());
			format!(
				"{}{}: {}",
				mut_str,
				name,
				type_to_string(param.type_.inner())
			)
		})
		.collect();
	let ret = meta
		.return_type
		.as_ref()
		.map(|t| type_to_string(t.inner()))
		.or_else(|| body.and_then(infer_expr_type))
		.unwrap_or_else(|| "void".to_string());
	format!(
		"func {}{}({}) -> {}",
		meta.name.inner(),
		generics,
		params.join(", "),
		ret
	)
}

#[allow(dead_code)]
fn format_func_signature(meta: &FuncDeclaration) -> String {
	format_func_signature_with_body(meta, None)
}

#[cfg(test)]
mod tests {
	use super::*;

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
}
