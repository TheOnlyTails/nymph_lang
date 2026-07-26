use std::sync::Arc;

use nymph_ast::{
	Span,
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
fn parse_builtin(db: &dyn Db, module: BuiltinModuleInput) -> Arc<ParsedModule> {
	parse_source(module.source(db), format!("std/{}.nym", module.key(db).0))
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
fn builtin_direct_imports(db: &dyn Db, module: BuiltinModuleInput) -> Arc<DirectImports> {
	collect_imports(
		&parse_builtin(db, module),
		&format!("std::{}", module.key(db).0),
	)
}

fn collect_imports(parsed: &ParsedModule, importer: &str) -> Arc<DirectImports> {
	let mut imports = Vec::new();
	for declaration in &parsed.tree.members {
		if let Declaration::Import {
			root, path, alias, ..
		} = declaration
		{
			let span = alias
				.as_ref()
				.map(|item| item.1)
				.or_else(|| path.last().map(|item| item.1))
				.unwrap_or(Span::new(0, 0));
			let target = resolve_import_target(root, path, importer, span);
			imports.push(DirectImport { target, span });
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
	let mut colors = BTreeMap::new();
	let mut order = Vec::new();
	let mut direct = Vec::new();
	let mut diagnostics = Vec::new();
	let mut stack = vec![(key.entry(db).to_string(), false, None::<(String, Span)>)];
	while let Some((path, exiting, import_site)) = stack.pop() {
		if exiting {
			colors.insert(path.clone(), Color::Black);
			if let Ok(path) = ModulePath::new(&path)
				&& let Some(module) = active.get(&path)
			{
				order.push(*module);
			}
			continue;
		}
		match colors.get(&path) {
			Some(Color::Black) => continue,
			Some(Color::Gray) => {
				diagnostics.push(ProjectDiagnostic {
					module: path.to_string(),
					diag: Diagnostic::error(
						"IMPORT-CYCLE".into(),
						format!("import cycle detected involving `{path}`"),
						Span::new(0, 0),
					),
				});
				continue;
			}
			None => {}
		}
		let builtin = path
			.strip_prefix(super::resolve::STD_KEY_PREFIX)
			.and_then(|stripped| {
				builtins
					.get(&BuiltinModuleKey(Arc::from(stripped)))
					.copied()
			});
		let project = ModulePath::new(&path)
			.ok()
			.and_then(|path| active.get(&path).copied());
		if builtin.is_none() && project.is_none() {
			let (blame, span) = import_site.unwrap_or((path.clone(), Span::new(0, 0)));
			diagnostics.push(ProjectDiagnostic {
				module: blame.to_string(),
				diag: Diagnostic::error(
					"IMPORT-UNRESOLVED".into(),
					format!("module `{path}` could not be resolved (no source file found)"),
					span,
				),
			});
			continue;
		}
		let parsed = builtin
			.map(|module| parse_builtin(db, module))
			.unwrap_or_else(|| parse(db, project.unwrap()));
		if parsed.diagnostics.iter().any(Diagnostic::is_error) {
			diagnostics.extend(
				parsed
					.diagnostics
					.iter()
					.filter(|item| item.is_error())
					.cloned()
					.map(|diag| ProjectDiagnostic {
						module: path.to_string(),
						diag,
					}),
			);
			continue;
		}
		colors.insert(path.clone(), Color::Gray);
		stack.push((path.clone(), true, None));
		let imports = builtin
			.map(|module| builtin_direct_imports(db, module))
			.unwrap_or_else(|| direct_imports(db, project.unwrap()));
		let mut handles = Vec::new();
		for import in imports.iter().rev() {
			let Ok(target_key) = &import.target else {
				diagnostics.push(ProjectDiagnostic {
					module: path.to_string(),
					diag: import.target.as_ref().unwrap_err().clone(),
				});
				continue;
			};
			if !target_key.starts_with(super::resolve::STD_KEY_PREFIX) {
				let target = ModulePath::new(target_key).expect("resolved local import is canonical");
				if let Some(handle) = active.get(&target) {
					handles.push(*handle);
				}
			}
			stack.push((target_key.clone(), false, Some((path.clone(), import.span))));
		}
		handles.reverse();
		if let Some(module) = project {
			direct.push((module, handles.into()));
		}
	}
	if !diagnostics.is_empty() {
		order.clear();
	}
	Arc::new(ProjectGraph {
		order: order.into(),
		direct: direct.into(),
		diagnostics: diagnostics.into(),
	})
}
