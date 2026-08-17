//! Every stdlib `.nym` file must parse cleanly. Full stdlib type-checking is
//! covered separately.

use std::path::PathBuf;

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
fn all_stdlib_files_parse() {
	let mut files = Vec::new();
	nym_files(&stdlib_dir(), &mut files);
	files.sort();
	let mut failures = Vec::new();
	for file in &files {
		let source = std::fs::read_to_string(file).unwrap();
		let parsed = parse_module(&source, file.to_str().unwrap());
		let errors: Vec<_> = parsed
			.diagnostics
			.iter()
			.filter(|d| d.is_error())
			.map(|d| format!("  {}: {}", file.display(), d.message))
			.collect();
		failures.extend(errors);
	}
	assert!(
		failures.is_empty(),
		"stdlib parse errors:\n{}",
		failures.join("\n")
	);
}
