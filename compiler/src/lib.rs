#![feature(trait_alias)]
#![warn(clippy::all)]

use std::ops::Range;

use crate::{
	ast::{Spanned, declaration::Module},
	lexer::lexer,
	parser::{make_input, parser},
};
use ariadne::{Color, Label, Report, ReportKind};
use chumsky::Parser;
use itertools::Itertools;

pub mod ast;
pub(crate) mod lexer;
pub(crate) mod parser;

pub fn parse<'src>(
	filename: &'src str,
	source: &'src str,
) -> (
	Option<Spanned<Module>>,
	Vec<Report<'src, (&'src str, Range<usize>)>>,
) {
	let (tokens, lexer_errors) = lexer().parse(source).into_output_errors();

	let (module, parser_errors) = if let Some(tokens) = tokens {
		// info!("{tokens:?}");

		let (module, parser_errors) = parser(make_input)
			.parse(make_input((source.len()..source.len()).into(), &tokens))
			.into_output_errors();

		(
			module,
			parser_errors.into_iter().map(|e| e.into_owned()).collect(),
		)
	} else {
		(None, vec![])
	};

	let reports = lexer_errors
		.into_iter()
		.map(|e| e.map_token(|c| c.to_string()))
		.chain(
			parser_errors
				.into_iter()
				.map(|e| e.map_token(|tok| tok.to_string())),
		)
		.map(|e| {
			Report::build(ReportKind::Error, (filename, e.span().into_range()))
				.with_config(ariadne::Config::new().with_tab_width(2))
				.with_message(e.to_string())
				.with_label(
					Label::new((filename, e.span().into_range()))
						.with_message(e.reason())
						.with_color(Color::Red),
				)
				.with_labels(e.contexts().map(|(label, span)| {
					Label::new((filename, span.into_range()))
						.with_message(format!("while parsing this {label}"))
						.with_color(Color::Yellow)
				}))
				.finish()
		})
		.collect_vec();

	(module, reports)
}
