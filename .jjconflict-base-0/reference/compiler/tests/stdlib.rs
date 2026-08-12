use ariadne::{Color, Config, Label, Report, ReportKind, Source};
use nymph_compiler::config::load_compiler_project_config;
use nymph_compiler::db::{DiagnosticKind, Diagnostics, NymphDatabase, SourceFile};
use nymph_compiler::queries::{parse_file, typecheck_file};
use std::fs::read_to_string;
use std::path::PathBuf;

#[test]
fn stdlib_tests() {
	let db = NymphDatabase::default();
	let stdlib_root = PathBuf::from("../stdlib")
		.canonicalize()
		.expect("stdlib directory not found");
	let config = load_compiler_project_config(&db, stdlib_root, PathBuf::from("dist"))
		.expect("expected stdlib config to load");

	for file in glob::glob("../stdlib/src/**/*.nym")
		.unwrap()
		.filter_map(Result::ok)
	{
		let abs_path = file.canonicalize().unwrap_or_else(|_| file.clone());
		let abs_str = abs_path.to_string_lossy().to_string();
		let source = read_to_string(&abs_path).unwrap();

		let sf = SourceFile::new(&db, abs_str.clone(), source.clone());

		// Check for parse errors
		let result = parse_file(&db, sf);
		let parse_diagnostics: Vec<_> = parse_file::accumulated::<Diagnostics>(&db, sf)
			.into_iter()
			.filter(|d| d.0.kind == DiagnosticKind::ParseError)
			.collect();

		if !parse_diagnostics.is_empty() {
			for diag in &parse_diagnostics {
				let d = &diag.0;
				eprintln!("Parse error in {abs_str}: {}", d.message);
			}
			panic!("Parse errors in {abs_str}");
		}
		assert!(result.module.is_some(), "Failed to parse {abs_str}");

		// Type-check using salsa (results are cached across files)
		let _tc_result = typecheck_file(&db, sf, config);
		let tc_diagnostics: Vec<_> = typecheck_file::accumulated::<Diagnostics>(&db, sf, config)
			.into_iter()
			.filter(|d| d.0.kind == DiagnosticKind::TypeError)
			.collect();

		if !tc_diagnostics.is_empty() {
			for diag in &tc_diagnostics {
				let d = &diag.0;
				let error_source = if d.file_path == abs_str.as_str() {
					source.clone()
				} else {
					read_to_string(d.file_path.as_str()).unwrap_or_default()
				};
				let report = Report::build(
					ReportKind::Error,
					(d.file_path.clone(), d.span.start..d.span.end),
				)
				.with_config(Config::new().with_tab_width(2))
				.with_message(&d.message)
				.with_label(
					Label::new((d.file_path.clone(), d.span.start..d.span.end))
						.with_message(&d.message)
						.with_color(Color::Red),
				)
				.finish();
				report
					.eprint((d.file_path.clone(), Source::from(error_source)))
					.unwrap();
			}
			panic!("Type check failed for {abs_str}");
		}
	}
}
