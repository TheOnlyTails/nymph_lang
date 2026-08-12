use crate::document::Document;
use nymph_compiler::ast::{
	Span, Spanned,
	declaration::{Declaration, ImplMember, InterfaceElement, InterfaceMember, StructInnerMember},
	expr::{Expr, MatchArm, Pattern, Statement, StringPart, StructPatternField},
	ops::{
		AssignOperator, BinaryOperator, PatternOperator, PostfixOperator, PrefixOperator, TypeOperator,
	},
	types::Type,
};

/// Semantic token type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenType {
	Keyword,
	Type,
	Function,
	Variable,
	Parameter,
	Number,
	String,
	Comment,
	Operator,
	Interface,
	Member,
}

impl TokenType {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Keyword => "keyword",
			Self::Type => "type",
			Self::Function => "function",
			Self::Variable => "variable",
			Self::Parameter => "parameter",
			Self::Number => "number",
			Self::String => "string",
			Self::Comment => "comment",
			Self::Operator => "operator",
			Self::Interface => "interface",
			Self::Member => "member",
		}
	}
}

/// Token modifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenModifier {
	Declaration,
	Definition,
	Builtin,
	Mutable,
}

impl TokenModifier {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Declaration => "declaration",
			Self::Definition => "definition",
			Self::Builtin => "builtin",
			Self::Mutable => "mutable",
		}
	}
}

/// A semantic token with position and type information
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticToken {
	pub line: usize,
	pub start_char: usize,
	pub length: usize,
	pub token_type: TokenType,
	pub modifiers: Vec<TokenModifier>,
}

pub struct SemanticTokenizer {
	tokens: Vec<SemanticToken>,
	content: String,
}

impl SemanticTokenizer {
	pub fn new() -> Self {
		Self {
			tokens: Vec::new(),
			content: String::new(),
		}
	}

	/// Tokenize content and return semantic tokens
	pub fn tokenize(&mut self, content: &str) -> Vec<SemanticToken> {
		self.tokens.clear();
		self.content.clear();
		self.content.push_str(content);
		self.collect_keyword_tokens(content);
		self.sort_and_dedupe();
		self.tokens.clone()
	}

	/// Tokenize a parsed document and return semantic tokens
	pub fn tokenize_document(&mut self, document: &Document) -> Vec<SemanticToken> {
		self.tokens.clear();
		self.content.clear();
		self.content.push_str(&document.content);
		self.collect_keyword_tokens(&document.content);

		if let Some(module) = &document.ast {
			for declaration in &module.0.members {
				self.visit_declaration(declaration, document);
			}
		}

		self.sort_and_dedupe();
		self.tokens.clone()
	}

	fn collect_keyword_tokens(&mut self, content: &str) {
		let keywords = [
			"let",
			"func",
			"if",
			"else",
			"struct",
			"enum",
			"namespace",
			"interface",
			"impl",
			"match",
			"for",
			"while",
			"return",
			"break",
			"continue",
			"true",
			"false",
		];

		for (line_idx, line) in content.lines().enumerate() {
			for keyword in &keywords {
				let mut search_start = 0;
				while let Some(pos) = line[search_start..].find(keyword) {
					let absolute_pos = search_start + pos;
					let before_ok = absolute_pos == 0 || {
						let before_char = line[..absolute_pos].chars().next_back();
						before_char.is_none() || !is_identifier_char(before_char.unwrap())
					};
					let after_end = absolute_pos + keyword.len();
					let after_ok = after_end >= line.len() || {
						let after_char = line[after_end..].chars().next();
						after_char.is_none() || !is_identifier_char(after_char.unwrap())
					};

					if before_ok && after_ok {
						let start_char = line[..absolute_pos].encode_utf16().count();
						self.add_token(
							line_idx,
							start_char,
							keyword.len(),
							TokenType::Keyword,
							vec![],
						);
					}

					search_start = absolute_pos + keyword.len();
				}
			}
		}
	}

	fn visit_declaration(&mut self, declaration: &Declaration, document: &Document) {
		match declaration {
			Declaration::Import { path, idents, .. } => {
				for segment in path {
					self.add_ident(segment.1, &segment.0, TokenType::Type, vec![]);
				}
				if let Some(imported) = idents {
					for (name, alias) in imported {
						self.add_ident(name.1, &name.0, TokenType::Variable, vec![]);
						if let Some(alias) = alias {
							self.add_ident(
								alias.1,
								&alias.0,
								TokenType::Variable,
								vec![TokenModifier::Declaration],
							);
						}
					}
				}
			}
			Declaration::Let { meta, value, .. } => {
				self.visit_pattern(
					&meta.name,
					TokenType::Variable,
					modifiers_for_binding(meta.mutable),
					document,
				);
				if let Some(type_) = &meta.type_ {
					self.visit_type(&type_.0, document);
				}
				self.visit_expr(value, document);
			}
			Declaration::ExternalLet(_, _, meta) => {
				self.visit_pattern(
					&meta.name,
					TokenType::Variable,
					modifiers_for_binding(meta.mutable),
					document,
				);
				if let Some(type_) = &meta.type_ {
					self.visit_type(&type_.0, document);
				}
			}
			Declaration::Func { meta, body, .. } => {
				self.add_ident(
					meta.name.1,
					&meta.name.0,
					TokenType::Function,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for generic in &meta.generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for param in &meta.params {
					self.visit_pattern(
						&param.0.name,
						TokenType::Parameter,
						modifiers_for_binding(param.0.mutable),
						document,
					);
					self.visit_type(&param.0.type_.0, document);
				}
				if let Some(return_type) = &meta.return_type {
					self.visit_type(&return_type.0, document);
				}
				self.visit_expr(body, document);
			}
			Declaration::ExternalFunc(_, _, meta) => {
				self.add_ident(
					meta.name.1,
					&meta.name.0,
					TokenType::Function,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for generic in &meta.generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for param in &meta.params {
					self.visit_pattern(
						&param.0.name,
						TokenType::Parameter,
						modifiers_for_binding(param.0.mutable),
						document,
					);
					self.visit_type(&param.0.type_.0, document);
				}
				if let Some(return_type) = &meta.return_type {
					self.visit_type(&return_type.0, document);
				}
			}
			Declaration::TypeAlias { meta, value, .. } => {
				self.add_ident(
					meta.name.1,
					&meta.name.0,
					TokenType::Type,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for generic in &meta.generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				self.visit_type(&value.0, document);
			}
			Declaration::Struct {
				name,
				generics,
				fields,
				members,
				..
			} => {
				self.add_ident(
					name.1,
					&name.0,
					TokenType::Type,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for generic in generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for field in fields {
					self.add_ident(
						field.0.name.1,
						&field.0.name.0,
						TokenType::Member,
						vec![TokenModifier::Declaration],
					);
					self.visit_type(&field.0.type_.0, document);
					if let Some(default) = &field.0.default {
						self.visit_expr(default, document);
					}
				}
				for member in members {
					self.visit_struct_inner_member(&member.0, document);
				}
			}
			Declaration::Enum {
				name,
				generics,
				variants,
				members,
				..
			} => {
				self.add_ident(
					name.1,
					&name.0,
					TokenType::Type,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for generic in generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for variant in variants {
					self.add_ident(
						variant.0.name.1,
						&variant.0.name.0,
						TokenType::Member,
						vec![TokenModifier::Declaration],
					);
					for field in &variant.0.fields {
						self.add_ident(
							field.0.name.1,
							&field.0.name.0,
							TokenType::Member,
							vec![TokenModifier::Declaration],
						);
						self.visit_type(&field.0.type_.0, document);
					}
				}
				for member in members {
					self.visit_struct_inner_member(&member.0, document);
				}
			}
			Declaration::Namespace { name, members, .. } => {
				self.add_ident(
					name.1,
					&name.0,
					TokenType::Type,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for member in members {
					self.visit_impl_member(&member.0, document);
				}
			}
			Declaration::Interface {
				name,
				generics,
				super_interfaces,
				members,
				..
			} => {
				self.add_ident(
					name.1,
					&name.0,
					TokenType::Interface,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for generic in generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for super_interface in super_interfaces {
					self.add_ident(
						super_interface.0.0.1,
						&super_interface.0.0.0,
						TokenType::Interface,
						vec![],
					);
				}
				for member in members {
					self.visit_interface_member(&member.0, document);
				}
			}
			Declaration::Impl {
				type_,
				generics,
				members,
				..
			} => {
				self.visit_type(&type_.0, document);
				for generic in generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for member in members {
					self.visit_impl_member(&member.0, document);
				}
			}
			Declaration::ImplFor {
				type_,
				generics,
				for_interface,
				members,
				..
			} => {
				self.visit_type(&type_.0, document);
				self.add_ident(
					for_interface.0.1,
					&for_interface.0.0,
					TokenType::Interface,
					vec![],
				);
				for generic in generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for member in members {
					self.visit_impl_member(&member.0, document);
				}
			}
		}
	}

	fn visit_struct_inner_member(&mut self, member: &StructInnerMember, document: &Document) {
		match member {
			StructInnerMember::Member(member) => self.visit_impl_member(&member.0, document),
			StructInnerMember::Namespace(members) | StructInnerMember::ImplMut(members) => {
				for member in members {
					self.visit_impl_member(&member.0, document);
				}
			}
			StructInnerMember::Impl {
				interface,
				generics,
				members,
			} => {
				self.add_ident(interface.0.1, &interface.0.0, TokenType::Interface, vec![]);
				for generic in generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for member in members {
					self.visit_impl_member(&member.0, document);
				}
			}
		}
	}

	fn visit_interface_member(&mut self, member: &InterfaceMember, document: &Document) {
		match member {
			InterfaceMember::Element(element) => self.visit_interface_element(&element.0, document),
			InterfaceMember::Namespace(members) => {
				for member in members {
					self.visit_impl_member(&member.0, document);
				}
			}
			InterfaceMember::ImplMut(elements) => {
				for element in elements {
					self.visit_interface_element(&element.0, document);
				}
			}
			InterfaceMember::Impl {
				interface,
				generics,
				members,
			} => {
				self.add_ident(interface.0.1, &interface.0.0, TokenType::Interface, vec![]);
				for generic in generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for member in members {
					self.visit_impl_member(&member.0, document);
				}
			}
		}
	}

	fn visit_interface_element(&mut self, element: &InterfaceElement, document: &Document) {
		match element {
			InterfaceElement::Let { meta, value } => {
				self.visit_pattern(
					&meta.name,
					TokenType::Member,
					modifiers_for_binding(meta.mutable),
					document,
				);
				if let Some(type_) = &meta.type_ {
					self.visit_type(&type_.0, document);
				}
				if let Some(value) = value {
					self.visit_expr(value, document);
				}
			}
			InterfaceElement::Func { meta, body } => {
				self.add_ident(
					meta.name.1,
					&meta.name.0,
					TokenType::Function,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for param in &meta.params {
					self.visit_pattern(
						&param.0.name,
						TokenType::Parameter,
						modifiers_for_binding(param.0.mutable),
						document,
					);
					self.visit_type(&param.0.type_.0, document);
				}
				if let Some(return_type) = &meta.return_type {
					self.visit_type(&return_type.0, document);
				}
				if let Some(body) = body {
					self.visit_expr(body, document);
				}
			}
		}
	}

	fn visit_impl_member(&mut self, member: &ImplMember, document: &Document) {
		match member {
			ImplMember::Let { meta, value, .. } => {
				self.visit_pattern(
					&meta.name,
					TokenType::Member,
					modifiers_for_binding(meta.mutable),
					document,
				);
				if let Some(type_) = &meta.type_ {
					self.visit_type(&type_.0, document);
				}
				self.visit_expr(value, document);
			}
			ImplMember::ExternalLet(_, _, meta) => {
				self.visit_pattern(
					&meta.name,
					TokenType::Member,
					modifiers_for_binding(meta.mutable),
					document,
				);
				if let Some(type_) = &meta.type_ {
					self.visit_type(&type_.0, document);
				}
			}
			ImplMember::Func { meta, body, .. } => {
				self.add_ident(
					meta.name.1,
					&meta.name.0,
					TokenType::Function,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for param in &meta.params {
					self.visit_pattern(
						&param.0.name,
						TokenType::Parameter,
						modifiers_for_binding(param.0.mutable),
						document,
					);
					self.visit_type(&param.0.type_.0, document);
				}
				if let Some(return_type) = &meta.return_type {
					self.visit_type(&return_type.0, document);
				}
				self.visit_expr(body, document);
			}
			ImplMember::ExternalFunc(_, _, meta) => {
				self.add_ident(
					meta.name.1,
					&meta.name.0,
					TokenType::Function,
					vec![TokenModifier::Declaration, TokenModifier::Definition],
				);
				for param in &meta.params {
					self.visit_pattern(
						&param.0.name,
						TokenType::Parameter,
						modifiers_for_binding(param.0.mutable),
						document,
					);
					self.visit_type(&param.0.type_.0, document);
				}
				if let Some(return_type) = &meta.return_type {
					self.visit_type(&return_type.0, document);
				}
			}
		}
	}

	fn visit_statement(&mut self, statement: &Spanned<Statement>, document: &Document) {
		match &statement.0 {
			Statement::Expr(expr) => self.visit_expr(expr, document),
			Statement::Let { meta, value } => {
				self.visit_pattern(
					&meta.name,
					TokenType::Variable,
					modifiers_for_binding(meta.mutable),
					document,
				);
				if let Some(type_) = &meta.type_ {
					self.visit_type(&type_.0, document);
				}
				self.visit_expr(value, document);
			}
		}
	}

	fn visit_expr(&mut self, expr: &Spanned<Expr>, document: &Document) {
		match &expr.0 {
			Expr::Int(_) | Expr::UInt(_) | Expr::Float(_) | Expr::Char(_) => {
				self.add_span(expr.1, TokenType::Number, vec![], document);
			}
			Expr::String(parts) => {
				self.add_span(expr.1, TokenType::String, vec![], document);
				for part in parts {
					if let StringPart::InterpolatedExpr(inner) = &part.0 {
						self.visit_expr(inner, document);
					}
				}
			}
			Expr::Boolean(_) => {
				self.add_span(expr.1, TokenType::Keyword, vec![], document);
			}
			Expr::Identifier(ident) => {
				self.add_ident(ident.1, &ident.0, TokenType::Variable, vec![]);
			}
			Expr::AnonymousParam(_) => {
				self.add_span(expr.1, TokenType::Parameter, vec![], document);
			}
			Expr::List(items) | Expr::Tuple(items) => {
				for item in items {
					match &item.0 {
						nymph_compiler::ast::expr::ListItem::Expr(inner) => self.visit_expr(inner, document),
						nymph_compiler::ast::expr::ListItem::Spread(inner) => {
							if let Some(spread_span) = find_operator_between(
								&self.content,
								item.1.start,
								inner.1.start.min(item.1.end),
								&["..."],
							) {
								self.add_operator_span(spread_span);
							}
							self.visit_expr(inner, document);
						}
					}
				}
			}
			Expr::Map(entries) => {
				for entry in entries {
					match &entry.0 {
						nymph_compiler::ast::expr::MapEntry::Expr(key, value) => {
							self.visit_expr(key, document);
							self.visit_expr(value, document);
						}
						nymph_compiler::ast::expr::MapEntry::Spread(expr) => {
							if let Some(spread_span) = find_operator_between(
								&self.content,
								entry.1.start,
								expr.1.start.min(entry.1.end),
								&["..."],
							) {
								self.add_operator_span(spread_span);
							}
							self.visit_expr(expr, document);
						}
					}
				}
			}
			Expr::Range(range) => self.visit_range_expr(range, document),
			Expr::Call {
				func,
				args,
				generics,
			} => {
				self.visit_expr(func, document);
				for generic in generics {
					self.visit_type(&generic.0.value.0, document);
				}
				for arg in args {
					if arg.0.spread {
						let search_end = arg.0.value.1.start.min(expr.1.end);
						if let Some(spread_span) =
							find_operator_between(&self.content, arg.1.start, search_end, &["..."])
						{
							self.add_operator_span(spread_span);
						}
					}
					self.visit_expr(&arg.0.value, document);
				}
			}
			Expr::MemberAccess { parent, member, .. } => {
				self.visit_expr(parent, document);
				self.add_ident(member.1, &member.0, TokenType::Member, vec![]);
			}
			Expr::IndexAccess { parent, index, .. } => {
				self.visit_expr(parent, document);
				self.visit_expr(index, document);
			}
			Expr::Closure {
				params,
				generics,
				return_type,
				body,
			} => {
				for generic in generics {
					self.add_ident(
						generic.0.name.1,
						&generic.0.name.0,
						TokenType::Type,
						vec![TokenModifier::Declaration],
					);
				}
				for param in params {
					self.visit_pattern(
						&param.0.name,
						TokenType::Parameter,
						modifiers_for_binding(param.0.mutable),
						document,
					);
					if let Some(type_) = &param.0.type_ {
						self.visit_type(&type_.0, document);
					}
				}
				if let Some(return_type) = return_type {
					self.visit_type(&return_type.0, document);
				}
				if let Some(arrow_span) =
					find_operator_between_last(&self.content, expr.1.start, body.1.start, &["->"])
				{
					self.add_operator_span(arrow_span);
				}
				self.visit_expr(body, document);
			}
			Expr::PrefixOp { op, value } => {
				let operator_text = prefix_operator_text(*op);
				if let Some(operator_span) =
					find_operator_between(&self.content, expr.1.start, value.1.start, &[operator_text])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_expr(value, document);
			}
			Expr::PostfixOp { op, value } => {
				let operator_text = postfix_operator_text(*op);
				if let Some(operator_span) =
					find_operator_between(&self.content, value.1.end, expr.1.end, &[operator_text])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_expr(value, document);
			}
			Expr::BinaryOp { lhs, op, rhs } => {
				self.visit_expr(lhs, document);
				let operator_text = binary_operator_text(*op);
				if let Some(operator_span) =
					find_operator_between(&self.content, lhs.1.end, rhs.1.start, &[operator_text])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_expr(rhs, document);
			}
			Expr::AssignOp { lhs, op, rhs } => {
				self.visit_expr(lhs, document);
				let operator_text = assign_operator_text(*op);
				if let Some(operator_span) =
					find_operator_between(&self.content, lhs.1.end, rhs.1.start, &[operator_text])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_expr(rhs, document);
			}
			Expr::TypeOp { lhs, op, rhs } => {
				self.visit_expr(lhs, document);
				let operator_text = type_operator_text(*op);
				if let Some(operator_span) =
					find_operator_between(&self.content, lhs.1.end, rhs.1.start, &[operator_text])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_type(&rhs.0, document);
			}
			Expr::PatternOp { lhs, op, rhs } => {
				self.visit_expr(lhs, document);
				let operator_text = pattern_operator_text(*op);
				if let Some(operator_span) =
					find_operator_between(&self.content, lhs.1.end, rhs.1.start, &[operator_text])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_pattern(rhs, TokenType::Variable, vec![], document);
			}
			Expr::Return { value, .. } | Expr::Break { value, .. } => {
				if let Some(value) = value {
					self.visit_expr(value, document);
				}
			}
			Expr::Continue { .. } | Expr::This | Expr::Placeholder => {}
			Expr::While {
				condition, body, ..
			} => {
				self.visit_expr(condition, document);
				self.visit_expr(body, document);
			}
			Expr::For {
				variable,
				iterable,
				body,
				..
			} => {
				self.visit_pattern(
					variable,
					TokenType::Variable,
					vec![TokenModifier::Declaration],
					document,
				);
				self.visit_expr(iterable, document);
				self.visit_expr(body, document);
			}
			Expr::If {
				condition,
				then,
				otherwise,
			} => {
				self.visit_expr(condition, document);
				self.visit_expr(then, document);
				if let Some(otherwise) = otherwise {
					self.visit_expr(otherwise, document);
				}
			}
			Expr::Match { value, arms } => {
				self.visit_expr(value, document);
				for arm in arms {
					self.visit_match_arm(arm, document);
				}
			}
			Expr::Block { body, .. } => {
				for statement in body {
					self.visit_statement(statement, document);
				}
			}
			Expr::Grouped(inner) => self.visit_expr(inner, document),
		}
	}

	fn visit_match_arm(&mut self, arm: &MatchArm, document: &Document) {
		self.visit_pattern(&arm.pattern, TokenType::Variable, vec![], document);
		if let Some(guard) = &arm.guard {
			self.visit_expr(guard, document);
		}
		self.visit_expr(&arm.body, document);
	}

	fn visit_pattern(
		&mut self,
		pattern: &Spanned<Pattern>,
		binding_type: TokenType,
		modifiers: Vec<TokenModifier>,
		document: &Document,
	) {
		match &pattern.0 {
			Pattern::Int(_) | Pattern::UInt(_) | Pattern::Float(_) | Pattern::Char(_) => {
				self.add_span(pattern.1, TokenType::Number, vec![], document);
			}
			Pattern::String(_) => self.add_span(pattern.1, TokenType::String, vec![], document),
			Pattern::Boolean(_) => self.add_span(pattern.1, TokenType::Keyword, vec![], document),
			Pattern::Binding { name, inner } => {
				self.add_ident(name.1, &name.0, binding_type, modifiers.clone());
				self.visit_pattern(inner, binding_type, modifiers, document);
			}
			Pattern::List(items) | Pattern::Tuple(items) => {
				for item in items {
					match &item.0 {
						nymph_compiler::ast::expr::ListPatternEntry::Item(pattern) => {
							self.visit_pattern(pattern, binding_type, modifiers.clone(), document);
						}
						nymph_compiler::ast::expr::ListPatternEntry::Rest(Some(rest)) => {
							self.add_ident(rest.1, &rest.0, binding_type, modifiers.clone());
						}
						nymph_compiler::ast::expr::ListPatternEntry::Rest(None) => {}
					}
				}
			}
			Pattern::Map(entries) => {
				for entry in entries {
					match &entry.0 {
						nymph_compiler::ast::expr::MapPatternEntry::Entry(key, value) => {
							self.visit_pattern(key, binding_type, modifiers.clone(), document);
							self.visit_pattern(value, binding_type, modifiers.clone(), document);
						}
						nymph_compiler::ast::expr::MapPatternEntry::Rest(Some(rest)) => {
							self.add_ident(rest.1, &rest.0, binding_type, modifiers.clone());
						}
						nymph_compiler::ast::expr::MapPatternEntry::Rest(None) => {}
					}
				}
			}
			Pattern::Range(range) => self.visit_range_pattern(range, document),
			Pattern::Struct { path, fields } => {
				for segment in path {
					self.add_ident(segment.1, &segment.0, TokenType::Type, vec![]);
				}
				for field in fields {
					match &field.0 {
						StructPatternField::Named(name) => {
							self.add_ident(name.1, &name.0, TokenType::Member, vec![]);
						}
						StructPatternField::Value { name, value } => {
							self.add_ident(name.1, &name.0, TokenType::Member, vec![]);
							self.visit_pattern(value, binding_type, modifiers.clone(), document);
						}
						StructPatternField::Rest => {}
					}
				}
			}
			Pattern::Placeholder => {}
			Pattern::Union(left, right) => {
				self.visit_pattern(left, binding_type, modifiers.clone(), document);
				self.visit_pattern(right, binding_type, modifiers, document);
			}
			Pattern::Grouped(inner) => self.visit_pattern(inner, binding_type, modifiers, document),
		}
	}

	fn visit_range_expr(
		&mut self,
		range: &nymph_compiler::ast::expr::RangeKind,
		document: &Document,
	) {
		match range {
			nymph_compiler::ast::expr::RangeKind::From(expr) => {
				if let Some(operator_span) = find_operator_before(&self.content, expr.1.start, &["..<"]) {
					self.add_operator_span(operator_span);
				}
				self.visit_expr(expr, document);
			}
			nymph_compiler::ast::expr::RangeKind::To(expr) => {
				if let Some(operator_span) = find_operator_before(&self.content, expr.1.start, &["..<"]) {
					self.add_operator_span(operator_span);
				}
				self.visit_expr(expr, document);
			}
			nymph_compiler::ast::expr::RangeKind::ToInclusive(expr) => {
				if let Some(operator_span) = find_operator_before(&self.content, expr.1.start, &["..="]) {
					self.add_operator_span(operator_span);
				}
				self.visit_expr(expr, document);
			}
			nymph_compiler::ast::expr::RangeKind::Exclusive { min, max } => {
				self.visit_expr(min, document);
				if let Some(operator_span) =
					find_operator_between(&self.content, min.1.end, max.1.start, &["..<"])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_expr(max, document);
			}
			nymph_compiler::ast::expr::RangeKind::Inclusive { min, max } => {
				self.visit_expr(min, document);
				if let Some(operator_span) =
					find_operator_between(&self.content, min.1.end, max.1.start, &["..="])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_expr(max, document);
			}
		}
	}

	fn visit_range_pattern(
		&mut self,
		range: &nymph_compiler::ast::expr::RangePatternKind,
		document: &Document,
	) {
		match range {
			nymph_compiler::ast::expr::RangePatternKind::ExclusiveMin(pattern) => {
				if let Some(operator_span) = find_operator_after(&self.content, pattern.1.end, &["..<"]) {
					self.add_operator_span(operator_span);
				}
				self.visit_pattern(pattern, TokenType::Variable, vec![], document);
			}
			nymph_compiler::ast::expr::RangePatternKind::InclusiveMax(pattern) => {
				if let Some(operator_span) = find_operator_before(&self.content, pattern.1.start, &["..="])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_pattern(pattern, TokenType::Variable, vec![], document);
			}
			nymph_compiler::ast::expr::RangePatternKind::ExclusiveBoth { min, max } => {
				self.visit_pattern(min, TokenType::Variable, vec![], document);
				if let Some(operator_span) =
					find_operator_between(&self.content, min.1.end, max.1.start, &["..<"])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_pattern(max, TokenType::Variable, vec![], document);
			}
			nymph_compiler::ast::expr::RangePatternKind::InclusiveBoth { min, max } => {
				self.visit_pattern(min, TokenType::Variable, vec![], document);
				if let Some(operator_span) =
					find_operator_between(&self.content, min.1.end, max.1.start, &["..="])
				{
					self.add_operator_span(operator_span);
				}
				self.visit_pattern(max, TokenType::Variable, vec![], document);
			}
		}
	}

	fn visit_type(&mut self, type_: &Type, document: &Document) {
		match type_ {
			Type::Int
			| Type::UInt
			| Type::Float
			| Type::Char
			| Type::String
			| Type::Boolean
			| Type::Void
			| Type::Never
			| Type::Self_
			| Type::Infer => {}
			Type::Intersection(left, right) => {
				self.visit_type(&left.0, document);
				self.visit_type(&right.0, document);
			}
			Type::List(item) => self.visit_type(&item.0, document),
			Type::Tuple(items) => {
				for item in items {
					self.visit_type(&item.0, document);
				}
			}
			Type::Map(key, value) => {
				self.visit_type(&key.0, document);
				self.visit_type(&value.0, document);
			}
			Type::Function {
				params,
				return_type,
			} => {
				for (_, param_type) in params {
					self.visit_type(&param_type.0, document);
				}
				self.visit_type(&return_type.0, document);
			}
			Type::Reference { name, generics } => {
				self.add_ident(name.1, &name.0, TokenType::Type, vec![]);
				for generic in generics {
					self.visit_type(&generic.0.value.0, document);
				}
			}
			Type::Grouped(inner) => self.visit_type(&inner.0, document),
		}
	}

	fn add_ident(
		&mut self,
		span: Span,
		text: &str,
		token_type: TokenType,
		modifiers: Vec<TokenModifier>,
	) {
		self.add_token_from_span(span, text.len(), token_type, modifiers);
	}

	fn add_span(
		&mut self,
		span: Span,
		token_type: TokenType,
		modifiers: Vec<TokenModifier>,
		_document: &Document,
	) {
		if span.end <= span.start {
			return;
		}
		let source = &self.content;
		if !source.is_char_boundary(span.start) || !source.is_char_boundary(span.end) {
			return;
		}
		let length = source[span.start..span.end].encode_utf16().count();
		self.add_token_from_span(span, length, token_type, modifiers);
	}

	fn add_operator_span(&mut self, span: Span) {
		let source = &self.content;
		if span.end <= span.start
			|| !source.is_char_boundary(span.start)
			|| !source.is_char_boundary(span.end)
		{
			return;
		}
		let length = source[span.start..span.end].encode_utf16().count();
		self.add_token_from_span(span, length, TokenType::Operator, vec![]);
	}

	fn add_token_from_span(
		&mut self,
		span: Span,
		length: usize,
		token_type: TokenType,
		modifiers: Vec<TokenModifier>,
	) {
		if length == 0 {
			return;
		}
		let Some((line, start_char)) = line_col_for_offset(&self.content, span.start) else {
			return;
		};
		self.add_token(line, start_char, length, token_type, modifiers);
	}

	/// Add a semantic token
	fn add_token(
		&mut self,
		line: usize,
		col: usize,
		length: usize,
		token_type: TokenType,
		modifiers: Vec<TokenModifier>,
	) {
		self.tokens.push(SemanticToken {
			line,
			start_char: col,
			length,
			token_type,
			modifiers,
		});
	}

	fn sort_and_dedupe(&mut self) {
		self.tokens.sort_by(|a, b| {
			(
				a.line,
				a.start_char,
				a.length,
				a.token_type,
				a.modifiers.len(),
			)
				.cmp(&(
					b.line,
					b.start_char,
					b.length,
					b.token_type,
					b.modifiers.len(),
				))
		});
		self.tokens.dedup();
	}
}

fn line_col_for_offset(content: &str, target_offset: usize) -> Option<(usize, usize)> {
	let mut line = 0usize;
	let mut line_start = 0usize;

	for (idx, ch) in content.char_indices() {
		if idx >= target_offset {
			break;
		}
		if ch == '\n' {
			line += 1;
			line_start = idx + ch.len_utf8();
		}
	}

	let col = content[line_start..target_offset].encode_utf16().count();
	Some((line, col))
}

fn is_identifier_char(ch: char) -> bool {
	ch.is_alphanumeric() || ch == '_'
}

fn modifiers_for_binding(mutable: bool) -> Vec<TokenModifier> {
	let mut modifiers = vec![TokenModifier::Declaration, TokenModifier::Definition];
	if mutable {
		modifiers.push(TokenModifier::Mutable);
	}
	modifiers
}

fn prefix_operator_text(op: PrefixOperator) -> &'static str {
	match op {
		PrefixOperator::BoolNot => "!",
		PrefixOperator::Negate => "-",
		PrefixOperator::BitNot => "~",
	}
}

fn postfix_operator_text(op: PostfixOperator) -> &'static str {
	match op {
		PostfixOperator::ErrorReturn => "?",
	}
}

fn binary_operator_text(op: BinaryOperator) -> &'static str {
	match op {
		BinaryOperator::Plus => "+",
		BinaryOperator::Minus => "-",
		BinaryOperator::Times => "*",
		BinaryOperator::Divide => "/",
		BinaryOperator::Remainder => "%",
		BinaryOperator::Power => "**",
		BinaryOperator::BitAnd => "&",
		BinaryOperator::BitOr => "|",
		BinaryOperator::BitXor => "^",
		BinaryOperator::LeftShift => "<<",
		BinaryOperator::RightShift => ">>",
		BinaryOperator::Equals => "==",
		BinaryOperator::NotEquals => "!=",
		BinaryOperator::LessThan => "<",
		BinaryOperator::LessThanEquals => "<=",
		BinaryOperator::GreaterThan => ">",
		BinaryOperator::GreaterThanEquals => ">=",
		BinaryOperator::In => "in",
		BinaryOperator::NotIn => "!in",
		BinaryOperator::BoolAnd => "&&",
		BinaryOperator::BoolOr => "||",
		BinaryOperator::Pipe => "|>",
		BinaryOperator::Unwrap => "??",
	}
}

fn assign_operator_text(op: AssignOperator) -> &'static str {
	match op {
		AssignOperator::Assign => "=",
		AssignOperator::PlusAssign => "+=",
		AssignOperator::MinusAssign => "-=",
		AssignOperator::TimesAssign => "*=",
		AssignOperator::DivideAssign => "/=",
		AssignOperator::RemainderAssign => "%=",
		AssignOperator::PowerAssign => "**=",
		AssignOperator::LeftShiftAssign => "<<=",
		AssignOperator::RightShiftAssign => ">>=",
		AssignOperator::BitAndAssign => "&=",
		AssignOperator::BitXorAssign => "^=",
		AssignOperator::BitOrAssign => "|=",
		AssignOperator::BitNotAssign => "~=",
		AssignOperator::BoolAndAssign => "&&=",
		AssignOperator::BoolOrAssign => "||=",
	}
}

fn type_operator_text(op: TypeOperator) -> &'static str {
	match op {
		TypeOperator::As => "as",
	}
}

fn pattern_operator_text(op: PatternOperator) -> &'static str {
	match op {
		PatternOperator::Is => "is",
		PatternOperator::NotIs => "!is",
	}
}

fn find_operator_between(
	source: &str,
	start: usize,
	end: usize,
	candidates: &[&str],
) -> Option<Span> {
	if start >= end
		|| end > source.len()
		|| !source.is_char_boundary(start)
		|| !source.is_char_boundary(end)
	{
		return None;
	}
	let window = &source[start..end];
	for candidate in candidates {
		if let Some(rel) = window.find(candidate) {
			let op_start = start + rel;
			let op_end = op_start + candidate.len();
			if source.is_char_boundary(op_start) && source.is_char_boundary(op_end) {
				return Some(Span::new(op_start, op_end));
			}
		}
	}
	None
}

fn find_operator_between_last(
	source: &str,
	start: usize,
	end: usize,
	candidates: &[&str],
) -> Option<Span> {
	if start >= end
		|| end > source.len()
		|| !source.is_char_boundary(start)
		|| !source.is_char_boundary(end)
	{
		return None;
	}
	let window = &source[start..end];
	for candidate in candidates {
		if let Some(rel) = window.rfind(candidate) {
			let op_start = start + rel;
			let op_end = op_start + candidate.len();
			if source.is_char_boundary(op_start) && source.is_char_boundary(op_end) {
				return Some(Span::new(op_start, op_end));
			}
		}
	}
	None
}

fn find_operator_before(source: &str, end: usize, candidates: &[&str]) -> Option<Span> {
	let trimmed_end = skip_backward_whitespace(source, end);
	for candidate in candidates {
		if trimmed_end < candidate.len() {
			continue;
		}
		let start = trimmed_end - candidate.len();
		if !source.is_char_boundary(start) || !source.is_char_boundary(trimmed_end) {
			continue;
		}
		if &source[start..trimmed_end] == *candidate {
			return Some(Span::new(start, trimmed_end));
		}
	}
	None
}

fn find_operator_after(source: &str, start: usize, candidates: &[&str]) -> Option<Span> {
	let trimmed_start = skip_forward_whitespace(source, start);
	for candidate in candidates {
		let end = trimmed_start + candidate.len();
		if end > source.len()
			|| !source.is_char_boundary(trimmed_start)
			|| !source.is_char_boundary(end)
		{
			continue;
		}
		if &source[trimmed_start..end] == *candidate {
			return Some(Span::new(trimmed_start, end));
		}
	}
	None
}

fn skip_backward_whitespace(source: &str, mut index: usize) -> usize {
	while index > 0 {
		let Some(ch) = source[..index].chars().next_back() else {
			break;
		};
		if !ch.is_whitespace() {
			break;
		}
		index -= ch.len_utf8();
	}
	index
}

fn skip_forward_whitespace(source: &str, mut index: usize) -> usize {
	while index < source.len() {
		let Some(ch) = source[index..].chars().next() else {
			break;
		};
		if !ch.is_whitespace() {
			break;
		}
		index += ch.len_utf8();
	}
	index
}

impl Default for SemanticTokenizer {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_token_type_as_str() {
		assert_eq!(TokenType::Keyword.as_str(), "keyword");
		assert_eq!(TokenType::Function.as_str(), "function");
		assert_eq!(TokenType::Variable.as_str(), "variable");
	}

	#[test]
	fn test_token_modifier_as_str() {
		assert_eq!(TokenModifier::Declaration.as_str(), "declaration");
		assert_eq!(TokenModifier::Mutable.as_str(), "mutable");
	}

	#[test]
	fn test_keyword_token_positions_are_zero_based() {
		let mut tokenizer = SemanticTokenizer::new();
		let tokens = tokenizer.tokenize("let x = 5\nfunc foo() -> 1");
		assert!(
			tokens.iter().any(|token| {
				token.token_type == TokenType::Keyword
					&& token.line == 0
					&& token.start_char == 0
					&& token.length == 3
			}),
			"expected zero-based semantic token positions"
		);
	}

	#[test]
	fn test_operator_token_covers_only_arrow_glyphs() {
		let doc = Document::new(
			"file:///test.nym".to_string(),
			"let f = (value: int) -> value\n".to_string(),
		);
		let mut tokenizer = SemanticTokenizer::new();
		let tokens = tokenizer.tokenize_document(&doc);

		let arrow_token = tokens
			.iter()
			.find(|token| {
				token.token_type == TokenType::Operator
					&& doc
						.lsp_position_to_offset(token.line as u32, token.start_char as u32)
						.and_then(|start| {
							doc
								.offset_to_lsp_position(start + 2)
								.map(|end| (start, end.character))
						})
						.is_some_and(|(start, _)| &doc.content[start..start + 2] == "->")
			})
			.expect("expected arrow operator token");

		assert_eq!(arrow_token.length, 2);
	}

	#[test]
	fn test_operator_token_covers_only_spread_glyphs() {
		let doc = Document::new(
			"file:///test.nym".to_string(),
			"func main() -> call(...items)\n".to_string(),
		);
		let mut tokenizer = SemanticTokenizer::new();
		let tokens = tokenizer.tokenize_document(&doc);

		let spread_token = tokens
			.iter()
			.find(|token| {
				token.token_type == TokenType::Operator
					&& doc
						.lsp_position_to_offset(token.line as u32, token.start_char as u32)
						.is_some_and(|start| {
							start + 3 <= doc.content.len() && &doc.content[start..start + 3] == "..."
						})
			})
			.expect("expected spread operator token");

		assert_eq!(spread_token.length, 3);
	}

	#[test]
	fn test_operator_token_does_not_cover_whole_binary_expression() {
		let doc = Document::new(
			"file:///test.nym".to_string(),
			"func main() -> 1 + 2\n".to_string(),
		);
		let mut tokenizer = SemanticTokenizer::new();
		let tokens = tokenizer.tokenize_document(&doc);

		let plus_token = tokens
			.iter()
			.find(|token| {
				token.token_type == TokenType::Operator
					&& doc
						.lsp_position_to_offset(token.line as u32, token.start_char as u32)
						.is_some_and(|start| start < doc.content.len() && &doc.content[start..start + 1] == "+")
			})
			.expect("expected plus operator token");

		assert_eq!(plus_token.length, 1);
	}
}
