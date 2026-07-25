//! Parsing of patterns, shared by `match`, `let`, function parameters, and the
//! `is` / `!is` operators.

use crate::errors::ParseError;
use nymph_ast::{
	Spanned,
	expr::{
		ListPatternEntry, MapPatternEntry, Pattern, RangePatternKind, StringPatternPart,
		StructPatternField,
	},
	token::{StrFragment, Token},
};
use ordered_float::OrderedFloat;

use super::Parser;

impl Parser<'_> {
	/// Parse a full pattern, including `|` unions (the lowest-precedence form).
	pub(super) fn parse_pattern(&mut self) -> Spanned<Pattern> {
		let start = self.position();
		let mut lhs = self.parse_range_pattern();
		while self.check(&Token::Pipe) {
			self.advance();
			let rhs = self.parse_range_pattern();
			lhs = Spanned(
				Pattern::Union(Box::new(lhs), Box::new(rhs)),
				self.span_from(start),
			);
		}
		lhs
	}

	fn parse_range_pattern(&mut self) -> Spanned<Pattern> {
		let start = self.position();

		// Leading unbounded ranges: `..max` / `..=max`.
		if self.check(&Token::DotDot) {
			self.advance();
			let max = self.parse_pattern_primary();
			return Spanned(
				Pattern::Range(RangePatternKind::To(Box::new(max))),
				self.span_from(start),
			);
		}
		if self.check(&Token::DotDotEq) {
			self.advance();
			let max = self.parse_pattern_primary();
			return Spanned(
				Pattern::Range(RangePatternKind::ToInclusive(Box::new(max))),
				self.span_from(start),
			);
		}

		let lhs = self.parse_pattern_primary();

		if self.check(&Token::DotDot) {
			self.advance();
			if self.can_start_pattern() {
				let max = self.parse_pattern_primary();
				return Spanned(
					Pattern::Range(RangePatternKind::Exclusive {
						min: Box::new(lhs),
						max: Box::new(max),
					}),
					self.span_from(start),
				);
			}
			return Spanned(
				Pattern::Range(RangePatternKind::From(Box::new(lhs))),
				self.span_from(start),
			);
		}
		if self.check(&Token::DotDotEq) {
			let operator_span = self.advance().unwrap().1;
			let max = if self.can_start_pattern() {
				self.parse_pattern_primary()
			} else {
				self.emit(operator_span, ParseError::MissingInclusiveRangeUpperBound);
				Spanned(Pattern::Placeholder, self.current_span())
			};
			return Spanned(
				Pattern::Range(RangePatternKind::Inclusive {
					min: Box::new(lhs),
					max: Box::new(max),
				}),
				self.span_from(start),
			);
		}

		lhs
	}

	fn can_start_pattern(&self) -> bool {
		matches!(
			self.peek(),
			Some(
				Token::Int(_)
					| Token::UInt(_)
					| Token::Float(_)
					| Token::Char(_)
					| Token::Str(_)
					| Token::True
					| Token::False
					| Token::Minus
					| Token::Underscore
					| Token::Identifier(_)
					| Token::HashLBracket
					| Token::HashLParen
					| Token::HashLBrace
					| Token::LParen
			)
		)
	}

	fn parse_pattern_primary(&mut self) -> Spanned<Pattern> {
		let start = self.position();
		let Some(token) = self.peek() else {
			let span = self.current_span();
			self.emit(span, ParseError::ExpectedPatternFoundEof);
			return Spanned(Pattern::Placeholder, span);
		};

		match token {
			Token::Underscore => {
				let span = self.advance().unwrap().1;
				Spanned(Pattern::Placeholder, span)
			}
			Token::True => {
				let span = self.advance().unwrap().1;
				Spanned(Pattern::Boolean(Spanned(true, span)), span)
			}
			Token::False => {
				let span = self.advance().unwrap().1;
				Spanned(Pattern::Boolean(Spanned(false, span)), span)
			}
			Token::Int(v) => {
				let v = *v as i64;
				let span = self.advance().unwrap().1;
				Spanned(Pattern::Int(Spanned(v, span)), span)
			}
			Token::UInt(v) => {
				let v = *v;
				let span = self.advance().unwrap().1;
				Spanned(Pattern::UInt(Spanned(v, span)), span)
			}
			Token::Float(v) => {
				let v = *v;
				let span = self.advance().unwrap().1;
				Spanned(Pattern::Float(Spanned(v, span)), span)
			}
			Token::Char(c) => {
				let c = *c;
				let span = self.advance().unwrap().1;
				Spanned(Pattern::Char(Spanned(c, span)), span)
			}
			Token::Minus => self.parse_negative_number_pattern(),
			Token::Str(_) => self.parse_string_pattern(),
			Token::HashLBracket => self.parse_list_pattern(),
			Token::HashLParen => self.parse_tuple_pattern(),
			Token::HashLBrace => self.parse_map_pattern(),
			Token::LParen => {
				self.advance();
				let inner = self.parse_pattern();
				self.expect(&Token::RParen);
				Spanned(Pattern::Grouped(Box::new(inner)), self.span_from(start))
			}
			Token::Identifier(_) => self.parse_ident_pattern(),
			other => {
				let span = self.current_span();
				self.emit(
					span,
					ParseError::ExpectedPattern {
						found: other.describe().into(),
					},
				);
				self.advance();
				Spanned(Pattern::Placeholder, span)
			}
		}
	}

	fn parse_negative_number_pattern(&mut self) -> Spanned<Pattern> {
		let start = self.position();
		self.advance(); // `-`
		match self.peek() {
			Some(Token::Int(v)) => {
				let v = -(*v as i64);
				self.advance();
				Spanned(
					Pattern::Int(Spanned(v, self.span_from(start))),
					self.span_from(start),
				)
			}
			Some(Token::Float(v)) => {
				let v = OrderedFloat(-v.0);
				self.advance();
				Spanned(
					Pattern::Float(Spanned(v, self.span_from(start))),
					self.span_from(start),
				)
			}
			_ => {
				let span = self.span_from(start);
				self.emit(span, ParseError::ExpectedNumberAfterMinus);
				Spanned(Pattern::Placeholder, span)
			}
		}
	}

	fn parse_string_pattern(&mut self) -> Spanned<Pattern> {
		let start = self.position();
		let Some(Token::Str(fragments)) = self.peek() else {
			unreachable!("guarded by caller");
		};
		let mut parts = Vec::new();
		for fragment in fragments {
			match &fragment.0 {
				StrFragment::Text(text) => {
					parts.push(Spanned(StringPatternPart::Text(text.clone()), fragment.1))
				}
				StrFragment::Escape(escape) => parts.push(Spanned(
					StringPatternPart::EscapeSequence(*escape),
					fragment.1,
				)),
				StrFragment::Interpolation(_) => self.emit(fragment.1, ParseError::InterpolationInPattern),
			}
		}
		self.advance();
		Spanned(Pattern::String(parts), self.span_from(start))
	}

	fn parse_list_pattern(&mut self) -> Spanned<Pattern> {
		let start = self.position();
		self.advance(); // `#[`
		let entries = self.comma_separated(&Token::RBracket, |p| p.parse_list_pattern_entry());
		Spanned(Pattern::List(entries), self.span_from(start))
	}

	fn parse_tuple_pattern(&mut self) -> Spanned<Pattern> {
		let start = self.position();
		self.advance(); // `#(`
		let entries = self.comma_separated(&Token::RParen, |p| p.parse_list_pattern_entry());
		Spanned(Pattern::Tuple(entries), self.span_from(start))
	}

	fn parse_list_pattern_entry(&mut self) -> Spanned<ListPatternEntry> {
		let start = self.position();
		if self.check(&Token::DotDotDot) {
			self.advance();
			let name = if let Some(Token::Identifier(_)) = self.peek() {
				Some(self.expect_ident())
			} else {
				None
			};
			Spanned(ListPatternEntry::Rest(name), self.span_from(start))
		} else {
			let pat = self.parse_pattern();
			Spanned(ListPatternEntry::Item(pat), self.span_from(start))
		}
	}

	fn parse_map_pattern(&mut self) -> Spanned<Pattern> {
		let start = self.position();
		self.advance(); // `#{`
		let entries = self.comma_separated(&Token::RBrace, |p| {
			let entry_start = p.position();
			if p.check(&Token::DotDotDot) {
				p.advance();
				let name = if let Some(Token::Identifier(_)) = p.peek() {
					Some(p.expect_ident())
				} else {
					None
				};
				Spanned(MapPatternEntry::Rest(name), p.span_from(entry_start))
			} else {
				let key = p.parse_pattern();
				p.expect(&Token::Colon);
				let value = p.parse_pattern();
				Spanned(MapPatternEntry::Entry(key, value), p.span_from(entry_start))
			}
		});
		Spanned(Pattern::Map(entries), self.span_from(start))
	}

	/// A bare identifier: a binding, an as-pattern (`name = inner`), or a struct/enum
	/// pattern (`Path.To(fields)`), distinguished by what follows.
	fn parse_ident_pattern(&mut self) -> Spanned<Pattern> {
		let start = self.position();
		let first = self.expect_ident();

		// A dotted path or a `(` means this is a struct/enum-variant pattern.
		if self.check(&Token::Dot) || self.check(&Token::LParen) {
			let mut path = vec![first];
			while self.eat(&Token::Dot).is_some() {
				path.push(self.expect_ident());
			}
			let fields = if self.check(&Token::LParen) {
				self.advance();
				self.comma_separated(&Token::RParen, |p| p.parse_struct_pattern_field())
			} else {
				Vec::new()
			};
			return Spanned(Pattern::Struct { path, fields }, self.span_from(start));
		}

		if self.eat(&Token::Eq).is_some() {
			let inner = self.parse_range_pattern();
			return Spanned(
				Pattern::Binding {
					name: first,
					inner: Box::new(inner),
				},
				self.span_from(start),
			);
		}

		// A plain binding has an implicit wildcard sub-pattern.
		let span = self.span_from(start);
		Spanned(
			Pattern::Binding {
				name: first,
				inner: Box::new(Spanned(Pattern::Placeholder, span)),
			},
			span,
		)
	}

	fn parse_struct_pattern_field(&mut self) -> Spanned<StructPatternField> {
		let start = self.position();
		if self.check(&Token::DotDotDot) {
			self.advance();
			return Spanned(StructPatternField::Rest, self.span_from(start));
		}
		// A field name is a bare identifier that stands alone (`field`, ending the
		// field) or introduces a sub-pattern (`field = pattern`). An identifier that
		// instead HEADS a larger pattern — a nested variant `Add(..)` or a qualified
		// path `Cmd.Add` — is a positional sub-pattern, not a field name; so is any
		// non-identifier pattern (`_`, a literal, `#(..)`, ...). Positional fields are
		// only valid on a single-field constructor, which the checker enforces.
		if let Some(Token::Identifier(_)) = self.peek() {
			match self.peek_nth(1) {
				Some(Token::Eq) => {
					let name = self.expect_ident();
					self.advance(); // `=`
					let value = self.parse_pattern();
					return Spanned(
						StructPatternField::Value { name, value },
						self.span_from(start),
					);
				}
				Some(Token::Comma | Token::RParen) | None => {
					let name = self.expect_ident();
					return Spanned(StructPatternField::Named(name), self.span_from(start));
				}
				_ => {}
			}
		}
		let pat = self.parse_pattern();
		Spanned(StructPatternField::Positional(pat), self.span_from(start))
	}

	/// Used by `let` bindings and parameters, which take a pattern but not a top-level
	/// union or range (those only make sense in `match`).
	pub(super) fn parse_binding_pattern(&mut self) -> Spanned<Pattern> {
		self.parse_pattern_primary()
	}

	/// Parse the pattern before a `let` initializer. A bare `name = value` uses
	/// that `=` as the initializer delimiter; `name = pattern = value` has a
	/// second top-level `=` and therefore uses the first one as a subpattern.
	pub(super) fn parse_let_binding_pattern(&mut self) -> Spanned<Pattern> {
		if matches!(self.peek(), Some(Token::Identifier(_))) && self.peek_nth(1) == Some(&Token::Eq) {
			let position = self.position();
			let diagnostics = self.diagnostics.len();
			let candidate = self.parse_binding_pattern();
			if self.check(&Token::Eq) {
				return candidate;
			}
			let candidate_end = self.position();
			if self.eat(&Token::Colon).is_some() {
				self.parse_type();
				if self.check(&Token::Eq) {
					self.cursor.restore(candidate_end);
					self.diagnostics.truncate(diagnostics);
					return candidate;
				}
			}
			self.cursor.restore(position);
			self.diagnostics.truncate(diagnostics);
			let name = self.expect_ident();
			let span = name.1;
			return Spanned(
				Pattern::Binding {
					name,
					inner: Box::new(Spanned(Pattern::Placeholder, span)),
				},
				span,
			);
		}
		self.parse_binding_pattern()
	}
}
