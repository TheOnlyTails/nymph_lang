//! The lexer's and parser's diagnostics as typed catalogs.
//!
//! [`ParseError`] and [`LexError`] each collect one phase's messages into a single
//! enum whose [`IntoDiagnostic`] impl is the one place a variant becomes a rendered
//! message. The parser emits a [`ParseError`] with a span via `Parser::emit`; the
//! lexer runs on chumsky, whose `Rich::custom` carries a `&'static str`, so
//! [`LexError`] exposes its text via [`LexError::text`] (and still implements
//! [`IntoDiagnostic`] for when the code scheme lands). Codes are not assigned yet —
//! `code()` inherits the trait default (`None`).

use ecow::EcoString;
use nymph_diagnostics::IntoDiagnostic;

/// A diagnostic produced while parsing (after lexing).
#[derive(Clone, Debug, PartialEq)]
pub enum ParseError {
	/// A type was expected but the input ended.
	ExpectedTypeFoundEof,
	/// A type was expected but a different token was found.
	ExpectedType { found: EcoString },
	/// A function type is missing its `->`.
	ExpectedArrowInFnType,
	/// A pattern was expected but the input ended.
	ExpectedPatternFoundEof,
	/// A pattern was expected but a different token was found.
	ExpectedPattern { found: EcoString },
	/// A `-` in a pattern was not followed by a number.
	ExpectedNumberAfterMinus,
	/// String interpolation appeared inside a pattern.
	InterpolationInPattern,
	/// An expression was expected but the input ended.
	ExpectedExpressionFoundEof,
	/// An expression was expected but a different token was found.
	ExpectedExpression { found: EcoString },
	/// An interface name was expected before `for` in an `impl … for …`.
	ExpectedInterfaceName,
	/// Extra tokens remained after a complete expression.
	TrailingTokens,
	/// A specific token was expected but a different one was found.
	ExpectedToken {
		expected: EcoString,
		found: EcoString,
	},
	/// An identifier was expected but a different token was found.
	ExpectedIdentifier { found: EcoString },
	/// A top-level declaration was expected but a different token was found.
	ExpectedDeclaration { found: EcoString },
	/// A struct/enum member was expected but a different token was found.
	ExpectedMember { found: EcoString },
	/// An interface member was expected but a different token was found.
	ExpectedInterfaceMember { found: EcoString },
}

impl IntoDiagnostic for ParseError {
	fn message(&self) -> EcoString {
		use ParseError as E;
		match self {
			E::ExpectedTypeFoundEof => "expected a type, found end of input".into(),
			E::ExpectedType { found } => format!("expected a type, found {found}").into(),
			E::ExpectedArrowInFnType => "expected `->` to complete a function type".into(),
			E::ExpectedPatternFoundEof => "expected a pattern, found end of input".into(),
			E::ExpectedPattern { found } => format!("expected a pattern, found {found}").into(),
			E::ExpectedNumberAfterMinus => "expected a number after `-` in a pattern".into(),
			E::InterpolationInPattern => "string interpolation is not allowed in patterns".into(),
			E::ExpectedExpressionFoundEof => "expected an expression, found end of input".into(),
			E::ExpectedExpression { found } => format!("expected an expression, found {found}").into(),
			E::ExpectedInterfaceName => "expected an interface name before `for`".into(),
			E::TrailingTokens => "unexpected trailing tokens after expression".into(),
			E::ExpectedToken { expected, found } => format!("expected {expected}, found {found}").into(),
			E::ExpectedIdentifier { found } => format!("expected an identifier, found {found}").into(),
			E::ExpectedDeclaration { found } => format!("expected a declaration, found {found}").into(),
			E::ExpectedMember { found } => {
				format!("expected a member (`func`, `let`, or `external`), found {found}").into()
			}
			E::ExpectedInterfaceMember { found } => {
				format!("expected an interface member, found {found}").into()
			}
		}
	}
}

/// A diagnostic produced while lexing. Because lexing runs on chumsky (whose
/// `Rich::custom` message is a `&'static str`), the lexer passes [`LexError::text`]
/// at the error site; the enum is still the single home for the message text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LexError {
	/// A `\u{…}` escape named a code point outside the Unicode range.
	InvalidUnicodeCodePoint,
	/// A `$N` closure parameter index did not fit in a `u8`.
	ClosureIndexTooLarge,
}

impl LexError {
	/// The message text, as a `&'static str` for chumsky's `Rich::custom`.
	pub fn text(self) -> &'static str {
		match self {
			LexError::InvalidUnicodeCodePoint => "invalid unicode code point",
			LexError::ClosureIndexTooLarge => {
				"closure parameter index is too large, must be smaller than 256"
			}
		}
	}
}

impl IntoDiagnostic for LexError {
	fn message(&self) -> EcoString {
		self.text().into()
	}
}
