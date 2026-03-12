use std::path::{Path, PathBuf};

/// Given a `.nym` source file path, find the companion external module
/// (`.mjs`, `.cjs`, or `.ts`) that contains native JS implementations.
///
/// Returns the first path that exists.
pub fn find_external_module(nym_path: &Path) -> Option<PathBuf> {
	let stem = nym_path.with_extension("");
	for ext in &["mts", "mjs", "cts", "cjs", "ts", "js"] {
		let candidate = stem.with_extension(ext);
		if candidate.exists() {
			return Some(candidate);
		}
	}
	None
}

/// Build the JS export name for an external declaration.
///
/// Top-level declarations use their own name directly.
/// Declarations nested inside a struct/enum/interface use
/// `OuterName$decl_name` to avoid collisions.
pub fn external_export_name(outer: Option<&str>, decl_name: &str) -> String {
	match outer {
		Some(parent) => format!("{parent}${decl_name}"),
		None => decl_name.to_string(),
	}
}
