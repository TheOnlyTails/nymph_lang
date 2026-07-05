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

pub struct Parser<'src> {
	cursor: TokenCursor<'src>,
	diagnostics: Vec<Diagnostic>,
	next_id: u32,
}

/// The result of parsing: the (best-effort) tree plus every diagnostic encountered.
pub struct ParseResult<T> {
	pub tree: T,
	pub diagnostics: Vec<Diagnostic>,
}

/// Lex and parse a whole source file into a [`Module`].
pub fn parse_module(source: &str, module_path: impl Into<EcoString>) -> ParseResult<Module> {
	let lexed = lex(source);
	let eoi = Span::new(source.len(), source.len());
	let mut parser = Parser::new(&lexed.tokens, eoi);
	let members = parser.parse_module_members();
	let mut diagnostics = lexed.diagnostics;
	diagnostics.extend(parser.diagnostics);
	ParseResult {
		tree: Module {
			members,
			path: module_path.into(),
		},
		diagnostics,
	}
}

/// Lex and parse a single expression (used by the REPL and tests).
pub fn parse_expression(source: &str) -> ParseResult<Expr> {
	let lexed = lex(source);
	let eoi = Span::new(source.len(), source.len());
	let mut parser = Parser::new(&lexed.tokens, eoi);
	let expr = parser.parse_expr();
	if !parser.at_end() {
		let span = parser.current_span();
		parser.error(Diagnostic::error(
			"unexpected trailing tokens after expression",
			span,
		));
	}
	let mut diagnostics = lexed.diagnostics;
	diagnostics.extend(parser.diagnostics);
	ParseResult {
		tree: expr,
		diagnostics,
	}
}

impl<'src> Parser<'src> {
	pub fn new(tokens: &'src [Spanned<Token>], eoi: Span) -> Self {
		Self {
			cursor: TokenCursor::new(tokens, eoi),
			diagnostics: Vec::new(),
			next_id: 0,
		}
	}

	/// Build a self-spanned expression, assigning the next fresh node id.
	pub(super) fn mk_expr(&mut self, kind: ExprKind, span: Span) -> Expr {
		let id = NodeId(self.next_id);
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

	fn error(&mut self, diagnostic: Diagnostic) {
		self.diagnostics.push(diagnostic);
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
			self.error(Diagnostic::error(
				format!("expected {}, found {found}", token.describe()),
				span,
			));
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
			_ => {
				let found = self.peek().map_or("end of input", Token::describe);
				let span = self.current_span();
				self.error(Diagnostic::error(
					format!("expected an identifier, found {found}"),
					span,
				));
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
