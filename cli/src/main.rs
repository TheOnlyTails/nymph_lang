#![warn(clippy::all)]

use std::{ffi::OsStr, fs::read_to_string, path::PathBuf};

use ariadne::{Color, Label, Report, ReportKind, Source};
use ecow::EcoString;
use nymph_compiler::{
	ast::{Spanned, declaration::Module},
	db::{DiagnosticKind, Diagnostics, NymphDatabase, ProjectConfig, SourceFile},
	queries::{parse_file, typecheck_file},
	types::TypeChecker,
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
					":c" | ":clear" => print!("\x1B[2J\x1B[1;1H"),
					source => {
						if let Some(module) = run("<stdin>", source)? {
							println!("{}", module.inner())
						}
					}
				},
				_ => break,
			}
		}
	} else {
		args.filename.par_iter().try_for_each(|filename| {
			let Some(name) = filename.file_name().and_then(OsStr::to_str) else {
				return Err::<_, anyhow::Error>(
					io::Error::new(io::ErrorKind::InvalidFilename, "Invalid filename").into(),
				);
			};
			let source = read_to_string(filename)?;
			let abs_path = filename.canonicalize().unwrap_or_else(|_| filename.clone());
			let _ = run_with_path(name, source.as_str(), abs_path);

			Ok(())
		})?;
	};

	Ok(())
}

fn print_diagnostics(
	diagnostics: Vec<&Diagnostics>,
	default_filename: &EcoString,
	default_source: &str,
) -> anyhow::Result<()> {
	use std::collections::BTreeMap;

	let mut by_file: BTreeMap<EcoString, Vec<&Diagnostics>> = BTreeMap::new();
	for d in diagnostics {
		let file_path = if d.0.file_path.is_empty() {
			default_filename.clone()
		} else {
			d.0.file_path.clone()
		};
		by_file.entry(file_path).or_default().push(d);
	}

	for (file_path, diags) in by_file {
		let source = if file_path == *default_filename {
			default_source.to_string()
		} else {
			std::fs::read_to_string(file_path.as_str()).unwrap_or_default()
		};

		for d in diags {
			let diag = &d.0;
			let report = Report::build(
				ReportKind::Error,
				(file_path.clone(), diag.span.start..diag.span.end),
			)
			.with_config(ariadne::Config::new().with_tab_width(2))
			.with_message(&diag.message)
			.with_label(
				Label::new((file_path.clone(), diag.span.start..diag.span.end))
					.with_message(&diag.message)
					.with_color(Color::Red),
			)
			.finish();
			report.eprint((file_path.clone(), Source::from(source.as_str())))?;
		}
	}
	Ok(())
}

fn run(filename: &str, source: &str) -> anyhow::Result<Option<Spanned<Module>>> {
	let db = NymphDatabase::default();
	let file = SourceFile::new(&db, filename.to_string(), source.to_string());
	let config = ProjectConfig::new(&db, PathBuf::from("."));

	let result = parse_file(&db, file);
	let parse_diagnostics = parse_file::accumulated::<Diagnostics>(&db, file);
	let filename_eco = EcoString::from(filename);

	if !parse_diagnostics.is_empty() {
		print_diagnostics(parse_diagnostics, &filename_eco, source)?;
	}

	if result.module.is_some() {
		let _tc_result = typecheck_file(&db, file, config);
		let tc_diagnostics = typecheck_file::accumulated::<Diagnostics>(&db, file, config);
		let type_errors: Vec<_> = tc_diagnostics
			.into_iter()
			.filter(|d| d.0.kind == DiagnosticKind::TypeError)
			.collect();
		if !type_errors.is_empty() {
			print_diagnostics(type_errors, &filename_eco, source)?;
		}
	}

	Ok(result.module)
}

fn run_with_path(
	filename: &str,
	source: &str,
	file_path: PathBuf,
) -> anyhow::Result<Option<Spanned<Module>>> {
	let db = NymphDatabase::default();
	let file = SourceFile::new(
		&db,
		file_path.to_string_lossy().to_string(),
		source.to_string(),
	);

	let project_root = TypeChecker::find_project_root(&file_path)
		.unwrap_or_else(|| file_path.parent().unwrap_or(&file_path).to_path_buf());
	let config = ProjectConfig::new(&db, project_root);

	let result = parse_file(&db, file);
	let parse_diagnostics = parse_file::accumulated::<Diagnostics>(&db, file);
	let filename_eco = EcoString::from(filename);

	if !parse_diagnostics.is_empty() {
		print_diagnostics(parse_diagnostics, &filename_eco, source)?;
	}

	if result.module.is_some() {
		let _tc_result = typecheck_file(&db, file, config);
		let tc_diagnostics = typecheck_file::accumulated::<Diagnostics>(&db, file, config);
		let type_errors: Vec<_> = tc_diagnostics
			.into_iter()
			.filter(|d| d.0.kind == DiagnosticKind::TypeError)
			.collect();
		if !type_errors.is_empty() {
			print_diagnostics(type_errors, &filename_eco, source)?;
		}
	}

	Ok(result.module)
}
