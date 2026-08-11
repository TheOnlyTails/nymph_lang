//! Pratt (precedence-climbing) parsing of expressions.

use crate::errors::ParseError;
use nymph_ast::{
	Ident, Span, Spanned,
	expr::{
		CallArg, ClosureParam, Expr, ExprKind, ListItem, MapEntry, MatchArm, RangeKind, Statement,
		StringPart,
	},
	ops::{AssignOperator, BinaryOperator, Precedence, PrefixOperator},
	token::{StrFragment, Token},
};

use super::Parser;

/// What an infix position can hold, along with how many tokens to consume.
enum Infix {
	Binary(BinaryOperator),
	Assign(AssignOperator),
	As,
	Is,
	NotIs,
	RangeExclusive,
	RangeInclusive,
}

/// Binding powers for a precedence level. Left-associative operators recurse with a
/// slightly higher right binding power; right-associative ones with a lower one.
fn bp(prec: Precedence, right_assoc: bool) -> (u16, u16) {
	let level = (prec as u16) + 1;
	if right_assoc {
		(2 * level + 1, 2 * level)
	} else {
		(2 * level, 2 * level + 1)
	}
}

impl Parser<'_> {
	pub(super) fn parse_expr(&mut self) -> Expr {
		self.parse_bp(0)
	}

	fn parse_bp(&mut self, min_bp: u16) -> Expr {
		let start = self.position();
		let mut lhs = self.parse_prefix();

		while let Some((infix, l_bp, r_bp, consume)) = self.peek_infix() {
			if l_bp < min_bp {
				break;
			}
			let infix_span = self.current_span();
			for _ in 0..consume {
				self.advance();
			}
			lhs = match infix {
				Infix::Binary(op) => {
					let rhs = self.parse_bp(r_bp);
					self.mk_expr(
						ExprKind::BinaryOp {
							lhs: Box::new(lhs),
							op,
							rhs: Box::new(rhs),
						},
						self.span_from(start),
					)
				}
				Infix::Assign(op) => {
					let rhs = self.parse_bp(r_bp);
					self.mk_expr(
						ExprKind::AssignOp {
							lhs: Box::new(lhs),
							op,
							rhs: Box::new(rhs),
						},
						self.span_from(start),
					)
				}
				Infix::As => {
					let ty = self.parse_type();
					self.mk_expr(
						ExprKind::TypeOp {
							lhs: Box::new(lhs),
							op: nymph_ast::ops::TypeOperator::As,
							rhs: ty,
						},
						self.span_from(start),
					)
				}
				Infix::Is | Infix::NotIs => {
					let op = if matches!(infix, Infix::Is) {
						nymph_ast::ops::PatternOperator::Is
					} else {
						nymph_ast::ops::PatternOperator::NotIs
					};
					let pat = self.parse_pattern();
					self.mk_expr(
						ExprKind::PatternOp {
							lhs: Box::new(lhs),
							op,
							rhs: pat,
						},
						self.span_from(start),
					)
				}
				Infix::RangeExclusive => {
					if self.can_start_expr() {
						let max = self.parse_bp(r_bp);
						self.mk_expr(
							ExprKind::Range(RangeKind::Exclusive {
								min: Box::new(lhs),
								max: Box::new(max),
							}),
							self.span_from(start),
						)
					} else {
						self.mk_expr(
							ExprKind::Range(RangeKind::From(Box::new(lhs))),
							self.span_from(start),
						)
					}
				}
				Infix::RangeInclusive => {
					let max = if self.can_start_expr() {
						self.parse_bp(r_bp)
					} else {
						self.emit(infix_span, ParseError::MissingInclusiveRangeUpperBound);
						self.mk_expr(ExprKind::Tuple(Vec::new()), self.current_span())
					};
					self.mk_expr(
						ExprKind::Range(RangeKind::Inclusive {
							min: Box::new(lhs),
							max: Box::new(max),
						}),
						self.span_from(start),
					)
				}
			};
		}
		lhs
	}

	/// Inspect the infix operator at the cursor, returning its kind, binding powers, and
	/// how many tokens it spans (2 for the recombined shift operators `<<` / `>>`).
	fn peek_infix(&self) -> Option<(Infix, u16, u16, usize)> {
		let token = self.peek()?;
		let binary = |op: BinaryOperator| {
			let (l, r) = bp(op.precedence(), matches!(op, BinaryOperator::Power));
			Some((Infix::Binary(op), l, r, 1))
		};

		match token {
			Token::Plus => binary(BinaryOperator::Plus),
			Token::Minus => binary(BinaryOperator::Minus),
			Token::Star => binary(BinaryOperator::Times),
			Token::Slash => binary(BinaryOperator::Divide),
			Token::Percent => binary(BinaryOperator::Remainder),
			Token::StarStar => binary(BinaryOperator::Power),
			Token::Amp => binary(BinaryOperator::BitAnd),
			Token::Pipe => binary(BinaryOperator::BitOr),
			Token::Caret => binary(BinaryOperator::BitXor),
			Token::EqEq => binary(BinaryOperator::Equals),
			Token::BangEq => binary(BinaryOperator::NotEquals),
			Token::LtEq => binary(BinaryOperator::LessThanEquals),
			Token::GtEq => binary(BinaryOperator::GreaterThanEquals),
			Token::In => binary(BinaryOperator::In),
			Token::BangIn => binary(BinaryOperator::NotIn),
			Token::AmpAmp => binary(BinaryOperator::BoolAnd),
			Token::PipePipe => binary(BinaryOperator::BoolOr),
			Token::PipeArrow => binary(BinaryOperator::Pipe),
			Token::DoubleQuestion => binary(BinaryOperator::Unwrap),
			// `<` and `>` may be a shift (two adjacent tokens) or a comparison.
			Token::Lt => {
				if self.adjacent_next(Token::Lt) {
					let (l, r) = bp(Precedence::BitShift, false);
					Some((Infix::Binary(BinaryOperator::LeftShift), l, r, 2))
				} else {
					binary(BinaryOperator::LessThan)
				}
			}
			Token::Gt => {
				if self.adjacent_next(Token::Gt) {
					let (l, r) = bp(Precedence::BitShift, false);
					Some((Infix::Binary(BinaryOperator::RightShift), l, r, 2))
				} else {
					binary(BinaryOperator::GreaterThan)
				}
			}
			Token::DotDot => {
				let (l, r) = bp(Precedence::Range, false);
				Some((Infix::RangeExclusive, l, r, 1))
			}
			Token::DotDotEq => {
				let (l, r) = bp(Precedence::Range, false);
				Some((Infix::RangeInclusive, l, r, 1))
			}
			Token::As => {
				let (l, r) = bp(Precedence::As, false);
				Some((Infix::As, l, r, 1))
			}
			Token::Is => {
				let (l, r) = bp(Precedence::Is, false);
				Some((Infix::Is, l, r, 1))
			}
			Token::BangIs => {
				let (l, r) = bp(Precedence::Is, false);
				Some((Infix::NotIs, l, r, 1))
			}
			_ => self.peek_assign(),
		}
	}

	fn peek_assign(&self) -> Option<(Infix, u16, u16, usize)> {
		let op = match self.peek()? {
			Token::Eq => AssignOperator::Assign,
			Token::PlusEq => AssignOperator::PlusAssign,
			Token::MinusEq => AssignOperator::MinusAssign,
			Token::StarEq => AssignOperator::TimesAssign,
			Token::SlashEq => AssignOperator::DivideAssign,
			Token::PercentEq => AssignOperator::RemainderAssign,
			Token::StarStarEq => AssignOperator::PowerAssign,
			Token::LtLtEq => AssignOperator::LeftShiftAssign,
			Token::GtGtEq => AssignOperator::RightShiftAssign,
			Token::AmpEq => AssignOperator::BitAndAssign,
			Token::CaretEq => AssignOperator::BitXorAssign,
			Token::PipeEq => AssignOperator::BitOrAssign,
			Token::TildeEq => AssignOperator::BitNotAssign,
			Token::AmpAmpEq => AssignOperator::BoolAndAssign,
			Token::PipePipeEq => AssignOperator::BoolOrAssign,
			_ => return None,
		};
		let (l, r) = bp(Precedence::Assignment, true);
		Some((Infix::Assign(op), l, r, 1))
	}

	/// True when the next token equals `token` and its span immediately follows the
	/// current token's (used to recombine `< <` into `<<`).
	fn adjacent_next(&self, token: Token) -> bool {
		if self.peek_nth(1) != Some(&token) {
			return false;
		}
		match (self.peek_nth_span(0), self.peek_nth_span(1)) {
			(Some(cur), Some(next)) => next.start == cur.end,
			_ => false,
		}
	}

	fn parse_prefix(&mut self) -> Expr {
		let start = self.position();
		let prefix = match self.peek() {
			Some(Token::Bang) => Some(PrefixOperator::BoolNot),
			Some(Token::Minus) => Some(PrefixOperator::Negate),
			Some(Token::Tilde) => Some(PrefixOperator::BitNot),
			_ => None,
		};
		if let Some(op) = prefix {
			self.advance();
			let (_, r) = bp(Precedence::Unary, false);
			let operand = self.parse_bp(r);
			return self.mk_expr(
				ExprKind::PrefixOp {
					op,
					value: Box::new(operand),
				},
				self.span_from(start),
			);
		}

		// Prefix (unbounded-below) ranges.
		if self.check(&Token::DotDot) {
			self.advance();
			let (_, r) = bp(Precedence::Range, false);
			let max = self.parse_bp(r);
			return self.mk_expr(
				ExprKind::Range(RangeKind::To(Box::new(max))),
				self.span_from(start),
			);
		}
		if self.check(&Token::DotDotEq) {
			self.advance();
			let (_, r) = bp(Precedence::Range, false);
			let max = self.parse_bp(r);
			return self.mk_expr(
				ExprKind::Range(RangeKind::ToInclusive(Box::new(max))),
				self.span_from(start),
			);
		}

		let atom = self.parse_primary();
		self.parse_postfix(atom)
	}

	fn parse_postfix(&mut self, mut expr: Expr) -> Expr {
		let start = self.position();
		loop {
			expr = match self.peek() {
				Some(Token::Dot) => {
					self.advance();
					let member = self.expect_ident();
					self.mk_expr(
						ExprKind::MemberAccess {
							parent: Box::new(expr),
							member,
							optional: false,
						},
						self.span_from(start),
					)
				}
				Some(Token::QuestionDot) => {
					self.advance();
					if self.check(&Token::LBracket) {
						self.advance();
						let index = self.parse_expr();
						self.expect(&Token::RBracket);
						self.mk_expr(
							ExprKind::IndexAccess {
								parent: Box::new(expr),
								index: Box::new(index),
								optional: true,
							},
							self.span_from(start),
						)
					} else {
						let member = self.expect_ident();
						self.mk_expr(
							ExprKind::MemberAccess {
								parent: Box::new(expr),
								member,
								optional: true,
							},
							self.span_from(start),
						)
					}
				}
				Some(Token::LBracket) => {
					self.advance();
					let index = self.parse_expr();
					self.expect(&Token::RBracket);
					self.mk_expr(
						ExprKind::IndexAccess {
							parent: Box::new(expr),
							index: Box::new(index),
							optional: false,
						},
						self.span_from(start),
					)
				}
				Some(Token::LParen) => {
					self.advance();
					let args = self.comma_separated(&Token::RParen, |p| p.parse_call_arg());
					self.mk_expr(
						ExprKind::Call {
							func: Box::new(expr),
							generics: Vec::new(),
							args,
						},
						self.span_from(start),
					)
				}
				Some(Token::Question) => {
					self.advance();
					self.mk_expr(
						ExprKind::PostfixOp {
							op: nymph_ast::ops::PostfixOperator::ErrorReturn,
							value: Box::new(expr),
						},
						self.span_from(start),
					)
				}
				_ => break,
			};
		}
		expr
	}

	fn parse_call_arg(&mut self) -> Spanned<CallArg> {
		let start = self.position();
		if self.check(&Token::DotDotDot) {
			self.advance();
			let value = self.parse_expr();
			return Spanned(
				CallArg {
					value,
					name: None,
					spread: true,
				},
				self.span_from(start),
			);
		}
		let name = if matches!(self.peek(), Some(Token::Identifier(_)))
			&& self.peek_nth(1) == Some(&Token::Eq)
		{
			let name = self.expect_ident();
			self.advance(); // `=`
			Some(name)
		} else {
			None
		};
		let value = self.parse_expr();
		Spanned(
			CallArg {
				value,
				name,
				spread: false,
			},
			self.span_from(start),
		)
	}

	fn parse_primary(&mut self) -> Expr {
		let start = self.position();
		let Some(token) = self.peek() else {
			let span = self.current_span();
			self.emit(span, ParseError::ExpectedExpressionFoundEof);
			return self.mk_expr(ExprKind::Tuple(Vec::new()), span);
		};

		match token {
			Token::Int(v) => {
				let v = *v;
				let span = self.advance().unwrap().1;
				self.mk_expr(ExprKind::Int(Spanned(v, span)), span)
			}
			Token::UInt(v) => {
				let v = *v;
				let span = self.advance().unwrap().1;
				self.mk_expr(ExprKind::UInt(Spanned(v, span)), span)
			}
			Token::Float(v) => {
				let v = *v;
				let span = self.advance().unwrap().1;
				self.mk_expr(ExprKind::Float(Spanned(v, span)), span)
			}
			Token::Char(c) => {
				let c = *c;
				let span = self.advance().unwrap().1;
				self.mk_expr(ExprKind::Char(Spanned(c, span)), span)
			}
			Token::True => {
				let span = self.advance().unwrap().1;
				self.mk_expr(ExprKind::Boolean(Spanned(true, span)), span)
			}
			Token::False => {
				let span = self.advance().unwrap().1;
				self.mk_expr(ExprKind::Boolean(Spanned(false, span)), span)
			}
			Token::Str(_) => self.parse_string(),
			Token::This => {
				let span = self.advance().unwrap().1;
				self.mk_expr(ExprKind::This, span)
			}
			Token::AnonymousParam(index) => {
				let index = *index;
				let span = self.advance().unwrap().1;
				self.mk_expr(ExprKind::AnonymousParam(index), span)
			}
			Token::Identifier(_) => {
				// A labeled closure or block: `label@(params) -> body`, `label@{...}`.
				if self.peek_nth(1) == Some(&Token::At) {
					let label = self.expect_ident();
					let at = self.expect(&Token::At).unwrap();
					self.require_label_adjacency(label.1, at);
					if let Some(opener) = self.peek_nth_span(0) {
						self.require_label_adjacency(at, opener);
					}
					if self.check(&Token::LParen) {
						self.parse_labeled_closure(label, start)
					} else if self.check(&Token::LBrace) {
						self.parse_block_with_label(Some(label), start)
					} else {
						let span = self.current_span();
						self.emit(
							span,
							ParseError::ExpectedExpression {
								found: self
									.peek()
									.map_or("end of input".into(), |token| token.describe().into()),
							},
						);
						self.mk_expr(ExprKind::Tuple(Vec::new()), span)
					}
				// A single-parameter closure: `x -> body`.
				} else if self.peek_nth(1) == Some(&Token::Arrow) {
					self.parse_ident_closure()
				} else {
					let name = self.expect_ident();
					self.mk_expr(ExprKind::Identifier(name.clone()), name.1)
				}
			}
			Token::HashLBracket => self.parse_list_literal(),
			Token::HashLParen => self.parse_tuple_literal(),
			Token::HashLBrace => self.parse_map_literal(),
			Token::LParen => self.parse_paren_or_closure(),
			Token::LBrace => self.parse_block(),
			Token::If => self.parse_if(),
			Token::Match => self.parse_match(),
			Token::While => self.parse_while(),
			Token::For => self.parse_for(),
			Token::Return => {
				let keyword = self.advance().unwrap().1;
				let label = self.parse_control_label(keyword);
				let value = if self.can_start_expr() {
					Some(Box::new(self.parse_expr()))
				} else {
					None
				};
				self.mk_expr(ExprKind::Return { value, label }, self.span_from(start))
			}
			Token::Break => {
				let keyword = self.advance().unwrap().1;
				let label = self.parse_control_label(keyword);
				let value = if self.can_start_expr() {
					Some(Box::new(self.parse_expr()))
				} else {
					None
				};
				self.mk_expr(ExprKind::Break { value, label }, self.span_from(start))
			}
			Token::Continue => {
				let keyword = self.advance().unwrap().1;
				let label = self.parse_control_label(keyword);
				self.mk_expr(ExprKind::Continue { label }, self.span_from(start))
			}
			other => {
				let span = self.current_span();
				self.emit(
					span,
					ParseError::ExpectedExpression {
						found: other.describe().into(),
					},
				);
				self.advance();
				self.mk_expr(ExprKind::Tuple(Vec::new()), span)
			}
		}
	}

	fn parse_string(&mut self) -> Expr {
		let start = self.position();
		let fragments = match self.peek() {
			Some(Token::Str(f)) => f.clone(),
			_ => unreachable!("guarded by caller"),
		};
		let mut parts = Vec::new();
		for fragment in &fragments {
			match &fragment.0 {
				StrFragment::Text(text) => parts.push(Spanned(StringPart::Text(text.clone()), fragment.1)),
				StrFragment::Escape(escape) => {
					parts.push(Spanned(StringPart::EscapeSequence(*escape), fragment.1))
				}
				StrFragment::Interpolation(tokens) => {
					let eoi = Span::new(fragment.1.end, fragment.1.end);
					let mut sub = Parser::new(tokens, eoi);
					// Continue THIS parser's node-id counter through the sub-parser and read
					// it back, so an interpolated expression's node ids never collide with the
					// surrounding tree's. A fresh `Parser` restarts `next_id` at 0, so without
					// this an interpolated `${a + b}` and some unrelated node would share an id
					// — and the second to be `record`ed would clobber the first's
					// resolution/type (e.g. the operator's dispatch, lost at lowering as "no
					// operator resolution recorded").
					sub.next_id = self.next_id;
					let expr = if sub.at_end() {
						sub.emit(fragment.1, ParseError::EmptyInterpolation);
						sub.mk_expr(ExprKind::Tuple(Vec::new()), fragment.1)
					} else {
						let expr = sub.parse_expr();
						if !sub.at_end() {
							sub.emit(sub.current_span(), ParseError::TrailingInterpolationContent);
						}
						expr
					};
					self.next_id = sub.next_id;
					// The interpolation token is already closed. Syntax errors inside it
					// cannot be repaired by appending source after the closing `}`, even
					// when the nested parser happened to reach its local end-of-input.
					if !sub.diagnostics.is_empty() {
						self.incomplete = false;
					}
					self.diagnostics.extend(sub.diagnostics);
					parts.push(Spanned(StringPart::InterpolatedExpr(expr), fragment.1));
				}
			}
		}
		self.advance();
		self.mk_expr(ExprKind::String(parts), self.span_from(start))
	}

	fn parse_list_literal(&mut self) -> Expr {
		let start = self.position();
		self.advance(); // `#[`
		let items = self.comma_separated(&Token::RBracket, |p| p.parse_list_item());
		self.mk_expr(ExprKind::List(items), self.span_from(start))
	}

	fn parse_tuple_literal(&mut self) -> Expr {
		let start = self.position();
		self.advance(); // `#(`
		let items = self.comma_separated(&Token::RParen, |p| p.parse_list_item());
		self.mk_expr(ExprKind::Tuple(items), self.span_from(start))
	}

	fn parse_list_item(&mut self) -> Spanned<ListItem> {
		let start = self.position();
		if self.check(&Token::DotDotDot) {
			self.advance();
			let value = self.parse_expr();
			Spanned(ListItem::Spread(value), self.span_from(start))
		} else {
			let value = self.parse_expr();
			Spanned(ListItem::Expr(value), self.span_from(start))
		}
	}

	fn parse_map_literal(&mut self) -> Expr {
		let start = self.position();
		self.advance(); // `#{`
		let entries = self.comma_separated(&Token::RBrace, |p| {
			let entry_start = p.position();
			if p.check(&Token::DotDotDot) {
				p.advance();
				let value = p.parse_expr();
				Spanned(MapEntry::Spread(value), p.span_from(entry_start))
			} else {
				let key = p.parse_expr();
				p.expect(&Token::Colon);
				let value = p.parse_expr();
				Spanned(MapEntry::Entry(key, value), p.span_from(entry_start))
			}
		});
		self.mk_expr(ExprKind::Map(entries), self.span_from(start))
	}

	/// `(` may open a grouped expression or a parenthesised closure `(params) -> body`.
	fn parse_paren_or_closure(&mut self) -> Expr {
		let save = self.position();
		if let Some(closure) = self.try_parse_paren_closure() {
			return closure;
		}
		self.restore(save);
		let start = self.position();
		self.advance(); // `(`
		let inner = self.parse_expr();
		self.expect(&Token::RParen);
		self.mk_expr(ExprKind::Grouped(Box::new(inner)), self.span_from(start))
	}

	fn try_parse_paren_closure(&mut self) -> Option<Expr> {
		let start = self.position();
		let before = self.diagnostics.len();
		self.advance(); // `(`
		let mut params = Vec::new();
		while !self.check(&Token::RParen) && !self.at_end() {
			let param = self.parse_closure_param();
			params.push(param);
			if self.eat(&Token::Comma).is_none() {
				break;
			}
		}
		if self.eat(&Token::RParen).is_none() {
			self.diagnostics.truncate(before);
			return None;
		}
		let return_type = if self.eat(&Token::Colon).is_some() {
			Some(self.parse_type())
		} else {
			None
		};
		if self.eat(&Token::Arrow).is_none() {
			self.diagnostics.truncate(before);
			return None;
		}
		let mut body = self.parse_expr();
		let label = if let ExprKind::Block { label, .. } = &mut body.kind {
			label.take()
		} else {
			None
		};
		Some(self.mk_expr(
			ExprKind::Closure {
				label,
				params,
				generics: Vec::new(),
				return_type,
				body: Box::new(body),
			},
			self.span_from(start),
		))
	}

	fn parse_labeled_closure(&mut self, label: Ident, start: usize) -> Expr {
		let mut closure = self.try_parse_paren_closure().unwrap_or_else(|| {
			let span = self.current_span();
			self.mk_expr(ExprKind::Tuple(Vec::new()), span)
		});
		if let ExprKind::Closure { label: slot, .. } = &mut closure.kind {
			if let Some(inner) = slot.as_ref().filter(|inner| inner.0 != label.0) {
				self.emit(
					label.1,
					ParseError::MismatchedClosureLabels {
						outer: label.0.clone(),
						inner: inner.0.clone(),
						inner_span: inner.1,
					},
				);
			}
			*slot = Some(label);
		}
		closure.span = self.span_from(start);
		closure
	}

	fn parse_control_label(&mut self, keyword: Span) -> Option<Ident> {
		self.eat(&Token::At).map(|at| {
			self.require_label_adjacency(keyword, at);
			let label = self.expect_ident();
			self.require_label_adjacency(at, label.1);
			label
		})
	}

	fn require_label_adjacency(&mut self, left: Span, right: Span) {
		if left.end != right.start {
			self.emit(
				Span::new(left.end, right.start),
				ParseError::WhitespaceInLabel,
			);
		}
	}

	fn parse_closure_param(&mut self) -> Spanned<ClosureParam> {
		let start = self.position();
		let spread = self.eat(&Token::DotDotDot).is_some();
		let mutable = self.eat(&Token::Mut).is_some();
		let name = self.parse_binding_pattern();
		let type_ = if self.eat(&Token::Colon).is_some() {
			Some(self.parse_type())
		} else {
			None
		};
		Spanned(
			ClosureParam {
				name,
				type_,
				mutable,
				spread,
			},
			self.span_from(start),
		)
	}

	fn parse_ident_closure(&mut self) -> Expr {
		let start = self.position();
		let name = self.expect_ident();
		let param = Spanned(
			ClosureParam {
				name: Spanned(
					nymph_ast::expr::Pattern::Binding {
						name: name.clone(),
						inner: Box::new(Spanned(nymph_ast::expr::Pattern::Placeholder, name.1)),
					},
					name.1,
				),
				type_: None,
				mutable: false,
				spread: false,
			},
			name.1,
		);
		self.expect(&Token::Arrow);
		let body = self.parse_expr();
		self.mk_expr(
			ExprKind::Closure {
				label: None,
				params: vec![param],
				generics: Vec::new(),
				return_type: None,
				body: Box::new(body),
			},
			self.span_from(start),
		)
	}

	fn parse_if(&mut self) -> Expr {
		let start = self.position();
		self.advance(); // `if`
		self.expect(&Token::LParen);
		let condition = self.parse_expr();
		self.expect(&Token::RParen);
		let then = self.parse_expr();
		let otherwise = if self.eat(&Token::Else).is_some() {
			Some(Box::new(self.parse_expr()))
		} else {
			None
		};
		self.mk_expr(
			ExprKind::If {
				condition: Box::new(condition),
				then: Box::new(then),
				otherwise,
			},
			self.span_from(start),
		)
	}

	fn parse_match(&mut self) -> Expr {
		let start = self.position();
		self.advance(); // `match`
		self.expect(&Token::LParen);
		let value = self.parse_expr();
		self.expect(&Token::RParen);
		self.expect(&Token::LBrace);
		let arms = self.comma_separated(&Token::RBrace, |p| {
			let pattern = p.parse_pattern();
			let guard = if p.eat(&Token::If).is_some() {
				Some(p.parse_expr())
			} else {
				None
			};
			p.expect(&Token::Arrow);
			let body = p.parse_expr();
			MatchArm {
				pattern,
				guard,
				body,
			}
		});
		self.mk_expr(
			ExprKind::Match {
				value: Box::new(value),
				arms,
			},
			self.span_from(start),
		)
	}

	fn parse_while(&mut self) -> Expr {
		let start = self.position();
		let keyword = self.advance().unwrap().1; // `while`
		let label = self.parse_control_label(keyword);
		self.expect(&Token::LParen);
		let condition = self.parse_expr();
		self.expect(&Token::RParen);
		let body = self.parse_expr();
		self.mk_expr(
			ExprKind::While {
				condition: Box::new(condition),
				body: Box::new(body),
				label,
			},
			self.span_from(start),
		)
	}

	fn parse_for(&mut self) -> Expr {
		let start = self.position();
		let keyword = self.advance().unwrap().1; // `for`
		let label = self.parse_control_label(keyword);
		self.expect(&Token::LParen);
		let variable = self.parse_binding_pattern();
		self.expect(&Token::In);
		let iterable = self.parse_expr();
		self.expect(&Token::RParen);
		let body = self.parse_expr();
		self.mk_expr(
			ExprKind::For {
				variable,
				iterable: Box::new(iterable),
				body: Box::new(body),
				label,
			},
			self.span_from(start),
		)
	}

	pub(super) fn parse_block(&mut self) -> Expr {
		let start = self.position();
		self.parse_block_with_label(None, start)
	}

	fn parse_block_with_label(&mut self, label: Option<Ident>, start: usize) -> Expr {
		self.expect(&Token::LBrace);
		let mut body = Vec::new();
		while !self.check(&Token::RBrace) && !self.at_end() {
			body.push(self.parse_statement());
		}
		self.expect(&Token::RBrace);
		self.mk_expr(ExprKind::Block { body, label }, self.span_from(start))
	}

	fn parse_statement(&mut self) -> Spanned<Statement> {
		let start = self.position();
		if self.check(&Token::Let) {
			let (meta, value) = self.parse_let_binding();
			Spanned(Statement::Let { meta, value }, self.span_from(start))
		} else {
			let expr = self.parse_expr();
			Spanned(Statement::Expr(expr), self.span_from(start))
		}
	}

	fn can_start_expr(&self) -> bool {
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
					| Token::This
					| Token::Identifier(_)
					| Token::AnonymousParam(_)
					| Token::HashLBracket
					| Token::HashLParen
					| Token::HashLBrace
					| Token::LParen
					| Token::LBrace
					| Token::If
					| Token::Match
					| Token::While
					| Token::For
					| Token::Bang
					| Token::Minus
					| Token::Tilde
					| Token::Return
					| Token::Break
					| Token::Continue
			)
		)
	}
}
