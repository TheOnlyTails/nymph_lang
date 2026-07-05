#![feature(trait_alias)]
#![warn(clippy::all)]

use std::ops::Range;

use crate::{
	ast::{Spanned, declaration::Module},
	db::{DiagnosticKind, Diagnostics, NymphDatabase, SourceFile},
	queries::parse_file,
};
use ariadne::{Color, Label, Report, ReportBuilder, ReportKind};
use ecow::EcoString;
use itertools::Itertools;

pub mod ast;
pub mod config;
pub mod db;
pub(crate) mod lexer;
pub(crate) mod parser;
pub(crate) mod prelude;
pub mod queries;
pub(crate) mod resolver;
pub mod transpiler;
pub mod types;

pub type ParseResult<'src> = (
	Option<Spanned<Module>>,
	Vec<ReportBuilder<'src, (EcoString, Range<usize>)>>,
);

pub fn parse<'src>(filename: EcoString, source: &'src str) -> ParseResult<'src> {
	let db = NymphDatabase::default();
	let file = SourceFile::new(&db, filename.to_string(), source.to_string());

	let result = parse_file(&db, file);
	let diagnostics = parse_file::accumulated::<Diagnostics>(&db, file);

	let reports = diagnostics
		.into_iter()
		.filter(|d| d.0.kind == DiagnosticKind::ParseError)
		.map(|d| {
			let diag = &d.0;
			Report::build(
				ReportKind::Error,
				(filename.clone(), diag.span.start..diag.span.end),
			)
			.with_config(ariadne::Config::new().with_tab_width(2))
			.with_message(&diag.message)
			.with_label(
				Label::new((filename.clone(), diag.span.start..diag.span.end))
					.with_message(&diag.message)
					.with_color(Color::Red),
			)
		})
		.collect_vec();

	(result.module, reports)
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
