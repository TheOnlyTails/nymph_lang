//! Lexing and parsing for Nymph. The lexer turns source text into a flat token
//! stream; the parser (added in a later layer) turns tokens into a [`nymph_ast`] tree.

pub mod errors;
pub mod lexer;
pub mod parser;

pub use errors::{LexError, ParseError};
pub use lexer::{LexResult, lex};
pub use parser::{ParseResult, parse_expression, parse_module};
