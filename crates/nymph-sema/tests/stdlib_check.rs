//! Milestone B acceptance test: the whole refined `stdlib/src/*.nym`, checked as one
//! program, produces **zero** diagnostics (no errors and no warnings).

use std::path::PathBuf;

use nymph_sema::check_program;
use nymph_syntax::parse_module;

fn stdlib_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../stdlib/src")
		.canonicalize()
		.unwrap()
}

fn nym_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
	for entry in std::fs::read_dir(dir).unwrap() {
		let path = entry.unwrap().path();
		if path.is_dir() {
			nym_files(&path, out);
		} else if path.extension().is_some_and(|e| e == "nym") {
			out.push(path);
		}
	}
}

#[test]
fn stdlib_typechecks_cleanly() {
	let mut files = Vec::new();
	nym_files(&stdlib_dir(), &mut files);
	files.sort();

	let modules: Vec<_> = files
		.iter()
		.map(|f| {
			let source = std::fs::read_to_string(f).unwrap();
			let parsed = parse_module(&source, f.to_str().unwrap());
			let parse_errors: Vec<_> = parsed
				.diagnostics
				.iter()
				.filter(|d| d.is_error())
				.map(|d| format!("{}: {}", f.display(), d.message))
				.collect();
			assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}");
			parsed.tree
		})
		.collect();

	let diagnostics: Vec<String> = check_program(&modules)
		.diags
		.iter()
		.map(|d| {
			let kind = if d.is_error() { "error" } else { "warning" };
			format!("{kind}: {}", d.message)
		})
		.collect();

	assert!(
		diagnostics.is_empty(),
		"expected the stdlib to typecheck cleanly, got {} diagnostic(s):\n{}",
		diagnostics.len(),
		diagnostics.join("\n")
	);
}
