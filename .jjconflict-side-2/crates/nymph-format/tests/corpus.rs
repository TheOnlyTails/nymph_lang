use std::{fs, path::Path};

use nymph_format::format;
use nymph_syntax::parse_module;

#[test]
fn selected_repository_corpus_is_parseable_and_idempotent_after_formatting() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
	let mut files = Vec::new();
	let mut pending = vec![root.join("stdlib/src"), root.join("examples")];
	while let Some(directory) = pending.pop() {
		for entry in fs::read_dir(directory).expect("read corpus directory") {
			let path = entry.expect("read corpus entry").path();
			if path.is_dir() {
				pending.push(path);
			} else if path.extension().and_then(|extension| extension.to_str()) == Some("nym") {
				files.push(path);
			}
		}
	}
	files.sort();
	assert!(files.len() >= 20, "corpus unexpectedly small");
	for path in files {
		let relative = path.strip_prefix(&root).unwrap().to_string_lossy();
		let source = fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
		let output =
			format(&source, &relative).unwrap_or_else(|error| panic!("format {relative}: {error:?}"));
		assert_eq!(
			format(&output, &relative).expect("second pass"),
			output,
			"corpus file is not idempotent: {relative}"
		);
		let parsed = parse_module(&output, relative.as_ref());
		assert!(
			parsed.diagnostics.is_empty(),
			"formatted corpus did not parse: {relative}: {:?}",
			parsed.diagnostics
		);
	}
}
