//! A simple position cursor over the flat token stream, with lookahead and
//! backtracking (via [`TokenCursor::position`] / [`TokenCursor::restore`]).

use nymph_ast::{Span, Spanned, token::Token};

#[derive(Debug, Clone)]
pub struct TokenCursor<'src> {
	tokens: &'src [Spanned<Token>],
	pos: usize,
	/// The end-of-input span, used when peeking past the last token.
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

	pub fn peek(&self) -> Option<&'src Spanned<Token>> {
		self.tokens.get(self.pos)
	}

	pub fn peek_token(&self) -> Option<&'src Token> {
		self.peek().map(|s| &s.0)
	}

	pub fn peek_nth_token(&self, n: usize) -> Option<&'src Token> {
		self.tokens.get(self.pos + n).map(|s| &s.0)
	}

	pub fn peek_nth_span(&self, n: usize) -> Option<Span> {
		self.tokens.get(self.pos + n).map(|s| s.1)
	}

	pub fn advance(&mut self) -> Option<&'src Spanned<Token>> {
		let token = self.tokens.get(self.pos);
		if token.is_some() {
			self.pos += 1;
		}
		token
	}

	pub fn check(&self, token: &Token) -> bool {
		self.peek_token() == Some(token)
	}

	pub fn at_end(&self) -> bool {
		self.pos >= self.tokens.len()
	}

	pub fn current_span(&self) -> Span {
		self.peek().map_or(self.eoi, |t| t.1)
	}

	pub fn position(&self) -> usize {
		self.pos
	}

	pub fn restore(&mut self, pos: usize) {
		self.pos = pos;
	}

	/// The span from the token at index `start` through the most recently consumed token.
	pub fn span_from(&self, start: usize) -> Span {
		let start_pos = self.tokens.get(start).map_or(self.eoi.start, |t| t.1.start);
		let end_pos = if self.pos > 0 {
			self
				.tokens
				.get(self.pos - 1)
				.map_or(self.eoi.end, |t| t.1.end)
		} else {
			start_pos
		};
		Span::new(start_pos, end_pos.max(start_pos))
	}
}
