use std::{fs, path::PathBuf};

use salsa::Accumulator;

use crate::{
	ast::{Span, Spanned, declaration::{Declaration, Module}},
	db::{
		Db, DefId, Diagnostic, DiagnosticKind, Diagnostics, ImportSpec, ImportedIdent, ProjectConfig,
		SourceFile, TypeErrors,
	},
	lexer::{lexer, token::Token},
	parser::{self, error::ParseError},
	types::{Context, TypeChecker},
};
use chumsky::Parser;
use ecow::EcoString;

#[salsa::tracked]
pub fn lex_file(db: &dyn Db, file: SourceFile) -> Vec<Spanned<Token>> {
	let source = file.text(db);
	let (tokens, errors) = lexer().parse(source.as_str()).into_output_errors();

	for error in errors {
		let span = error.span();
		Diagnostics(Diagnostic {
			file_path: EcoString::from(file.path(db).as_str()),
			span: Span::new(span.start, span.end),
			message: error.to_string(),
			kind: DiagnosticKind::ParseError,
		})
		.accumulate(db);
	}

	tokens.unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct ParseResult {
	pub module: Option<Spanned<Module>>,
	pub errors: Vec<ParseError>,
}

#[salsa::tracked]
pub fn parse_file(db: &dyn Db, file: SourceFile) -> ParseResult {
	let tokens = lex_file(db, file);
	let source = file.text(db);
	let path = file.path(db);

	if tokens.is_empty() && source.is_empty() {
		return ParseResult {
			module: Some(Spanned(
				Module {
					members: vec![],
					path: EcoString::from(path.as_str()),
				},
				Span::new(0, 0),
			)),
			errors: vec![],
		};
	}

	let eoi = Span::new(source.len(), source.len());
	let (module, errors) = parser::parse(&tokens, eoi, EcoString::from(path.as_str()));

	for error in &errors {
		Diagnostics(Diagnostic {
			file_path: EcoString::from(file.path(db).as_str()),
			span: error.span,
			message: error.reason().to_string(),
			kind: DiagnosticKind::ParseError,
		})
		.accumulate(db);
	}

	ParseResult {
		module: Some(module),
		errors,
	}
}

#[salsa::tracked]
pub fn module_imports(db: &dyn Db, file: SourceFile) -> Vec<ImportSpec> {
	use crate::ast::declaration::Declaration;

	let result = parse_file(db, file);
	let Some(module) = result.module else {
		return vec![];
	};

	module
		.inner()
		.members
		.iter()
		.filter_map(|decl| match decl {
			Declaration::Import {
				root,
				path,
				idents,
			} => {
				let path_strings: Vec<String> =
					path.iter().map(|seg| seg.inner().to_string()).collect();

				let imported_idents = idents.as_ref().map(|ids| {
					ids.iter()
						.map(|(name, alias)| ImportedIdent {
							name: name.inner().to_string(),
							alias: alias.as_ref().map(|a| a.inner().to_string()),
							span: name.span(),
						})
						.collect()
				});

				let span = if let Some(last) = path.last() {
					Span::new(path[0].start(), last.end())
				} else {
					Span::new(0, 0)
				};

				Some(ImportSpec {
					root: root.clone(),
					path: path_strings,
					idents: imported_idents,
					span,
				})
			}
			_ => None,
		})
		.collect()
}

#[salsa::tracked]
pub fn resolve_import(
	db: &dyn Db,
	file: SourceFile,
	config: ProjectConfig,
	import: ImportSpec,
) -> Option<PathBuf> {
	use crate::ast::declaration::ImportRoot;

	let src_file_path = EcoString::from(file.path(db).as_str());

	let base_dir = match &import.root {
		ImportRoot::Package(_) => {
			Diagnostics(Diagnostic {
				file_path: src_file_path,
				span: import.span,
				message: "external package imports are not yet supported".to_string(),
				kind: DiagnosticKind::TypeError,
			})
			.accumulate(db);
			return None;
		}
		ImportRoot::Project => {
			let root = config.root(db);
			root.join("src")
		}
		ImportRoot::Current => {
			let path = file.path(db);
			let file_path = PathBuf::from(path.as_str());
			match file_path.parent() {
				Some(parent) => parent.to_path_buf(),
				None => {
					Diagnostics(Diagnostic {
						file_path: src_file_path,
						span: import.span,
						message: "cannot resolve relative import: file has no parent directory"
							.to_string(),
						kind: DiagnosticKind::TypeError,
					})
					.accumulate(db);
					return None;
				}
			}
		}
		ImportRoot::Parent => {
			let path = file.path(db);
			let file_path = PathBuf::from(path.as_str());
			match file_path.parent().and_then(|p| p.parent()) {
				Some(grandparent) => grandparent.to_path_buf(),
				None => {
					Diagnostics(Diagnostic {
						file_path: src_file_path,
						span: import.span,
						message: "cannot resolve parent import: file has no grandparent directory"
							.to_string(),
						kind: DiagnosticKind::TypeError,
					})
					.accumulate(db);
					return None;
				}
			}
		}
	};

	let mut module_path = base_dir;
	for segment in &import.path {
		module_path = module_path.join(segment);
	}

	let file_path = module_path.with_extension("nym");
	let dir_path = module_path.join("mod.nym");

	let file_exists = file_path.exists();
	let dir_exists = dir_path.exists();

	match (file_exists, dir_exists) {
		(true, true) => {
			Diagnostics(Diagnostic {
				file_path: src_file_path,
				span: import.span,
				message: format!(
					"ambiguous module: both {} and {} exist",
					file_path.display(),
					dir_path.display()
				),
				kind: DiagnosticKind::TypeError,
			})
			.accumulate(db);
			None
		}
		(true, false) => Some(file_path),
		(false, true) => Some(dir_path),
		(false, false) => {
			Diagnostics(Diagnostic {
				file_path: src_file_path,
				span: import.span,
				message: format!(
					"module not found: {}",
					import.path.join("/"),
				),
				kind: DiagnosticKind::TypeError,
			})
			.accumulate(db);
			None
		}
	}
}

#[salsa::tracked]
pub fn module_deps(
	db: &dyn Db,
	file: SourceFile,
	config: ProjectConfig,
) -> Vec<PathBuf> {
	let imports = module_imports(db, file);
	imports
		.into_iter()
		.filter_map(|import| resolve_import(db, file, config, import))
		.collect()
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct TypecheckResult {
	pub ctx: Context,
}

pub fn load_source_file(db: &dyn Db, path: String) -> SourceFile {
	let text = fs::read_to_string(&path).unwrap_or_default();
	SourceFile::new(db, path, text)
}

#[salsa::tracked]
pub fn module_items(db: &dyn Db, file: SourceFile) -> Vec<DefId<'_>> {
	let result = parse_file(db, file);
	let Some(module) = result.module else {
		return vec![];
	};

	(0..module.inner().members.len())
		.map(|i| DefId::new(db, file, i as u32))
		.collect()
}

#[salsa::tracked]
pub fn def_ast(db: &dyn Db, def: DefId<'_>) -> Declaration {
	let file = def.file(db);
	let index = def.index(db) as usize;
	let result = parse_file(db, file);
	result.module.unwrap().inner().members[index].clone()
}

#[salsa::tracked]
pub fn context_after(
	db: &dyn Db,
	file: SourceFile,
	config: ProjectConfig,
	n: u32,
) -> Context {
	if n == 0 {
		return Context::default();
	}

	let prev_ctx = context_after(db, file, config, n - 1);
	let def = DefId::new(db, file, n - 1);
	let decl = def_ast(db, def);

	let mut checker = TypeChecker::with_salsa(
		PathBuf::from(file.path(db).as_str()),
		config.root(db).clone(),
		prev_ctx.next_type_var_id,
	);

	let result = match &decl {
		Declaration::Import { root, path, idents } => {
			checker.check_import_salsa(db, file, config, root, path, idents.as_ref(), &prev_ctx)
		}
		_ => checker.check_declaration(&decl, &prev_ctx),
	};

	match result {
		Ok(mut ctx) => {
			ctx.next_type_var_id = checker.next_type_var_id;
			ctx
		}
		Err(err) => {
			let err_span = err.span();
			let err_file = err.file_path()
				.unwrap_or_else(|| EcoString::from(file.path(db).as_str()));
			Diagnostics(Diagnostic {
				file_path: err_file,
				span: Span::new(err_span.start, err_span.end),
				message: err.to_string(),
				kind: DiagnosticKind::TypeError,
			})
			.accumulate(db);
			TypeErrors(err).accumulate(db);
			prev_ctx
		}
	}
}

#[salsa::tracked]
pub fn typecheck_file(db: &dyn Db, file: SourceFile, config: ProjectConfig) -> TypecheckResult {
	let items = module_items(db, file);
	let n = items.len() as u32;
	let ctx = context_after(db, file, config, n);
	TypecheckResult { ctx }
}
