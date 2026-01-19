use ordered_float::OrderedFloat;

use crate::{
	ast::{
		Ident, Spanned,
		declaration::LetDeclaration,
		expr::{
			CallArg, ClosureParam, Expr, ListItem, MapEntry, MatchArm, RangeKind, Statement, StringPart,
		},
		ops::{
			AssignOperator, BinaryOperator, PatternOperator, PostfixOperator, PrefixOperator,
			TypeOperator,
		},
	},
	lexer::token::Token, parser::error::ParseErrorKind,
};

use super::{
	core::{Parser, span},
	error::ParseError,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Assoc {
	Left,
	Right,
}

impl<'src> Parser<'src> {
	pub fn parse_expression(&mut self) -> Option<Spanned<Expr>> {
		self.parse_expr_pratt(0)
	}

	fn parse_expr_pratt(&mut self, min_bp: u8) -> Option<Spanned<Expr>> {
		let mut lhs = self.parse_expr_prefix()?;

		loop {
			if let Some((op_bp,)) = self.postfix_binding_power()
				&& op_bp >= min_bp
			{
				lhs = self.parse_postfix(lhs)?;
				continue;
			}

			let Some((l_bp, r_bp)) = self.infix_binding_power() else {
				break;
			};

			if l_bp < min_bp {
				break;
			}

			lhs = self.parse_infix(lhs, r_bp)?;
		}

		Some(lhs)
	}

	fn prefix_binding_power(&self) -> Option<((), u8)> {
		match self.peek()? {
			Token::Minus | Token::ExclamationMark | Token::Tilde => Some(((), 23)),
			Token::DotDot | Token::DotDotEq => Some(((), 15)),
			_ => None,
		}
	}

	fn postfix_binding_power(&self) -> Option<(u8,)> {
		match self.peek()? {
			Token::Lt => Some((27,)),
			Token::Parens(_) => Some((27,)),
			Token::Dot | Token::QuestionDot => Some((25,)),
			Token::Brackets(_) => Some((25,)),
			Token::QuestionMark => Some((23,)),
			Token::Is | Token::NotIs => Some((21,)),
			Token::As => Some((21,)),
			Token::DotDot => Some((15,)),
			_ => None,
		}
	}

	fn infix_binding_power(&self) -> Option<(u8, u8)> {
		match self.peek()? {
			Token::StarStar => Some((18, 17)),
			Token::Star | Token::Slash | Token::Percent => Some((17, 18)),
			Token::Plus | Token::Minus => Some((15, 16)),
			Token::DotDotEq => Some((14, 14)),
			Token::Lt if self.is_left_shift() => Some((13, 14)),
			Token::Gt if self.is_right_shift() => Some((13, 14)),
			Token::And => Some((11, 12)),
			Token::Caret => Some((10, 11)),
			Token::Pipe => Some((9, 10)),
			Token::DoubleQuestion => Some((8, 9)),
			Token::In | Token::NotIn => Some((7, 8)),
			Token::Lt | Token::LtEq | Token::Gt | Token::GtEq => Some((6, 7)),
			Token::EqEq | Token::NotEq => Some((5, 6)),
			Token::AndAnd => Some((4, 5)),
			Token::PipePipe => Some((3, 4)),
			Token::Triangle => Some((2, 1)),
			Token::Eq
			| Token::PlusEq
			| Token::MinusEq
			| Token::StarEq
			| Token::SlashEq
			| Token::PercentEq
			| Token::StarStarEq
			| Token::LtLtEq
			| Token::GtGtEq
			| Token::AndEq
			| Token::CaretEq
			| Token::PipeEq
			| Token::TildeEq
			| Token::AndAndEq
			| Token::PipePipeEq => Some((1, 2)),
			_ => None,
		}
	}

	fn is_left_shift(&self) -> bool {
		if let (Some(Spanned(Token::Lt, s1)), Some(Spanned(Token::Lt, s2))) =
			(self.cursor.peek(), self.cursor.peek_nth(1))
		{
			s2.start == s1.end
		} else {
			false
		}
	}

	fn is_right_shift(&self) -> bool {
		if let (Some(Spanned(Token::Gt, s1)), Some(Spanned(Token::Gt, s2))) =
			(self.cursor.peek(), self.cursor.peek_nth(1))
		{
			s2.start == s1.end
		} else {
			false
		}
	}

	fn parse_expr_prefix(&mut self) -> Option<Spanned<Expr>> {
		let start_span = self.current_span();

		if let Some(((), r_bp)) = self.prefix_binding_power() {
			match self.peek()? {
				Token::Minus => {
					self.advance();
					let value = self.parse_expr_pratt(r_bp)?;
					let span = span(start_span.start, value.span().end);
					return Some(Spanned(
						Expr::PrefixOp {
							op: PrefixOperator::Negate,
							value: value.into(),
						},
						span,
					));
				}
				Token::ExclamationMark => {
					self.advance();
					let value = self.parse_expr_pratt(r_bp)?;
					let span = span(start_span.start, value.span().end);
					return Some(Spanned(
						Expr::PrefixOp {
							op: PrefixOperator::BoolNot,
							value: value.into(),
						},
						span,
					));
				}
				Token::Tilde => {
					self.advance();
					let value = self.parse_expr_pratt(r_bp)?;
					let span = span(start_span.start, value.span().end);
					return Some(Spanned(
						Expr::PrefixOp {
							op: PrefixOperator::BitNot,
							value: value.into(),
						},
						span,
					));
				}
				Token::DotDot => {
					self.advance();
					let max = self.parse_expr_pratt(r_bp)?;
					let span = span(start_span.start, max.span().end);
					return Some(Spanned(Expr::Range(RangeKind::To(max.into())), span));
				}
				Token::DotDotEq => {
					self.advance();
					let max = self.parse_expr_pratt(r_bp)?;
					let span = span(start_span.start, max.span().end);
					return Some(Spanned(
						Expr::Range(RangeKind::ToInclusive(max.into())),
						span,
					));
				}
				_ => {}
			}
		}

		self.parse_expr_atom()
	}

	fn parse_postfix(&mut self, lhs: Spanned<Expr>) -> Option<Spanned<Expr>> {
		let start = lhs.span().start;

		match self.peek()?.clone() {
			Token::Lt => {
				// Parse generics, then continue to check for function call
				let generics = self.parse_generic_args()?;
				if let Some(Token::Parens(inner)) = self.peek().cloned() {
					let parens_span = self.current_span();
					self.advance();
					let args = self.with_nested(&inner, parens_span, |p| p.parse_call_args());
					let span = span(start, self.previous_span().end);
					Some(Spanned(
						Expr::Call {
							func: lhs.into(),
							generics,
							args,
						},
						span,
					))
				} else {
					// Generics without a function call - just return lhs
					// (This handles cases like `Vec<int>`)
					Some(lhs)
				}
			}
			Token::Parens(inner) => {
				let generics = self.parse_generic_args().unwrap_or_default();
				let parens_span = self.current_span();
				self.advance();
				let args = self.with_nested(&inner, parens_span, |p| p.parse_call_args());
				let span = span(start, self.previous_span().end);
				Some(Spanned(
					Expr::Call {
						func: lhs.into(),
						generics,
						args,
					},
					span,
				))
			}
			Token::Dot => {
				self.advance();
				let member = self.expect_identifier()?;
				let span = span(start, member.span().end);
				Some(Spanned(
					Expr::MemberAccess {
						parent: lhs.into(),
						member,
						optional: false,
					},
					span,
				))
			}
			Token::QuestionDot => {
				self.advance();
				if let Some(Token::Brackets(inner)) = self.peek() {
					let inner = inner.clone();
					let brackets_span = self.current_span();
					self.advance();
					let index = self.with_nested(&inner, brackets_span, |p| p.parse_expression())?;
					let span = span(start, self.previous_span().end);
					Some(Spanned(
						Expr::IndexAccess {
							parent: lhs.into(),
							index: index.into(),
							optional: true,
						},
						span,
					))
				} else {
					let member = self.expect_identifier()?;
					let span = span(start, member.span().end);
					Some(Spanned(
						Expr::MemberAccess {
							parent: lhs.into(),
							member,
							optional: true,
						},
						span,
					))
				}
			}
			Token::Brackets(inner) => {
				let brackets_span = self.current_span();
				self.advance();
				let index = self.with_nested(&inner, brackets_span, |p| p.parse_expression())?;
				let span = span(start, self.previous_span().end);
				Some(Spanned(
					Expr::IndexAccess {
						parent: lhs.into(),
						index: index.into(),
						optional: false,
					},
					span,
				))
			}
			Token::QuestionMark => {
				self.advance();
				let span = span(start, self.previous_span().end);
				Some(Spanned(
					Expr::PostfixOp {
						op: PostfixOperator::ErrorReturn,
						value: lhs.into(),
					},
					span,
				))
			}
			Token::Is => {
				self.advance();
				let pattern = self.parse_pattern()?;
				let span = span(start, pattern.span().end);
				Some(Spanned(
					Expr::PatternOp {
						lhs: lhs.into(),
						op: PatternOperator::Is,
						rhs: pattern,
					},
					span,
				))
			}
			Token::NotIs => {
				self.advance();
				let pattern = self.parse_pattern()?;
				let span = span(start, pattern.span().end);
				Some(Spanned(
					Expr::PatternOp {
						lhs: lhs.into(),
						op: PatternOperator::NotIs,
						rhs: pattern,
					},
					span,
				))
			}
			Token::As => {
				self.advance();
				let ty = self.parse_type()?;
				let span = span(start, ty.span().end);
				Some(Spanned(
					Expr::TypeOp {
						lhs: lhs.into(),
						op: TypeOperator::As,
						rhs: ty,
					},
					span,
				))
			}
			Token::DotDot => {
				self.advance();
				if let Some(max) = self.parse_expr_pratt(15) {
					let span = span(start, max.span().end);
					Some(Spanned(
						Expr::Range(RangeKind::Exclusive {
							min: lhs.into(),
							max: max.into(),
						}),
						span,
					))
				} else {
					let span = span(start, self.previous_span().end);
					Some(Spanned(Expr::Range(RangeKind::From(lhs.into())), span))
				}
			}
			_ => Some(lhs),
		}
	}

	fn parse_infix(&mut self, lhs: Spanned<Expr>, r_bp: u8) -> Option<Spanned<Expr>> {
		let start = lhs.span().start;

		let (op, is_assign) = match self.peek()?.clone() {
			Token::StarStar => {
				self.advance();
				(BinaryOperator::Power, false)
			}
			Token::Star => {
				self.advance();
				(BinaryOperator::Times, false)
			}
			Token::Slash => {
				self.advance();
				(BinaryOperator::Divide, false)
			}
			Token::Percent => {
				self.advance();
				(BinaryOperator::Remainder, false)
			}
			Token::Plus => {
				self.advance();
				(BinaryOperator::Plus, false)
			}
			Token::Minus => {
				self.advance();
				(BinaryOperator::Minus, false)
			}
			Token::DotDotEq => {
				self.advance();
				let rhs = self.parse_expr_pratt(r_bp)?;
				let span = span(start, rhs.span().end);
				return Some(Spanned(
					Expr::Range(RangeKind::Inclusive {
						min: lhs.into(),
						max: rhs.into(),
					}),
					span,
				));
			}
			Token::Lt if self.is_left_shift() => {
				self.advance();
				self.advance();
				(BinaryOperator::LeftShift, false)
			}
			Token::Gt if self.is_right_shift() => {
				self.advance();
				self.advance();
				(BinaryOperator::RightShift, false)
			}
			Token::And => {
				self.advance();
				(BinaryOperator::BitAnd, false)
			}
			Token::Caret => {
				self.advance();
				(BinaryOperator::BitXor, false)
			}
			Token::Pipe => {
				self.advance();
				(BinaryOperator::BitOr, false)
			}
			Token::DoubleQuestion => {
				self.advance();
				(BinaryOperator::Unwrap, false)
			}
			Token::In => {
				self.advance();
				(BinaryOperator::In, false)
			}
			Token::NotIn => {
				self.advance();
				(BinaryOperator::NotIn, false)
			}
			Token::Lt => {
				self.advance();
				(BinaryOperator::LessThan, false)
			}
			Token::LtEq => {
				self.advance();
				(BinaryOperator::LessThanEquals, false)
			}
			Token::Gt => {
				self.advance();
				(BinaryOperator::GreaterThan, false)
			}
			Token::GtEq => {
				self.advance();
				(BinaryOperator::GreaterThanEquals, false)
			}
			Token::EqEq => {
				self.advance();
				(BinaryOperator::Equals, false)
			}
			Token::NotEq => {
				self.advance();
				(BinaryOperator::NotEquals, false)
			}
			Token::AndAnd => {
				self.advance();
				(BinaryOperator::BoolAnd, false)
			}
			Token::PipePipe => {
				self.advance();
				(BinaryOperator::BoolOr, false)
			}
			Token::Triangle => {
				self.advance();
				(BinaryOperator::Pipe, false)
			}
			Token::Eq => {
				self.advance();
				let rhs = self.parse_expr_pratt(r_bp)?;
				let span = span(start, rhs.span().end);
				return Some(Spanned(
					Expr::AssignOp {
						lhs: lhs.into(),
						op: AssignOperator::Assign,
						rhs: rhs.into(),
					},
					span,
				));
			}
			Token::PlusEq => return self.parse_compound_assign(lhs, AssignOperator::PlusAssign, r_bp),
			Token::MinusEq => return self.parse_compound_assign(lhs, AssignOperator::MinusAssign, r_bp),
			Token::StarEq => return self.parse_compound_assign(lhs, AssignOperator::TimesAssign, r_bp),
			Token::SlashEq => return self.parse_compound_assign(lhs, AssignOperator::DivideAssign, r_bp),
			Token::PercentEq => {
				return self.parse_compound_assign(lhs, AssignOperator::RemainderAssign, r_bp);
			}
			Token::StarStarEq => {
				return self.parse_compound_assign(lhs, AssignOperator::PowerAssign, r_bp);
			}
			Token::LtLtEq => {
				return self.parse_compound_assign(lhs, AssignOperator::LeftShiftAssign, r_bp);
			}
			Token::GtGtEq => {
				return self.parse_compound_assign(lhs, AssignOperator::RightShiftAssign, r_bp);
			}
			Token::AndEq => return self.parse_compound_assign(lhs, AssignOperator::BitAndAssign, r_bp),
			Token::CaretEq => return self.parse_compound_assign(lhs, AssignOperator::BitXorAssign, r_bp),
			Token::PipeEq => return self.parse_compound_assign(lhs, AssignOperator::BitOrAssign, r_bp),
			Token::TildeEq => return self.parse_compound_assign(lhs, AssignOperator::BitNotAssign, r_bp),
			Token::AndAndEq => {
				return self.parse_compound_assign(lhs, AssignOperator::BoolAndAssign, r_bp);
			}
			Token::PipePipeEq => {
				return self.parse_compound_assign(lhs, AssignOperator::BoolOrAssign, r_bp);
			}
			_ => return Some(lhs),
		};

		let rhs = self.parse_expr_pratt(r_bp)?;
		let span = span(start, rhs.span().end);
		Some(Spanned(
			Expr::BinaryOp {
				lhs: lhs.into(),
				op,
				rhs: rhs.into(),
			},
			span,
		))
	}

	fn parse_compound_assign(
		&mut self,
		lhs: Spanned<Expr>,
		op: AssignOperator,
		r_bp: u8,
	) -> Option<Spanned<Expr>> {
		let start = lhs.span().start;
		self.advance();
		let rhs = self.parse_expr_pratt(r_bp)?;
		let span = span(start, rhs.span().end);
		Some(Spanned(
			Expr::AssignOp {
				lhs: lhs.into(),
				op,
				rhs: rhs.into(),
			},
			span,
		))
	}

	fn parse_expr_atom(&mut self) -> Option<Spanned<Expr>> {
		let start_span = self.current_span();

		let expr = match self.peek()?.clone() {
			Token::Braces(inner) => {
				return self.parse_block_expr(&inner, None);
			}
			Token::DecimalInt(val)
			| Token::HexInt(val)
			| Token::BinaryInt(val)
			| Token::OctalInt(val) => {
				self.advance();
				Expr::Int(Spanned(val, start_span))
			}
			Token::Float(val) => {
				self.advance();
				Expr::Float(Spanned(val, start_span))
			}
			Token::IntFloat(val) => {
				self.advance();
				Expr::Float(Spanned(OrderedFloat(val as f64), start_span))
			}
			Token::IntExpFloat(mantissa, exp) => {
				self.advance();
				Expr::Float(Spanned(
					OrderedFloat(10f64.powi(exp) * mantissa as f64),
					start_span,
				))
			}
			Token::FloatExpFloat(mantissa, exp) => {
				self.advance();
				Expr::Float(Spanned(mantissa * 10f64.powi(exp), start_span))
			}
			Token::Char(c) => {
				self.advance();
				Expr::Char(Spanned(c, start_span))
			}
			Token::CharEscape(esc) => {
				self.advance();
				Expr::Char(Spanned(esc.into(), start_span))
			}
			Token::String(inner) => {
				self.advance();
				let parts = self.with_nested(&inner, start_span, |p| p.parse_string_parts());
				Expr::String(parts)
			}
			Token::True => {
				self.advance();
				Expr::Boolean(Spanned(true, start_span))
			}
			Token::False => {
				self.advance();
				Expr::Boolean(Spanned(false, start_span))
			}
			Token::Identifier(_) => {
				let ident = self.identifier()?;
				if self.consume(&Token::AtSign).is_some() {
					match self.advance() {
						Some(Spanned(Token::Braces(inner), _)) => {
							let inner = inner.clone();
							self.parse_block_expr(inner.as_slice(), Some(ident))
						}
						Some(found) => {
							let found = found.clone();
							self.error({
								let Spanned(found, span) = found;
								let expected = vec!["a block".into()];
								ParseError {
									kind: ParseErrorKind::UnexpectedToken { found, expected },
									span,
									context: vec![],
								}
							});
							None
						}
						None => {
							self.error(ParseError::unexpected_eof(
								vec!["a block".into()],
								start_span,
							));
							None
						}
					}?
					.0
				} else {
					Expr::Identifier(ident)
				}
			}
			Token::List(inner) => {
				self.advance();
				let items = self.with_nested(&inner, start_span, |p| p.parse_list_items());
				Expr::List(items)
			}
			Token::Tuple(inner) => {
				self.advance();
				let items = self.with_nested(&inner, start_span, |p| p.parse_list_items());
				Expr::Tuple(items)
			}
			Token::Map(inner) => {
				self.advance();
				let entries = self.with_nested(&inner, start_span, |p| p.parse_map_entries());
				Expr::Map(entries)
			}
			Token::Underscore => {
				self.advance();
				Expr::Placeholder
			}
			Token::This => {
				self.advance();
				Expr::This
			}
			Token::Parens(inner) => {
				return self.try_parse_closure_or_grouped();
			}
			Token::Lt => {
				return self.parse_generic_closure();
			}
			Token::If => {
				return self.parse_if_expr();
			}
			Token::While => {
				return self.parse_while_expr();
			}
			Token::For => {
				return self.parse_for_expr();
			}
			Token::Match => {
				return self.parse_match_expr();
			}
			Token::Return => {
				self.advance();
				let label = if self.consume(&Token::AtSign).is_some() {
					Some(self.expect_identifier()?)
				} else {
					None
				};
				let value = self.parse_expression().map(Box::new);
				let end = value
					.as_ref()
					.map(|v| v.span().end)
					.unwrap_or(start_span.end);
				return Some(Spanned(
					Expr::Return { value, label },
					span(start_span.start, end),
				));
			}
			Token::Break => {
				self.advance();
				let label = if self.consume(&Token::AtSign).is_some() {
					Some(self.expect_identifier()?)
				} else {
					None
				};
				let value = self.parse_expression().map(Box::new);
				let end = value
					.as_ref()
					.map(|v| v.span().end)
					.unwrap_or(start_span.end);
				return Some(Spanned(
					Expr::Break { value, label },
					span(start_span.start, end),
				));
			}
			Token::Continue => {
				self.advance();
				let label = if self.consume(&Token::AtSign).is_some() {
					Some(self.expect_identifier()?)
				} else {
					None
				};
				return Some(Spanned(
					Expr::Continue { label },
					self.span_to_current(start_span),
				));
			}
			_ => {
				let found = self.peek().cloned().unwrap();
				self.error(ParseError::expected_expression(found, start_span));
				return None;
			}
		};

		let end_span = self.previous_span();
		Some(Spanned(expr, span(start_span.start, end_span.end)))
	}

	fn parse_block_expr(
		&mut self,
		contents: &[Spanned<Token>],
		label: Option<Ident>,
	) -> Option<Spanned<Expr>> {
		let start_span = label
			.as_ref()
			.map(|l| l.span())
			.unwrap_or_else(|| self.current_span());

		let braces_span = self.current_span();
		self.advance();

		let body = self.with_nested(contents, braces_span, |p| p.parse_statements());

		let end_span = self.previous_span();
		Some(Spanned(
			Expr::Block { body, label },
			span(start_span.start, end_span.end),
		))
	}

	fn parse_statements(&mut self) -> Vec<Spanned<Statement>> {
		let mut statements = Vec::new();

		while !self.at_end() {
			let start_span = self.current_span();

			if self.check(&Token::Let)
				&& let Some(stmt) = self.parse_let_statement()
			{
				statements.push(stmt);
				continue;
			}

			if let Some(expr) = self.parse_expression() {
				let end_span = expr.span();
				statements.push(Spanned(
					Statement::Expr(expr),
					span(start_span.start, end_span.end),
				));
			} else {
				self.advance();
			}
		}

		statements
	}

	fn parse_let_statement(&mut self) -> Option<Spanned<Statement>> {
		let start_span = self.current_span();
		self.expect(&Token::Let, "let")?;

		let mutable = self.consume(&Token::Mut).is_some();
		let name = self.parse_pattern()?;
		let type_ = if self.consume(&Token::Colon).is_some() {
			Some(self.parse_type()?)
		} else {
			None
		};

		self.expect(&Token::Eq, "=")?;
		let value = self.parse_expression()?;

		let end_span = value.span();
		Some(Spanned(
			Statement::Let {
				meta: LetDeclaration {
					mutable,
					name,
					type_,
				},
				value,
			},
			span(start_span.start, end_span.end),
		))
	}

	fn try_parse_closure_or_grouped(&mut self) -> Option<Spanned<Expr>> {
		let Token::Parens(inner) = self.peek()?.clone() else {
			return None;
		};
		let start_span = self.current_span();
		let pos = self.position();

		self.advance();

		if self.check(&Token::Arrow) {
			self.restore(pos);
			return self.parse_closure();
		}

		if self.check(&Token::Colon) {
			self.restore(pos);
			return self.parse_closure();
		}

		self.restore(pos);

		if self.looks_like_closure(&inner) {
			return self.parse_closure();
		}

		self.advance();
		let expr = self.with_nested(&inner, start_span, |p| p.parse_expression())?;
		let end_span = self.previous_span();
		Some(Spanned(
			Expr::Grouped(expr.into()),
			span(start_span.start, end_span.end),
		))
	}

	fn looks_like_closure(&self, inner: &[Spanned<Token>]) -> bool {
		if inner.is_empty()
			&& let Some(Token::Arrow) = self.peek_nth(1)
		{
			return true;
		}

		let mut depth = 0;
		for Spanned(tok, _) in inner {
			match tok {
				Token::Parens(_) | Token::Brackets(_) | Token::Braces(_) => depth += 1,
				Token::Comma if depth == 0 => return true,
				Token::Colon if depth == 0 => return true,
				Token::DotDotDot if depth == 0 => return true,
				Token::Mut if depth == 0 => return true,
				_ => {}
			}
		}

		false
	}

	fn parse_closure(&mut self) -> Option<Spanned<Expr>> {
		let start_span = self.current_span();

		let generics = self.parse_generic_params().unwrap_or_default();

		let Token::Parens(inner) = self.peek()?.clone() else {
			return None;
		};
		let parens_span = self.current_span();
		self.advance();

		let params = self.with_nested(&inner, parens_span, |p| p.parse_closure_params());

		let return_type = if self.consume(&Token::Colon).is_some() {
			Some(self.parse_type()?)
		} else {
			None
		};

		self.expect(&Token::Arrow, "->")?;
		let body = self.parse_expression()?;

		let end_span = body.span();
		Some(Spanned(
			Expr::Closure {
				generics,
				params,
				return_type,
				body: body.into(),
			},
			span(start_span.start, end_span.end),
		))
	}

	fn parse_generic_closure(&mut self) -> Option<Spanned<Expr>> {
		let start_span = self.current_span();
		let pos = self.position();

		let Some(generics) = self.parse_generic_params() else {
			self.restore(pos);
			return None;
		};

		if !matches!(self.peek(), Some(Token::Parens(_))) {
			self.restore(pos);
			return None;
		}

		let Token::Parens(inner) = self.peek()?.clone() else {
			return None;
		};
		let parens_span = self.current_span();
		self.advance();

		let params = self.with_nested(&inner, parens_span, |p| p.parse_closure_params());

		let return_type = if self.consume(&Token::Colon).is_some() {
			Some(self.parse_type()?)
		} else {
			None
		};

		if self.expect(&Token::Arrow, "->").is_none() {
			self.restore(pos);
			return None;
		}

		let body = self.parse_expression()?;

		let end_span = body.span();
		Some(Spanned(
			Expr::Closure {
				generics,
				params,
				return_type,
				body: body.into(),
			},
			span(start_span.start, end_span.end),
		))
	}

	fn parse_closure_params(&mut self) -> Vec<Spanned<ClosureParam>> {
		let mut params = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let start_span = self.current_span();

			let spread = self.consume(&Token::DotDotDot).is_some();
			let mutable = self.consume(&Token::Mut).is_some();

			let Some(name) = self.parse_pattern() else {
				break;
			};

			let type_ = if self.consume(&Token::Colon).is_some() {
				self.parse_type()
			} else {
				None
			};

			let end_span = self.previous_span();
			params.push(Spanned(
				ClosureParam {
					spread,
					mutable,
					name,
					type_,
				},
				span(start_span.start, end_span.end),
			));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		params
	}

	fn parse_if_expr(&mut self) -> Option<Spanned<Expr>> {
		let start_span = self.current_span();
		self.expect(&Token::If, "if")?;

		let Token::Parens(cond_inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected condition in parentheses",
				self.current_span(),
			));
			return None;
		};
		let parens_span = self.current_span();
		self.advance();

		let condition = self.with_nested(&cond_inner, parens_span, |p| p.parse_expression())?;
		let then = self.parse_expression()?;

		let otherwise = if self.consume(&Token::Else).is_some() {
			Some(self.parse_expression()?.into())
		} else {
			None
		};

		let end_span = otherwise
			.as_ref()
			.map(|e: &Box<_>| e.span())
			.unwrap_or(then.span());

		Some(Spanned(
			Expr::If {
				condition: condition.into(),
				then: then.into(),
				otherwise,
			},
			span(start_span.start, end_span.end),
		))
	}

	fn parse_while_expr(&mut self) -> Option<Spanned<Expr>> {
		let start_span = self.current_span();
		self.expect(&Token::While, "while")?;

		let label = if self.consume(&Token::AtSign).is_some() {
			Some(self.expect_identifier()?)
		} else {
			None
		};

		let Token::Parens(cond_inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected condition in parentheses",
				self.current_span(),
			));
			return None;
		};
		let parens_span = self.current_span();
		self.advance();

		let condition = self.with_nested(&cond_inner, parens_span, |p| p.parse_expression())?;
		let body = self.parse_expression()?;

		let end_span = body.span();
		Some(Spanned(
			Expr::While {
				label,
				condition: condition.into(),
				body: body.into(),
			},
			span(start_span.start, end_span.end),
		))
	}

	fn parse_for_expr(&mut self) -> Option<Spanned<Expr>> {
		let start_span = self.current_span();
		self.expect(&Token::For, "for")?;

		let label = if self.consume(&Token::AtSign).is_some() {
			Some(self.expect_identifier()?)
		} else {
			None
		};

		let Token::Parens(inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected (pattern in iterable)",
				self.current_span(),
			));
			return None;
		};
		let parens_span = self.current_span();
		self.advance();

		let (variable, iterable) = self.with_nested(&inner, parens_span, |p| {
			let variable = p.parse_pattern()?;
			p.expect(&Token::In, "in")?;
			let iterable = p.parse_expression()?;
			Some((variable, iterable))
		})?;

		let body = self.parse_expression()?;

		let end_span = body.span();
		Some(Spanned(
			Expr::For {
				label,
				variable,
				iterable: iterable.into(),
				body: body.into(),
			},
			span(start_span.start, end_span.end),
		))
	}

	fn parse_match_expr(&mut self) -> Option<Spanned<Expr>> {
		let start_span = self.current_span();
		self.expect(&Token::Match, "match")?;

		let Token::Parens(value_inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected value in parentheses",
				self.current_span(),
			));
			return None;
		};
		let parens_span = self.current_span();
		self.advance();

		let value = self.with_nested(&value_inner, parens_span, |p| p.parse_expression())?;

		let Token::Braces(arms_inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected match arms in braces",
				self.current_span(),
			));
			return None;
		};
		let braces_span = self.current_span();
		self.advance();

		let arms = self.with_nested(&arms_inner, braces_span, |p| p.parse_match_arms());

		let end_span = self.previous_span();
		Some(Spanned(
			Expr::Match {
				value: value.into(),
				arms,
			},
			span(start_span.start, end_span.end),
		))
	}

	fn parse_match_arms(&mut self) -> Vec<MatchArm> {
		let mut arms = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let Some(pattern) = self.parse_pattern() else {
				break;
			};

			let guard = if self.consume(&Token::If).is_some() {
				self.parse_expression()
			} else {
				None
			};

			if self.expect(&Token::Arrow, "->").is_none() {
				break;
			}

			let Some(body) = self.parse_expression() else {
				break;
			};

			arms.push(MatchArm {
				pattern,
				guard,
				body,
			});

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		arms
	}

	fn parse_call_args(&mut self) -> Vec<Spanned<CallArg>> {
		let mut args = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let start_span = self.current_span();

			let (name, spread) = if let Some(Token::Identifier(_)) = self.peek() {
				if self.peek_nth(1) == Some(&Token::Eq) {
					let name = self.identifier();
					self.consume(&Token::Eq);
					let spread = self.consume(&Token::DotDotDot).is_some();
					(name, spread)
				} else {
					let spread = self.consume(&Token::DotDotDot).is_some();
					(None, spread)
				}
			} else {
				let spread = self.consume(&Token::DotDotDot).is_some();
				(None, spread)
			};

			let Some(value) = self.parse_expression() else {
				break;
			};

			let end_span = value.span();
			args.push(Spanned(
				CallArg {
					name,
					spread,
					value,
				},
				span(start_span.start, end_span.end),
			));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		args
	}

	fn parse_list_items(&mut self) -> Vec<Spanned<ListItem>> {
		let mut items = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let start_span = self.current_span();
			let spread = self.consume(&Token::DotDotDot).is_some();

			let Some(value) = self.parse_expression() else {
				break;
			};

			let item = if spread {
				ListItem::Spread(value)
			} else {
				ListItem::Expr(value)
			};

			let end_span = self.previous_span();
			items.push(Spanned(item, span(start_span.start, end_span.end)));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		items
	}

	fn parse_map_entries(&mut self) -> Vec<Spanned<MapEntry>> {
		let mut entries = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let start_span = self.current_span();

			if self.consume(&Token::DotDotDot).is_some() {
				let Some(value) = self.parse_expression() else {
					break;
				};
				let end_span = value.span();
				entries.push(Spanned(
					MapEntry::Spread(value),
					span(start_span.start, end_span.end),
				));
			} else {
				let Some(key) = self.parse_expression() else {
					break;
				};
				if self.expect(&Token::Colon, ":").is_none() {
					break;
				}
				let Some(value) = self.parse_expression() else {
					break;
				};
				let end_span = value.span();
				entries.push(Spanned(
					MapEntry::Expr(key, value),
					span(start_span.start, end_span.end),
				));
			}

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		entries
	}

	fn parse_string_parts(&mut self) -> Vec<Spanned<StringPart>> {
		let mut parts = Vec::new();

		while !self.at_end() {
			let start_span = self.current_span();

			let part = match self.peek().cloned() {
				Some(Token::StringText(text)) => {
					self.advance();
					StringPart::Text(text)
				}
				Some(Token::StringEscape(esc)) => {
					self.advance();
					StringPart::EscapeSequence(esc)
				}
				Some(Token::StringInterpolation(inner)) => {
					self.advance();
					let expr = self.with_nested(&inner, start_span, |p| p.parse_expression());
					if let Some(expr) = expr {
						StringPart::InterpolatedExpr(expr)
					} else {
						continue;
					}
				}
				_ => break,
			};

			let end_span = self.previous_span();
			parts.push(Spanned(part, span(start_span.start, end_span.end)));
		}

		parts
	}
}
