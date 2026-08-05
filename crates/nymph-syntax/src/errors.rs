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
use nymph_ast::Span;
use nymph_diagnostics::{ErrorCode, IntoDiagnostic, Label};
use nymph_errorcode::ErrorCode;

/// A diagnostic produced while parsing (after lexing).
#[derive(Clone, Debug, PartialEq, ErrorCode)]
#[error_code(1)]
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
	/// A `mut func` / `namespace func` / `namespace let` appeared at module top
	/// level, where there is no receiver type to attach it to.
	FuncKindOutsideType { kind: EcoString },
	/// A `mut func` / `namespace func` appeared inside a `namespace` block, which
	/// may only hold regular `func`s.
	FuncKindInNamespace { kind: EcoString },
	/// A `namespace let` was written `mut`. A static binding can never be mutable.
	MutableNamespaceLet,
	/// A struct/enum-variant field was written with a `mut` modifier
	/// (`mut n: int`). Field mutability is expressed on the field's *type*
	/// (`n: mut int`), not as a modifier on the field itself.
	MutFieldModifier,
	/// A string interpolation contained no expression.
	EmptyInterpolation,
	/// Tokens remained after the single expression in a string interpolation.
	TrailingInterpolationContent,
	/// An inclusive range (`a..=`) omitted its required upper bound.
	MissingInclusiveRangeUpperBound,
	/// Whitespace separated tokens that form a label's `@` edge.
	WhitespaceInLabel,
	/// The prefix and body labels on a closure did not agree.
	MismatchedClosureLabels {
		outer: EcoString,
		inner: EcoString,
		inner_span: Span,
	},
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
			E::FuncKindOutsideType { kind } => {
				format!("a `{kind}` is only valid inside a struct, enum, interface, or impl body").into()
			}
			E::FuncKindInNamespace { kind } => {
				format!("a `namespace` may only contain regular `func`s and `let`s, not a `{kind}`").into()
			}
			E::MutableNamespaceLet => "a `namespace let` is a static binding and cannot be `mut`".into(),
			E::MutFieldModifier => {
				"a field's mutability is written on its type (`n: mut int`), not as a `mut` field modifier"
					.into()
			}
			E::EmptyInterpolation => "string interpolation requires an expression".into(),
			E::TrailingInterpolationContent => {
				"unexpected trailing content in string interpolation".into()
			}
			E::MissingInclusiveRangeUpperBound => "an inclusive range needs an upper bound".into(),
			E::WhitespaceInLabel => "label syntax cannot contain whitespace around `@`".into(),
			E::MismatchedClosureLabels { outer, inner, .. } => {
				format!("closure labels must match: `{outer}` and `{inner}` differ").into()
			}
		}
	}

	fn labels(&self) -> Vec<Label> {
		match self {
			Self::MismatchedClosureLabels {
				inner, inner_span, ..
			} => vec![Label::new(
				*inner_span,
				format!("secondary label `{inner}`"),
			)],
			_ => Vec::new(),
		}
	}
}

/// A diagnostic produced while lexing. Because lexing runs on chumsky (whose
/// `Rich::custom` message is a `&'static str`), the lexer passes [`LexError::text`]
/// at the error site; the enum is still the single home for the message text.
#[derive(Clone, Debug, PartialEq, Eq, ErrorCode)]
#[error_code(0)]
pub enum LexError {
	ExpectedFound {
		found: Option<char>,
		expected: Vec<EcoString>,
	},
	/// A `\u{…}` escape named a code point outside the Unicode range.
	InvalidUnicodeCodePoint,
	/// A `$N` closure parameter index did not fit in a `u8`.
	ClosureIndexTooLarge,
}

impl IntoDiagnostic for LexError {
	fn message(&self) -> EcoString {
		match self {
			LexError::ExpectedFound { found, expected } => format!(
				"Expected {}{}",
				expected.join(", "),
				match found {
					Some(found) => format!(", Found {found}"),
					None => String::new(),
				}
			),
			LexError::InvalidUnicodeCodePoint => "invalid unicode code point".into(),
			LexError::ClosureIndexTooLarge => {
				"closure parameter index is too large, must be smaller than 256".into()
			}
		}
		.into()
	}
}
