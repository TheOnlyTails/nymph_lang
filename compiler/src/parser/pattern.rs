use ordered_float::OrderedFloat;

use crate::ast::Span;

use crate::{
	ast::{
		Spanned,
		expr::{
			ListPatternEntry, MapPatternEntry, Pattern, RangePatternKind, StringPatternPart,
			StructPatternField,
		},
	},
	lexer::token::Token,
};

use super::{
	core::{Parser, span},
	error::ParseError,
};

impl<'src> Parser<'src> {
	pub fn parse_pattern(&mut self) -> Option<Spanned<Pattern>> {
		self.parse_pattern_pratt(0)
	}

	fn parse_pattern_pratt(&mut self, min_bp: u8) -> Option<Spanned<Pattern>> {
		let mut lhs = self.parse_pattern_atom()?;

		while let Some((op_bp, assoc)) = self.pattern_infix_binding_power() {
			if op_bp < min_bp {
				break;
			}

			lhs = self.parse_pattern_infix(lhs, assoc)?;
		}

		Some(lhs)
	}

	fn pattern_infix_binding_power(&self) -> Option<(u8, u8)> {
		match self.peek()? {
			Token::As => Some((3, 4)),
			Token::Pipe => Some((1, 2)),
			_ => None,
		}
	}

	fn parse_pattern_infix(
		&mut self,
		lhs: Spanned<Pattern>,
		next_bp: u8,
	) -> Option<Spanned<Pattern>> {
		match self.peek()? {
			Token::As => {
				self.advance();
				let name = self.expect_identifier()?;
				let span = span(lhs.span().start, name.span().end);
				Some(Spanned(
					Pattern::Binding {
						name,
						inner: lhs.into(),
					},
					span,
				))
			}
			Token::Pipe => {
				self.advance();
				let rhs = self.parse_pattern_pratt(next_bp)?;
				let span = span(lhs.span().start, rhs.span().end);
				Some(Spanned(Pattern::Union(lhs.into(), rhs.into()), span))
			}
			_ => Some(lhs),
		}
	}

	fn parse_pattern_atom(&mut self) -> Option<Spanned<Pattern>> {
		let start_span = self.current_span();

		if let Some(range_pattern) = self.try_parse_range_pattern() {
			return Some(range_pattern);
		}

		let pattern = match self.peek()?.clone() {
			Token::Minus => {
				self.advance();
				return self.parse_signed_literal_pattern(true, start_span);
			}
			Token::DecimalInt(val)
			| Token::HexInt(val)
			| Token::BinaryInt(val)
			| Token::OctalInt(val) => {
				self.advance();
				Pattern::Int(Spanned(val as i64, start_span))
			}
			Token::Float(val) => {
				self.advance();
				Pattern::Float(Spanned(val, start_span))
			}
			Token::IntFloat(val) => {
				self.advance();
				Pattern::Float(Spanned(OrderedFloat(val as f64), start_span))
			}
			Token::IntExpFloat(mantissa, exp) => {
				self.advance();
				Pattern::Float(Spanned(
					OrderedFloat(10f64.powi(exp) * mantissa as f64),
					start_span,
				))
			}
			Token::FloatExpFloat(mantissa, exp) => {
				self.advance();
				Pattern::Float(Spanned(mantissa * 10f64.powi(exp), start_span))
			}
			Token::Char(c) => {
				self.advance();
				Pattern::Char(Spanned(c, start_span))
			}
			Token::CharEscape(esc) => {
				self.advance();
				Pattern::Char(Spanned(esc.into(), start_span))
			}
			Token::String(inner) => {
				self.advance();
				let parts = self.with_nested(&inner, start_span, |p| p.parse_string_pattern_parts());
				Pattern::String(parts)
			}
			Token::True => {
				self.advance();
				Pattern::Boolean(Spanned(true, start_span))
			}
			Token::False => {
				self.advance();
				Pattern::Boolean(Spanned(false, start_span))
			}
			Token::Underscore => {
				self.advance();
				Pattern::Placeholder
			}
			Token::List(inner) => {
				self.advance();
				let entries = self.with_nested(&inner, start_span, |p| p.parse_list_pattern_entries());
				Pattern::List(entries)
			}
			Token::Tuple(inner) => {
				self.advance();
				let entries = self.with_nested(&inner, start_span, |p| p.parse_list_pattern_entries());
				Pattern::Tuple(entries)
			}
			Token::Map(inner) => {
				self.advance();
				let entries = self.with_nested(&inner, start_span, |p| p.parse_map_pattern_entries());
				Pattern::Map(entries)
			}
			Token::Parens(inner) => {
				self.advance();
				let inner_pattern = self.with_nested(&inner, start_span, |p| p.parse_pattern())?;
				Pattern::Grouped(inner_pattern.into())
			}
			Token::Identifier(_) => {
				return self.parse_struct_or_binding_pattern();
			}
			_ => {
				let found = self.peek().cloned().unwrap();
				self.error(ParseError::expected_pattern(found, start_span));
				return None;
			}
		};

		let end_span = self.previous_span();
		Some(Spanned(pattern, span(start_span.start, end_span.end)))
	}

	fn parse_signed_literal_pattern(
		&mut self,
		negative: bool,
		start_span: Span,
	) -> Option<Spanned<Pattern>> {
		let sign = if negative { -1 } else { 1 };

		let pattern = match self.peek()?.clone() {
			Token::DecimalInt(val)
			| Token::HexInt(val)
			| Token::BinaryInt(val)
			| Token::OctalInt(val) => {
				self.advance();
				Pattern::Int(Spanned(val as i64 * sign, self.previous_span()))
			}
			Token::Float(val) => {
				self.advance();
				Pattern::Float(Spanned(val * sign as f64, self.previous_span()))
			}
			Token::IntFloat(val) => {
				self.advance();
				Pattern::Float(Spanned(
					OrderedFloat(val as f64 * sign as f64),
					self.previous_span(),
				))
			}
			Token::IntExpFloat(mantissa, exp) => {
				self.advance();
				Pattern::Float(Spanned(
					OrderedFloat(10f64.powi(exp) * mantissa as f64 * sign as f64),
					self.previous_span(),
				))
			}
			Token::FloatExpFloat(mantissa, exp) => {
				self.advance();
				Pattern::Float(Spanned(
					mantissa * 10f64.powi(exp) * sign as f64,
					self.previous_span(),
				))
			}
			_ => {
				let found = self.peek().cloned().unwrap();
				self.error(ParseError::expected_pattern(found, self.current_span()));
				return None;
			}
		};

		let end_span = self.previous_span();
		Some(Spanned(pattern, span(start_span.start, end_span.end)))
	}

	fn try_parse_range_pattern(&mut self) -> Option<Spanned<Pattern>> {
		let start_span = self.current_span();
		let pos = self.position();

		if self.consume(&Token::DotDotEq).is_some() {
			if let Some(max) = self.parse_range_bound() {
				let end_span = self.previous_span();
				return Some(Spanned(
					Pattern::Range(RangePatternKind::InclusiveMax(max.into())),
					span(start_span.start, end_span.end),
				));
			}
			self.restore(pos);
			return None;
		}

		let min = self.parse_range_bound()?;

		if self.consume(&Token::DotDotEq).is_some() {
			let max = self.parse_range_bound()?;
			let end_span = self.previous_span();
			return Some(Spanned(
				Pattern::Range(RangePatternKind::InclusiveBoth {
					min: min.into(),
					max: max.into(),
				}),
				span(start_span.start, end_span.end),
			));
		}

		if self.consume(&Token::DotDot).is_some() {
			if let Some(max) = self.parse_range_bound() {
				let end_span = self.previous_span();
				return Some(Spanned(
					Pattern::Range(RangePatternKind::ExclusiveBoth {
						min: min.into(),
						max: max.into(),
					}),
					span(start_span.start, end_span.end),
				));
			}
			let end_span = self.previous_span();
			return Some(Spanned(
				Pattern::Range(RangePatternKind::ExclusiveMin(min.into())),
				span(start_span.start, end_span.end),
			));
		}

		self.restore(pos);
		None
	}

	fn parse_range_bound(&mut self) -> Option<Spanned<Pattern>> {
		let start_span = self.current_span();
		let negative = self.consume(&Token::Minus).is_some();
		let sign = if negative { -1 } else { 1 };

		let pattern = match self.peek()?.clone() {
			Token::DecimalInt(val)
			| Token::HexInt(val)
			| Token::BinaryInt(val)
			| Token::OctalInt(val) => {
				self.advance();
				Pattern::Int(Spanned(val as i64 * sign, self.previous_span()))
			}
			Token::Float(val) => {
				self.advance();
				Pattern::Float(Spanned(val * sign as f64, self.previous_span()))
			}
			Token::IntFloat(val) => {
				self.advance();
				Pattern::Float(Spanned(
					OrderedFloat(val as f64 * sign as f64),
					self.previous_span(),
				))
			}
			Token::Char(c) if !negative => {
				self.advance();
				Pattern::Char(Spanned(c, self.previous_span()))
			}
			Token::CharEscape(esc) if !negative => {
				self.advance();
				Pattern::Char(Spanned(esc.into(), self.previous_span()))
			}
			_ => return None,
		};

		let end_span = self.previous_span();
		Some(Spanned(pattern, span(start_span.start, end_span.end)))
	}

	fn parse_struct_or_binding_pattern(&mut self) -> Option<Spanned<Pattern>> {
		let start_span = self.current_span();

		let mut path = vec![self.identifier()?];

		while self.consume(&Token::Dot).is_some() {
			path.push(self.expect_identifier()?);
		}

		let fields = if let Some(Token::Parens(inner)) = self.peek() {
			let inner = inner.clone();
			let parens_span = self.current_span();
			self.advance();
			self.with_nested(&inner, parens_span, |p| p.parse_struct_pattern_fields())
		} else {
			vec![]
		};

		let end_span = self.previous_span();
		Some(Spanned(
			Pattern::Struct { path, fields },
			span(start_span.start, end_span.end),
		))
	}

	fn parse_struct_pattern_fields(&mut self) -> Vec<Spanned<StructPatternField>> {
		let mut fields = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let start_span = self.current_span();

			let field = if self.consume(&Token::DotDotDot).is_some() {
				StructPatternField::Rest
			} else if let Some(Token::Identifier(_)) = self.peek() {
				let name = self.identifier().unwrap();

				if self.consume(&Token::Eq).is_some() {
					let Some(value) = self.parse_pattern() else {
						break;
					};
					StructPatternField::Value { name, value }
				} else {
					StructPatternField::Named(name)
				}
			} else {
				break;
			};

			let end_span = self.previous_span();
			fields.push(Spanned(field, span(start_span.start, end_span.end)));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		fields
	}

	fn parse_list_pattern_entries(&mut self) -> Vec<Spanned<ListPatternEntry>> {
		let mut entries = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let start_span = self.current_span();

			let entry = if self.consume(&Token::DotDotDot).is_some() {
				let name = self.identifier();
				ListPatternEntry::Rest(name)
			} else {
				let Some(pattern) = self.parse_pattern() else {
					break;
				};
				ListPatternEntry::Item(pattern)
			};

			let end_span = self.previous_span();
			entries.push(Spanned(entry, span(start_span.start, end_span.end)));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		entries
	}

	fn parse_map_pattern_entries(&mut self) -> Vec<Spanned<MapPatternEntry>> {
		let mut entries = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let start_span = self.current_span();

			let entry = if self.consume(&Token::DotDotDot).is_some() {
				let name = self.identifier();
				MapPatternEntry::Rest(name)
			} else {
				let Some(key) = self.parse_pattern() else {
					break;
				};
				if self.expect(&Token::Colon, ":").is_none() {
					break;
				}
				let Some(value) = self.parse_pattern() else {
					break;
				};
				MapPatternEntry::Entry(key, value)
			};

			let end_span = self.previous_span();
			entries.push(Spanned(entry, span(start_span.start, end_span.end)));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		entries
	}

	fn parse_string_pattern_parts(&mut self) -> Vec<Spanned<StringPatternPart>> {
		let mut parts = Vec::new();

		while !self.at_end() {
			let start_span = self.current_span();

			let part = match self.peek().cloned() {
				Some(Token::StringText(text)) => {
					self.advance();
					StringPatternPart::Text(text)
				}
				Some(Token::StringEscape(esc)) => {
					self.advance();
					StringPatternPart::EscapeSequence(esc)
				}
				_ => break,
			};

			let end_span = self.previous_span();
			parts.push(Spanned(part, span(start_span.start, end_span.end)));
		}

		parts
	}
}
