#![warn(clippy::all)]

use std::{
	env, fs,
	path::{Path, PathBuf},
};

use ariadne::{Color, Label, Report, ReportKind, Source};
use clap::{Args, Parser, Subcommand};
use ecow::EcoString;
use nymph_compiler::{
	VERSION,
	ast::{Spanned, declaration::Module},
	config::load_compiler_project_config,
	db::{Diagnostics, NymphDatabase, ProjectConfig, SourceFile, TypeErrors},
	queries::{bundle_project, parse_file, transpile_standalone_file, typecheck_file},
	transpiler::transpile,
	types::{self, TypeChecker, type_error_to_report},
};
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
	#[command(subcommand)]
	command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
	Build(BuildArgs),
}

#[derive(Args, Debug)]
struct BuildArgs {
	path: Option<PathBuf>,
	#[arg(short, long)]
	output: Option<PathBuf>,
}

const DEFAULT_OUTPUT_DIR: &str = "dist";

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReplProjectRoot {
	ExistingProject(PathBuf),
	BundledStdlib(PathBuf),
	CurrentDir(PathBuf),
}

fn main() -> anyhow::Result<()> {
	tracing_subscriber::fmt().init();

	let args = Cli::parse();

	match args.command {
		None => repl(),
		Some(Command::Build(args)) => build(args),
	}
}

fn repl() -> anyhow::Result<()> {
	println!("Nymph v{VERSION}");
	println!("type :help for more info");

	let mut editor = Reedline::create();
	let prompt = DefaultPrompt::new(
		DefaultPromptSegment::Basic("> ".into()),
		DefaultPromptSegment::Empty,
	);

	let mut accumulated_source = String::new();

	loop {
		let signal = editor.read_line(&prompt)?;
		match signal {
			Signal::Success(source) => match source.as_str() {
				":q" | ":quit" | ":exit" => break,
				":c" | ":clear" => print!("\x1B[2J\x1B[1;1H"),

				":h" | ":help" => println!(
					"Nymph REPL
:quit, :exit => exit the REPL
:clear => clear the screen
:help => print this message"
				),

				source => {
					let trial_source = format!("{accumulated_source}\n{source}");

					if let Some((module, ctx, has_errors)) = run("<stdin>", &trial_source)?
						&& !has_errors
					{
						accumulated_source = trial_source;
						let result = transpile(&module.0, &ctx, None);
						println!("{}", result.code);
					}
				}
			},
			_ => break,
		}
	}

	Ok(())
}

fn normalize_path(path: PathBuf) -> PathBuf {
	path.canonicalize().unwrap_or(path)
}

fn bundled_stdlib_root() -> Option<PathBuf> {
	let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../stdlib");
	root.join("src").is_dir().then(|| normalize_path(root))
}

fn resolve_repl_project_root(current_dir: &Path) -> ReplProjectRoot {
	if let Some(root) = TypeChecker::find_project_root(current_dir) {
		return ReplProjectRoot::ExistingProject(normalize_path(root));
	}

	if let Some(root) = bundled_stdlib_root() {
		return ReplProjectRoot::BundledStdlib(root);
	}

	ReplProjectRoot::CurrentDir(normalize_path(current_dir.to_path_buf()))
}

fn repl_project_config(db: &NymphDatabase) -> anyhow::Result<ProjectConfig> {
	let output_dir = PathBuf::from(DEFAULT_OUTPUT_DIR);
	let current_dir = env::current_dir()?;

	match resolve_repl_project_root(&current_dir) {
		ReplProjectRoot::ExistingProject(root) => load_compiler_project_config(db, root, output_dir),
		ReplProjectRoot::BundledStdlib(root) | ReplProjectRoot::CurrentDir(root) => {
			Ok(ProjectConfig::new(db, root, output_dir, true))
		}
	}
}

fn build(args: BuildArgs) -> anyhow::Result<()> {
	let path = match args.path {
		Some(path) => path,
		None => std::env::current_dir()?,
	};

	if path.is_dir() {
		build_project(path, args.output)
	} else {
		build_file(path, args.output)
	}
}

fn build_project(path: PathBuf, output: Option<PathBuf>) -> anyhow::Result<()> {
	let db = NymphDatabase::default();
	let project_root = path.canonicalize().unwrap_or(path);
	let output_dir = output.unwrap_or_else(|| PathBuf::from(DEFAULT_OUTPUT_DIR));
	let config = load_compiler_project_config(&db, project_root.clone(), output_dir)?;

	let result = bundle_project(&db, config);
	let diagnostics = bundle_project::accumulated::<Diagnostics>(&db, config);
	let type_errors = bundle_project::accumulated::<TypeErrors>(&db, config);

	print_diagnostics(diagnostics, None)?;
	print_type_errors(type_errors, None)?;

	println!(
		"emitted {} module(s) and copied {} asset(s) into {}",
		result.emitted_modules.len(),
		result.copied_assets.len(),
		project_output_dir(&db, config).display()
	);

	Ok(())
}

fn build_file(path: PathBuf, output: Option<PathBuf>) -> anyhow::Result<()> {
	let db = NymphDatabase::default();
	let abs_path = path.canonicalize().unwrap_or(path);
	let source = fs::read_to_string(&abs_path)?;
	let file = SourceFile::new(&db, abs_path.to_string_lossy().to_string(), source.clone());
	let project_root = TypeChecker::find_project_root(&abs_path)
		.unwrap_or_else(|| abs_path.parent().unwrap_or(&abs_path).to_path_buf());
	let config = load_compiler_project_config(&db, project_root, PathBuf::from(DEFAULT_OUTPUT_DIR))?;

	let result = transpile_standalone_file(&db, file, config);
	let default_filename = EcoString::from(abs_path.to_string_lossy().as_ref());
	let diagnostics = transpile_standalone_file::accumulated::<Diagnostics>(&db, file, config);
	let type_errors = transpile_standalone_file::accumulated::<TypeErrors>(&db, file, config);

	print_diagnostics(diagnostics, Some((&default_filename, source.as_str())))?;
	print_type_errors(type_errors, Some((&default_filename, source.as_str())))?;

	if let Some(module) = result {
		let output_path = output.unwrap_or(module.output_path);
		write_if_changed(&output_path, module.code.as_bytes())?;
		println!("emitted {}", output_path.display());
	}

	Ok(())
}

fn print_diagnostics(
	diagnostics: Vec<&Diagnostics>,
	default: Option<(&EcoString, &str)>,
) -> anyhow::Result<()> {
	use std::collections::BTreeMap;

	let mut by_file: BTreeMap<EcoString, Vec<&Diagnostics>> = BTreeMap::new();
	for d in diagnostics {
		let file_path = if d.0.file_path.is_empty() {
			default
				.map(|(filename, _)| filename.clone())
				.unwrap_or_default()
		} else {
			d.0.file_path.clone()
		};
		by_file.entry(file_path).or_default().push(d);
	}

	for (file_path, diags) in by_file {
		let source = load_source(&file_path, default);

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

fn print_type_errors(
	errors: Vec<&TypeErrors>,
	default: Option<(&EcoString, &str)>,
) -> anyhow::Result<()> {
	use std::collections::BTreeMap;

	let mut by_file: BTreeMap<EcoString, Vec<&TypeErrors>> = BTreeMap::new();
	for e in errors {
		let file_path = e
			.0
			.file_path()
			.or_else(|| default.map(|(filename, _)| filename.clone()))
			.unwrap_or_default();
		by_file.entry(file_path).or_default().push(e);
	}

	for (file_path, errs) in by_file {
		let source = load_source(&file_path, default);

		for e in errs {
			let report = type_error_to_report(file_path.clone(), &e.0);
			report.eprint((file_path.clone(), Source::from(source.as_str())))?;
		}
	}
	Ok(())
}

fn load_source(file_path: &EcoString, default: Option<(&EcoString, &str)>) -> String {
	if let Some((default_filename, default_source)) = default
		&& file_path == default_filename
	{
		return default_source.to_string();
	}

	fs::read_to_string(file_path.as_str()).unwrap_or_default()
}

fn project_output_dir(db: &NymphDatabase, config: ProjectConfig) -> PathBuf {
	let output_dir = config.output_dir(db);
	if output_dir.is_absolute() {
		output_dir.clone()
	} else {
		config.root(db).join(output_dir)
	}
}

fn write_if_changed(path: &Path, contents: &[u8]) -> std::io::Result<()> {
	if let Ok(existing) = fs::read(path)
		&& existing == contents
	{
		return Ok(());
	}

	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}

	fs::write(path, contents)
}

fn run(
	filename: &str,
	source: &str,
) -> anyhow::Result<Option<(Spanned<Module>, types::Context, bool)>> {
	let db = NymphDatabase::default();
	let file = SourceFile::new(&db, filename.to_string(), source.to_string());
	let config = if filename == "<stdin>" {
		repl_project_config(&db)?
	} else {
		load_compiler_project_config(&db, PathBuf::from("."), PathBuf::from(DEFAULT_OUTPUT_DIR))?
	};

	let result = parse_file(&db, file);
	let parse_diagnostics = parse_file::accumulated::<Diagnostics>(&db, file);
	let filename_eco = EcoString::from(filename);
	let mut has_errors = false;

	if !parse_diagnostics.is_empty() {
		has_errors = true;
		print_diagnostics(parse_diagnostics, Some((&filename_eco, source)))?;
	}

	if result.module.is_some() {
		let tc_result = typecheck_file(&db, file, config);
		let type_errors = typecheck_file::accumulated::<TypeErrors>(&db, file, config);
		if !type_errors.is_empty() {
			has_errors = true;
			print_type_errors(type_errors, Some((&filename_eco, source)))?;
		}
		return Ok(result.module.map(|m| (m, tc_result.ctx, has_errors)));
	}

	Ok(None)
}

#[cfg(test)]
mod tests {
	use std::{
		fs,
		path::PathBuf,
		time::{SystemTime, UNIX_EPOCH},
	};

	use ecow::EcoString;

	use super::{ReplProjectRoot, bundled_stdlib_root, normalize_path, resolve_repl_project_root};

	#[test]
	fn resolve_repl_project_root_prefers_local_project() {
		let project_root = unique_temp_dir("nymph-cli-repl-project");
		let nested_dir = project_root.join("src/repl");
		fs::create_dir_all(&nested_dir).expect("nested project directory should be creatable");
		fs::write(
			project_root.join("nymph.toml"),
			"name = 'fixture'\nversion = '0.1.0'\n",
		)
		.expect("project config should be writable");

		assert_eq!(
			resolve_repl_project_root(&nested_dir),
			ReplProjectRoot::ExistingProject(normalize_path(project_root.clone()))
		);

		let _ = fs::remove_dir_all(project_root);
	}

	#[test]
	fn resolve_repl_project_root_falls_back_to_bundled_stdlib() {
		let temp_dir = unique_temp_dir("nymph-cli-repl-standalone");
		let bundled_stdlib = bundled_stdlib_root().expect("bundled stdlib should exist in the repo");

		assert_eq!(
			resolve_repl_project_root(&temp_dir),
			ReplProjectRoot::BundledStdlib(bundled_stdlib)
		);

		let _ = fs::remove_dir_all(temp_dir);
	}

	#[test]
	fn run_uses_bundled_prelude_for_repl_input() {
		let (_, ctx, has_errors) = super::run(
			"<stdin>",
			"let x = Some(true)\nlet y = Option.Some(false)\n",
		)
		.expect("repl input should compile")
		.expect("repl input should parse");

		assert!(
			!has_errors,
			"expected repl input to type-check without errors"
		);
		assert!(ctx.lookup_type(&EcoString::from("x")).is_some());
		assert!(ctx.lookup_type(&EcoString::from("y")).is_some());
	}

	fn unique_temp_dir(prefix: &str) -> PathBuf {
		let unique = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("system clock should be after unix epoch")
			.as_nanos();
		let path = std::env::temp_dir().join(format!("{prefix}-{unique}"));
		fs::create_dir_all(&path).expect("temp directory should be creatable");
		path
	}
}
