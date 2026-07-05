use ecow::EcoString;

use crate::{
	ast::{Ident, Span, Spanned},
	lexer::token::Token,
	parser::error::ParseErrorKind,
};

use super::{cursor::TokenCursor, error::ParseError};

pub(super) fn span(start: usize, end: usize) -> Span {
	Span::new(start, end)
}

pub struct Parser<'src> {
	pub(super) cursor: TokenCursor<'src>,
	pub(super) errors: Vec<ParseError>,
	pub(super) file_path: EcoString,
}

impl<'src> Parser<'src> {
	pub fn new(tokens: &'src [Spanned<Token>], eoi: Span, file_path: EcoString) -> Self {
		Self {
			cursor: TokenCursor::new(tokens, eoi),
			errors: Vec::new(),
			file_path,
		}
	}

	pub fn into_errors(self) -> Vec<ParseError> {
		self.errors
	}

	pub(super) fn error(&mut self, error: ParseError) {
		self.errors.push(error);
	}

	pub(super) fn peek(&self) -> Option<&Token> {
		self.cursor.peek_token()
	}

	pub(super) fn peek_spanned(&self) -> Option<&Spanned<Token>> {
		self.cursor.peek()
	}

	pub(super) fn peek_nth(&self, n: usize) -> Option<&Token> {
		self.cursor.peek_nth_token(n)
	}

	pub(super) fn advance(&mut self) -> Option<&Spanned<Token>> {
		self.cursor.advance()
	}

	pub(super) fn check(&self, token: &Token) -> bool {
		self.cursor.check(token)
	}

	pub(super) fn at_end(&self) -> bool {
		self.cursor.at_end()
	}

	pub(super) fn current_span(&self) -> Span {
		self.cursor.current_span()
	}

	pub(super) fn previous_span(&self) -> Span {
		self.cursor.previous_span()
	}

	pub(super) fn span_from(&self, start: usize) -> Span {
		self.cursor.span_from(start)
	}

	pub(super) fn position(&self) -> usize {
		self.cursor.position()
	}

	pub(super) fn restore(&mut self, pos: usize) {
		self.cursor.restore(pos);
	}

	pub(super) fn consume(&mut self, token: &Token) -> Option<Span> {
		self.cursor.consume(token).map(|t| t.1)
	}

	pub(super) fn expect(&mut self, token: &Token, expected: impl Into<EcoString>) -> Option<Span> {
		if let Some(span) = self.consume(token) {
			Some(span)
		} else {
			let span = self.current_span();
			let expected_str = expected.into();
			if self.at_end() {
				self.error(ParseError::unexpected_eof(vec![expected_str], span));
			} else if let Some(found) = self.peek().cloned() {
				self.error({
					let expected = vec![expected_str];
					ParseError {
						kind: ParseErrorKind::UnexpectedToken { found, expected },
						span,
						context: vec![],
					}
				});
			}
			None
		}
	}

	pub(super) fn identifier(&mut self) -> Option<Ident> {
		if let Some(Spanned(Token::Identifier(name), span)) = self.peek_spanned() {
			let ident = Spanned(name.clone(), *span);
			self.advance();
			Some(ident)
		} else {
			None
		}
	}

	pub(super) fn expect_identifier(&mut self) -> Option<Ident> {
		if let Some(ident) = self.identifier() {
			Some(ident)
		} else {
			let span = self.current_span();
			if self.at_end() {
				self.error(ParseError::unexpected_eof(
					vec!["an identifier".into()],
					span,
				));
			} else if let Some(found) = self.peek().cloned() {
				self.error(ParseError::expected_identifier(found, span));
			}
			None
		}
	}

	pub(super) fn span_to_current(&self, start_span: Span) -> Span {
		self.cursor.span_to_current(start_span)
	}

	pub(super) fn synchronize_to_declaration(&mut self) {
		while !self.at_end() {
			match self.peek() {
				Some(
					Token::Import
					| Token::Let
					| Token::Func
					| Token::Type
					| Token::Struct
					| Token::Enum
					| Token::Namespace
					| Token::Interface
					| Token::Impl
					| Token::Public
					| Token::Internal
					| Token::Private
					| Token::External,
				) => return,
				_ => {
					self.advance();
				}
			}
		}
	}

	pub(super) fn with_nested<T, F>(&mut self, tokens: &[Spanned<Token>], eoi: Span, f: F) -> T
	where
		F: for<'a> FnOnce(&mut Parser<'a>) -> T,
	{
		let mut nested = Parser {
			cursor: TokenCursor::new(tokens, eoi),
			errors: Vec::new(),
			file_path: self.file_path.clone(),
		};
		let result = f(&mut nested);
		self.errors.extend(nested.errors);
		result
	}
}
