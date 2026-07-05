use crate::{
	ast::{
		Ident, Spanned,
		types::{FunctionTypeParam, GenericArg, GenericParam, Type},
	},
	lexer::token::Token,
};

use super::{
	core::{Parser, span},
	error::ParseError,
};

impl<'src> Parser<'src> {
	pub fn parse_type(&mut self) -> Option<Spanned<Type>> {
		self.parse_type_pratt(0)
	}

	fn parse_type_pratt(&mut self, min_bp: u8) -> Option<Spanned<Type>> {
		let mut lhs = self.parse_type_prefix()?;

		while let Some((op_bp, _)) = self.type_infix_binding_power() {
			if op_bp < min_bp {
				break;
			}

			lhs = self.parse_type_infix(lhs)?;
		}

		Some(lhs)
	}

	fn type_infix_binding_power(&self) -> Option<(u8, u8)> {
		match self.peek()? {
			Token::Plus => Some((1, 2)),
			_ => None,
		}
	}

	fn parse_type_prefix(&mut self) -> Option<Spanned<Type>> {
		if let Some(Spanned(params, arrow_span)) = self.try_parse_function_type_prefix() {
			let return_type = self.parse_type()?;
			let s = span(
				params
					.first()
					.map(|(_, t)| t.span().start)
					.unwrap_or(arrow_span.start),
				return_type.span().end,
			);
			return Some(Spanned(
				Type::Function {
					params,
					return_type: return_type.into(),
				},
				s,
			));
		}

		self.parse_type_atom()
	}

	fn try_parse_function_type_prefix(&mut self) -> Option<Spanned<Vec<FunctionTypeParam>>> {
		let pos = self.position();

		let Token::Parens(inner) = self.peek()? else {
			return None;
		};
		let inner = inner.clone();
		let parens_span = self.current_span();
		self.advance();

		if !self.check(&Token::Arrow) {
			self.restore(pos);
			return None;
		}
		let arrow_span = self.current_span();
		self.advance();

		let params = self.with_nested(&inner, parens_span, |p| p.parse_function_type_params());

		Some(Spanned(params, arrow_span))
	}

	fn parse_function_type_params(&mut self) -> Vec<(Option<Ident>, Spanned<Type>)> {
		let mut params = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let name = if let Some(Token::Identifier(_)) = self.peek() {
				if self.peek_nth(1) == Some(&Token::Colon) {
					let ident = self.identifier();
					self.consume(&Token::Colon);
					ident
				} else {
					None
				}
			} else {
				None
			};

			let Some(ty) = self.parse_type() else {
				break;
			};

			params.push((name, ty));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		params
	}

	fn parse_type_infix(&mut self, lhs: Spanned<Type>) -> Option<Spanned<Type>> {
		match self.peek()? {
			Token::Plus => {
				self.advance();
				let rhs = self.parse_type_pratt(2)?;
				let s = span(lhs.span().start, rhs.span().end);
				Some(Spanned(Type::Intersection(lhs.into(), rhs.into()), s))
			}
			_ => Some(lhs),
		}
	}

	fn parse_type_atom(&mut self) -> Option<Spanned<Type>> {
		let start_span = self.current_span();

		let ty = match self.peek()? {
			Token::IntType => {
				self.advance();
				Type::Int
			}
			Token::FloatType => {
				self.advance();
				Type::Float
			}
			Token::CharType => {
				self.advance();
				Type::Char
			}
			Token::StringType => {
				self.advance();
				Type::String
			}
			Token::BooleanType => {
				self.advance();
				Type::Boolean
			}
			Token::VoidType => {
				self.advance();
				Type::Void
			}
			Token::NeverType => {
				self.advance();
				Type::Never
			}
			Token::SelfType => {
				self.advance();
				Type::Self_
			}
			Token::Underscore => {
				self.advance();
				Type::Infer
			}
			Token::List(inner) => {
				let inner = inner.clone();
				self.advance();
				let inner_type = self.with_nested(&inner, start_span, |p| p.parse_type());
				let Some(inner_type) = inner_type else {
					self.error(ParseError::custom(
						"expected a type inside list type",
						start_span,
					));
					return None;
				};
				Type::List(inner_type.into())
			}
			Token::Tuple(inner) => {
				let inner = inner.clone();
				self.advance();
				let types = self.with_nested(&inner, start_span, |p| p.parse_comma_separated_types());
				Type::Tuple(types)
			}
			Token::Map(inner) => {
				let inner = inner.clone();
				self.advance();
				let (key, value) = self.with_nested(&inner, start_span, |p| {
					let key = p.parse_type()?;
					p.expect(&Token::Colon, ":")?;
					let value = p.parse_type()?;
					Some((key, value))
				})?;
				Type::Map(key.into(), value.into())
			}
			Token::Parens(inner) => {
				let inner = inner.clone();
				self.advance();
				let inner_type = self.with_nested(&inner, start_span, |p| p.parse_type())?;
				Type::Grouped(inner_type.into())
			}
			Token::Identifier(_) => {
				let name = self.identifier()?;
				let generics = self.parse_generic_args().unwrap_or_default();
				Type::Reference { name, generics }
			}
			_ => {
				let found = self.peek().cloned().unwrap();
				self.error(ParseError::expected_type(found, start_span));
				return None;
			}
		};

		let end_span = self.previous_span();
		Some(Spanned(ty, span(start_span.start, end_span.end)))
	}

	fn parse_comma_separated_types(&mut self) -> Vec<Spanned<Type>> {
		let mut types = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let Some(ty) = self.parse_type() else {
				break;
			};

			types.push(ty);

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		types
	}

	pub fn parse_generic_args(&mut self) -> Option<Vec<Spanned<GenericArg>>> {
		if !self.check(&Token::Lt) {
			return None;
		}
		self.advance();

		let mut args = Vec::new();

		loop {
			if self.check(&Token::Gt) {
				break;
			}

			let name = if let Some(Token::Identifier(_)) = self.peek() {
				if self.peek_nth(1) == Some(&Token::Eq) {
					let ident = self.identifier()?;
					self.consume(&Token::Eq);
					Some(ident)
				} else {
					None
				}
			} else {
				None
			};

			let value = self.parse_type()?;
			let s = if let Some(ref n) = name {
				span(n.span().start, value.span().end)
			} else {
				value.span()
			};
			args.push(Spanned(GenericArg { name, value }, s));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		self.expect(&Token::Gt, ">")?;
		Some(args)
	}

	pub fn parse_generic_params(&mut self) -> Option<Vec<Spanned<GenericParam>>> {
		if !self.check(&Token::Lt) {
			return None;
		}
		self.advance();

		let mut params = Vec::new();

		loop {
			if self.check(&Token::Gt) {
				break;
			}

			let start_span = self.current_span();
			let name = self.expect_identifier()?;

			let constraint = if self.consume(&Token::Colon).is_some() {
				Some(self.parse_type()?)
			} else {
				None
			};

			let default = if self.consume(&Token::Eq).is_some() {
				Some(self.parse_type()?)
			} else {
				None
			};

			let end_span = self.previous_span();
			params.push(Spanned(
				GenericParam {
					name,
					constraint,
					default,
				},
				span(start_span.start, end_span.end),
			));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		self.expect(&Token::Gt, ">")?;
		Some(params)
	}
}
