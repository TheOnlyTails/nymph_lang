use crate::{ast::types::Type, lexer::token::Token};
use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub(crate) enum ParserError<'ctx> {
	#[error("Found unexpected token: {0}")]
	UnexpectedToken(&'ctx Token),
	#[error("Tuple types begin with a hash `#()`")]
	WrongTupleSyntax(Vec<Type>),
}
