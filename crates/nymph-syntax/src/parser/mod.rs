//! A hand-written recursive-descent + Pratt parser over the flat token stream.
//!
//! Choosing recursive descent (rather than a combinator parser over tokens) buys
//! precise control over operator precedence, error recovery, and diagnostic quality.
//! The parser never panics on malformed input: it records a [`Diagnostic`], synthesises
//! a best-effort node, and keeps going so a single typo doesn't hide the rest of a file.

mod cursor;
mod decl;
mod expr;
mod pattern;
mod ty;

use crate::errors::ParseError;
use cursor::TokenCursor;
use ecow::EcoString;
use nymph_ast::{
	Ident, NodeId, Span, Spanned,
	decl::Module,
	expr::{Expr, ExprKind},
	token::Token,
};
use nymph_diagnostics::Diagnostic;

use crate::lex;

/// A parsed module and the token range consumed by each top-level declaration.
/// The ranges line up with `parsed.tree.members` and let expansion keep the
/// canonical token stream alongside the syntax tree.
pub struct ModuleParseResult {
	pub parsed: ParseResult<Module>,
	pub declaration_ranges: Vec<std::ops::Range<usize>>,
}

pub struct Parser<'src> {
	cursor: TokenCursor<'src>,
	diagnostics: Vec<Diagnostic>,
	incomplete: bool,
	next_id: u32,
}

/// The result of parsing: the (best-effort) tree plus every diagnostic encountered.
pub struct ParseResult<T> {
	pub tree: T,
	pub diagnostics: Vec<Diagnostic>,
	/// Whether every syntax failure is caused by reaching end-of-input while
	/// expecting more syntax. Appending input may complete this parse.
	pub incomplete: bool,
}

/// Lex and parse a whole source file into a [`Module`].
pub fn parse_module(source: &str, module_path: impl Into<EcoString>) -> ParseResult<Module> {
	let lexed = lex(source);
	let eoi = Span::new(source.len(), source.len());
	let mut parsed = parse_module_tokens(&lexed.tokens, eoi, module_path);
	let incomplete = lexed.incomplete
		|| (!parsed.diagnostics.is_empty() && lexed.diagnostics.is_empty() && parsed.incomplete);
	let mut diagnostics = lexed.diagnostics;
	diagnostics.append(&mut parsed.diagnostics);
	parsed.diagnostics = diagnostics;
	parsed.incomplete = incomplete;
	parsed
}

/// Parse an already lexed flat token stream as a module. Expansion uses this
/// entry point so token provenance and spans survive reparsing.
pub fn parse_module_tokens(
	tokens: &[Spanned<Token>],
	eoi: Span,
	module_path: impl Into<EcoString>,
) -> ParseResult<Module> {
	parse_module_tokens_from(tokens, eoi, module_path, 0)
}

/// Parse generated tokens while continuing a module's node-id sequence.
pub fn parse_module_tokens_from(
	tokens: &[Spanned<Token>],
	eoi: Span,
	module_path: impl Into<EcoString>,
	first_node_id: u32,
) -> ParseResult<Module> {
	parse_module_tokens_from_with_ranges(tokens, eoi, module_path, first_node_id).parsed
}

/// Parse generated tokens while retaining each top-level declaration's token range.
pub fn parse_module_tokens_from_with_ranges(
	tokens: &[Spanned<Token>],
	eoi: Span,
	module_path: impl Into<EcoString>,
	first_node_id: u32,
) -> ModuleParseResult {
	let mut parser = Parser::new(tokens, eoi);
	parser.next_id = first_node_id;
	let (members, declaration_ranges) = parser.parse_module_members_with_ranges();
	ModuleParseResult {
		parsed: ParseResult {
			tree: Module {
				members,
				path: module_path.into(),
			},
			diagnostics: parser.diagnostics,
			incomplete: parser.incomplete,
		},
		declaration_ranges,
	}
}

/// Lex and parse a single expression (used by the REPL and tests).
pub fn parse_expression(source: &str) -> ParseResult<Expr> {
	let lexed = lex(source);
	let eoi = Span::new(source.len(), source.len());
	let mut parsed = parse_expression_tokens_from(&lexed.tokens, eoi, 0);
	let incomplete = lexed.incomplete
		|| (!parsed.diagnostics.is_empty() && lexed.diagnostics.is_empty() && parsed.incomplete);
	let mut diagnostics = lexed.diagnostics;
	diagnostics.append(&mut parsed.diagnostics);
	parsed.diagnostics = diagnostics;
	parsed.incomplete = incomplete;
	parsed
}

/// Parse generated tokens as one expression while preserving syntax identity.
pub fn parse_expression_tokens_from(
	tokens: &[Spanned<Token>],
	eoi: Span,
	first_node_id: u32,
) -> ParseResult<Expr> {
	let mut parser = Parser::new(tokens, eoi);
	parser.next_id = first_node_id;
	let expr = parser.parse_expr();
	if !parser.at_end() {
		let span = parser.current_span();
		parser.emit(span, ParseError::TrailingTokens);
	}
	ParseResult {
		tree: expr,
		diagnostics: parser.diagnostics,
		incomplete: parser.incomplete,
	}
}

impl<'src> Parser<'src> {
	pub fn new(tokens: &'src [Spanned<Token>], eoi: Span) -> Self {
		Self {
			cursor: TokenCursor::new(tokens, eoi),
			diagnostics: Vec::new(),
			incomplete: true,
			next_id: 0,
		}
	}

	/// Build a self-spanned expression, assigning the next fresh node id.
	pub(super) fn mk_expr(&mut self, kind: ExprKind, span: Span) -> Expr {
		let id = NodeId::generated(self.next_id, span.origin);
		self.next_id += 1;
		Expr { kind, span, id }
	}

	// ── Cursor conveniences ──────────────────────────────────────────────────
	fn peek(&self) -> Option<&'src Token> {
		self.cursor.peek_token()
	}

	fn peek_nth(&self, n: usize) -> Option<&'src Token> {
		self.cursor.peek_nth_token(n)
	}

	fn advance(&mut self) -> Option<&'src Spanned<Token>> {
		self.cursor.advance()
	}

	fn check(&self, token: &Token) -> bool {
		self.cursor.check(token)
	}

	fn at_end(&self) -> bool {
		self.cursor.at_end()
	}

	fn current_span(&self) -> Span {
		self.cursor.current_span()
	}

	fn peek_nth_span(&self, n: usize) -> Option<Span> {
		self.cursor.peek_nth_span(n)
	}

	fn span_from(&self, start: usize) -> Span {
		self.cursor.span_from(start)
	}

	fn position(&self) -> usize {
		self.cursor.position()
	}

	fn restore(&mut self, pos: usize) {
		self.cursor.restore(pos);
	}

	fn token_slice(&self, start: usize, end: usize) -> &'src [Spanned<Token>] {
		self.cursor.slice(start, end)
	}

	/// Emit a typed [`ParseError`](crate::errors::ParseError), anchored at `span`.
	fn emit(&mut self, span: Span, err: crate::errors::ParseError) {
		use nymph_diagnostics::IntoDiagnostic;
		let incomplete = err.is_incomplete() || (self.at_end() && err.is_append_repairable());
		self.incomplete &= incomplete;
		self.diagnostics.push(err.as_diagnostic(span));
	}

	/// Consume the given token if present, returning its span.
	fn eat(&mut self, token: &Token) -> Option<Span> {
		if self.check(token) {
			self.advance().map(|t| t.1)
		} else {
			None
		}
	}

	/// Consume the given token or record an "expected X" error (without advancing).
	fn expect(&mut self, token: &Token) -> Option<Span> {
		if let Some(span) = self.eat(token) {
			Some(span)
		} else {
			let found = self.peek().map_or("end of input", Token::describe);
			let span = self.current_span();
			self.emit(
				span,
				ParseError::ExpectedToken {
					expected: token.describe().into(),
					found: found.into(),
				},
			);
			None
		}
	}

	/// Parse an identifier token, or record an error and return a placeholder.
	fn expect_ident(&mut self) -> Ident {
		match self.peek() {
			Some(Token::Identifier(name)) => {
				let name = name.clone();
				let span = self
					.advance()
					.map(|t| t.1)
					.unwrap_or_else(|| self.current_span());
				Spanned(name, span)
			}
			Some(Token::Echo) => {
				let span = self
					.advance()
					.map(|token| token.1)
					.unwrap_or_else(|| self.current_span());
				Spanned("echo".into(), span)
			}
			_ => {
				let found = self.peek().map_or("end of input", Token::describe);
				let span = self.current_span();
				self.emit(
					span,
					ParseError::ExpectedIdentifier {
						found: found.into(),
					},
				);
				Spanned(EcoString::new(), span)
			}
		}
	}

	/// Skip tokens until a likely declaration boundary, used to recover after an error.
	fn recover_to_declaration(&mut self) {
		while let Some(token) = self.peek() {
			if matches!(
				token,
				Token::Func
					| Token::Const
					| Token::Async
					| Token::Let
					| Token::Struct
					| Token::Enum
					| Token::Interface
					| Token::Impl
					| Token::Type
					| Token::Import
					| Token::Namespace
					| Token::Public
					| Token::Internal
					| Token::Private
					| Token::External
					| Token::Effect
			) {
				break;
			}
			self.advance();
		}
	}

	/// Parse a comma-separated list of `item`s until `close`, allowing a trailing comma.
	/// The caller is responsible for having consumed the opening delimiter; this consumes
	/// the closing one.
	fn comma_separated<T>(&mut self, close: &Token, mut item: impl FnMut(&mut Self) -> T) -> Vec<T> {
		let mut items = Vec::new();
		while !self.check(close) && !self.at_end() {
			items.push(item(self));
			if self.eat(&Token::Comma).is_none() {
				break;
			}
		}
		self.expect(close);
		items
	}
}
