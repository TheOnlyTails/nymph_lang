use core::fmt;
use ecow::EcoString;
use ordered_float::OrderedFloat;
use std::fmt::{Display, Formatter};

use crate::ast::{
	Spanned,
	expr::{CharEscape, StringEscape},
};

#[derive(Clone, PartialEq, Eq, Debug, Hash, salsa::Update)]
pub enum Token {
	/// `0b1101001`
	BinaryInt(u64),
	/// `0o7165341`
	OctalInt(u64),
	/// `0xDEADF00D`
	HexInt(u64),
	/// `1234`
	DecimalInt(u64),

	/// `1234u`
	DecimalUInt(u64),
	/// `0b1101001u`
	BinaryUInt(u64),
	/// `0o7165341u`
	OctalUInt(u64),
	/// `0xDEADF00Du`
	HexUInt(u64),

	/// `1.2`
	Float(OrderedFloat<f64>),
	/// `1.2e3`
	IntExpFloat(u64, i32),
	/// `1.2e-3`
	FloatExpFloat(OrderedFloat<f64>, i32),
	/// `12f`
	IntFloat(u64),

	/// `'a'`
	Char(char),
	/// `'\r'`
	CharEscape(CharEscape),

	/// `"abc"`, `"${b}"`, `"\n\u32"`
	String(Vec<Spanned<Self>>),
	StringText(EcoString),
	StringEscape(StringEscape),
	StringInterpolation(Vec<Spanned<Self>>),

	Identifier(EcoString),
	/// `$`, `$0`, `$1`
	AnonymousParam(Option<u32>),

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
	/// `()`
	Parens(Vec<Spanned<Self>>),
	// `[]`
	Brackets(Vec<Spanned<Self>>),
	// `{}`
	Braces(Vec<Spanned<Self>>),
	// `#[]`
	List(Vec<Spanned<Self>>),
	// `#()`
	Tuple(Vec<Spanned<Self>>),
	// `#{}`
	Map(Vec<Spanned<Self>>),
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
	/// `uint`
	UIntType,
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
	/// `..<`
	DotDotLt,
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

	Error,
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
				BinaryUInt(_) => "a binary unsigned integer literal",
				OctalUInt(_) => "an octal unsigned integer literal",
				HexUInt(_) => "a hexadecimal unsigned integer literal",
				DecimalUInt(_) => "an unsigned integer literal",
				Float(_) | IntFloat(_) | IntExpFloat(_, _) | FloatExpFloat(_, _) =>
					"a floating point literal",
				Identifier(_) => "an identifier",
				AnonymousParam(_) => "an anonymous function parameter",
				Char(_) | CharEscape(_) => "a character literal",
				String(_) => "a string literal",
				StringText(_) => "a character in a string",
				StringEscape(_) => "an escape sequence",
				StringInterpolation(_) => "an interpolated expression",
				True => "true",
				False => "false",
				Public => "public",
				Internal => "internal",
				Private => "private",
				Import => "import",
				With => "with",
				Parens(_) => "a pair of parentheses",
				Brackets(_) => "a pair of brackets",
				Braces(_) => "a pair of braces",
				List(_) => "list literal braces",
				Tuple(_) => "tuple parentheses",
				Map(_) => "map literal braces",
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
				UIntType => "uint",
				FloatType => "float",
				BooleanType => "boolean",
				CharType => "char",
				StringType => "string",
				VoidType => "void",
				NeverType => "never",
				SelfType => "self",
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
				DotDotLt => "..<",
				DotDotEq => "..=",
				Continue => "continue",
				Break => "break",
				Return => "return",
				This => "this",
				Error => "invalid input",
			}
		)
	}
}
