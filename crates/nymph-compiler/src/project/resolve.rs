//! Import path resolution and whole-program module-graph discovery (IB1).
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
//! Resolution and parsing (this file) is phase 1 of the driver: a depth-first
//! walk from the entry module, discovering every transitively-imported module,
//! detecting cycles (a clean diagnostic, never a hang), and producing a
//! dependency-first (topological) order phase 2 (`super::bind`) consumes.

use ecow::EcoString;
use nymph_ast::{
	Ident, Span,
	decl::{Declaration, ImportRoot, Module},
};
use nymph_diagnostics::Diagnostic;
use rustc_hash::FxHashMap;

use super::ProjectDiagnostic;

/// One `import` statement, resolved to its target module key and carrying
/// everything phase 2 (binding) needs: the namespace it's always bound under
/// (the `as` alias, or the last path segment) and any `with (...)` names.
#[allow(dead_code)]
pub(crate) struct ParsedImport {
	pub target_key: String,
	pub ns_name: EcoString,
	pub ns_span: Span,
	pub has_with_list: bool,
	pub with_idents: Vec<(Ident, Option<Ident>)>,
}

/// One resolved-and-parsed module: its raw AST (import declarations still
/// present — phase 2 strips and desugars them) and its resolved import
/// edges. A diagnostic against this module is keyed by its canonical path
/// (see [`super::ProjectDiagnostic`]); rendering it against source text is
/// the caller's job — re-fetch it through the same `load` closure, since
/// that's exactly what this driver used to get it in the first place.
#[allow(dead_code)]
pub(crate) struct RawModule {
	pub tree: Module,
	pub imports: Vec<ParsedImport>,
}

/// The directory segments containing `key` (drops `key`'s own last segment —
/// its file name). `"geometry/vec"` → `["geometry"]`; `"main"` → `[]`.
pub(crate) fn dir_segments(key: &str) -> Vec<String> {
	let mut segs: Vec<String> = key.split('/').map(String::from).collect();
	segs.pop();
	segs
}

/// Resolve one `import`'s root+path against `importer_key` to a canonical
/// module key, or a diagnostic (an escaping `../`, an empty path, or an
/// unsupported `pkg/` import — packages other than `std` are a later slice).
/// Returning `Diagnostic` itself (not `Box`ed) matches every other
/// diagnostic-producing path in this driver (`ProjectDiagnostic` also stores
/// one inline) — this call isn't hot (once per `import` statement, at most a
/// few per module), so the larger `Err` variant clippy flags isn't a real
/// perf concern here.
///
/// `std/…` is the one package root resolved today (Slice B, core/std split):
/// its key is `std::<path>` — `::` can never appear in an ordinary
/// `@/`/`./`/`../`-rooted key (those are always `/`-joined identifier
/// segments), so a `std` key can never collide with a project key, however
/// deeply nested (`import std/collections/tree` → `std::collections/tree`,
/// never confusable with a project module literally named
/// `std/collections/tree`, which would key as `std/collections/tree` — no
/// `::`). [`GraphBuilder::visit_inner`] recognizes this prefix and loads the
/// module through the driver's `std_provider` instead of `load`.
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
	Gray,
	Black,
}

/// Depth-first module-graph builder: resolves and parses every module
/// transitively reachable from the entry, detecting cycles (a diagnostic
/// naming the cycle, never a hang — the gray/black coloring guarantees each
/// module is visited at most once) and recording a dependency-first
/// (post-order) traversal order.
pub(crate) struct GraphBuilder<'a> {
	load: &'a dyn Fn(&str) -> Option<String>,
	/// Resolves a `std/…` import's source: the argument is the key with its
	/// `std::` prefix already stripped (e.g. `collections/tree`, for `import
	/// std/collections/tree`) — see [`Self::visit_inner`]. Kept as a second,
	/// separate closure (rather than folding `std::`-handling into `load`
	/// itself) so every existing `load` closure (every test's virtual
	/// `FxHashMap` loader, the CLI's real filesystem loader) stays exactly as
	/// it was — nobody else needs to learn the `std::` key scheme.
	std_provider: &'a dyn Fn(&str) -> Option<String>,
	color: FxHashMap<String, Color>,
	stack: Vec<String>,
	pub modules: FxHashMap<String, RawModule>,
	pub order: Vec<String>,
	pub diags: Vec<ProjectDiagnostic>,
}

/// The `std::`-key prefix a resolved `import std/…` target carries — see
/// [`resolve_import_target`]'s doc comment for why `::` is collision-proof
/// against any `/`-joined project key. `pub(crate)` so [`Self::visit_inner`]
/// can strip it back off before handing the path to `std_provider`.
pub(crate) const STD_KEY_PREFIX: &str = "std::";

impl<'a> GraphBuilder<'a> {
	pub fn new(
		load: &'a dyn Fn(&str) -> Option<String>,
		std_provider: &'a dyn Fn(&str) -> Option<String>,
	) -> Self {
		Self {
			load,
			std_provider,
			color: FxHashMap::default(),
			stack: Vec::new(),
			modules: FxHashMap::default(),
			order: Vec::new(),
			diags: Vec::new(),
		}
	}

	fn err(&mut self, module: &str, diag: Diagnostic) {
		self.diags.push(ProjectDiagnostic {
			module: module.to_string(),
			diag,
		});
	}

	/// Visit `key`, resolving and parsing it (and everything it transitively
	/// imports) if not already visited. Returns whether `key`'s own subtree is
	/// clean (no cycle, no unresolved import, no parse error anywhere in it).
	pub fn visit(&mut self, key: &str) -> bool {
		self.visit_inner(key, None)
	}

	/// Same as [`Self::visit`], but for a recursive call made while resolving
	/// one `import` statement: `import_site` carries the *importer's* module
	/// key and the `import` statement's own span, so that if `key` turns out
	/// to not exist, the "unresolved import" diagnostic is attributed to the
	/// module that wrote the bad import (whose source exists and can be
	/// rendered) rather than to the nonexistent target. `None` means `key` is
	/// the entry module itself (no importer to blame), so a missing entry is
	/// attributed to itself.
	fn visit_inner(&mut self, key: &str, import_site: Option<(&str, Span)>) -> bool {
		match self.color.get(key) {
			Some(Color::Black) => return true,
			Some(Color::Gray) => {
				let start = self.stack.iter().position(|k| k == key).unwrap_or(0);
				let mut cycle: Vec<String> = self.stack[start..].to_vec();
				cycle.push(key.to_string());
				self.err(
					key,
					Diagnostic::error(
						"IMPORT-CYCLE".into(),
						format!("import cycle detected: {}", cycle.join(" -> ")),
						Span::new(0, 0),
					),
				);
				return false;
			}
			None => {}
		}
		self.color.insert(key.to_string(), Color::Gray);
		self.stack.push(key.to_string());

		let source = if let Some(stripped) = key.strip_prefix(STD_KEY_PREFIX) {
			(self.std_provider)(stripped)
		} else {
			(self.load)(key)
		};
		let Some(source) = source else {
			let (blame_module, blame_span) = import_site.unwrap_or((key, Span::new(0, 0)));
			self.err(
				blame_module,
				Diagnostic::error(
					"IMPORT-UNRESOLVED".into(),
					format!("module `{key}` could not be resolved (no source file found)"),
					blame_span,
				),
			);
			self.color.insert(key.to_string(), Color::Black);
			self.stack.pop();
			return false;
		};

		let display_path = format!("{key}.nym");
		let parsed = nymph_syntax::parse_module(&source, &display_path);
		let mut ok = true;
		for d in &parsed.diagnostics {
			if d.is_error() {
				self.err(key, d.clone());
				ok = false;
			}
		}

		let mut imports = Vec::new();
		for decl in &parsed.tree.members {
			if let Declaration::Import {
				root,
				path,
				alias,
				idents,
			} = decl
			{
				let span = alias
					.as_ref()
					.map(|a| a.1)
					.or_else(|| path.last().map(|p| p.1))
					.or_else(|| path.first().map(|p| p.1))
					.unwrap_or(Span::new(0, 0));
				match resolve_import_target(root, path, key, span) {
					Ok(target_key) => {
						let ns_name = match alias
							.clone()
							.map(|a| a.0)
							.or_else(|| path.last().map(|p| p.0.clone()))
						{
							Some(n) => n,
							None => {
								self.err(
									key,
									Diagnostic::error(
										"IMPORT-NO-NAMESPACE".into(),
										"import has no path segment to name its namespace (add `as <name>`)",
										span,
									),
								);
								ok = false;
								continue;
							}
						};
						let child_ok = self.visit_inner(&target_key, Some((key, span)));
						ok = ok && child_ok;
						imports.push(ParsedImport {
							target_key,
							ns_name,
							ns_span: span,
							has_with_list: idents.is_some(),
							with_idents: idents.clone().unwrap_or_default(),
						});
					}
					Err(diag) => {
						self.err(key, diag);
						ok = false;
					}
				}
			}
		}

		self.modules.insert(
			key.to_string(),
			RawModule {
				tree: parsed.tree,
				imports,
			},
		);
		self.color.insert(key.to_string(), Color::Black);
		self.stack.pop();
		if ok {
			self.order.push(key.to_string());
		}
		ok
	}
}

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
