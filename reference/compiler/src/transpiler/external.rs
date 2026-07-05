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

pub fn bundled_external_module_name(ext_path: &Path) -> Option<String> {
	let stem = ext_path.file_stem()?.to_string_lossy();
	let ext = ext_path.extension()?.to_string_lossy();
	Some(format!("{stem}.external.{ext}"))
}
