use std::sync::Arc;

use nymph_ast::{
	Ident, Span,
	decl::{Declaration, Module},
};
use nymph_diagnostics::Diagnostic;

use super::{
	ProjectDiagnostic,
	resolve::resolve_import_target,
	session::{
		BuiltinModuleInput, BuiltinModuleKey, ModuleInput, ModulePath, ProjectInput, ProjectKey,
	},
};

#[salsa::db]
pub(crate) trait Db: salsa::Database {}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedModule {
	pub tree: Module,
	pub diagnostics: Arc<[Diagnostic]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectImport {
	pub target: Result<String, Diagnostic>,
	pub span: Span,
	pub namespace: Ident,
	pub with_idents: Vec<(Ident, Option<Ident>)>,
}

pub type DirectImports = [DirectImport];

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectGraph {
	pub order: Arc<[ModuleInput]>,
	#[allow(dead_code)]
	pub direct: Arc<[(ModuleInput, Arc<[ModuleInput]>)]>,
	pub diagnostics: Arc<[ProjectDiagnostic]>,
}

#[salsa::tracked]
pub(crate) fn parse(db: &dyn Db, module: ModuleInput) -> Arc<ParsedModule> {
	parse_source(
		module.source(db).unwrap_or_default(),
		format!("{}.nym", module.path(db)),
	)
}

#[salsa::tracked]
pub(crate) fn compat_parse_builtin(db: &dyn Db, module: BuiltinModuleInput) -> Arc<ParsedModule> {
	parse_source(module.source(db), format!("std::{}.nym", module.key(db).0))
}

fn parse_source(source: Arc<str>, path: String) -> Arc<ParsedModule> {
	let parsed = nymph_syntax::parse_module(&source, path);
	Arc::new(ParsedModule {
		tree: parsed.tree,
		diagnostics: parsed.diagnostics.into(),
	})
}

#[salsa::tracked]
pub(crate) fn direct_imports(db: &dyn Db, module: ModuleInput) -> Arc<DirectImports> {
	collect_imports(&parse(db, module), module.path(db).as_str())
}

#[salsa::tracked]
pub(crate) fn compat_builtin_direct_imports(
	db: &dyn Db,
	module: BuiltinModuleInput,
) -> Arc<DirectImports> {
	collect_imports(
		compat_parse_builtin(db, module),
		&format!("std::{}", module.key(db).0),
	)
}

fn collect_imports(parsed: &ParsedModule, importer: &str) -> Arc<DirectImports> {
	let mut imports = Vec::new();
	for declaration in &parsed.tree.members {
		if let Declaration::Import {
			root,
			path,
			alias,
			idents,
		} = declaration
		{
			let span = alias
				.as_ref()
				.map(|item| item.1)
				.or_else(|| path.last().map(|item| item.1))
				.unwrap_or(Span::new(0, 0));
			let target = resolve_import_target(root, path, importer, span);
			let namespace = alias
				.clone()
				.or_else(|| path.last().cloned())
				.unwrap_or_else(|| nymph_ast::Spanned("".into(), span));
			imports.push(DirectImport {
				target,
				span,
				namespace,
				with_idents: idents.clone().unwrap_or_default(),
			});
		}
	}
	imports.into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
	Gray,
	Black,
}

#[salsa::tracked]
pub(crate) fn project_graph<'db>(db: &'db dyn Db, key: ProjectKey<'db>) -> Arc<ProjectGraph> {
	use std::collections::BTreeMap;

	struct Walker<'a> {
		db: &'a dyn Db,
		active: &'a BTreeMap<ModulePath, ModuleInput>,
		builtins: &'a BTreeMap<BuiltinModuleKey, BuiltinModuleInput>,
		colors: BTreeMap<String, Color>,
		stack: Vec<String>,
		order: Vec<ModuleInput>,
		direct: Vec<(ModuleInput, Arc<[ModuleInput]>)>,
		diagnostics: Vec<ProjectDiagnostic>,
	}

	impl Walker<'_> {
		fn diagnostic(&mut self, module: &str, diag: Diagnostic) {
			self.diagnostics.push(ProjectDiagnostic {
				module: module.to_string(),
				diag,
			});
		}

		fn visit(&mut self, path: &str, import_site: Option<(&str, Span)>) -> bool {
			match self.colors.get(path) {
				Some(Color::Black) => return true,
				Some(Color::Gray) => {
					let start = self.stack.iter().position(|item| item == path).unwrap_or(0);
					let mut cycle = self.stack[start..].to_vec();
					cycle.push(path.to_string());
					self.diagnostic(
						path,
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
			self.colors.insert(path.to_string(), Color::Gray);
			self.stack.push(path.to_string());

			let builtin = path
				.strip_prefix(super::resolve::STD_KEY_PREFIX)
				.and_then(|stripped| {
					self
						.builtins
						.get(&BuiltinModuleKey(Arc::from(stripped)))
						.copied()
				});
			let project = ModulePath::new(path)
				.ok()
				.and_then(|module_path| self.active.get(&module_path).copied());
			if builtin.is_none() && project.is_none() {
				let (blame, span) = import_site.unwrap_or((path, Span::new(0, 0)));
				self.diagnostic(
					blame,
					Diagnostic::error(
						"IMPORT-UNRESOLVED".into(),
						format!("module `{path}` could not be resolved (no source file found)"),
						span,
					),
				);
				self.colors.insert(path.to_string(), Color::Black);
				self.stack.pop();
				return false;
			}

			let parsed = builtin
				.map(|module| compat_parse_builtin(self.db, module))
				.unwrap_or_else(|| parse(self.db, project.unwrap()));
			let mut ok = true;
			for diag in parsed.diagnostics.iter().filter(|diag| diag.is_error()) {
				self.diagnostic(path, diag.clone());
				ok = false;
			}
			let imports = builtin
				.map(|module| compat_builtin_direct_imports(self.db, module))
				.unwrap_or_else(|| direct_imports(self.db, project.unwrap()));
			let mut handles = Vec::new();
			for import in imports.iter() {
				match &import.target {
					Ok(target) => {
						if !target.starts_with(super::resolve::STD_KEY_PREFIX) {
							let target_path =
								ModulePath::new(target).expect("resolved local import is canonical");
							if let Some(handle) = self.active.get(&target_path) {
								handles.push(*handle);
							}
						}
						let child_ok = self.visit(target, Some((path, import.span)));
						ok = ok && child_ok;
					}
					Err(diag) => {
						self.diagnostic(path, diag.clone());
						ok = false;
					}
				}
			}
			if let Some(module) = project {
				self.direct.push((module, handles.into()));
			}
			self.colors.insert(path.to_string(), Color::Black);
			self.stack.pop();
			if ok && let Some(module) = project {
				self.order.push(module);
			}
			ok
		}
	}

	let project_input: ProjectInput = key.project_input(db);
	let active: BTreeMap<ModulePath, ModuleInput> = project_input
		.active_modules(db)
		.iter()
		.map(|module| (module.path(db).clone(), *module))
		.collect();
	let builtins: BTreeMap<BuiltinModuleKey, BuiltinModuleInput> = key
		.builtin_registry(db)
		.modules(db)
		.iter()
		.map(|module| (module.key(db), *module))
		.collect();
	let mut walker = Walker {
		db,
		active: &active,
		builtins: &builtins,
		colors: BTreeMap::new(),
		stack: Vec::new(),
		order: Vec::new(),
		direct: Vec::new(),
		diagnostics: Vec::new(),
	};
	walker.visit(key.entry(db).as_str(), None);
	Arc::new(ProjectGraph {
		order: walker.order.into(),
		direct: walker.direct.into(),
		diagnostics: walker.diagnostics.into(),
	})
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use nymph_sema::EntryMode;

	use super::*;
	use crate::project::{
		resolve::GraphBuilder,
		session::{BuiltinRegistryInput, ProjectId},
	};

	#[salsa::db]
	#[derive(Clone)]
	struct TestDb {
		storage: salsa::Storage<Self>,
	}
	#[salsa::db]
	impl salsa::Database for TestDb {}
	#[salsa::db]
	impl Db for TestDb {}

	fn fixture(files: &[(&str, &str)], builtins: &[(&str, &str)]) -> (TestDb, ProjectKey<'static>) {
		let db = TestDb {
			storage: salsa::Storage::default(),
		};
		let project = ProjectId::new("graph-regression");
		let modules: Arc<[ModuleInput]> = files
			.iter()
			.map(|(path, source)| {
				ModuleInput::new(
					&db,
					project.clone(),
					ModulePath::new(path).unwrap(),
					Some(Arc::from(*source)),
				)
			})
			.collect::<Vec<_>>()
			.into();
		let builtin_modules: Arc<[BuiltinModuleInput]> = builtins
			.iter()
			.map(|(key, source)| {
				BuiltinModuleInput::new(&db, BuiltinModuleKey(Arc::from(*key)), Arc::from(*source))
			})
			.collect::<Vec<_>>()
			.into();
		let input = ProjectInput::new(&db, project, modules);
		let registry = BuiltinRegistryInput::new(&db, builtin_modules);
		let key = ProjectKey::new(
			&db,
			input,
			registry,
			ModulePath::new("main").unwrap(),
			EntryMode::Entry,
			false,
		);
		// Test databases outlive each key in these tests; Salsa's key does not
		// contain an actual reference despite its invariant database lifetime.
		let key = unsafe { std::mem::transmute::<ProjectKey<'_>, ProjectKey<'static>>(key) };
		(db, key)
	}

	type DiagnosticTuple = (String, String, String, usize, usize);

	fn tuples(diags: &[ProjectDiagnostic]) -> Vec<DiagnosticTuple> {
		diags
			.iter()
			.map(|item| {
				(
					item.module.clone(),
					item.diag.code.to_string(),
					item.diag.message.to_string(),
					item.diag.span.start,
					item.diag.span.end,
				)
			})
			.collect()
	}

	fn assert_graph_matches_legacy(files: &[(&str, &str)]) -> Vec<DiagnosticTuple> {
		let sources: BTreeMap<_, _> = files.iter().copied().collect();
		let load = |key: &str| sources.get(key).map(ToString::to_string);
		let mut legacy = GraphBuilder::new(&load, &|_| None);
		assert!(!legacy.visit("main"));
		let expected = tuples(&legacy.diags);
		let (db, key) = fixture(files, &[]);
		assert_eq!(tuples(&project_graph(&db, key).diagnostics), expected);
		expected
	}

	#[test]
	fn graph_diagnostics_exactly_match_legacy_dfs_order_and_deduplication() {
		let cycle = assert_graph_matches_legacy(&[
			("main", "import @/a"),
			("a", "import @/b"),
			("b", "import @/a"),
		]);
		assert_eq!(cycle[0].0, "a");
		assert_eq!(cycle[0].1, "IMPORT-CYCLE");
		assert_eq!(cycle[0].2, "import cycle detected: a -> b -> a");

		let recovered =
			assert_graph_matches_legacy(&[("main", "import @/missing\nfunc broken(: int = 1")]);
		assert!(recovered.len() >= 2);
		assert_ne!(recovered[0].1, "IMPORT-UNRESOLVED");
		assert_eq!(recovered.last().unwrap().1, "IMPORT-UNRESOLVED");

		let mixed = assert_graph_matches_legacy(&[("main", "import pkg/nope\nimport @/missing")]);
		assert_eq!(
			mixed.iter().map(|item| item.1.as_str()).collect::<Vec<_>>(),
			["IMPORT-PACKAGE-UNSUPPORTED", "IMPORT-UNRESOLVED"]
		);

		let duplicate = assert_graph_matches_legacy(&[("main", "import @/missing\nimport @/missing")]);
		assert_eq!(
			duplicate
				.iter()
				.filter(|item| item.1 == "IMPORT-UNRESOLVED")
				.count(),
			1
		);
	}

	#[test]
	fn graph_ignores_unreachable_errors_and_preserves_clean_public_contracts() {
		let (db, key) = fixture(
			&[
				("main", "import @/a"),
				("a", "import @/b"),
				("b", "let value = 1"),
				("unreachable", "import @/missing"),
			],
			&[("tool", "public let answer = 42")],
		);
		let graph = project_graph(&db, key);
		assert!(graph.diagnostics.is_empty());
		assert_eq!(
			graph
				.order
				.iter()
				.map(|module| module.path(&db).as_str().to_string())
				.collect::<Vec<_>>(),
			["b", "a", "main"]
		);
		assert_eq!(graph.direct.len(), 3);
	}

	#[test]
	fn builtin_parse_uses_the_legacy_module_path_through_the_compat_query() {
		let (db, key) = fixture(&[("main", "")], &[("custom", "public let answer = 42")]);
		let builtin = key.builtin_registry(&db).modules(&db)[0];
		assert_eq!(
			compat_parse_builtin(&db, builtin).tree.path.as_str(),
			"std::custom.nym"
		);
	}
}
