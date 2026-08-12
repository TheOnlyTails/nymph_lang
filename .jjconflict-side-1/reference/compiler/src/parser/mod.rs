mod core;
mod cursor;
mod decl;
pub mod error;
mod expr;
mod pattern;
mod types;

use ecow::EcoString;

use crate::{
	ast::{Span, Spanned, declaration::Module},
	lexer::token::Token,
};

use self::{core::Parser, error::ParseError};

pub type ParseOutput = (Spanned<Module>, Vec<ParseError>);

pub fn parse(tokens: &[Spanned<Token>], eoi: Span, file_path: EcoString) -> ParseOutput {
	let mut parser = Parser::new(tokens, eoi, file_path);
	let module = parser.parse_module();
	let errors = parser.into_errors();
	(module, errors)
}

#[cfg(test)]
mod tests;
