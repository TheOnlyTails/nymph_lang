//! Lexing and parsing for Nymph. The lexer turns source text into a flat token
//! stream; the parser (added in a later layer) turns tokens into a [`nymph_ast`] tree.

pub mod errors;
pub mod lexer;
pub mod parser;

pub use errors::{LexError, ParseError};
pub use lexer::{LexResult, lex};
pub use parser::{
	ModuleParseResult, ParseResult, parse_expression, parse_expression_tokens_from, parse_module,
	parse_module_tokens, parse_module_tokens_from, parse_module_tokens_from_with_ranges,
};
