//! The token vocabulary produced by the lexer and consumed by the parser.
//!
//! Unlike the previous implementation — which produced *token trees* (delimiters
//! nested a `Vec<Spanned<Token>>`) — this is a **flat** token stream. Balanced
//! delimiters are individual open/close tokens, and the `#`-collection sigils are
//! single combined tokens (`#[`, `#(`, `#{`). The only place a nested structure
//! survives is inside string literals, where interpolation genuinely requires it
//! (see [`StrFragment`]).

use ecow::EcoString;
use ordered_float::OrderedFloat;
use strum::Display;

use crate::Spanned;
use crate::expr::StringEscape;

#[derive(Clone, PartialEq, Debug, salsa::Update)]
pub enum Token {
	// ── Literals ────────────────────────────────────────────────────────────
	/// A signed integer literal, already decoded from its radix: `1234`,
	/// `0xDEADF00D`, `0b1010`, `1_000`.
	Int(u64),
	/// An unsigned integer literal (the `u` suffix): `1234u`, `0b1010u`.
	UInt(u64),
	/// A floating-point literal: `1.0`, `9e-1`, `2f`.
	Float(OrderedFloat<f64>),
	/// A character literal: `'a'`, `'\n'`, `'ሴ'`.
	Char(char),
	/// A string literal, split into its interpolation-aware fragments.
	Str(Vec<Spanned<StrFragment>>),
	/// `true`
	True,
	/// `false`
	False,

	/// An identifier. `_` alone is lexed as [`Token::Underscore`] instead.
	Identifier(EcoString),
	/// A positional closure parameter: `$` (== `$0`), `$0`, `$1`, ...
	AnonymousParam(Option<u8>),

	// ── Keywords ────────────────────────────────────────────────────────────
	Public,
	Internal,
	Private,
	Import,
	With,
	Type,
	Struct,
	Enum,
	Let,
	Mut,
	External,
	Func,
	Interface,
	Impl,
	Namespace,
	For,
	While,
	If,
	Else,
	Match,
	Continue,
	Break,
	Return,
	This,
	/// `in` — used both as a binary operator and in `for` loops.
	In,
	/// `as` — the type-cast operator.
	As,
	/// `is` — the pattern-test operator.
	Is,
	/// Reserved for a future async model; the parser rejects it with a clear
	/// "reserved keyword" error so it can never be used as an identifier.
	Async,
	/// Reserved (see [`Token::Async`]).
	Await,

	// ── Built-in type keywords ──────────────────────────────────────────────
	IntType,
	UIntType,
	FloatType,
	BooleanType,
	CharType,
	StringType,
	VoidType,
	NeverType,
	SelfType,

	// ── Delimiters ──────────────────────────────────────────────────────────
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
	/// `#(` — tuple literal opener.
	HashLParen,
	/// `#[` — list literal opener.
	HashLBracket,
	/// `#{` — map literal opener.
	HashLBrace,

	// ── Punctuation & operators ─────────────────────────────────────────────
	/// `->` — used for function types (`(A) -> B`), closures (`(x) -> x + 1`), and
	/// match arms (`pattern -> body`). Function *declaration* bodies use `=` instead.
	Arrow,
	/// `...`
	DotDotDot,
	/// `?`
	Question,
	/// `??`
	DoubleQuestion,
	/// `?.`
	QuestionDot,
	/// `.`
	Dot,
	/// `@`
	At,
	/// `,`
	Comma,
	/// `:`
	Colon,
	/// `::`
	ColonColon,
	/// `_`
	Underscore,
	/// `|>`
	PipeArrow,
	/// `!`
	Bang,
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
	Amp,
	/// `|`
	Pipe,
	/// `^`
	Caret,
	/// `~`
	Tilde,
	/// `==`
	EqEq,
	/// `!=`
	BangEq,
	/// `<`
	Lt,
	/// `>`
	Gt,
	/// `<=`
	LtEq,
	/// `>=`
	GtEq,
	/// `!in`
	BangIn,
	/// `!is`
	BangIs,
	/// `&&`
	AmpAmp,
	/// `||`
	PipePipe,
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
	AmpAmpEq,
	/// `||=`
	PipePipeEq,
	/// `&=`
	AmpEq,
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

	/// A lexer error placeholder, carried so parsing can recover.
	Error,
}

/// One fragment of a string literal. A plain string is a single [`StrFragment::Text`];
/// interpolation and escapes split it into multiple fragments.
#[derive(Clone, PartialEq, Debug, salsa::Update)]
pub enum StrFragment {
	/// Raw text between escapes/interpolations.
	Text(EcoString),
	/// A recognised escape sequence such as `\n` or `ሴ`.
	Escape(StringEscape),
	/// An interpolated `${ ... }` expression, lexed but not yet parsed. The parser
	/// recursively turns these tokens into an expression.
	Interpolation(Vec<Spanned<Token>>),
}

impl Token {
	/// A short human-readable description used in parser diagnostics
	/// ("expected `,`, found a string literal").
	pub fn describe(&self) -> &'static str {
		use Token::*;
		match self {
			Int(_) => "an integer literal",
			UInt(_) => "an unsigned integer literal",
			Float(_) => "a floating-point literal",
			Char(_) => "a character literal",
			Str(_) => "a string literal",
			True => "`true`",
			False => "`false`",
			Identifier(_) => "an identifier",
			AnonymousParam(_) => "a closure parameter",
			Public => "`public`",
			Internal => "`internal`",
			Private => "`private`",
			Import => "`import`",
			With => "`with`",
			Type => "`type`",
			Struct => "`struct`",
			Enum => "`enum`",
			Let => "`let`",
			Mut => "`mut`",
			External => "`external`",
			Func => "`func`",
			Interface => "`interface`",
			Impl => "`impl`",
			Namespace => "`namespace`",
			For => "`for`",
			While => "`while`",
			If => "`if`",
			Else => "`else`",
			Match => "`match`",
			Continue => "`continue`",
			Break => "`break`",
			Return => "`return`",
			This => "`this`",
			In => "`in`",
			As => "`as`",
			Is => "`is`",
			Async => "`async`",
			Await => "`await`",
			IntType => "`int`",
			UIntType => "`uint`",
			FloatType => "`float`",
			BooleanType => "`boolean`",
			CharType => "`char`",
			StringType => "`string`",
			VoidType => "`void`",
			NeverType => "`never`",
			SelfType => "`self`",
			LParen => "`(`",
			RParen => "`)`",
			LBracket => "`[`",
			RBracket => "`]`",
			LBrace => "`{`",
			RBrace => "`}`",
			HashLParen => "`#(`",
			HashLBracket => "`#[`",
			HashLBrace => "`#{`",
			Arrow => "`->`",
			DotDotDot => "`...`",
			Question => "`?`",
			DoubleQuestion => "`??`",
			QuestionDot => "`?.`",
			Dot => "`.`",
			At => "`@`",
			Comma => "`,`",
			Colon => "`:`",
			ColonColon => "`::`",
			Underscore => "`_`",
			PipeArrow => "`|>`",
			Bang => "`!`",
			Plus => "`+`",
			Minus => "`-`",
			Star => "`*`",
			Slash => "`/`",
			Percent => "`%`",
			StarStar => "`**`",
			Amp => "`&`",
			Pipe => "`|`",
			Caret => "`^`",
			Tilde => "`~`",
			EqEq => "`==`",
			BangEq => "`!=`",
			Lt => "`<`",
			Gt => "`>`",
			LtEq => "`<=`",
			GtEq => "`>=`",
			BangIn => "`!in`",
			BangIs => "`!is`",
			AmpAmp => "`&&`",
			PipePipe => "`||`",
			Eq => "`=`",
			PlusEq => "`+=`",
			MinusEq => "`-=`",
			StarEq => "`*=`",
			SlashEq => "`/=`",
			PercentEq => "`%=`",
			StarStarEq => "`**=`",
			AmpAmpEq => "`&&=`",
			PipePipeEq => "`||=`",
			AmpEq => "`&=`",
			PipeEq => "`|=`",
			CaretEq => "`^=`",
			TildeEq => "`~=`",
			LtLtEq => "`<<=`",
			GtGtEq => "`>>=`",
			DotDot => "`..`",
			DotDotEq => "`..=`",
			Error => "invalid input",
		}
	}
}

impl std::fmt::Display for Token {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.describe())
	}
}

/// Categories used by the lexer/formatter and by LSP semantic-token classification.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Display)]
pub enum TokenKind {
	Literal,
	Identifier,
	Keyword,
	TypeKeyword,
	Delimiter,
	Operator,
	Punctuation,
	Error,
}
