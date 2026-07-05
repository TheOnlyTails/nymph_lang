use crate::{
	ast::{Span, Spanned},
	lexer::token::Token,
};

#[derive(Debug, Clone)]
pub struct TokenCursor<'src> {
	tokens: &'src [Spanned<Token>],
	pos: usize,
	eoi: Span,
}

impl<'src> TokenCursor<'src> {
	pub fn new(tokens: &'src [Spanned<Token>], eoi: Span) -> Self {
		Self {
			tokens,
			pos: 0,
			eoi,
		}
	}

	pub fn peek(&self) -> Option<&Spanned<Token>> {
		self.tokens.get(self.pos)
	}

	pub fn peek_token(&self) -> Option<&Token> {
		self.peek().map(|s| &s.0)
	}

	pub fn peek_nth(&self, n: usize) -> Option<&Spanned<Token>> {
		self.tokens.get(self.pos + n)
	}

	pub fn peek_nth_token(&self, n: usize) -> Option<&Token> {
		self.peek_nth(n).map(|s| &s.0)
	}

	pub fn advance(&mut self) -> Option<&Spanned<Token>> {
		let token = self.tokens.get(self.pos);
		if token.is_some() {
			self.pos += 1;
		}
		token
	}

	pub fn check(&self, token: &Token) -> bool {
		self.peek_token() == Some(token)
	}

	pub fn consume(&mut self, token: &Token) -> Option<&Spanned<Token>> {
		if self.check(token) {
			self.advance()
		} else {
			None
		}
	}

	pub fn at_end(&self) -> bool {
		self.pos >= self.tokens.len()
	}

	pub fn current_span(&self) -> Span {
		self.peek().map(|t| t.1).unwrap_or(self.eoi)
	}

	pub fn previous_span(&self) -> Span {
		if self.pos > 0 {
			self.tokens[self.pos - 1].1
		} else {
			self.eoi
		}
	}

	pub fn position(&self) -> usize {
		self.pos
	}

	pub fn restore(&mut self, pos: usize) {
		self.pos = pos;
	}

	pub fn eoi(&self) -> Span {
		self.eoi
	}

	pub fn span_from(&self, start: usize) -> Span {
		let start_span = self
			.tokens
			.get(start)
			.map(|t| t.1.start)
			.unwrap_or(self.eoi.start);
		let end_span = if self.pos > 0 {
			self
				.tokens
				.get(self.pos - 1)
				.map(|t| t.1.end)
				.unwrap_or(self.eoi.end)
		} else {
			start_span
		};
		Span::new(start_span, end_span)
	}

	pub fn span_to_current(&self, start_span: Span) -> Span {
		let end = self.previous_span().end;
		Span::new(start_span.start, end.max(start_span.start))
	}
}
