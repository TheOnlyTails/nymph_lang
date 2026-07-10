//! Parsing of the surface type grammar.

use crate::errors::ParseError;
use nymph_ast::{
	Spanned,
	token::Token,
	ty::{FunctionTypeParam, GenericArg, GenericParam, Type},
};

use super::Parser;

impl Parser<'_> {
	/// Parse a full type, including `+` intersections (the lowest-precedence form).
	pub(super) fn parse_type(&mut self) -> Spanned<Type> {
		let start = self.position();
		let mut lhs = self.parse_type_primary();
		while self.check(&Token::Plus) {
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
			let return_type = self.parse_type();
			return Spanned(
				Type::Function {
					params,
					return_type: Box::new(return_type),
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
		Spanned(Type::Reference { name, generics }, self.span_from(start))
	}

	/// Parse `<A, Output = B>` if present, else an empty list. Each argument may be
	/// labelled (`Output = T`).
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
			let value = self.parse_type();
			args.push(Spanned(GenericArg { value, name }, self.span_from(start)));
			if self.eat(&Token::Comma).is_none() {
				break;
			}
		}
		self.expect(&Token::Gt);
		args
	}

	/// Parse `<T, U: Constraint, V = Default>` generic *parameters* if present.
	pub(super) fn parse_generic_params(&mut self) -> Vec<Spanned<GenericParam>> {
		if !self.check(&Token::Lt) {
			return Vec::new();
		}
		self.advance(); // `<`
		let mut params = Vec::new();
		while !self.check(&Token::Gt) && !self.at_end() {
			let start = self.position();
			let name = self.expect_ident();
			let constraint = if self.eat(&Token::Colon).is_some() {
				Some(self.parse_type())
			} else {
				None
			};
			let default = if self.eat(&Token::Eq).is_some() {
				Some(self.parse_type())
			} else {
				None
			};
			params.push(Spanned(
				GenericParam {
					name,
					constraint,
					default,
				},
				self.span_from(start),
			));
			if self.eat(&Token::Comma).is_none() {
				break;
			}
		}
		self.expect(&Token::Gt);
		params
	}
}
