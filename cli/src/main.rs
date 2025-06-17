#![warn(clippy::all)]

use std::{ffi::OsStr, fs::read_to_string, path::PathBuf};

use ariadne::Source;
use nymph_compiler::{
	ast::{Spanned, declaration::Module},
	parse,
};
use rayon::prelude::*;
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};
use tokio::io;

#[derive(clap::Parser, Debug)]
#[command(version, about)]
struct NymphCli {
	filename: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	tracing_subscriber::fmt().init();

	let args = <NymphCli as clap::Parser>::parse();

	if args.filename.is_empty() {
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
					source => {
						if let Some(module) = run("<stdin>", source)? {
							println!("{module:#?}")
						}
					}
				},
				_ => break,
			}
		}
	} else {
		let parsed = args
			.filename
			.par_iter()
			.map(|filename| {
				let Some(name) = filename.file_name().and_then(OsStr::to_str) else {
					return Err(io::Error::new(io::ErrorKind::InvalidFilename, "Invalid filename").into());
				};
				let source = read_to_string(filename)?;

				let module = run(name, source.as_str())?;

				anyhow::Ok(module)
			})
			.collect::<Result<Vec<_>, _>>()?;

		for module in parsed {
			if let Some(module) = module {
				println!("{module:#?}");
			}
		}
	};

	Ok(())
}

fn run(filename: &str, source: &str) -> anyhow::Result<Option<Spanned<Module>>> {
	let (module, reports) = parse(filename, source);

	for report in reports {
		report.eprint((filename, Source::from(source)))?
	}

	Ok(module)
}
