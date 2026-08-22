//! Import path resolution and whole-program module-graph discovery (IB1).
//!
//! `@/a/b` stays in the importer's package; `./x`/`../x` resolve relative to
//! the importing module's directory. Package-root imports retain the resolver
//! alias separately from the canonical, `/`-separated, extension-less module
//! path. This module never touches the filesystem.
//!
//! The canonical Salsa graph query owns traversal, diagnostics, and ordering;
//! this module owns only exact import-key normalization shared by source
//! acquisition and that graph.

use nymph_ast::{Ident, Span, decl::ImportRoot};
use nymph_diagnostics::Diagnostic;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ResolvedImportTarget {
	CurrentPackage(String),
	Package { alias: String, path: String },
	ImportableStd(String),
}

impl ResolvedImportTarget {
	pub(crate) fn loader_key(&self) -> Option<String> {
		match self {
			Self::CurrentPackage(path) => Some(path.clone()),
			Self::ImportableStd(path) => Some(format!("{STD_KEY_PREFIX}{path}")),
			Self::Package { .. } => None,
		}
	}

	pub(crate) fn current_package_path(&self) -> Option<&str> {
		match self {
			Self::CurrentPackage(path) => Some(path),
			Self::Package { .. } | Self::ImportableStd(_) => None,
		}
	}
}

impl std::fmt::Display for ResolvedImportTarget {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::CurrentPackage(path) => formatter.write_str(path),
			Self::Package { alias, path } => write!(formatter, "{alias}/{path}"),
			Self::ImportableStd(path) => write!(formatter, "std/{path}"),
		}
	}
}

/// The directory segments containing `key` (drops `key`'s own last segment —
/// its file name). `"geometry/vec"` → `["geometry"]`; `"main"` → `[]`.
pub(crate) fn dir_segments(key: &str) -> Vec<String> {
	let mut segs: Vec<String> = key.split('/').map(String::from).collect();
	segs.pop();
	segs
}

/// Resolve one `import`'s root+path against `importer_key` to a structured
/// package target, or a diagnostic for an escaping `../` or empty path.
/// Returning `Diagnostic` itself (not `Box`ed) matches every other
/// diagnostic-producing path in this driver (`ProjectDiagnostic` also stores
/// one inline) — this call isn't hot (once per `import` statement, at most a
/// few per module), so the larger `Err` variant clippy flags isn't a real
/// perf concern here.
///
/// `std/…` has its own compiler-reserved package identity. Its loader key is
/// `std::<path>` — `::` can never appear in an ordinary
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
) -> Result<ResolvedImportTarget, Diagnostic> {
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
			return Ok(ResolvedImportTarget::ImportableStd(joined));
		}
		ImportRoot::Package(name) => {
			if path.is_empty() {
				return Err(Diagnostic::error(
					"IMPORT-EMPTY-PATH".into(),
					"import path is empty",
					span,
				));
			}
			return Ok(ResolvedImportTarget::Package {
				alias: name.0.to_string(),
				path: path
					.iter()
					.map(|segment| segment.0.to_string())
					.collect::<Vec<_>>()
					.join("/"),
			});
		}
		ImportRoot::Project => {
			if importer_key.starts_with(STD_KEY_PREFIX) {
				vec![STD_KEY_PREFIX.to_string()]
			} else {
				Vec::new()
			}
		}
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
	if let Some(path) = resolved.strip_prefix(&format!("{STD_KEY_PREFIX}/")) {
		Ok(ResolvedImportTarget::ImportableStd(path.to_string()))
	} else {
		Ok(ResolvedImportTarget::CurrentPackage(resolved))
	}
}

/// The `std::`-key prefix a resolved `import std/…` target carries — see
/// [`resolve_import_target`]'s doc comment for why `::` is collision-proof
/// against any `/`-joined project key.
pub(crate) const STD_KEY_PREFIX: &str = "std::";

#[cfg(test)]
mod tests {
	use nymph_ast::decl::Declaration;

	use super::*;

	fn resolve(source: &str, importer: &str) -> ResolvedImportTarget {
		let parsed = nymph_syntax::parse_module(source, "test.nym");
		let Declaration::Import { root, path, .. } = &parsed.tree.members[0] else {
			panic!("expected import declaration");
		};
		resolve_import_target(root, path, importer, Span::new(0, 0)).unwrap()
	}

	#[test]
	fn builtin_relative_imports_remain_in_the_builtin_namespace() {
		assert_eq!(
			resolve("import ./x", "std::io"),
			ResolvedImportTarget::ImportableStd("x".into())
		);
		assert_eq!(
			resolve("import ./x", "std::collections/tree"),
			ResolvedImportTarget::ImportableStd("collections/x".into())
		);
		assert_eq!(
			resolve("import ../x", "std::collections/tree"),
			ResolvedImportTarget::ImportableStd("x".into())
		);
	}

	#[test]
	fn project_root_import_stays_in_the_current_reserved_package() {
		assert_eq!(
			resolve("import @/x", "std::collections/tree"),
			ResolvedImportTarget::ImportableStd("x".into())
		);
	}

	#[test]
	fn package_root_retains_alias_separately_from_module_path() {
		assert_eq!(
			resolve("import dependency/types", "main"),
			ResolvedImportTarget::Package {
				alias: "dependency".into(),
				path: "types".into(),
			}
		);
	}
}
