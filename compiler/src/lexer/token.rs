use core::fmt;
use ecow::EcoString;
use ordered_float::OrderedFloat;
use std::fmt::{Display, Formatter};

use crate::ast::{
	Spanned,
	expr::{CharEscape, StringEscape},
};

#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub(crate) enum Token {
	/// `0b1101001`
	BinaryInt(u64),
	/// `0o7165341`
	OctalInt(u64),
	/// `0xDEADF00D`
	HexInt(u64),
	/// `1234`
	DecimalInt(u64),

	/// `1.2`, `.12`, `1.2e3`, `12f`
	Float(OrderedFloat<f64>),

	/// `'a'`
	Char(char),
	/// `'\r'`
	CharEscape(CharEscape),

	/// `"abc"`, `"${b}"`, `"\n\u32"`
	String(Vec<Spanned<Self>>),
	StringChar(char),
	StringEscape(StringEscape),
	StringInterpolation(Vec<Spanned<Self>>),

	Identifier(EcoString),

	/// `true`
	True,
	/// `false`
	False,
	/// `public`
	Public,
	/// `internal`
	Internal,
	/// `private`
	Private,
	/// `import`
	Import,
	/// `with`
	With,
	/// `(`
	LParen,
	/// `)`
	RParen,
	/// `[`
	LBracket,
	/// `]`
	RBracket,
	/// `{`
	LBrace,
	/// `}`
	RBrace,
	/// `async`
	Async,
	/// `await`
	Await,
	/// `type`
	Type,
	/// `struct`
	Struct,
	/// `enum`
	Enum,
	/// `let`
	Let,
	/// `mut`
	Mut,
	/// `external`
	External,
	/// `func`
	Func,
	/// `interface`
	Interface,
	/// `impl`
	Impl,
	/// `namespace`
	Namespace,
	/// `for`
	For,
	/// `while`
	While,
	/// `if`
	If,
	/// `else`
	Else,
	/// `match`
	Match,
	/// `int`
	IntType,
	/// `float`
	FloatType,
	/// `boolean`
	BooleanType,
	/// `char`
	CharType,
	/// `string`
	StringType,
	/// `void`
	VoidType,
	/// `never`
	NeverType,
	/// `self`
	SelfType,
	/// `#[`
	ListStart,
	/// `#(`
	TupleStart,
	/// `#{`
	MapStart,
	/// `->`
	Arrow,
	/// `...`
	DotDotDot,
	/// `?`
	QuestionMark,
	/// `??`
	DoubleQuestion,
	/// `?.`
	QuestionDot,
	/// `.`
	Dot,
	/// `@`
	AtSign,
	/// `,`
	Comma,
	/// `:`
	Colon,
	/// `::`
	ColonColon,
	/// `_`
	Underscore,
	/// `|>`
	Triangle,
	/// `!`
	ExclamationMark,
	/// `+`
	Plus,
	/// `-`
	Minus,
	/// `*`
	Star,
	/// `/`
	Slash,
	/// `%`
	Percent,
	/// `**`
	StarStar,
	/// `&`
	And,
	/// `|`
	Pipe,
	/// `^`
	Caret,
	/// `~`
	Tilde,
	/// `==`
	EqEq,
	/// `!=`
	NotEq,
	/// `<`
	Lt,
	/// `>`
	Gt,
	/// `<=`
	LtEq,
	/// `>=`
	GtEq,
	/// `in`
	In,
	/// `!in`
	NotIn,
	/// `&&`
	AndAnd,
	/// `||`
	PipePipe,
	/// `as`
	As,
	/// `is`
	Is,
	/// `!is`
	NotIs,
	/// `=`
	Eq,
	/// `+=`
	PlusEq,
	/// `-=`
	MinusEq,
	/// `*=`
	StarEq,
	/// `/=`
	SlashEq,
	/// `%=`
	PercentEq,
	/// `**=`
	StarStarEq,
	/// `&&=`
	AndAndEq,
	/// `||=`
	PipePipeEq,
	/// `&=`
	AndEq,
	/// `|=`
	PipeEq,
	/// `^=`
	CaretEq,
	/// `~=`
	TildeEq,
	/// `<<=`
	LtLtEq,
	/// `>>=`
	GtGtEq,
	/// `..`
	DotDot,
	/// `..=`
	DotDotEq,
	/// `continue`
	Continue,
	/// `break`
	Break,
	/// `return`
	Return,
	/// `this`
	This,
}

impl Display for Token {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		use Token::*;
		write!(
			f,
			"{}",
			match self {
				BinaryInt(_) => "a binary integer literal",
				OctalInt(_) => "an octal integer literal",
				HexInt(_) => "a hexadecimal integer literal",
				DecimalInt(_) => "an integer literal",
				Float(_) => "a floating point literal",
				Identifier(_) => "an identifier",
				Char(_) | CharEscape(_) => "a character literal",
				String(_) => "a string literal",
				StringChar(_) => "a character in a string",
				StringEscape(_) => "an escape sequence",
				StringInterpolation(_) => "an interpolated expression",
				True => "true",
				False => "false",
				Public => "public",
				Internal => "internal",
				Private => "private",
				Import => "import",
				With => "with",
				LParen => "(",
				RParen => ")",
				LBracket => "[",
				RBracket => "]",
				LBrace => "{",
				RBrace => "}",
				Async => "async",
				Await => "await",
				Type => "type",
				Struct => "struct",
				Enum => "enum",
				Let => "let",
				Mut => "mut",
				External => "external",
				Func => "func",
				Interface => "interface",
				Impl => "impl",
				Namespace => "namespace",
				For => "for",
				While => "while",
				If => "if",
				Else => "else",
				Match => "match",
				IntType => "int",
				FloatType => "float",
				BooleanType => "boolean",
				CharType => "char",
				StringType => "string",
				VoidType => "void",
				NeverType => "never",
				SelfType => "self",
				ListStart => "#[",
				TupleStart => "#(",
				MapStart => "#{",
				Arrow => "->",
				DotDotDot => "...",
				QuestionMark => "?",
				DoubleQuestion => "??",
				QuestionDot => "?.",
				Dot => ".",
				AtSign => "@",
				Comma => ",",
				Colon => ":",
				ColonColon => "::",
				Underscore => "_",
				Triangle => "|>",
				ExclamationMark => "!",
				Plus => "+",
				Minus => "-",
				Star => "*",
				Slash => "/",
				Percent => "%",
				StarStar => "**",
				And => "&",
				Pipe => "|",
				Caret => "^",
				Tilde => "~",
				EqEq => "==",
				NotEq => "!=",
				Lt => "<",
				Gt => ">",
				LtEq => "<=",
				GtEq => ">=",
				In => "in",
				NotIn => "!in",
				AndAnd => "&&",
				PipePipe => "||",
				As => "as",
				Is => "is",
				NotIs => "!is",
				Eq => "=",
				PlusEq => "+=",
				MinusEq => "-=",
				StarEq => "*=",
				SlashEq => "/=",
				PercentEq => "%=",
				StarStarEq => "**=",
				AndAndEq => "&&=",
				PipePipeEq => "||=",
				AndEq => "&=",
				PipeEq => "|=",
				CaretEq => "^=",
				TildeEq => "~=",
				LtLtEq => "<<=",
				GtGtEq => ">>=",
				DotDot => "..",
				DotDotEq => "..=",
				Continue => "continue",
				Break => "break",
				Return => "return",
				This => "this",
			}
		)
	}
}
