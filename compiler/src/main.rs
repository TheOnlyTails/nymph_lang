#![warn(clippy::all)]
#![feature(trait_alias, never_type)]

use std::{ffi::OsStr, path::PathBuf};

use crate::{
	lexer::lexer,
	parser::{make_input, parser},
};
use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::Parser;
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};
use tokio::{fs::read_to_string, io};
use tracing::info;

pub(crate) mod ast;
pub(crate) mod lexer;
pub(crate) mod parser;

#[derive(clap::Parser, Debug)]
#[command(version, about)]
struct NymphCli {
	filename: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	tracing_subscriber::fmt().init();

	let args = <NymphCli as clap::Parser>::parse();

	match args.filename {
		Some(filename) => {
			let Some(name) = filename.file_name().and_then(OsStr::to_str) else {
				return Err(io::Error::new(io::ErrorKind::InvalidFilename, "Invalid filename").into());
			};
			let source = read_to_string(filename.clone()).await?;

			run(name, source.as_str())?
		}
		None => {
			let mut editor = Reedline::create();
			let prompt = DefaultPrompt::new(
				DefaultPromptSegment::Basic("> ".into()),
				DefaultPromptSegment::Empty,
			);

			loop {
				let signal = editor.read_line(&prompt)?;
				match signal {
					Signal::Success(source) => match source.as_str() {
						":q" | ":quit" | ":exit" => break,
						":c" | ":clear" => print!("\x1B[2J\x1B[1;1H"), // ANSI sequence for clearing the screen
						source => run("<stdin>", source)?,
					},
					_ => break,
				}
			}
		}
	};

	Ok(())
}

fn run(filename: &str, source: &str) -> anyhow::Result<()> {
	let (tokens, lexer_errors) = lexer().parse(source).into_output_errors();

	let parser_errors = if let Some(tokens) = tokens {
		for token in tokens.clone() {
			info!("{token:?}");
		}

		let (module, parser_errors) = parser(make_input)
			.parse(make_input((source.len()..source.len()).into(), &tokens))
			.into_output_errors();

		if let Some(output) = module {
			info!("{output:#?}");
		}

		parser_errors.into_iter().map(|e| e.into_owned()).collect()
	} else {
		vec![]
	};

	let errors = lexer_errors
		.into_iter()
		.map(|e| e.map_token(|c| c.to_string()))
		.chain(
			parser_errors
				.into_iter()
				.map(|e| e.map_token(|tok| tok.to_string())),
		);

	for e in errors {
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
			.eprint((filename, Source::from(source)))?
	}

	Ok(())
}
