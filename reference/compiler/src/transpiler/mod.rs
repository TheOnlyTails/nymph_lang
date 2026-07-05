pub mod emit;
pub mod external;
pub mod operators;

#[cfg(test)]
mod tests;

use std::{
	collections::HashSet,
	path::{Path, PathBuf},
};

use oxc::{
	allocator::Allocator,
	ast::AstBuilder,
	codegen::{Codegen, CodegenOptions, CodegenReturn},
	span::{SPAN, SourceType},
};

use crate::{
	ast::{
		declaration::{Declaration, Module, Visibility},
		expr::Pattern,
	},
	db::{Db, ImportSpec, ImportedIdent, ProjectConfig, SourceFile},
	prelude::IMPLICIT_PRELUDE_MODULES,
	queries::{load_source_file, resolve_import, typecheck_file},
	types::{Context, ContextEntry, Type},
};

use emit::Emitter;

/// Transpile a type-checked Nymph module to ES6 JavaScript.
///
/// `module` is the parsed AST (from the parser).
/// `ctx` is the type-checking context (from the type checker).
/// `source_path` is the path to the `.nym` file being compiled,
/// used for resolving external declarations.
pub fn transpile(module: &Module, ctx: &Context, source_path: Option<&Path>) -> CodegenReturn {
	let allocator = Allocator::default();
	let ast = AstBuilder::new(&allocator);

	let mut emitter = Emitter::new(&allocator, ctx, source_path);
	let js_stmts = emitter.emit_module(module);

	// Build a JS Program node
	let program = ast.program(
		SPAN,
		SourceType::mjs(),
		"",
		ast.vec(),
		None,
		ast.vec(),
		js_stmts,
	);

	// Use OXC Codegen to print the JS AST
	Codegen::new()
		.with_options(CodegenOptions {
			single_quote: true,
			source_map_path: source_path.map(|it| it.to_path_buf()),
			..CodegenOptions::default()
		})
		.build(&program)
}

pub fn transpile_with_imports(
	db: &dyn Db,
	file: SourceFile,
	config: ProjectConfig,
	module: &Module,
	ctx: &Context,
) -> String {
	let mut code = emit_import_prelude(db, file, config, module);
	code.push_str(&transpile(module, ctx, Some(Path::new(file.path(db).as_str()))).code);
	code
}

fn emit_import_prelude(
	db: &dyn Db,
	file: SourceFile,
	config: ProjectConfig,
	module: &Module,
) -> String {
	let current_path = Path::new(file.path(db).as_str());
	let mut prelude = String::new();
	let bound_names = top_level_binding_names(module);

	for decl in &module.members {
		let Declaration::Import { root, path, idents } = decl else {
			continue;
		};

		let Some(module_name) = path.last().map(|segment| segment.0.as_str()) else {
			continue;
		};

		let import_spec = ImportSpec {
			root: root.clone(),
			path: path.iter().map(|segment| segment.0.to_string()).collect(),
			idents: idents.as_ref().map(|items| {
				items
					.iter()
					.map(|(name, alias)| ImportedIdent {
						name: name.0.to_string(),
						alias: alias.as_ref().map(|it| it.0.to_string()),
						span: name.1,
					})
					.collect()
			}),
			span: if let Some(last) = path.last() {
				crate::ast::Span::new(path[0].1.start, last.1.end)
			} else {
				crate::ast::Span::new(0, 0)
			},
		};

		let Some(target_path) = resolve_import(db, file, config, import_spec) else {
			continue;
		};
		let specifier = js_module_specifier(current_path, &target_path);

		prelude.push_str(&format!("import * as {module_name} from '{specifier}';\n"));

		if let Some(idents) = idents
			&& !idents.is_empty()
		{
			let named_imports = idents
				.iter()
				.map(|(name, alias)| match alias {
					Some(alias) => format!("{} as {}", name.0, alias.0),
					None => name.0.to_string(),
				})
				.collect::<Vec<_>>()
				.join(", ");

			prelude.push_str(&format!(
				"import {{ {named_imports} }} from '{specifier}';\n"
			));
		}
	}

	if !config.implicit_prelude(db) {
		return prelude;
	}
	if is_implicit_prelude_module(db, config, current_path) {
		return prelude;
	}

	for implicit in IMPLICIT_PRELUDE_MODULES {
		let Some(target_path) = project_module_path(config.root(db), implicit.path) else {
			continue;
		};
		if same_module_path(current_path, &target_path) {
			continue;
		}

		let import =
			public_implicit_prelude_import(db, config, &target_path, implicit.names, &bound_names);
		if import.names.is_empty() {
			continue;
		}

		let specifier = js_module_specifier(current_path, &target_path);
		prelude.push_str(&format!(
			"import {{ {} }} from '{specifier}';\n",
			import.names.join(", ")
		));
		for (enum_name, variant_names) in import.enum_variants {
			prelude.push_str(&format!(
				"const {{ {} }} = {enum_name};\n",
				variant_names.join(", ")
			));
		}
	}

	prelude
}

fn project_module_path(project_root: &Path, path: &[&str]) -> Option<PathBuf> {
	let mut module_path = project_root.join("src");
	for segment in path {
		module_path = module_path.join(segment);
	}

	let file_path = module_path.with_extension("nym");
	let dir_path = module_path.join("mod.nym");

	match (file_path.exists(), dir_path.exists()) {
		(true, false) => Some(file_path),
		(false, true) | (true, true) => Some(dir_path),
		(false, false) => None,
	}
}

fn is_implicit_prelude_module(db: &dyn Db, config: ProjectConfig, current_path: &Path) -> bool {
	IMPLICIT_PRELUDE_MODULES.iter().any(|module| {
		project_module_path(config.root(db), module.path)
			.as_ref()
			.is_some_and(|path| same_module_path(current_path, path))
	})
}

#[derive(Default)]
struct ImplicitPreludeImport {
	names: Vec<String>,
	enum_variants: Vec<(String, Vec<String>)>,
}

fn public_implicit_prelude_import(
	db: &dyn Db,
	config: ProjectConfig,
	target_path: &Path,
	candidates: &[&str],
	bound_names: &HashSet<String>,
) -> ImplicitPreludeImport {
	let file = load_source_file(db, target_path.to_string_lossy().to_string());
	let module_ctx = typecheck_file(db, file, config).ctx;
	let mut import = ImplicitPreludeImport::default();

	for name in candidates
		.iter()
		.copied()
		.filter(|name| !bound_names.contains(*name))
	{
		let Some(entry) = module_ctx.local_ctx.get(name) else {
			continue;
		};
		let visibility = match entry {
			ContextEntry::Value(value) => value.visibility,
			ContextEntry::Impl { parent, .. } => parent.visibility,
		};

		if visibility != Visibility::Public {
			continue;
		}

		import.names.push(name.to_string());

		let ContextEntry::Value(value) = entry else {
			continue;
		};
		let Type::Enum { variants, .. } = &value.type_ else {
			continue;
		};
		let variant_names = variants
			.keys()
			.filter(|variant_name| !bound_names.contains(variant_name.as_str()))
			.map(|variant_name| variant_name.to_string())
			.collect::<Vec<_>>();

		if !variant_names.is_empty() {
			import.enum_variants.push((name.to_string(), variant_names));
		}
	}

	import
}

fn top_level_binding_names(module: &Module) -> HashSet<String> {
	let mut names = HashSet::new();

	for decl in &module.members {
		match decl {
			Declaration::Import { path, idents, .. } => {
				if let Some(module_name) = path.last() {
					names.insert(module_name.0.to_string());
				}

				if let Some(idents) = idents {
					for (name, alias) in idents {
						names.insert(
							alias
								.as_ref()
								.map(|it| it.0.to_string())
								.unwrap_or_else(|| name.0.to_string()),
						);
					}
				}
			}
			Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
				if let Some(name) = top_level_pattern_name(&meta.name.0) {
					names.insert(name.to_string());
				}
			}
			Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
				names.insert(meta.name.0.to_string());
			}
			Declaration::Struct { name, .. }
			| Declaration::Enum { name, .. }
			| Declaration::Interface { name, .. }
			| Declaration::Namespace { name, .. } => {
				names.insert(name.0.to_string());
			}
			Declaration::TypeAlias { meta, .. } => {
				names.insert(meta.name.0.to_string());
			}
			Declaration::ImplFor { .. } | Declaration::Impl { .. } => {}
		}
	}

	names
}

fn top_level_pattern_name(pattern: &Pattern) -> Option<&str> {
	match pattern {
		Pattern::Binding { name, .. } => Some(name.0.as_ref()),
		Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
			Some(path[0].0.as_ref())
		}
		_ => None,
	}
}

fn same_module_path(left: &Path, right: &Path) -> bool {
	let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
	let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
	left == right
}

fn js_module_specifier(from: &Path, to: &Path) -> String {
	let from_dir = from.parent().unwrap_or_else(|| Path::new(""));
	let target = to.with_extension("js");
	let relative = relative_path(from_dir, &target);
	let specifier = relative.to_string_lossy().replace('\\', "/");

	if specifier.starts_with('.') {
		specifier
	} else {
		format!("./{specifier}")
	}
}

fn relative_path(from: &Path, to: &Path) -> std::path::PathBuf {
	use std::path::Component;

	let from_components = from.components().collect::<Vec<_>>();
	let to_components = to.components().collect::<Vec<_>>();

	let common_len = from_components
		.iter()
		.zip(&to_components)
		.take_while(|(left, right)| left == right)
		.count();

	let mut relative = std::path::PathBuf::new();

	for component in &from_components[common_len..] {
		if matches!(component, Component::Normal(_)) {
			relative.push("..");
		}
	}

	for component in &to_components[common_len..] {
		relative.push(component.as_os_str());
	}

	relative
}
