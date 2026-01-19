use crate::ast::Span;
use ecow::EcoString;

use crate::lexer::token::Token;

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct ParseError {
	pub kind: ParseErrorKind,
	pub span: Span,
	pub context: Vec<(EcoString, Span)>,
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub enum ParseErrorKind {
	UnexpectedToken {
		found: Token,
		expected: Vec<EcoString>,
	},
	UnexpectedEof {
		expected: Vec<EcoString>,
	},
	ExpectedExpression {
		found: Token,
	},
	ExpectedPattern {
		found: Token,
	},
	ExpectedType {
		found: Token,
	},
	ExpectedDeclaration {
		found: Token,
	},
	ExpectedIdentifier {
		found: Token,
	},
	InvalidPattern {
		message: EcoString,
	},
	Custom {
		message: EcoString,
	},
}

impl ParseError {
	pub(crate) fn unexpected_token(found: Token, expected: Vec<EcoString>, span: Span) -> Self {
		Self {
			kind: ParseErrorKind::UnexpectedToken { found, expected },
			span,
			context: vec![],
		}
	}

	pub fn unexpected_eof(expected: Vec<EcoString>, span: Span) -> Self {
		Self {
			kind: ParseErrorKind::UnexpectedEof { expected },
			span,
			context: vec![],
		}
	}

	pub(crate) fn expected_expression(found: Token, span: Span) -> Self {
		Self {
			kind: ParseErrorKind::ExpectedExpression { found },
			span,
			context: vec![],
		}
	}

	pub(crate) fn expected_pattern(found: Token, span: Span) -> Self {
		Self {
			kind: ParseErrorKind::ExpectedPattern { found },
			span,
			context: vec![],
		}
	}

	pub(crate) fn expected_type(found: Token, span: Span) -> Self {
		Self {
			kind: ParseErrorKind::ExpectedType { found },
			span,
			context: vec![],
		}
	}

	pub(crate) fn expected_declaration(found: Token, span: Span) -> Self {
		Self {
			kind: ParseErrorKind::ExpectedDeclaration { found },
			span,
			context: vec![],
		}
	}

	pub(crate) fn expected_identifier(found: Token, span: Span) -> Self {
		Self {
			kind: ParseErrorKind::ExpectedIdentifier { found },
			span,
			context: vec![],
		}
	}

	pub fn custom(message: impl Into<EcoString>, span: Span) -> Self {
		Self {
			kind: ParseErrorKind::Custom {
				message: message.into(),
			},
			span,
			context: vec![],
		}
	}

	pub fn with_context(mut self, label: impl Into<EcoString>, span: Span) -> Self {
		self.context.push((label.into(), span));
		self
	}

	pub fn reason(&self) -> EcoString {
		match &self.kind {
			ParseErrorKind::UnexpectedToken { found, expected } => {
				if expected.is_empty() {
					format!("unexpected {found}").into()
				} else if expected.len() == 1 {
					format!("expected {}, found {found}", expected[0]).into()
				} else {
					let expected_str = expected
						.iter()
						.take(expected.len() - 1)
						.map(|s| s.as_str())
						.collect::<Vec<_>>()
						.join(", ");
					format!(
						"expected {expected_str}, or {}, found {found}",
						expected.last().unwrap()
					)
					.into()
				}
			}
			ParseErrorKind::UnexpectedEof { expected } => {
				if expected.is_empty() {
					"unexpected end of file".into()
				} else if expected.len() == 1 {
					format!("expected {}, found end of file", expected[0]).into()
				} else {
					let expected_str = expected
						.iter()
						.take(expected.len() - 1)
						.map(|s| s.as_str())
						.collect::<Vec<_>>()
						.join(", ");
					format!(
						"expected {expected_str}, or {}, found end of file",
						expected.last().unwrap()
					)
					.into()
				}
			}
			ParseErrorKind::ExpectedExpression { found } => {
				format!("expected an expression, found {found}").into()
			}
			ParseErrorKind::ExpectedPattern { found } => {
				format!("expected a pattern, found {found}").into()
			}
			ParseErrorKind::ExpectedType { found } => format!("expected a type, found {found}").into(),
			ParseErrorKind::ExpectedDeclaration { found } => {
				format!("expected a declaration, found {found}").into()
			}
			ParseErrorKind::ExpectedIdentifier { found } => {
				format!("expected an identifier, found {found}").into()
			}
			ParseErrorKind::InvalidPattern { message } | ParseErrorKind::Custom { message } => {
				message.clone()
			}
		}
	}
}

impl std::fmt::Display for ParseError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.reason())
	}
}

impl std::error::Error for ParseError {}
