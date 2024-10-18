pub(crate) mod token;

use chumsky::{prelude::*, text::Char};
use ecow::EcoString;
use ordered_float::OrderedFloat;

use crate::{
	ast::{
		Spanned, SpannedExt,
		expr::{CharEscape, StringEscape},
	},
	lexer::token::Token,
};

type LexerError<'src> = Rich<'src, char, SimpleSpan>;

type LexerExtra<'src> = extra::Err<LexerError<'src>>;

pub(crate) trait Lexer<'src, O> = Parser<'src, &'src str, O, LexerExtra<'src>> + Clone + 'src;

pub(crate) fn lexer<'src>() -> impl Lexer<'src, Vec<Spanned<Token>>> {
	let token = recursive(|token| {
		choice((
			float_lexer(),
			int_lexer(),
			char_lexer(),
			string_lexer(token),
			delimiter_lexer(),
			keyword_lexer(),
			punct_lexer(),
			ident_lexer(),
		))
		.map_with(Spanned::new)
		.padded_by(comment_lexer().repeated())
		.padded()
		.recover_with(skip_then_retry_until(any().ignored(), end()))
	});

	token.repeated().collect()
}

fn int_lexer<'src>() -> impl Lexer<'src, Token> {
	choice((
		regex(r"0[bB][01](_?[01])*")
			.map(|it: &str| u64::from_str_radix(&it[2..].replace("_", ""), 2))
			.unwrapped()
			.map(Token::BinaryInt),
		regex(r"0[oO][0-7](_?[0-7])*")
			.map(|it: &str| u64::from_str_radix(&it[2..].replace("_", ""), 8))
			.unwrapped()
			.map(Token::OctalInt),
		regex(r"0[xX][a-fA-F\d](_?[a-fA-F\d])*")
			.map(|it: &str| u64::from_str_radix(&it[2..].replace("_", ""), 16))
			.unwrapped()
			.map(Token::HexInt),
		regex(r"\d(_?\d)*")
			.map(|it: &str| it.replace("_", "").parse::<u64>())
			.unwrapped()
			.map(Token::DecimalInt),
	))
	.then_ignore(
		any()
			.filter(|c: &char| !c.is_ident_start())
			.rewind()
			.or_not(),
	)
	.boxed()
}

fn float_lexer<'src>() -> impl Lexer<'src, Token> {
	choice((
		// 1.2e-3
		regex(r"\d(_?\d)*\.\d(_?\d)*[eE][-+]?\d(_?\d)*")
			.map(|it: &str| it.replace("_", "").parse::<OrderedFloat<f64>>())
			.unwrapped()
			.map(Token::Float),
		// 1e-2
		regex(r"\d(_?\d)*[eE][-+]?\d(_?\d)*")
			.map(|it: &str| it.replace("_", "").parse::<OrderedFloat<f64>>())
			.unwrapped()
			.map(Token::Float),
		// 1.2
		regex(r"(0|[1-9](_?\d)*)\.\d(_?\d)*")
			.map(|it: &str| it.replace("_", "").parse::<OrderedFloat<f64>>())
			.unwrapped()
			.map(Token::Float),
		// 1f
		regex(r"\d(_?\d)*[fF]")
			.map(|it: &str| {
				it.strip_suffix(['f', 'F'])
					.unwrap()
					.replace("_", "")
					.parse::<OrderedFloat<f64>>()
			})
			.unwrapped()
			.map(Token::Float),
	))
	.labelled("floating point literal")
	.boxed()
}

fn char_lexer<'src>() -> impl Lexer<'src, Token> {
	choice((
		just('\\')
			.ignore_then(one_of("uU"))
			.ignore_then(
				text::digits(16)
					.repeated()
					.at_least(1)
					.at_most(6)
					.to_slice(),
			)
			.map(|s| u32::from_str_radix(s, 16))
			.unwrapped()
			.map(char::from_u32)
			.unwrapped()
			.map(|c| Token::CharEscape(CharEscape::Unicode(c))),
		just(r"\n").to(Token::CharEscape(CharEscape::Newline)),
		just(r"\N").to(Token::CharEscape(CharEscape::Newline)),
		just(r"\r").to(Token::CharEscape(CharEscape::Carriage)),
		just(r"\R").to(Token::CharEscape(CharEscape::Carriage)),
		just(r"\t").to(Token::CharEscape(CharEscape::Tab)),
		just(r"\T").to(Token::CharEscape(CharEscape::Tab)),
		just(r"\\\").to(Token::CharEscape(CharEscape::Backslash)),
		just(r"\'").to(Token::CharEscape(CharEscape::Apostrophe)),
		none_of(r"[^\\']").map(Token::Char),
	))
	.delimited_by(just('\''), just('\''))
	.labelled("character literal")
	.boxed()
}

fn string_lexer<'src, T: Lexer<'src, Spanned<Token>>>(token: T) -> impl Lexer<'src, Token> {
	let string_token = choice((
		// interpolation
		just("${")
			.ignore_then(token.repeated().at_least(1).collect())
			.then_ignore(just("}"))
			.map(Token::StringInterpolation),
		// Unicode escape
		regex(r"\\[uU][a-fA-F\d]{1,6}")
			.map(|s: &str| u32::from_str_radix(&s[2..], 16))
			.unwrapped()
			.map(char::from_u32)
			.unwrapped()
			.map(|c| Token::StringEscape(StringEscape::Unicode(c))),
		// escape sequences
		just(r"\n").to(Token::StringEscape(StringEscape::Newline)),
		just(r"\N").to(Token::StringEscape(StringEscape::Newline)),
		just(r"\r").to(Token::StringEscape(StringEscape::Carriage)),
		just(r"\R").to(Token::StringEscape(StringEscape::Carriage)),
		just(r"\t").to(Token::StringEscape(StringEscape::Tab)),
		just(r"\T").to(Token::StringEscape(StringEscape::Tab)),
		just(r"\\").to(Token::StringEscape(StringEscape::Backslash)),
		just(r#"\""#).to(Token::StringEscape(StringEscape::Quote)),
		just(r"\${").to(Token::StringEscape(StringEscape::Interpolation)),
		// regular text
		none_of(r#""\"#).map(|c: char| Token::StringChar(c)),
	));

	string_token
		.map_with(Spanned::new)
		.repeated()
		.collect()
		.delimited_by(just('"'), just('"'))
		.map(Token::String)
		.boxed()
}

fn ident_lexer<'src>() -> impl Lexer<'src, Token> {
	text::ident()
		.map(|t: &str| match t {
			"_" => Token::Underscore,
			t => Token::Identifier(EcoString::from(t.to_string())),
		})
		.boxed()
}

fn delimiter_lexer<'src>() -> impl Lexer<'src, Token> {
	choice((
		just('(').to(Token::LParen),
		just(')').to(Token::RParen),
		just('[').to(Token::LBracket),
		just(']').to(Token::RBracket),
		just('{').to(Token::LBrace),
		just('}').to(Token::RBrace),
		just("#(").to(Token::TupleStart),
		just("#[").to(Token::ListStart),
		just("#{").to(Token::MapStart),
	))
	.boxed()
}

fn keyword_lexer<'src>() -> impl Lexer<'src, Token> {
	choice([
		text::keyword("true").to(Token::True),
		text::keyword("false").to(Token::False),
		text::keyword("public").to(Token::Public),
		text::keyword("internal").to(Token::Internal),
		text::keyword("private").to(Token::Private),
		text::keyword("import").to(Token::Import),
		text::keyword("with").to(Token::With),
		text::keyword("async").to(Token::Async),
		text::keyword("await").to(Token::Await),
		text::keyword("type").to(Token::Type),
		text::keyword("struct").to(Token::Struct),
		text::keyword("enum").to(Token::Enum),
		text::keyword("let").to(Token::Let),
		text::keyword("mut").to(Token::Mut),
		text::keyword("external").to(Token::External),
		text::keyword("func").to(Token::Func),
		text::keyword("interface").to(Token::Interface),
		text::keyword("impl").to(Token::Impl),
		text::keyword("namespace").to(Token::Namespace),
		text::keyword("for").to(Token::For),
		text::keyword("while").to(Token::While),
		text::keyword("if").to(Token::If),
		text::keyword("else").to(Token::Else),
		text::keyword("match").to(Token::Match),
		text::keyword("int").to(Token::IntType),
		text::keyword("float").to(Token::FloatType),
		text::keyword("boolean").to(Token::BooleanType),
		text::keyword("char").to(Token::CharType),
		text::keyword("string").to(Token::StringType),
		text::keyword("void").to(Token::VoidType),
		text::keyword("never").to(Token::NeverType),
		text::keyword("self").to(Token::SelfType),
		text::keyword("as").to(Token::As),
		text::keyword("is").to(Token::Is),
		text::keyword("in").to(Token::In),
		text::keyword("return").to(Token::Return),
		text::keyword("break").to(Token::Break),
		text::keyword("continue").to(Token::Continue),
		text::keyword("this").to(Token::This),
	])
	.boxed()
}

fn punct_lexer<'src>() -> impl Lexer<'src, Token> {
	choice([
		just("...").to(Token::DotDotDot),
		just("..=").to(Token::DotDotEq),
		just("..").to(Token::DotDot),
		just(".").to(Token::Dot),
		just("??").to(Token::DoubleQuestion),
		just("?.").to(Token::QuestionDot),
		just("?").to(Token::QuestionMark),
		just("@").to(Token::AtSign),
		just(",").to(Token::Comma),
		just("::").to(Token::ColonColon),
		just(":").to(Token::Colon),
		just("!in").to(Token::NotIn),
		just("!is").to(Token::NotIs),
		just("!=").to(Token::NotEq),
		just("!").to(Token::ExclamationMark),
		just("+=").to(Token::PlusEq),
		just("+").to(Token::Plus),
		just("->").to(Token::Arrow),
		just("-=").to(Token::MinusEq),
		just("-").to(Token::Minus),
		just("**=").to(Token::StarStarEq),
		just("**").to(Token::StarStar),
		just("*=").to(Token::StarEq),
		just("*").to(Token::Star),
		just("/=").to(Token::SlashEq),
		just("/").to(Token::Slash),
		just("%=").to(Token::PercentEq),
		just("%").to(Token::Percent),
		just("&&=").to(Token::AndAndEq),
		just("&&").to(Token::AndAnd),
		just("&=").to(Token::AndEq),
		just("&").to(Token::And),
		just("|>").to(Token::Triangle),
		just("||=").to(Token::PipePipeEq),
		just("||").to(Token::PipePipe),
		just("|=").to(Token::PipeEq),
		just("|").to(Token::Pipe),
		just("^=").to(Token::CaretEq),
		just("^").to(Token::Caret),
		just("~=").to(Token::TildeEq),
		just("~").to(Token::Tilde),
		just("==").to(Token::EqEq),
		just("=").to(Token::Eq),
		just("<<=").to(Token::LtLtEq),
		just("<=").to(Token::LtEq),
		just("<").to(Token::Lt),
		just(">>=").to(Token::GtGtEq),
		just(">=").to(Token::GtEq),
		just(">").to(Token::Gt),
	])
	.boxed()
}

fn comment_lexer<'src>() -> impl Lexer<'src, ()> {
	choice((
		just("/*")
			.ignore_then(any().repeated())
			.then_ignore(just("*/"))
			.to(()),
		just("//")
			.then(any().and_is(just("\n").not()).repeated())
			.to(()),
	))
	.padded()
	.boxed()
}

#[cfg(test)]
mod tests {
	use super::*;
	use test_case::test_case;

	#[test_case("1234" => Ok(vec![Token::DecimalInt(1234).spanned(0..4)]) ; "decimal")]
	#[test_case("1_234_567" => Ok(vec![Token::DecimalInt(1234567).spanned(0..9)]) ; "decimal with separators")]
	#[test_case("0xFF" => Ok(vec![Token::HexInt(255).spanned(0..4)]) ; "hex")]
	#[test_case("0xFF_FF" => Ok(vec![Token::HexInt(65535).spanned(0..7)]) ; "hex with separator")]
	#[test_case("0b1010" => Ok(vec![Token::BinaryInt(10).spanned(0..6)]) ; "binary")]
	#[test_case("0b1010_1010" => Ok(vec![Token::BinaryInt(170).spanned(0..11)]) ; "binary with separator")]
	#[test_case("0o755" => Ok(vec![Token::OctalInt(493).spanned(0..5)]) ; "octal")]
	#[test_case("0o77_77" => Ok(vec![Token::OctalInt(4095).spanned(0..7)]) ; "octal with separator")]
	fn test_integer_literals(
		input: &str,
	) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("1.23" => Ok(vec![Token::Float(OrderedFloat(1.23)).spanned(0..4)]) ; "simple float")]
	#[test_case("1_234.567_89" => Ok(vec![Token::Float(OrderedFloat(1234.56789)).spanned(0..12)]) ; "float with separators")]
	#[test_case("1.2e-3" => Ok(vec![Token::Float(OrderedFloat(0.0012)).spanned(0..6)]) ; "scientific notation")]
	#[test_case("1_234e-1_0" => Ok(vec![Token::Float(OrderedFloat(1234e-10)).spanned(0..10)]) ; "scientific with separators")]
	#[test_case("1e2" => Ok(vec![Token::Float(OrderedFloat(100.0)).spanned(0..3)]) ; "scientific without decimal")]
	#[test_case("1f" => Ok(vec![Token::Float(OrderedFloat(1.0)).spanned(0..2)]) ; "float suffix")]
	fn test_float_literals(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("'a'" => Ok(vec![Token::Char('a').spanned(0..3)]) ; "simple char")]
	#[test_case("'β'" => Ok(vec![Token::Char('β').spanned(0..4)]) ; "unicode char")]
	#[test_case(r"'\n'" => Ok(vec![Token::CharEscape(CharEscape::Newline).spanned(0..4)]) ; "newline escape")]
	#[test_case(r"'\t'" => Ok(vec![Token::CharEscape(CharEscape::Tab).spanned(0..4)]) ; "tab escape")]
	#[test_case(r"'\''" => Ok(vec![Token::CharEscape(CharEscape::Apostrophe).spanned(0..4)]) ; "apostrophe escape")]
	#[test_case(r"'\u0041'" => Ok(vec![Token::CharEscape(CharEscape::Unicode('A')).spanned(0..8)]) ; "unicode escape A")]
	#[test_case(r"'\u1'" => Ok(vec![Token::CharEscape(CharEscape::Unicode('\u{0001}')).spanned(0..5)]) ; "unicode escape SOH")]
	#[test_case(r"'\u2764'" => Ok(vec![Token::CharEscape(CharEscape::Unicode('❤')).spanned(0..8)]) ; "unicode escape emoji 1")]
	#[test_case(r"'\u1F600'" => Ok(vec![Token::CharEscape(CharEscape::Unicode('😀')).spanned(0..9)]) ; "unicode escape emoji 2")]
	fn test_char_literals(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("foo_bar123" => Ok(vec![Token::Identifier("foo_bar123".into()).spanned(0..10)]) ; "identifier")]
	#[test_case("x" => Ok(vec![Token::Identifier("x".into()).spanned(0..1)]) ; "single char")]
	#[test_case("_x" => Ok(vec![Token::Identifier("_x".into()).spanned(0..2)]) ; "starts with underscore")]
	#[test_case("x1" => Ok(vec![Token::Identifier("x1".into()).spanned(0..2)]) ; "alphanumeric")]
	#[test_case("snake_case" => Ok(vec![Token::Identifier("snake_case".into()).spanned(0..10)]) ; "snake case")]
	#[test_case("camelCase" => Ok(vec![Token::Identifier("camelCase".into()).spanned(0..9)]) ; "camel case")]
	#[test_case("PascalCase" => Ok(vec![Token::Identifier("PascalCase".into()).spanned(0..10)]) ; "pascal case")]
	#[test_case("SCREAMING_SNAKE" => Ok(vec![Token::Identifier("SCREAMING_SNAKE".into()).spanned(0..15)]) ; "screaming snake")]
	#[test_case("αβγ" => Ok(vec![Token::Identifier("αβγ".into()).spanned(0..6)]) ; "unicode letters")]
	#[test_case("foo123_αβγ" => Ok(vec![Token::Identifier("foo123_αβγ".into()).spanned(0..13)]) ; "mixed alphanumeric unicode")]
	fn test_identifiers(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("func" => Ok(vec![Token::Func.spanned(0..4)]) ; "func keyword")]
	#[test_case("let" => Ok(vec![Token::Let.spanned(0..3)]) ; "let keyword")]
	#[test_case("mut" => Ok(vec![Token::Mut.spanned(0..3)]) ; "mut keyword")]
	#[test_case("if" => Ok(vec![Token::If.spanned(0..2)]) ; "if keyword")]
	#[test_case("else" => Ok(vec![Token::Else.spanned(0..4)]) ; "else keyword")]
	#[test_case("return" => Ok(vec![Token::Return.spanned(0..6)]) ; "return keyword")]
	#[test_case("true" => Ok(vec![Token::True.spanned(0..4)]) ; "true keyword")]
	#[test_case("false" => Ok(vec![Token::False.spanned(0..5)]) ; "false keyword")]
	#[test_case("public" => Ok(vec![Token::Public.spanned(0..6)]) ; "public keyword")]
	#[test_case("internal" => Ok(vec![Token::Internal.spanned(0..8)]) ; "internal keyword")]
	#[test_case("private" => Ok(vec![Token::Private.spanned(0..7)]) ; "private keyword")]
	#[test_case("import" => Ok(vec![Token::Import.spanned(0..6)]) ; "import keyword")]
	#[test_case("with" => Ok(vec![Token::With.spanned(0..4)]) ; "with keyword")]
	#[test_case("async" => Ok(vec![Token::Async.spanned(0..5)]) ; "async keyword")]
	#[test_case("await" => Ok(vec![Token::Await.spanned(0..5)]) ; "await keyword")]
	#[test_case("type" => Ok(vec![Token::Type.spanned(0..4)]) ; "type keyword")]
	#[test_case("struct" => Ok(vec![Token::Struct.spanned(0..6)]) ; "struct keyword")]
	#[test_case("enum" => Ok(vec![Token::Enum.spanned(0..4)]) ; "enum keyword")]
	#[test_case("external" => Ok(vec![Token::External.spanned(0..8)]) ; "external keyword")]
	#[test_case("interface" => Ok(vec![Token::Interface.spanned(0..9)]) ; "interface keyword")]
	#[test_case("impl" => Ok(vec![Token::Impl.spanned(0..4)]) ; "impl keyword")]
	#[test_case("namespace" => Ok(vec![Token::Namespace.spanned(0..9)]) ; "namespace keyword")]
	#[test_case("for" => Ok(vec![Token::For.spanned(0..3)]) ; "for keyword")]
	#[test_case("while" => Ok(vec![Token::While.spanned(0..5)]) ; "while keyword")]
	#[test_case("match" => Ok(vec![Token::Match.spanned(0..5)]) ; "match keyword")]
	#[test_case("int" => Ok(vec![Token::IntType.spanned(0..3)]) ; "int type keyword")]
	#[test_case("float" => Ok(vec![Token::FloatType.spanned(0..5)]) ; "float type keyword")]
	#[test_case("boolean" => Ok(vec![Token::BooleanType.spanned(0..7)]) ; "boolean type keyword")]
	#[test_case("char" => Ok(vec![Token::CharType.spanned(0..4)]) ; "char type keyword")]
	#[test_case("string" => Ok(vec![Token::StringType.spanned(0..6)]) ; "string type keyword")]
	#[test_case("void" => Ok(vec![Token::VoidType.spanned(0..4)]) ; "void type keyword")]
	#[test_case("never" => Ok(vec![Token::NeverType.spanned(0..5)]) ; "never type keyword")]
	#[test_case("self" => Ok(vec![Token::SelfType.spanned(0..4)]) ; "self type keyword")]
	#[test_case("as" => Ok(vec![Token::As.spanned(0..2)]) ; "as keyword")]
	#[test_case("is" => Ok(vec![Token::Is.spanned(0..2)]) ; "is keyword")]
	#[test_case("in" => Ok(vec![Token::In.spanned(0..2)]) ; "in keyword")]
	#[test_case("break" => Ok(vec![Token::Break.spanned(0..5)]) ; "break keyword")]
	#[test_case("continue" => Ok(vec![Token::Continue.spanned(0..8)]) ; "continue keyword")]
	#[test_case("this" => Ok(vec![Token::This.spanned(0..4)]) ; "this keyword")]
	fn test_keywords(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("." => Ok(vec![Token::Dot.spanned(0..1)]) ; "dot")]
	#[test_case("->" => Ok(vec![Token::Arrow.spanned(0..2)]) ; "arrow")]
	#[test_case("==" => Ok(vec![Token::EqEq.spanned(0..2)]) ; "equals")]
	#[test_case("!=" => Ok(vec![Token::NotEq.spanned(0..2)]) ; "not equals")]
	#[test_case("&&" => Ok(vec![Token::AndAnd.spanned(0..2)]) ; "logical and")]
	#[test_case("||" => Ok(vec![Token::PipePipe.spanned(0..2)]) ; "logical or")]
	#[test_case("..." => Ok(vec![Token::DotDotDot.spanned(0..3)]) ; "spread")]
	#[test_case("..=" => Ok(vec![Token::DotDotEq.spanned(0..3)]) ; "inclusive range")]
	#[test_case(".." => Ok(vec![Token::DotDot.spanned(0..2)]) ; "range")]
	#[test_case("??" => Ok(vec![Token::DoubleQuestion.spanned(0..2)]) ; "null coalescing")]
	#[test_case("?." => Ok(vec![Token::QuestionDot.spanned(0..2)]) ; "optional chaining")]
	#[test_case("?" => Ok(vec![Token::QuestionMark.spanned(0..1)]) ; "question mark")]
	#[test_case("@" => Ok(vec![Token::AtSign.spanned(0..1)]) ; "at sign")]
	#[test_case("::" => Ok(vec![Token::ColonColon.spanned(0..2)]) ; "double colon")]
	#[test_case(":" => Ok(vec![Token::Colon.spanned(0..1)]) ; "colon")]
	#[test_case("_" => Ok(vec![Token::Underscore.spanned(0..1)]) ; "underscore")]
	#[test_case("!in" => Ok(vec![Token::NotIn.spanned(0..3)]) ; "not in")]
	#[test_case("!is" => Ok(vec![Token::NotIs.spanned(0..3)]) ; "not is")]
	#[test_case("+=" => Ok(vec![Token::PlusEq.spanned(0..2)]) ; "plus equals")]
	#[test_case("+" => Ok(vec![Token::Plus.spanned(0..1)]) ; "plus")]
	#[test_case("-=" => Ok(vec![Token::MinusEq.spanned(0..2)]) ; "minus equals")]
	#[test_case("-" => Ok(vec![Token::Minus.spanned(0..1)]) ; "minus")]
	#[test_case("**=" => Ok(vec![Token::StarStarEq.spanned(0..3)]) ; "power equals")]
	#[test_case("**" => Ok(vec![Token::StarStar.spanned(0..2)]) ; "power")]
	#[test_case("*=" => Ok(vec![Token::StarEq.spanned(0..2)]) ; "multiply equals")]
	#[test_case("*" => Ok(vec![Token::Star.spanned(0..1)]) ; "multiply")]
	#[test_case("/=" => Ok(vec![Token::SlashEq.spanned(0..2)]) ; "divide equals")]
	#[test_case("/" => Ok(vec![Token::Slash.spanned(0..1)]) ; "divide")]
	#[test_case("%=" => Ok(vec![Token::PercentEq.spanned(0..2)]) ; "modulo equals")]
	#[test_case("%" => Ok(vec![Token::Percent.spanned(0..1)]) ; "modulo")]
	#[test_case("&&=" => Ok(vec![Token::AndAndEq.spanned(0..3)]) ; "logical and equals")]
	#[test_case("&=" => Ok(vec![Token::AndEq.spanned(0..2)]) ; "bitwise and equals")]
	#[test_case("&" => Ok(vec![Token::And.spanned(0..1)]) ; "bitwise and")]
	#[test_case("|>" => Ok(vec![Token::Triangle.spanned(0..2)]) ; "pipeline")]
	#[test_case("||=" => Ok(vec![Token::PipePipeEq.spanned(0..3)]) ; "logical or equals")]
	#[test_case("|=" => Ok(vec![Token::PipeEq.spanned(0..2)]) ; "bitwise or equals")]
	#[test_case("|" => Ok(vec![Token::Pipe.spanned(0..1)]) ; "bitwise or")]
	#[test_case("^=" => Ok(vec![Token::CaretEq.spanned(0..2)]) ; "xor equals")]
	#[test_case("^" => Ok(vec![Token::Caret.spanned(0..1)]) ; "xor")]
	#[test_case("~=" => Ok(vec![Token::TildeEq.spanned(0..2)]) ; "bitwise not equals")]
	#[test_case("~" => Ok(vec![Token::Tilde.spanned(0..1)]) ; "bitwise not")]
	#[test_case("<<=" => Ok(vec![Token::LtLtEq.spanned(0..3)]) ; "left shift equals")]
	#[test_case(">>=" => Ok(vec![Token::GtGtEq.spanned(0..3)]) ; "right shift equals")]
	#[test_case("<=" => Ok(vec![Token::LtEq.spanned(0..2)]) ; "less than or equal")]
	#[test_case(">=" => Ok(vec![Token::GtEq.spanned(0..2)]) ; "greater than or equal")]
	fn test_punctuation(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("1// This is a comment" => Ok(vec![Token::DecimalInt(1).spanned(0..1)]) ; "single line comment")]
	#[test_case("/* This is a comment */1" => Ok(vec![Token::DecimalInt(1).spanned(23..24)]) ; "multi line comment")]
	fn test_comments(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case(r#""Hello, world!""# => Ok(vec![Token::String(vec![
        Token::StringChar('H').spanned(1..2),
        Token::StringChar('e').spanned(2..3),
        Token::StringChar('l').spanned(3..4),
        Token::StringChar('l').spanned(4..5),
        Token::StringChar('o').spanned(5..6),
        Token::StringChar(',').spanned(6..7),
        Token::StringChar(' ').spanned(7..8),
        Token::StringChar('w').spanned(8..9),
        Token::StringChar('o').spanned(9..10),
        Token::StringChar('r').spanned(10..11),
        Token::StringChar('l').spanned(11..12),
        Token::StringChar('d').spanned(12..13),
        Token::StringChar('!').spanned(13..14),
    ]).spanned(0..15)]) ; "simple string")]
	#[test_case(r#""Hello\nWorld""# => Ok(vec![Token::String(vec![
        Token::StringChar('H').spanned(1..2),
        Token::StringChar('e').spanned(2..3),
        Token::StringChar('l').spanned(3..4),
        Token::StringChar('l').spanned(4..5),
        Token::StringChar('o').spanned(5..6),
        Token::StringEscape(StringEscape::Newline).spanned(6..8),
        Token::StringChar('W').spanned(8..9),
        Token::StringChar('o').spanned(9..10),
        Token::StringChar('r').spanned(10..11),
        Token::StringChar('l').spanned(11..12),
        Token::StringChar('d').spanned(12..13),
    ]).spanned(0..14)]) ; "string with escape sequence")]
	#[test_case(r#""Value: ${42}""# => Ok(vec![Token::String(vec![
        Token::StringChar('V').spanned(1..2),
        Token::StringChar('a').spanned(2..3),
        Token::StringChar('l').spanned(3..4),
        Token::StringChar('u').spanned(4..5),
        Token::StringChar('e').spanned(5..6),
        Token::StringChar(':').spanned(6..7),
        Token::StringChar(' ').spanned(7..8),
        Token::StringInterpolation(vec![Token::DecimalInt(42).spanned(10..12)]).spanned(8..13),
    ]).spanned(0..14)]) ; "string with interpolation")]
	#[test_case(r#""Escaped \${not interpolation}""# => Ok(vec![Token::String(vec![
        Token::StringChar('E').spanned(1..2),
        Token::StringChar('s').spanned(2..3),
        Token::StringChar('c').spanned(3..4),
        Token::StringChar('a').spanned(4..5),
        Token::StringChar('p').spanned(5..6),
        Token::StringChar('e').spanned(6..7),
        Token::StringChar('d').spanned(7..8),
        Token::StringChar(' ').spanned(8..9),
        Token::StringEscape(StringEscape::Interpolation).spanned(9..12),
        Token::StringChar('n').spanned(12 ..13),
        Token::StringChar('o').spanned(13 ..14),
        Token::StringChar('t').spanned(14 ..15),
        Token::StringChar(' ').spanned(15 ..16),
        Token::StringChar('i').spanned(16 ..17),
        Token::StringChar('n').spanned(17 ..18),
        Token::StringChar('t').spanned(18 ..19),
        Token::StringChar('e').spanned(19 ..20),
        Token::StringChar('r').spanned(20 ..21),
        Token::StringChar('p').spanned(21 ..22),
        Token::StringChar('o').spanned(22 ..23),
        Token::StringChar('l').spanned(23 ..24),
        Token::StringChar('a').spanned(24 ..25),
        Token::StringChar('t').spanned(25 ..26),
        Token::StringChar('i').spanned(26 ..27),
        Token::StringChar('o').spanned(27 ..28),
        Token::StringChar('n').spanned(28 ..29),
        Token::StringChar('}').spanned(29 ..30),
    ]).spanned(0..31)]) ; "string with escaped interpolation")]
	#[test_case(r#""Unicode: \u0048\u0065\u006C\u006C\u006F""# => Ok(vec![Token::String(vec![
        Token::StringChar('U').spanned(1..2),
        Token::StringChar('n').spanned(2..3),
        Token::StringChar('i').spanned(3..4),
        Token::StringChar('c').spanned(4..5),
        Token::StringChar('o').spanned(5..6),
        Token::StringChar('d').spanned(6..7),
        Token::StringChar('e').spanned(7..8),
        Token::StringChar(':').spanned(8..9),
        Token::StringChar(' ').spanned(9..10),
        Token::StringEscape(StringEscape::Unicode('H')).spanned(10..16),
        Token::StringEscape(StringEscape::Unicode('e')).spanned(16..22),
        Token::StringEscape(StringEscape::Unicode('l')).spanned(22..28),
        Token::StringEscape(StringEscape::Unicode('l')).spanned(28..34),
        Token::StringEscape(StringEscape::Unicode('o')).spanned(34..40),
    ]).spanned(0..41)]) ; "string with unicode escapes")]
	fn test_string_literals(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("0x" => matches Err(_) ; "invalid hex")]
	#[test_case("0b2" => matches Err(_) ; "invalid binary")]
	#[test_case("0o8" => matches Err(_) ; "invalid octal")]
	fn test_invalid_integers(
		input: &str,
	) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("1.2e" => matches Err(_) ; "incomplete exponent")]
	#[test_case("1.2e+" => matches Err(_) ; "missing exponent after sign")]
	#[test_case("1._2" => matches Err(_) ; "separator in wrong position")]
	fn test_invalid_floats(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("''" => matches Err(_) ; "empty char")]
	#[test_case("'ab'" => matches Err(_) ; "char too long")]
	#[test_case("'\\'" => matches Err(_) ; "incomplete escape")]
	#[test_case(r"'\u{110000}'" => matches Err(_) ; "invalid unicode")]
	fn test_invalid_chars(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case(r#""Hello"#  => matches Err(_) ; "unclosed string")]
	#[test_case(r#""${1"#   => matches Err(_) ; "unclosed interpolation")]
	#[test_case(r#""\x""#   => matches Err(_) ; "invalid escape sequence")]
	fn test_invalid_strings(input: &str) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}

	#[test_case("/* unclosed comment" => matches Err(_) ; "unclosed block comment")]
	#[test_case("/* nested /* comment */ */" => matches Err(_) ; "nested comments not supported")]
	fn test_invalid_comments(
		input: &str,
	) -> Result<Vec<Spanned<Token>>, Vec<Rich<char, SimpleSpan>>> {
		lexer().parse(input).into_result()
	}
}
