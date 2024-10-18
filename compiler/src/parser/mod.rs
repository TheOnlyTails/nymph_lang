pub(crate) mod error;

use crate::{
	ast::{
		Ident, Spanned,
		declaration::{
			Declaration, EnumVariant, FuncDeclaration, FuncParam, ImplMember, ImportRoot,
			InterfaceElement, InterfaceMember, LetDeclaration, Module, StructField, StructInnerMember,
			TypeAliasDeclaration, Visibility,
		},
		expr::{
			CallArg, ClosureParam, Expr, ListItem, ListPatternEntry, MapEntry, MapPatternEntry, MatchArm,
			Pattern, RangeKind, RangePatternKind, Statement, StringPart, StringPatternPart,
			StructPatternField,
		},
		ops::{
			AssignOperator, BinaryOperator, PatternOperator, PostfixOperator, Precedence, PrefixOperator,
			TypeOperator,
		},
		types::{GenericArg, GenericParam, Type},
	},
	lexer::token::Token,
};
use chumsky::{
	input::{BorrowInput, ValueInput},
	pratt::*,
	prelude::*,
};
use ordered_float::OrderedFloat;

pub(crate) trait TokenInput<'src> =
	BorrowInput<'src, Token = Token, Span = SimpleSpan> + ValueInput<'src>;
pub(crate) trait NymphParser<'src, I: TokenInput<'src>, O> =
	Parser<'src, I, O, extra::Err<Rich<'src, Token>>> + Clone + 'src;

pub(crate) fn parser<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Spanned<Module>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	declaration(make_input)
		.repeated()
		.collect()
		.map_with(|members, e| Spanned(Module { members }, e.span()))
}

fn declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Declaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	choice((
		// import
		import(),
		// let
		visibility()
			.or_not()
			.then(let_declaration(make_input))
			.then_ignore(just(Token::Eq))
			.then(expression(make_input))
			.map(|((visibility, meta), value)| Declaration::Let {
				visibility,
				meta,
				value,
			}),
		// external let
		visibility()
			.or_not()
			.then_ignore(just(Token::External))
			.then(let_declaration(make_input))
			.map(|(visibility, meta)| Declaration::ExternalLet(visibility, meta)),
		// func
		visibility()
			.or_not()
			.then(func_declaration(make_input))
			.then_ignore(just(Token::Arrow))
			.then(expression(make_input))
			.map(|((visibility, meta), body)| Declaration::Func {
				visibility,
				meta,
				body,
			}),
		// external func
		visibility()
			.or_not()
			.then_ignore(just(Token::External))
			.then(func_declaration(make_input))
			.map(|(visibility, meta)| Declaration::ExternalFunc(visibility, meta)),
		// type alias
		visibility()
			.or_not()
			.then(type_alias_declaration(make_input))
			.then_ignore(just(Token::Eq))
			.then(type_def(make_input))
			.map(|((visibility, meta), value)| Declaration::TypeAlias {
				visibility,
				meta,
				value,
			}),
		struct_declaration(make_input),
		enum_declaration(make_input),
		namespace_declaration(make_input),
		interface_declaration(make_input),
		impl_declaration(make_input),
		impl_for_declaration(make_input),
	))
	.labelled("a declaration")
	.boxed()
}

fn import<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, Declaration> {
	let import_ident = identifier()
		.then(just(Token::As).ignore_then(identifier()).or_not())
		.labelled("an import identifier")
		.boxed();

	just(Token::Import)
		.ignore_then(
			choice((
				just(Token::DotDot).to(ImportRoot::Parent),
				just(Token::Dot).to(ImportRoot::Current),
			))
			.then_ignore(just(Token::Slash).or_not())
			.or_not(),
		)
		.then(
			identifier()
				.separated_by(just(Token::Slash))
				.allow_trailing()
				.collect(),
		)
		.then(
			just(Token::With)
				.ignore_then(
					import_ident
						.separated_by(just(Token::Comma))
						.allow_trailing()
						.collect()
						.delimited_by(just(Token::LParen), just(Token::RParen)),
				)
				.or_not(),
		)
		.map(|((root, path), idents)| Declaration::Import {
			root: root.unwrap_or(ImportRoot::PackageRoot),
			path,
			idents,
		})
		.labelled("an import declaration")
		.boxed()
}

fn let_declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, LetDeclaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	just(Token::Let)
		.ignore_then(just(Token::Mut).or_not())
		.then(pattern(make_input))
		.then(
			just(Token::Colon)
				.ignore_then(type_def(make_input))
				.or_not(),
		)
		.map(|((mutable, name), type_)| LetDeclaration {
			mutable: mutable.is_some(),
			name,
			type_,
		})
		.labelled("a let declaration")
		.boxed()
}

fn func_declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, FuncDeclaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	let func_param = just(Token::DotDotDot)
		.or_not()
		.then(just(Token::Mut).or_not())
		.then(pattern(make_input))
		.then_ignore(just(Token::Colon))
		.then(type_def(make_input))
		.then(just(Token::Eq).ignore_then(expression(make_input)).or_not())
		.map_with(|((((spread, mutable), name), type_), default), e| {
			Spanned(
				FuncParam {
					spread: spread.is_some(),
					mutable: mutable.is_some(),
					name,
					type_,
					default,
				},
				e.span(),
			)
		})
		.labelled("a function parameter")
		.boxed();

	just(Token::Func)
		.ignore_then(identifier())
		.then(generic_params(make_input).or_not())
		.then(
			func_param
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.collect()
				.delimited_by(just(Token::LParen), just(Token::RParen)),
		)
		.then(
			just(Token::Colon)
				.ignore_then(type_def(make_input))
				.or_not(),
		)
		.map(
			|(((name, generics), params), return_type)| FuncDeclaration {
				name,
				generics: generics.unwrap_or(vec![]),
				params,
				return_type,
			},
		)
		.labelled("a let declaration")
		.boxed()
}

fn type_alias_declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, TypeAliasDeclaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	just(Token::Type)
		.ignore_then(identifier())
		.then(generic_params(make_input).or_not())
		.map(|(name, generics)| TypeAliasDeclaration {
			name,
			generics: generics.unwrap_or(vec![]),
		})
		.labelled("a type alias declaration")
		.boxed()
}

pub(crate) fn struct_declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Declaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	visibility()
		.or_not()
		.then_ignore(just(Token::Struct))
		.then(identifier())
		.then(generic_params(make_input).or_not())
		.then(
			struct_field(make_input)
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.at_least(1)
				.collect()
				.delimited_by(just(Token::LParen), just(Token::RParen))
				.or_not(),
		)
		.then(
			struct_member(make_input)
				.repeated()
				.at_least(1)
				.collect()
				.delimited_by(just(Token::LBrace), just(Token::RBrace))
				.or_not(),
		)
		.map(
			|((((visibility, name), generics), fields), members)| Declaration::Struct {
				visibility,
				name,
				generics: generics.unwrap_or(vec![]),
				fields: fields.unwrap_or(vec![]),
				members: members.unwrap_or(vec![]),
			},
		)
}

pub(crate) fn struct_field<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Spanned<StructField>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	visibility()
		.or_not()
		.then(identifier())
		.then_ignore(just(Token::Colon))
		.then(type_def(make_input))
		.then(just(Token::Eq).ignore_then(expression(make_input)).or_not())
		.map_with(|(((visibility, name), type_), default), e| {
			Spanned(
				StructField {
					visibility,
					name,
					type_,
					default,
				},
				e.span(),
			)
		})
		.labelled("a struct field")
		.boxed()
}

pub(crate) fn struct_member<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Spanned<StructInnerMember>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	choice((
		impl_member(make_input).map(StructInnerMember::Member),
		just(Token::Namespace)
			.ignore_then(
				impl_member(make_input)
					.repeated()
					.collect()
					.delimited_by(just(Token::LBrace), just(Token::RBrace)),
			)
			.map(StructInnerMember::Namespace),
		just(Token::Impl)
			.ignore_then(just(Token::Mut))
			.ignore_then(
				impl_member(make_input)
					.repeated()
					.collect()
					.delimited_by(just(Token::LBrace), just(Token::RBrace)),
			)
			.map(StructInnerMember::ImplMut),
		// impl A<B>
		just(Token::Impl)
			.ignore_then(generic_params(make_input).or_not())
			.then(identifier())
			.then(generic_args(type_def(make_input)).or_not())
			.then(
				impl_member(make_input)
					.repeated()
					.collect()
					.delimited_by(just(Token::LBrace), just(Token::RBrace)),
			)
			.map(
				|(((generics, interface), interface_generics), members)| StructInnerMember::Impl {
					interface: (interface, interface_generics.unwrap_or(vec![])),
					generics: generics.unwrap_or(vec![]),
					members,
				},
			),
	))
	.map_with(Spanned::new)
	.boxed()
}

pub(crate) fn enum_declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Declaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	let enum_variant = identifier()
		.then(
			struct_field(make_input)
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.at_least(1)
				.collect()
				.delimited_by(just(Token::LParen), just(Token::RParen))
				.or_not(),
		)
		.map_with(|(name, fields), e| {
			Spanned(
				EnumVariant {
					name,
					fields: fields.unwrap_or(vec![]),
				},
				e.span(),
			)
		});

	visibility()
		.or_not()
		.then_ignore(just(Token::Enum))
		.then(identifier())
		.then(generic_params(make_input).or_not())
		.then(
			enum_variant
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.at_least(1)
				.collect()
				.then(struct_member(make_input).repeated().collect::<Vec<_>>())
				.delimited_by(just(Token::LBrace), just(Token::RBrace)),
		)
		.map(
			|(((visibility, name), generics), (variants, members))| Declaration::Enum {
				visibility,
				name,
				generics: generics.unwrap_or(vec![]),
				variants,
				members,
			},
		)
}

pub(crate) fn namespace_declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Declaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	visibility()
		.or_not()
		.then_ignore(just(Token::Namespace))
		.then(identifier())
		.then(
			impl_member(make_input)
				.repeated()
				.collect()
				.delimited_by(just(Token::LBrace), just(Token::RBrace)),
		)
		.map(|((visibility, name), members)| Declaration::Namespace {
			visibility,
			name,
			members,
		})
}

pub(crate) fn impl_declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Declaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	visibility()
		.or_not()
		.then_ignore(just(Token::Impl))
		.then(generic_params(make_input).or_not())
		.then(just(Token::Mut).or_not())
		.then(type_def(make_input))
		.then(
			impl_member(make_input)
				.repeated()
				.collect()
				.delimited_by(just(Token::LBrace), just(Token::RBrace)),
		)
		.map(
			|((((visibility, generics), mutable), type_), members)| Declaration::Impl {
				visibility,
				generics: generics.unwrap_or(vec![]),
				mutable: mutable.is_some(),
				type_,
				members,
			},
		)
}

pub(crate) fn impl_for_declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Declaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	visibility()
		.or_not()
		.then_ignore(just(Token::Impl))
		.then(generic_params(make_input).or_not())
		.then(just(Token::Mut).or_not())
		.then(identifier().then(generic_args(type_def(make_input)).or_not()))
		.then_ignore(just(Token::For))
		.then(type_def(make_input))
		.then(
			impl_member(make_input)
				.repeated()
				.collect()
				.delimited_by(just(Token::LBrace), just(Token::RBrace)),
		)
		.map(
			|(((((visibility, generics), mutable), for_interface), type_), members)| {
				Declaration::ImplFor {
					visibility,
					generics: generics.unwrap_or(vec![]),
					mutable: mutable.is_some(),
					members,
					type_,
					for_interface: (for_interface.0, for_interface.1.unwrap_or(vec![])),
				}
			},
		)
}

pub(crate) fn impl_member<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Spanned<ImplMember>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	choice((
		// let
		visibility()
			.or_not()
			.then(let_declaration(make_input))
			.then_ignore(just(Token::Eq))
			.then(expression(make_input))
			.map(|((visibility, meta), value)| ImplMember::Let {
				visibility,
				meta,
				value,
			}),
		// external let
		visibility()
			.or_not()
			.then_ignore(just(Token::External))
			.then(let_declaration(make_input))
			.map(|(visibility, meta)| ImplMember::ExternalLet(visibility, meta)),
		// func
		visibility()
			.or_not()
			.then(func_declaration(make_input))
			.then_ignore(just(Token::Arrow))
			.then(expression(make_input))
			.map(|((visibility, meta), body)| ImplMember::Func {
				visibility,
				meta,
				body,
			}),
		// external func
		visibility()
			.or_not()
			.then_ignore(just(Token::External))
			.then(func_declaration(make_input))
			.map(|(visibility, meta)| ImplMember::ExternalFunc(visibility, meta)),
	))
	.map_with(Spanned::new)
	.boxed()
}

pub(crate) fn interface_declaration<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Declaration>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	visibility()
		.or_not()
		.then_ignore(just(Token::Interface))
		.then(just(Token::Mut).or_not())
		.then(identifier())
		.then(generic_params(make_input).or_not())
		.then(
			just(Token::Colon)
				.ignore_then(
					identifier()
						.then(generic_args(type_def(make_input)).or_not())
						.map_with(|(name, generics), e| Spanned((name, generics.unwrap_or(vec![])), e.span()))
						.separated_by(just(Token::Plus))
						.at_least(1)
						.collect(),
				)
				.or_not(),
		)
		.then(
			interface_member(make_input)
				.repeated()
				.collect()
				.delimited_by(just(Token::LBrace), just(Token::RBrace)),
		)
		.map(
			|(((((visibility, mutable), name), generics), super_interfaces), members)| {
				Declaration::Interface {
					visibility,
					mutable: mutable.is_some(),
					name,
					generics: generics.unwrap_or(vec![]),
					super_interfaces: super_interfaces.unwrap_or(vec![]),
					members,
				}
			},
		)
}

pub(crate) fn interface_member<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Spanned<InterfaceMember>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	choice((
		interface_element(make_input).map(InterfaceMember::Element),
		just(Token::Namespace)
			.ignore_then(
				impl_member(make_input)
					.repeated()
					.collect()
					.delimited_by(just(Token::LBrace), just(Token::RBrace)),
			)
			.map(InterfaceMember::Namespace),
		just(Token::Impl)
			.ignore_then(just(Token::Mut))
			.ignore_then(
				interface_element(make_input)
					.repeated()
					.collect()
					.delimited_by(just(Token::LBrace), just(Token::RBrace)),
			)
			.map(InterfaceMember::ImplMut),
		// impl A<B>
		just(Token::Impl)
			.ignore_then(generic_params(make_input).or_not())
			.then(identifier())
			.then(generic_args(type_def(make_input)).or_not())
			.then(
				impl_member(make_input)
					.repeated()
					.collect()
					.delimited_by(just(Token::LBrace), just(Token::RBrace)),
			)
			.map(
				|(((generics, interface_name), interface_generics), members)| InterfaceMember::Impl {
					interface: (interface_name, interface_generics.unwrap_or(vec![])),
					generics: generics.unwrap_or(vec![]),
					members,
				},
			),
	))
	.map_with(Spanned::new)
	.boxed()
}

pub(crate) fn interface_element<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Spanned<InterfaceElement>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	choice((
		// let
		let_declaration(make_input)
			.then(just(Token::Eq).ignore_then(expression(make_input)).or_not())
			.map(|(meta, value)| InterfaceElement::Let { meta, value }),
		// func
		func_declaration(make_input)
			.then(
				just(Token::Arrow)
					.ignore_then(expression(make_input))
					.or_not(),
			)
			.map(|(meta, body)| InterfaceElement::Func { meta, body }),
	))
	.map_with(Spanned::new)
	.boxed()
}

pub(crate) fn expression<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Spanned<Expr>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	recursive(|expression| {
		let expr_atom = choice((
			block(expression.clone(), make_input),
			// literals
			int().map(Expr::Int),
			float().map(Expr::Float),
			char().map(Expr::Char),
			string(expression.clone(), make_input).map(Expr::String),
			boolean().map(Expr::Boolean),
			identifier().map(Expr::Identifier),
			list_literal(expression.clone()).map(Expr::List),
			tuple_literal(expression.clone()).map(Expr::Tuple),
			map_literal(expression.clone()).map(Expr::Map),
			just(Token::Underscore).to(Expr::Placeholder),
			just(Token::This).to(Expr::This),
			closure(expression.clone(), make_input),
			// control flow
			if_expr(expression.clone()),
			while_expr(expression.clone()),
			for_expr(expression.clone(), make_input),
			match_expr(expression.clone(), make_input),
			just(Token::Return)
				.ignore_then(just(Token::AtSign).ignore_then(identifier()).or_not())
				.then(expression.clone().or_not())
				.map(|(label, value)| Expr::Return {
					label,
					value: value.map(Box::new),
				}),
			just(Token::Break)
				.ignore_then(just(Token::AtSign).ignore_then(identifier()).or_not())
				.then(expression.clone().or_not())
				.map(|(label, value)| Expr::Break {
					label,
					value: value.map(Box::new),
				}),
			just(Token::Continue)
				.ignore_then(just(Token::AtSign).ignore_then(identifier()).or_not())
				.map(|label| Expr::Continue { label }),
			// Grouped expr
			expression
				.clone()
				.delimited_by(just(Token::LParen), just(Token::RParen))
				.map(|ex| Expr::Grouped(ex.into())),
		))
		.map_with(Spanned::new);

		expr_atom.pratt(vec![
			postfix(
				Precedence::FuncCall as u16,
				generic_args(type_def(make_input)).or_not().then(
					call_arg(expression.clone())
						.separated_by(just(Token::Comma))
						.allow_trailing()
						.collect()
						.delimited_by(just(Token::LParen), just(Token::RParen)),
				),
				|func: Spanned<_>, (generics, args): (Option<_>, _), e| {
					Spanned(
						Expr::Call {
							func: func.into(),
							generics: generics.unwrap_or(vec![]),
							args,
						},
						e.span(),
					)
				},
			)
			.boxed(),
			postfix(
				Precedence::MemberAccess as u16,
				just(Token::Dot).ignore_then(identifier()),
				|parent: Spanned<_>, member, e| {
					Spanned(
						Expr::MemberAccess {
							parent: parent.into(),
							member,
							optional: false,
						},
						e.span(),
					)
				},
			)
			.boxed(),
			postfix(
				Precedence::MemberAccess as u16,
				just(Token::QuestionDot).ignore_then(identifier()),
				|parent: Spanned<_>, member, e| {
					Spanned(
						Expr::MemberAccess {
							parent: parent.into(),
							member,
							optional: true,
						},
						e.span(),
					)
				},
			)
			.boxed(),
			postfix(
				Precedence::IndexAccess as u16,
				expression
					.clone()
					.delimited_by(just(Token::LBracket), just(Token::RBracket)),
				|parent: Spanned<_>, index: Spanned<_>, e| {
					Spanned(
						Expr::IndexAccess {
							parent: parent.into(),
							index: index.into(),
							optional: false,
						},
						e.span(),
					)
				},
			)
			.boxed(),
			postfix(
				Precedence::IndexAccess as u16,
				expression.clone().delimited_by(
					just(Token::QuestionDot).then(just(Token::LBracket)),
					just(Token::RBracket),
				),
				|parent: Spanned<_>, index: Spanned<_>, e| {
					Spanned(
						Expr::IndexAccess {
							parent: parent.into(),
							index: index.into(),
							optional: true,
						},
						e.span(),
					)
				},
			)
			.boxed(),
			prefix(
				Precedence::Unary as u16,
				just(Token::Minus),
				|_, value: Spanned<_>, e| {
					Spanned(
						Expr::PrefixOp {
							op: PrefixOperator::Negate,
							value: value.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			prefix(
				Precedence::Unary as u16,
				just(Token::ExclamationMark),
				|_, value: Spanned<_>, e| {
					Spanned(
						Expr::PrefixOp {
							op: PrefixOperator::BoolNot,
							value: value.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			prefix(
				Precedence::Unary as u16,
				just(Token::Tilde),
				|_, value: Spanned<_>, e| {
					Spanned(
						Expr::PrefixOp {
							op: PrefixOperator::BitNot,
							value: value.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			postfix(
				Precedence::Unary as u16,
				just(Token::QuestionMark),
				|value: Spanned<_>, _, e| {
					Spanned(
						Expr::PostfixOp {
							op: PostfixOperator::ErrorReturn,
							value: value.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			postfix(
				Precedence::Is as u16,
				just(Token::Is).ignore_then(pattern(make_input)),
				|lhs: Spanned<_>, rhs, e| {
					Spanned(
						Expr::PatternOp {
							lhs: lhs.into(),
							op: PatternOperator::Is,
							rhs,
						},
						e.span(),
					)
				},
			)
			.boxed(),
			postfix(
				Precedence::Is as u16,
				just(Token::NotIs).ignore_then(pattern(make_input)),
				|lhs: Spanned<_>, rhs, e| {
					Spanned(
						Expr::PatternOp {
							lhs: lhs.into(),
							op: PatternOperator::NotIs,
							rhs,
						},
						e.span(),
					)
				},
			)
			.boxed(),
			postfix(
				Precedence::As as u16,
				just(Token::As).ignore_then(type_def(make_input)),
				|lhs: Spanned<_>, rhs, e| {
					Spanned(
						Expr::TypeOp {
							lhs: lhs.into(),
							op: TypeOperator::As,
							rhs,
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Power as u16),
				just(Token::StarStar),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::Power,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Multiplication as u16),
				just(Token::Star),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::Times,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Multiplication as u16),
				just(Token::Slash),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::Divide,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Multiplication as u16),
				just(Token::Percent),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::Remainder,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Addition as u16),
				just(Token::Plus),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::Plus,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Addition as u16),
				just(Token::Minus),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::Minus,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Range as u16),
				just(Token::DotDotEq),
				|min: Spanned<_>, _, max, e| {
					Spanned(
						Expr::Range(RangeKind::Inclusive {
							min: min.into(),
							max: max.into(),
						}),
						e.span(),
					)
				},
			)
			.boxed(),
			prefix(
				Precedence::Range as u16,
				just(Token::DotDot),
				|_, max: Spanned<_>, e| Spanned(Expr::Range(RangeKind::To(max.into())), e.span()),
			)
			.boxed(),
			prefix(
				Precedence::Range as u16,
				just(Token::DotDotEq),
				|_, max: Spanned<_>, e| Spanned(Expr::Range(RangeKind::ToInclusive(max.into())), e.span()),
			)
			.boxed(),
			postfix(
				Precedence::Range as u16,
				just(Token::DotDot).ignore_then(expression.clone().or_not()),
				|min: Spanned<_>, max: Option<Spanned<_>>, e| {
					Spanned(
						Expr::Range(match max {
							Some(max) => RangeKind::Exclusive {
								min: min.into(),
								max: max.into(),
							},
							None => RangeKind::From(min.into()),
						}),
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::BitShift as u16),
				// make sure there's no whitespace between the tokens
				just(Token::Lt)
					.then(just(Token::Lt))
					.map_with(Spanned::new)
					.filter(|Spanned(_, span)| span.end() - span.start() == 2),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::LeftShift,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::BitShift as u16),
				// make sure there's no whitespace between the tokens
				just(Token::Gt)
					.then(just(Token::Gt))
					.map_with(Spanned::new)
					.filter(|Spanned(_, span)| span.end() - span.start() == 2),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::RightShift,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::BitAnd as u16),
				just(Token::And),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::BitAnd,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::BitXor as u16),
				just(Token::Caret),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::BitXor,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::BitOr as u16),
				just(Token::Pipe),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::BitOr,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Unwrap as u16),
				just(Token::DoubleQuestion),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::Unwrap,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::In as u16),
				just(Token::In),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::In,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::In as u16),
				just(Token::NotIn),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::NotIn,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Comparison as u16),
				just(Token::Lt),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::LessThan,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Comparison as u16),
				just(Token::LtEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::LessThanEquals,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Comparison as u16),
				just(Token::Gt),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::GreaterThan,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Comparison as u16),
				just(Token::GtEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::GreaterThanEquals,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Equality as u16),
				just(Token::EqEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::Equals,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::Equality as u16),
				just(Token::NotEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::NotEquals,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::BoolAnd as u16),
				just(Token::AndAnd),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::BoolAnd,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				left(Precedence::BoolOr as u16),
				just(Token::PipePipe),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::BoolOr,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Pipeline as u16),
				just(Token::Triangle),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::BinaryOp {
							lhs: lhs.into(),
							op: BinaryOperator::Pipe,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::Eq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::Assign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::PlusEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::PlusAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::MinusEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::MinusAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::StarEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::TimesAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::SlashEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::DivideAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::PercentEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::RemainderAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::StarStarEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::PowerAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::LtLtEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::LeftShiftAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::GtGtEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::RightShiftAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::AndEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::BitAndAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::CaretEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::BitXorAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::PipeEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::BitOrAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::TildeEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::BitNotAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::AndAndEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::BoolAndAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
			infix(
				right(Precedence::Assignment as u16),
				just(Token::PipePipeEq),
				|lhs: Spanned<_>, _, rhs, e| {
					Spanned(
						Expr::AssignOp {
							lhs: lhs.into(),
							op: AssignOperator::BoolOrAssign,
							rhs: rhs.into(),
						},
						e.span(),
					)
				},
			)
			.boxed(),
		])
	})
	.labelled("an expression")
	.boxed()
}

fn call_arg<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>>(
	expression: E,
) -> impl NymphParser<'src, I, Spanned<CallArg>> {
	choice((
		identifier()
			.then_ignore(just(Token::Eq))
			.then(just(Token::DotDotDot).or_not())
			.then(expression.clone())
			.map(|((name, spread), value)| CallArg {
				name: Some(name),
				spread: spread.is_some(),
				value,
			}),
		just(Token::DotDotDot)
			.or_not()
			.then(expression)
			.map(|(spread, value)| CallArg {
				name: None,
				spread: spread.is_some(),
				value,
			}),
	))
	.map_with(Spanned::new)
	.labelled("a function call argument")
	.boxed()
}

fn list_literal<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>>(
	expression: E,
) -> impl NymphParser<'src, I, Vec<Spanned<ListItem>>> {
	just(Token::DotDotDot)
		.or_not()
		.then(expression)
		.map_with(|(spread, value), e| {
			Spanned(
				if spread.is_some() {
					ListItem::Spread(value)
				} else {
					ListItem::Expr(value)
				},
				e.span(),
			)
		})
		.separated_by(just(Token::Comma))
		.allow_trailing()
		.collect()
		.delimited_by(just(Token::ListStart), just(Token::RBracket))
		.labelled("a list literal")
		.boxed()
}

fn block<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>, M>(
	expression: E,
	make_input: M,
) -> impl NymphParser<'src, I, Expr>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	let statement = choice((
		let_declaration(make_input)
			.then_ignore(just(Token::Eq))
			.then(expression.clone())
			.map_with(|(meta, value), e| Spanned(Statement::Let { meta, value }, e.span())),
		expression.map_with(|expr, e| Spanned(Statement::Expr(expr), e.span())),
	));

	identifier()
		.then_ignore(just(Token::AtSign))
		.or_not()
		.then(
			statement
				.repeated()
				.collect()
				.delimited_by(just(Token::LBrace), just(Token::RBrace)),
		)
		.map(|(label, body)| Expr::Block { body, label })
		.labelled("a block expression")
		.boxed()
}

fn closure<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>, M>(
	expression: E,
	make_input: M,
) -> impl NymphParser<'src, I, Expr>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	let closure_param = just(Token::DotDotDot)
		.or_not()
		.then(just(Token::Mut).or_not())
		.then(pattern(make_input))
		.then(
			just(Token::Colon)
				.ignore_then(type_def(make_input))
				.or_not(),
		)
		.map_with(|(((spread, mutable), name), type_), e| {
			Spanned(
				ClosureParam {
					spread: spread.is_some(),
					mutable: mutable.is_some(),
					name,
					type_,
				},
				e.span(),
			)
		})
		.labelled("a closure parameter")
		.boxed();

	generic_params(make_input)
		.or_not()
		.then(
			closure_param
				.separated_by(just(Token::Comma))
				.collect()
				.delimited_by(just(Token::LParen), just(Token::RParen)),
		)
		.then(
			just(Token::Colon)
				.ignore_then(type_def(make_input))
				.or_not(),
		)
		.then_ignore(just(Token::Arrow))
		.then(expression)
		.map(|(((generics, params), return_type), body)| Expr::Closure {
			generics: generics.unwrap_or(vec![]),
			params,
			return_type,
			body: body.into(),
		})
		.labelled("a closure")
		.boxed()
}

fn tuple_literal<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>>(
	expression: E,
) -> impl NymphParser<'src, I, Vec<Spanned<ListItem>>> {
	just(Token::DotDotDot)
		.or_not()
		.then(expression)
		.map_with(|(spread, value), e| {
			Spanned(
				if spread.is_some() {
					ListItem::Spread(value)
				} else {
					ListItem::Expr(value)
				},
				e.span(),
			)
		})
		.separated_by(just(Token::Comma))
		.allow_trailing()
		.collect()
		.delimited_by(just(Token::TupleStart), just(Token::RParen))
		.labelled("a tuple literal")
		.boxed()
}

fn map_literal<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>>(
	expression: E,
) -> impl NymphParser<'src, I, Vec<Spanned<MapEntry>>> {
	choice((
		just(Token::DotDotDot)
			.ignore_then(expression.clone())
			.map_with(|val, e| Spanned(MapEntry::Spread(val), e.span())),
		expression
			.clone()
			.then_ignore(just(Token::Colon))
			.then(expression)
			.map_with(|(key, value), e| Spanned(MapEntry::Expr(key, value), e.span())),
	))
	.separated_by(just(Token::Comma))
	.allow_trailing()
	.collect()
	.delimited_by(just(Token::MapStart), just(Token::RBrace))
	.labelled("a map literal")
	.boxed()
}

fn if_expr<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>>(
	expression: E,
) -> impl NymphParser<'src, I, Expr> {
	just(Token::If)
		.ignore_then(
			expression
				.clone()
				.delimited_by(just(Token::LParen), just(Token::RParen)),
		)
		.then(expression.clone())
		.then(just(Token::Else).ignore_then(expression).or_not())
		.map(|((condition, then), otherwise)| Expr::If {
			condition: condition.into(),
			then: then.into(),
			otherwise: otherwise.map(Box::new),
		})
		.labelled("an if expression")
		.boxed()
}

fn while_expr<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>>(
	expression: E,
) -> impl NymphParser<'src, I, Expr> {
	just(Token::While)
		.ignore_then(just(Token::AtSign).ignore_then(identifier()).or_not())
		.then(
			expression
				.clone()
				.delimited_by(just(Token::LParen), just(Token::RParen)),
		)
		.then(expression.clone())
		.map(|((label, condition), body)| Expr::While {
			label,
			condition: condition.into(),
			body: body.into(),
		})
		.labelled("a while loop")
		.boxed()
}

fn for_expr<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>, M>(
	expression: E,
	make_input: M,
) -> impl NymphParser<'src, I, Expr>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	just(Token::For)
		.ignore_then(just(Token::AtSign).ignore_then(identifier()).or_not())
		.then(
			pattern(make_input)
				.then_ignore(just(Token::In))
				.then(expression.clone())
				.delimited_by(just(Token::LParen), just(Token::RParen)),
		)
		.then(expression.clone())
		.map(|((label, (variable, iterable)), body)| Expr::For {
			label,
			variable,
			iterable: iterable.into(),
			body: body.into(),
		})
		.labelled("a for loop")
		.boxed()
}

fn match_expr<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>, M>(
	expression: E,
	make_input: M,
) -> impl NymphParser<'src, I, Expr>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	let match_arm = pattern(make_input)
		.then(just(Token::If).ignore_then(expression.clone()).or_not())
		.then_ignore(just(Token::Arrow))
		.then(expression.clone())
		.map(|((pattern, guard), body)| MatchArm {
			pattern,
			guard,
			body,
		});

	just(Token::Match)
		.ignore_then(
			expression
				.clone()
				.delimited_by(just(Token::LParen), just(Token::RParen)),
		)
		.then(
			match_arm
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.collect()
				.delimited_by(just(Token::LBrace), just(Token::RBrace)),
		)
		.map(|(value, arms)| Expr::Match {
			value: value.into(),
			arms,
		})
		.labelled("a match expression")
		.boxed()
}

fn string<'src, I: TokenInput<'src>, E: NymphParser<'src, I, Spanned<Expr>>, M>(
	expression: E,
	make_input: M,
) -> impl NymphParser<'src, I, Vec<Spanned<StringPart>>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	let string_part = select! {
		Token::StringChar(c) = e => Spanned(StringPart::Char(c), e.span()),
		Token::StringEscape(c) = e => Spanned(StringPart::EscapeSequence(c), e.span()),
	}
	.or(
		expression
			.nested_in(select_ref! {
				Token::StringInterpolation(toks) = e => make_input(e.span(), toks),
			})
			.map(|Spanned(expr, s)| Spanned(StringPart::InterpolatedExpr(Spanned(expr, s)), s)),
	)
	.boxed();

	string_part
		.repeated()
		.collect()
		.nested_in(select_ref! {
			Token::String(toks) = e => make_input(e.span(), toks)
		})
		.labelled("a string literal")
		.boxed()
}

fn identifier<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, Ident> {
	select! {
		Token::Identifier(ident) = e => Spanned(ident, e.span())
	}
	.labelled("an identifier")
	.boxed()
}

fn int<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, Spanned<u64>> {
	select! {
		Token::DecimalInt(val) = e => Spanned(val, e.span()),
		Token::HexInt(val) = e => Spanned(val, e.span()),
		Token::BinaryInt(val) = e => Spanned(val, e.span()),
		Token::OctalInt(val) = e => Spanned(val, e.span()),
	}
	.labelled("an integer literal")
}

fn float<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, Spanned<OrderedFloat<f64>>> {
	select! {
		Token::Float(val) = e => Spanned(val, e.span()),
	}
	.labelled("a float literal")
}

fn char<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, Spanned<char>> {
	select! {
		Token::Char(val) = e => Spanned(val, e.span()),
		Token::CharEscape(val) = e => Spanned(val.into(), e.span()),
	}
	.labelled("a character literal")
}

fn boolean<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, Spanned<bool>> {
	select! {
		Token::True = e => Spanned(true, e.span()),
		Token::False = e => Spanned(false, e.span()),
	}
	.labelled("a boolean literal")
}

fn type_def<'src, I: TokenInput<'src>, M>(make_input: M) -> impl NymphParser<'src, I, Spanned<Type>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	recursive(|type_def| {
		let type_atom = choice((
			// Builtin types
			just(Token::IntType).to(Type::Int),
			just(Token::FloatType).to(Type::Float),
			just(Token::CharType).to(Type::Char),
			just(Token::StringType).to(Type::String),
			just(Token::BooleanType).to(Type::Boolean),
			just(Token::VoidType).to(Type::Void),
			just(Token::NeverType).to(Type::Never),
			just(Token::SelfType).to(Type::Self_),
			just(Token::Underscore).to(Type::Infer),
			// Data structures
			type_def
				.clone()
				.delimited_by(just(Token::ListStart), just(Token::RBracket))
				.map(|t: Spanned<Type>| Type::List(t.into())),
			type_def
				.clone()
				.then(just(Token::Colon))
				.then(type_def.clone())
				.delimited_by(just(Token::MapStart), just(Token::RBrace))
				.map(|((key, _), value)| Type::Map(key.into(), value.into())),
			type_def
				.clone()
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.collect()
				.delimited_by(just(Token::TupleStart), just(Token::RParen))
				.map(|elements| Type::Tuple(elements)),
			type_def
				.clone()
				.delimited_by(just(Token::LParen), just(Token::RParen))
				.map(|t| Type::Grouped(t.into())),
			identifier()
				.then(generic_args(type_def.clone()).or_not())
				.map(|(name, generics)| Type::Reference {
					name,
					generics: generics.unwrap_or(vec![]),
				}),
		))
		.map_with(Spanned::new);

		type_atom.pratt((
			postfix(
				2,
				just(Token::Is).ignore_then(pattern(make_input)),
				|type_: Spanned<_>, pattern, e| Spanned(Type::Pattern(type_.into(), pattern), e.span()),
			),
			postfix(
				2,
				just(Token::NotIs).ignore_then(pattern(make_input)),
				|type_: Spanned<_>, pattern, e| Spanned(Type::NotPattern(type_.into(), pattern), e.span()),
			),
			infix(left(1), just(Token::Plus), |lhs: Spanned<_>, _, rhs, e| {
				Spanned(Type::Intersection(lhs.into(), rhs.into()), e.span())
			}),
			prefix(
				0,
				type_def
					.separated_by(just(Token::Comma))
					.allow_trailing()
					.collect()
					.delimited_by(just(Token::LParen), just(Token::RParen))
					.then_ignore(just(Token::Arrow)),
				|params, return_type: Spanned<_>, e| {
					Spanned(
						Type::Function {
							params,
							return_type: return_type.into(),
						},
						e.span(),
					)
				},
			),
		))
	})
	.labelled("a type definition")
	.boxed()
}

fn generic_args<'src, I: TokenInput<'src>, T: NymphParser<'src, I, Spanned<Type>>>(
	type_def: T,
) -> impl NymphParser<'src, I, Vec<Spanned<GenericArg>>> {
	generic_arg(type_def)
		.separated_by(just(Token::Comma))
		.at_least(1)
		.allow_trailing()
		.collect()
		.delimited_by(just(Token::Lt), just(Token::Gt))
		.labelled("a list of generic type arguments")
		.boxed()
}

fn generic_arg<'src, I: TokenInput<'src>, T: NymphParser<'src, I, Spanned<Type>>>(
	type_def: T,
) -> impl NymphParser<'src, I, Spanned<GenericArg>> {
	identifier()
		.then_ignore(just(Token::Eq))
		.or_not()
		.then(type_def.clone())
		.map_with(|(name, value), e| Spanned(GenericArg { name, value }, e.span()))
		.labelled("a generic type argument")
		.boxed()
}

fn generic_params<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Vec<Spanned<GenericParam>>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	generic_param(make_input)
		.separated_by(just(Token::Comma))
		.at_least(1)
		.collect()
		.delimited_by(just(Token::Lt), just(Token::Gt))
		.labelled("a list of generic type parameters")
		.boxed()
}

fn generic_param<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Spanned<GenericParam>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	identifier()
		.then(
			just(Token::Colon)
				.ignore_then(type_def(make_input))
				.or_not(),
		)
		.then(just(Token::Eq).ignore_then(type_def(make_input)).or_not())
		.map_with(|((name, constraint), default), e| {
			Spanned(
				GenericParam {
					name,
					constraint,
					default,
				},
				e.span(),
			)
		})
		.labelled("a generic type parameter")
		.boxed()
}

fn pattern<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Spanned<Pattern>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	recursive(|pattern| {
		let pattern_atom = choice((
			range_pattern().map(Pattern::Range),
			signed_int().map(Pattern::Int),
			signed_float().map(Pattern::Float),
			char().map(Pattern::Char),
			string_pattern(make_input).map(Pattern::String),
			boolean().map(Pattern::Boolean),
			just(Token::Underscore).to(Pattern::Placeholder),
			// data structures
			list_pattern_entry(pattern.clone())
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.collect()
				.delimited_by(just(Token::ListStart), just(Token::RBracket))
				.map(Pattern::List),
			list_pattern_entry(pattern.clone())
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.collect()
				.delimited_by(just(Token::TupleStart), just(Token::RParen))
				.map(Pattern::Tuple),
			map_pattern_entry(pattern.clone())
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.collect()
				.delimited_by(just(Token::MapStart), just(Token::RBrace))
				.map(Pattern::Map),
			struct_pattern(pattern.clone()),
			pattern
				.clone()
				.delimited_by(just(Token::LParen), just(Token::RParen))
				.map(|p: Spanned<_>| Pattern::Grouped(p.into())),
		))
		.map_with(Spanned::new);

		pattern_atom.pratt((
			postfix(
				2,
				just(Token::As).ignore_then(identifier()),
				|inner: Spanned<_>, name, e| {
					Spanned(
						Pattern::Binding {
							name,
							inner: inner.into(),
						},
						e.span(),
					)
				},
			),
			infix(
				left(1),
				just(Token::Pipe),
				|lhs: Spanned<Pattern>, _, rhs, e| {
					Spanned(Pattern::Union(lhs.into(), rhs.into()), e.span())
				},
			),
		))
	})
	.labelled("a pattern")
	.boxed()
}

fn list_pattern_entry<'src, I: TokenInput<'src>, P: NymphParser<'src, I, Spanned<Pattern>>>(
	pattern: P,
) -> impl NymphParser<'src, I, Spanned<ListPatternEntry>> {
	choice((
		just(Token::DotDotDot)
			.ignore_then(identifier().or_not())
			.map(ListPatternEntry::Rest),
		pattern.map(ListPatternEntry::Item),
	))
	.map_with(Spanned::new)
	.boxed()
}

fn map_pattern_entry<'src, I: TokenInput<'src>, P: NymphParser<'src, I, Spanned<Pattern>>>(
	pattern: P,
) -> impl NymphParser<'src, I, Spanned<MapPatternEntry>> {
	choice((
		just(Token::DotDotDot)
			.ignore_then(identifier().or_not())
			.map(MapPatternEntry::Rest),
		pattern
			.clone()
			.then_ignore(just(Token::Colon))
			.then(pattern)
			.map(|(key, value)| MapPatternEntry::Entry(key, value)),
	))
	.map_with(Spanned::new)
	.boxed()
}

fn struct_pattern<'src, I: TokenInput<'src>, P: NymphParser<'src, I, Spanned<Pattern>>>(
	pattern: P,
) -> impl NymphParser<'src, I, Pattern> {
	let struct_pattern_field = choice((
		just(Token::DotDotDot).to(StructPatternField::Rest),
		identifier().map(StructPatternField::Named),
		identifier()
			.then_ignore(just(Token::Eq))
			.then(pattern)
			.map(|(name, value)| StructPatternField::Value { name, value }),
	))
	.map_with(Spanned::new);

	identifier()
		.then(
			struct_pattern_field
				.separated_by(just(Token::Comma))
				.allow_trailing()
				.collect()
				.delimited_by(just(Token::LParen), just(Token::RParen))
				.or_not(),
		)
		.map(|(name, fields)| Pattern::Struct {
			name,
			fields: fields.unwrap_or(vec![]),
		})
}

fn range_pattern<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, RangePatternKind> {
	let range_atom = choice((
		signed_int().map(Pattern::Int),
		signed_float().map(Pattern::Float),
		char().map(Pattern::Char),
	))
	.map_with(Spanned::new);

	choice((
		range_atom
			.clone()
			.then_ignore(just(Token::DotDot))
			.then(range_atom.clone())
			.map(|(min, max)| RangePatternKind::ExclusiveBoth {
				min: min.into(),
				max: max.into(),
			}),
		range_atom
			.clone()
			.then_ignore(just(Token::DotDot))
			.map(|min| RangePatternKind::ExclusiveMin(min.into())),
		range_atom
			.clone()
			.then_ignore(just(Token::DotDotEq))
			.then(range_atom.clone())
			.map(|(min, max)| RangePatternKind::InclusiveBoth {
				min: min.into(),
				max: max.into(),
			}),
		just(Token::DotDotEq)
			.ignore_then(range_atom)
			.map(|max| RangePatternKind::InclusiveMax(max.into())),
	))
	.labelled("a range pattern")
	.boxed()
}

fn signed_int<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, Spanned<i64>> {
	just(Token::Minus)
		.or_not()
		.then(int())
		.map_with(|(sign, val), e| {
			Spanned(val.0 as i64 * if sign.is_some() { -1 } else { 1 }, e.span())
		})
		.labelled("a signed integer literal")
		.boxed()
}

fn signed_float<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, Spanned<OrderedFloat<f64>>>
{
	just(Token::Minus)
		.or_not()
		.then(float())
		.map_with(|(sign, val), e| Spanned(val.0 * if sign.is_some() { -1f64 } else { 1f64 }, e.span()))
		.labelled("a signed float literal")
		.boxed()
}

fn string_pattern<'src, I: TokenInput<'src>, M>(
	make_input: M,
) -> impl NymphParser<'src, I, Vec<Spanned<StringPatternPart>>>
where
	M: Fn(SimpleSpan, &'src Vec<Spanned<Token>>) -> I + Copy + 'src,
{
	let string_part = select! {
		Token::StringChar(c) = e => Spanned(StringPatternPart::Char(c), e.span()),
		Token::StringEscape(c) = e => Spanned(StringPatternPart::EscapeSequence(c), e.span()),
	};

	string_part
		.repeated()
		.collect()
		.nested_in(select_ref! {
			Token::String(toks) = e => make_input(e.span(), toks)
		})
		.labelled("a constant string literal")
		.boxed()
}

fn visibility<'src, I: TokenInput<'src>>() -> impl NymphParser<'src, I, Visibility> {
	select! {
		Token::Public => Visibility::Public,
		Token::Internal => Visibility::Internal,
		Token::Private => Visibility::Private,
	}
	.labelled("a visibility modifier")
}

pub(crate) fn make_input<'src>(
	eoi: SimpleSpan,
	toks: &'src Vec<Spanned<Token>>,
) -> impl TokenInput<'src> {
	toks.map(eoi, |Spanned(t, s)| (t, s))
}
