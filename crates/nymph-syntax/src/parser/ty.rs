//! Parsing of the surface type grammar.

use crate::errors::ParseError;
use nymph_ast::{
	Span, Spanned,
	token::Token,
	ty::{
		Effect, EffectRow, FunctionTypeParam, GenericArg, GenericArgValue, GenericParam,
		GenericParamKind, Type,
	},
};

use super::Parser;

impl Parser<'_> {
	/// Parse a full type, including `+` intersections (the lowest-precedence form).
	pub(super) fn parse_type(&mut self) -> Spanned<Type> {
		let start = self.position();
		let mut lhs = self.parse_type_primary();
		while self.check(&Token::Plus) && self.peek_nth(1) != Some(&Token::Bang) {
			self.advance();
			let rhs = self.parse_type_primary();
			let span = self.span_from(start);
			lhs = Spanned(Type::Intersection(Box::new(lhs), Box::new(rhs)), span);
		}
		lhs
	}

	fn parse_type_primary(&mut self) -> Spanned<Type> {
		let start = self.position();

		let Some(token) = self.peek() else {
			let span = self.current_span();
			self.emit(span, ParseError::ExpectedTypeFoundEof);
			return Spanned(Type::Infer, span);
		};

		let ty = match token {
			Token::IntType => self.simple_type(Type::Int),
			Token::UIntType => self.simple_type(Type::UInt),
			Token::FloatType => self.simple_type(Type::Float),
			Token::CharType => self.simple_type(Type::Char),
			Token::StringType => self.simple_type(Type::String),
			Token::BooleanType => self.simple_type(Type::Boolean),
			Token::VoidType => self.simple_type(Type::Void),
			Token::NeverType => self.simple_type(Type::Never),
			Token::SelfType => self.simple_type(Type::SelfType),
			Token::Underscore => self.simple_type(Type::Infer),
			Token::HashLBracket => self.parse_list_type(),
			Token::HashLParen => self.parse_tuple_type(),
			Token::HashLBrace => self.parse_map_type(),
			Token::LParen => self.parse_paren_type(),
			Token::Identifier(_) => self.parse_reference_type(),
			other => {
				let span = self.current_span();
				self.emit(
					span,
					ParseError::ExpectedType {
						found: other.describe().into(),
					},
				);
				self.advance();
				return Spanned(Type::Infer, span);
			}
		};
		let _ = start;
		ty
	}

	fn simple_type(&mut self, ty: Type) -> Spanned<Type> {
		let span = self
			.advance()
			.map(|t| t.1)
			.unwrap_or_else(|| self.current_span());
		Spanned(ty, span)
	}

	fn parse_list_type(&mut self) -> Spanned<Type> {
		let start = self.position();
		self.advance(); // `#[`
		let inner = self.parse_type();
		self.expect(&Token::RBracket);
		Spanned(Type::List(Box::new(inner)), self.span_from(start))
	}

	fn parse_tuple_type(&mut self) -> Spanned<Type> {
		let start = self.position();
		self.advance(); // `#(`
		let items = self.comma_separated(&Token::RParen, |p| p.parse_type());
		Spanned(Type::Tuple(items), self.span_from(start))
	}

	fn parse_map_type(&mut self) -> Spanned<Type> {
		let start = self.position();
		self.advance(); // `#{`
		let key = self.parse_type();
		self.expect(&Token::Colon);
		let value = self.parse_type();
		self.expect(&Token::RBrace);
		Spanned(
			Type::Map(Box::new(key), Box::new(value)),
			self.span_from(start),
		)
	}

	/// Either a function type `(A, b: B) -> C` or a grouped type `(A)`.
	fn parse_paren_type(&mut self) -> Spanned<Type> {
		let start = self.position();
		self.advance(); // `(`
		let params = self.comma_separated(&Token::RParen, |p| p.parse_fn_type_param());

		if self.eat(&Token::Arrow).is_some() {
			let (return_type, effects) = self.parse_callable_return();
			return Spanned(
				Type::Function {
					params,
					return_type: Box::new(return_type),
					effects,
				},
				self.span_from(start),
			);
		}

		// Not a function type: must be a single unlabelled grouped type.
		if params.len() == 1 && params[0].0.is_none() {
			let inner = params.into_iter().next().unwrap().1;
			Spanned(Type::Grouped(Box::new(inner)), self.span_from(start))
		} else {
			let span = self.span_from(start);
			self.emit(span, ParseError::ExpectedArrowInFnType);
			Spanned(Type::Infer, span)
		}
	}

	fn parse_fn_type_param(&mut self) -> FunctionTypeParam {
		// A label is `name:` where the `:` disambiguates from a bare type.
		let label = if matches!(self.peek(), Some(Token::Identifier(_)))
			&& self.peek_nth(1) == Some(&Token::Colon)
		{
			let name = self.expect_ident();
			self.advance(); // `:`
			Some(name)
		} else {
			None
		};
		let ty = self.parse_type();
		(label, ty)
	}

	fn parse_reference_type(&mut self) -> Spanned<Type> {
		let start = self.position();
		let name = self.expect_ident();
		let generics = self.parse_generic_args();
		if self.eat(&Token::Dot).is_some() {
			let variant = self.expect_ident();
			Spanned(
				Type::Reference {
					name: Spanned(
						format!("{}.{}", name.0, variant.0).into(),
						self.span_from(start),
					),
					generics,
				},
				self.span_from(start),
			)
		} else {
			Spanned(Type::Reference { name, generics }, self.span_from(start))
		}
	}

	/// Parse the value/effect result after a callable's `:` or `->`.
	pub(super) fn parse_callable_return(&mut self) -> (Spanned<Type>, Option<Spanned<EffectRow>>) {
		if self.check(&Token::Bang) {
			let effects = self.parse_effect_row();
			return (Spanned(Type::Void, effects.1), Some(effects));
		}

		let return_type = self.parse_type();
		let effects = if self.check(&Token::Plus) && self.peek_nth(1) == Some(&Token::Bang) {
			self.advance();
			Some(self.parse_effect_row())
		} else {
			None
		};
		(return_type, effects)
	}

	pub(super) fn parse_effect_row(&mut self) -> Spanned<EffectRow> {
		let start = self.position();
		let mut effects = Vec::new();
		loop {
			let bang = self
				.eat(&Token::Bang)
				.unwrap_or_else(|| self.current_span());
			let effect = match self.peek() {
				Some(Token::LParen) => {
					self.advance();
					self.expect(&Token::RParen);
					None
				}
				Some(Token::Underscore) => {
					let span = self.advance().map_or(bang, |token| token.1);
					Some(Spanned(Effect::Infer, Span::new(bang.start, span.end)))
				}
				Some(Token::Identifier(_)) => {
					let name = self.expect_ident();
					Some(Spanned(
						Effect::Named(name.clone()),
						Span::new(bang.start, name.1.end),
					))
				}
				other => {
					let span = self.current_span();
					let found = other.map_or("end of input", Token::describe);
					self.emit(
						span,
						ParseError::ExpectedEffect {
							found: found.into(),
						},
					);
					Some(Spanned(Effect::Error, Span::new(bang.start, span.end)))
				}
			};
			if let Some(effect) = effect {
				effects.push(effect);
			}
			if self.eat(&Token::Plus).is_none() {
				break;
			}
			if !self.check(&Token::Bang) {
				let span = self.current_span();
				let found = self.peek().map_or("end of input", Token::describe);
				self.emit(
					span,
					ParseError::ExpectedEffectAfterPlus {
						found: found.into(),
					},
				);
				effects.push(Spanned(Effect::Error, span));
				break;
			}
		}
		Spanned(EffectRow { effects }, self.span_from(start))
	}

	/// Parse `<A, !E, Output = B, Effects = !Io>` if present.
	pub(super) fn parse_generic_args(&mut self) -> Vec<Spanned<GenericArg>> {
		if !self.check(&Token::Lt) {
			return Vec::new();
		}
		self.advance(); // `<`
		let mut args = Vec::new();
		while !self.check(&Token::Gt) && !self.at_end() {
			let start = self.position();
			let name = if matches!(self.peek(), Some(Token::Identifier(_)))
				&& self.peek_nth(1) == Some(&Token::Eq)
			{
				let name = self.expect_ident();
				self.advance(); // `=`
				Some(name)
			} else {
				None
			};
			let value = if self.check(&Token::Bang) {
				GenericArgValue::Effect(self.parse_effect_row())
			} else {
				GenericArgValue::Type(self.parse_type())
			};
			args.push(Spanned(GenericArg { value, name }, self.span_from(start)));
			if self.eat(&Token::Comma).is_none()
				&& !(self.check(&Token::Plus)
					&& self.peek_nth(1) == Some(&Token::Bang)
					&& self.advance().is_some())
			{
				break;
			}
		}
		self.expect(&Token::Gt);
		args
	}

	/// Parse `<T, U: Constraint, !E>` generic parameters if present.
	pub(super) fn parse_generic_params(&mut self) -> Vec<Spanned<GenericParam>> {
		if !self.check(&Token::Lt) {
			return Vec::new();
		}
		self.advance(); // `<`
		let mut params = Vec::new();
		while !self.check(&Token::Gt) && !self.at_end() {
			let start = self.position();
			let kind = if self.eat(&Token::Bang).is_some() {
				GenericParamKind::Effect
			} else {
				GenericParamKind::Type
			};
			let name = self.expect_ident();
			let constraint = if kind == GenericParamKind::Type && self.eat(&Token::Colon).is_some() {
				Some(self.parse_type())
			} else {
				None
			};
			let default = if kind == GenericParamKind::Type && self.eat(&Token::Eq).is_some() {
				Some(self.parse_type())
			} else {
				None
			};
			params.push(Spanned(
				GenericParam {
					name,
					kind,
					constraint,
					default,
				},
				self.span_from(start),
			));
			if self.eat(&Token::Comma).is_none()
				&& !(self.check(&Token::Plus)
					&& self.peek_nth(1) == Some(&Token::Bang)
					&& self.advance().is_some())
			{
				break;
			}
		}
		self.expect(&Token::Gt);
		params
	}
}
