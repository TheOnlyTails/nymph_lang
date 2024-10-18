use ecow::EcoString;

use super::expr::{CharEscape, StringEscape};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum Error {
	// literal errors
	#[error("{0} cannot be used in a character literal")]
	InvalidEscapeSequenceInChar(CharEscape),
	#[error("{0} cannot be used in a string literal")]
	InvalidEscapeSequenceInString(StringEscape),
	#[error("\\u{0:X} is not a valid Unicode character code")]
	InvalidUnicodeEscape(u32),

	// declaration errors
	#[error("Variable declaration {name} is missing an initial value")]
	MissingValueFromLet { name: EcoString },
	#[error("Function declaration {name} is missing its body")]
	MissingBodyFromFunc { name: EcoString },
	#[error("Type alias declaration {name} is missing its value")]
	MissingValueFromTypeAlias { name: EcoString },
	#[error("An external declaration may not have a value")]
	ExternalDeclWithValue,
	#[error("Function declaration {name} in an interface is missing both a body and a return type")]
	NoReturnTypeOrBody { name: EcoString },

	// pattern errors
	#[error("Interpolating expressions in strings is not allowed in string literal patterns")]
	InterpolationInStringPattern,
	// type errors
}
