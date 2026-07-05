//! Parsing of patterns, shared by `match`, `let`, function parameters, and the
//! `is` / `!is` operators.

use nymph_ast::{
	Spanned,
	expr::{
		ListPatternEntry, MapPatternEntry, Pattern, RangePatternKind, StringPatternPart,
		StructPatternField,
	},
	token::{StrFragment, Token},
};
use nymph_diagnostics::Diagnostic;
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
			self.advance();
			let max = self.parse_pattern_primary();
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
			self.error(Diagnostic::error(
				"expected a pattern, found end of input",
				span,
			));
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
				self.error(Diagnostic::error(
					format!("expected a pattern, found {}", other.describe()),
					span,
				));
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
				self.error(Diagnostic::error(
					"expected a number after `-` in a pattern",
					span,
				));
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
				StrFragment::Interpolation(_) => self.error(Diagnostic::error(
					"string interpolation is not allowed in patterns",
					fragment.1,
				)),
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

		// A plain binding. Its inner pattern is a wildcard. (The `name = pattern` form
		// is not a top-level pattern; it only exists as a struct field, handled by
		// `parse_struct_pattern_field`.)
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
		let name = self.expect_ident();
		if self.eat(&Token::Eq).is_some() {
			let value = self.parse_pattern();
			Spanned(
				StructPatternField::Value { name, value },
				self.span_from(start),
			)
		} else {
			Spanned(StructPatternField::Named(name), self.span_from(start))
		}
	}

	/// Used by `let` bindings and parameters, which take a pattern but not a top-level
	/// union or range (those only make sense in `match`).
	pub(super) fn parse_binding_pattern(&mut self) -> Spanned<Pattern> {
		self.parse_pattern_primary()
	}
}
