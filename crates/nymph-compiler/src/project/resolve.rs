//! Import path resolution and whole-program module-graph discovery.
//!
//! `@/a/b` resolves against the *source root*; `./x`/`../x` resolve relative to
//! the *importing file's* directory. Every module is keyed by a canonical,
//! `/`-separated, extension-less path relative to the source root (e.g.
//! `"main"`, `"geometry/vec"`) — this is the key the caller's `load` closure
//! is asked for, and the key every diagnostic in this module is attributed
//! to. The CLI's loader turns a key into `<src-root>/<key>.nym` and reads it
//! from disk; a test's loader is just a lookup into a virtual
//! `FxHashMap<String, String>` — this module never touches the filesystem.
//!
//! The canonical Salsa graph query owns traversal, diagnostics, and ordering;
//! this module owns only exact import-key normalization shared by source
//! acquisition and that graph.

use nymph_ast::{Ident, Span, decl::ImportRoot};
use nymph_diagnostics::Diagnostic;

/// The directory segments containing `key` (drops `key`'s own last segment —
/// its file name). `"geometry/vec"` → `["geometry"]`; `"main"` → `[]`.
pub(crate) fn dir_segments(key: &str) -> Vec<String> {
	let mut segs: Vec<String> = key.split('/').map(String::from).collect();
	segs.pop();
	segs
}

/// Resolve one `import`'s root+path against `importer_key` to a canonical
/// module key, or a diagnostic (an escaping `../`, an empty path, or an
/// unsupported `pkg/` import; `std` is the only supported package root).
/// Returning `Diagnostic` itself (not `Box`ed) matches every other
/// diagnostic-producing path in this driver (`ProjectDiagnostic` also stores
/// one inline) — this call isn't hot (once per `import` statement, at most a
/// few per module), so the larger `Err` variant clippy flags isn't a real
/// perf concern here.
///
/// A `std/…` import resolves to `std::<path>`; `::` can never appear in an ordinary
/// `@/`/`./`/`../`-rooted key (those are always `/`-joined identifier
/// segments), so a `std` key can never collide with a project key, however
/// deeply nested (`import std/collections/tree` → `std::collections/tree`,
/// never confusable with a project module literally named
/// `std/collections/tree`, which would key as `std/collections/tree` — no
/// `::`). Source acquisition recognizes this prefix and loads the module
/// through the caller's standard-library provider instead of the project
/// loader.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_import_target(
	root: &ImportRoot,
	path: &[Ident],
	importer_key: &str,
	span: Span,
) -> Result<String, Diagnostic> {
	let mut segs: Vec<String> = match root {
		ImportRoot::Package(name) if name.0 == "std" => {
			if path.is_empty() {
				return Err(Diagnostic::error(
					"IMPORT-EMPTY-PATH".into(),
					"import path is empty",
					span,
				));
			}
			let joined = path
				.iter()
				.map(|p| p.0.to_string())
				.collect::<Vec<_>>()
				.join("/");
			return Ok(format!("std::{joined}"));
		}
		ImportRoot::Package(name) => {
			return Err(Diagnostic::error(
				"IMPORT-PACKAGE-UNSUPPORTED".into(),
				format!(
					"package imports (`pkg/{}`) are not supported yet — only project-local `@/`, `./`, and `../` imports (and `std/…`) are",
					name.0
				),
				span,
			));
		}
		// `@/` always denotes the caller-owned project root, including in
		// compiler-owned sources. Only relative imports inherit `std::`.
		ImportRoot::Project => Vec::new(),
		ImportRoot::Current => {
			let (builtin, key) = importer_key
				.strip_prefix(STD_KEY_PREFIX)
				.map_or((false, importer_key), |key| (true, key));
			let mut segments = dir_segments(key);
			if builtin {
				segments.insert(0, STD_KEY_PREFIX.to_string());
			}
			segments
		}
		ImportRoot::Parent => {
			let (builtin, key) = importer_key
				.strip_prefix(STD_KEY_PREFIX)
				.map_or((false, importer_key), |key| (true, key));
			let mut d = dir_segments(key);
			if d.pop().is_none() {
				return Err(Diagnostic::error(
					"IMPORT-ESCAPES-ROOT".into(),
					"this import escapes the source root (too many `../`)",
					span,
				));
			}
			if builtin {
				d.insert(0, STD_KEY_PREFIX.to_string());
			}
			d
		}
	};
	for seg in path {
		segs.push(seg.0.to_string());
	}
	if segs.is_empty() {
		return Err(Diagnostic::error(
			"IMPORT-EMPTY-PATH".into(),
			"import path is empty",
			span,
		));
	}
	let resolved = segs.join("/");
	Ok(resolved.replacen("std::/", STD_KEY_PREFIX, 1))
}

/// The `std::`-key prefix a resolved `import std/…` target carries — see
/// [`resolve_import_target`]'s doc comment for why `::` is collision-proof
/// against any `/`-joined project key.
pub(crate) const STD_KEY_PREFIX: &str = "std::";

#[cfg(test)]
mod tests {
	use nymph_ast::decl::Declaration;

	use super::*;

	fn resolve(source: &str, importer: &str) -> String {
		let parsed = nymph_syntax::parse_module(source, "test.nym");
		let Declaration::Import { root, path, .. } = &parsed.tree.members[0] else {
			panic!("expected import declaration");
		};
		resolve_import_target(root, path, importer, Span::new(0, 0)).unwrap()
	}

	#[test]
	fn builtin_relative_imports_remain_in_the_builtin_namespace() {
		assert_eq!(resolve("import ./x", "std::io"), "std::x");
		assert_eq!(
			resolve("import ./x", "std::collections/tree"),
			"std::collections/x"
		);
		assert_eq!(resolve("import ../x", "std::collections/tree"), "std::x");
	}

	#[test]
	fn project_root_import_in_builtin_source_keeps_project_root_semantics() {
		assert_eq!(resolve("import @/x", "std::collections/tree"), "x");
	}
}
