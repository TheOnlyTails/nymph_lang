use std::{
	collections::HashSet,
	fs,
	path::{Path, PathBuf},
	sync::OnceLock,
};

use rayon::prelude::*;
use salsa::Accumulator;

use crate::{
	ast::{
		Span, Spanned,
		declaration::{Declaration, Module},
	},
	db::{
		Db, DefId, Diagnostic, DiagnosticKind, Diagnostics, ImportSpec, ImportedIdent, ProjectConfig,
		SourceFile, TypeErrors,
	},
	lexer::{lexer, token::Token},
	parser::{self, error::ParseError},
	transpiler::{
		external::{bundled_external_module_name, find_external_module},
		transpile_with_imports,
	},
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

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
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
		.0
		.members
		.iter()
		.filter_map(|decl| match decl {
			Declaration::Import { root, path, idents } => {
				let path_strings: Vec<String> = path.iter().map(|seg| seg.0.to_string()).collect();

				let imported_idents = idents.as_ref().map(|ids| {
					ids
						.iter()
						.map(|(name, alias)| ImportedIdent {
							name: name.0.to_string(),
							alias: alias.as_ref().map(|a| a.0.to_string()),
							span: name.1,
						})
						.collect()
				});

				let span = if let Some(last) = path.last() {
					Span::new(path[0].1.start, last.1.end)
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
						message: "cannot resolve relative import: file has no parent directory".to_string(),
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
						message: "cannot resolve parent import: file has no grandparent directory".to_string(),
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
				message: format!("module not found: {}", import.path.join("/"),),
				kind: DiagnosticKind::TypeError,
			})
			.accumulate(db);
			None
		}
	}
}

#[salsa::tracked]
pub fn module_deps(db: &dyn Db, file: SourceFile, config: ProjectConfig) -> Vec<PathBuf> {
	let imports = module_imports(db, file);
	imports
		.into_iter()
		.filter_map(|import| resolve_import(db, file, config, import))
		.collect()
}

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub struct TypecheckResult {
	pub ctx: Context,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct BundledModule {
	pub source_path: PathBuf,
	pub output_path: PathBuf,
	pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct CopiedAsset {
	pub source_path: PathBuf,
	pub output_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct BundleResult {
	pub emitted_modules: Vec<BundledModule>,
	pub copied_assets: Vec<CopiedAsset>,
}

#[derive(Debug)]
struct FileBundleResult {
	source_path: PathBuf,
	emitted_module: Option<BundledModule>,
	copied_asset: Option<CopiedAsset>,
	diagnostics: Vec<Diagnostic>,
	type_errors: Vec<crate::types::error::TypeError>,
}

pub fn load_source_file(db: &dyn Db, path: String) -> SourceFile {
	let text = fs::read_to_string(&path).unwrap_or_default();
	SourceFile::new(db, path, text)
}

#[salsa::tracked]
pub fn project_source_files(db: &dyn Db, config: ProjectConfig) -> Vec<SourceFile> {
	let mut paths = vec![];
	collect_nymph_files(&config.root(db).join("src"), &mut paths);
	paths.sort();

	paths
		.into_iter()
		.map(|path| load_source_file(db, path.to_string_lossy().to_string()))
		.collect()
}

#[salsa::tracked]
pub fn transpile_project_file(
	db: &dyn Db,
	file: SourceFile,
	config: ProjectConfig,
) -> Option<BundledModule> {
	let source_path = PathBuf::from(file.path(db).as_str());
	let output_path = output_path_for_source(db, config, &source_path);
	transpile_compilation_unit(db, file, config, output_path)
}

#[salsa::tracked]
pub fn transpile_standalone_file(
	db: &dyn Db,
	file: SourceFile,
	config: ProjectConfig,
) -> Option<BundledModule> {
	let source_path = PathBuf::from(file.path(db).as_str());
	let output_path = source_path.with_extension("js");
	transpile_compilation_unit(db, file, config, output_path)
}

fn transpile_compilation_unit(
	db: &dyn Db,
	file: SourceFile,
	config: ProjectConfig,
	output_path: PathBuf,
) -> Option<BundledModule> {
	let parse_result = parse_file(db, file);
	let parse_errors = parse_file::accumulated::<Diagnostics>(db, file)
		.into_iter()
		.any(|diag| diag.0.kind == DiagnosticKind::ParseError);
	if parse_errors {
		return None;
	}

	let typecheck_result = typecheck_file(db, file, config);
	let has_type_errors = typecheck_file::accumulated::<TypeErrors>(db, file, config)
		.into_iter()
		.next()
		.is_some();
	if has_type_errors {
		return None;
	}

	let module = parse_result.module?;
	let source_path = PathBuf::from(file.path(db).as_str());
	let code = transpile_with_imports(db, file, config, &module.0, &typecheck_result.ctx);

	Some(BundledModule {
		source_path,
		output_path,
		code,
	})
}

#[salsa::tracked]
pub fn external_project_asset(
	db: &dyn Db,
	file: SourceFile,
	config: ProjectConfig,
) -> Option<CopiedAsset> {
	let source_path = PathBuf::from(file.path(db).as_str());
	let external_path = find_external_module(&source_path)?;

	Some(CopiedAsset {
		output_path: output_path_for_asset(db, config, &external_path),
		source_path: external_path,
	})
}

#[salsa::tracked]
pub fn bundle_project(db: &dyn Db, config: ProjectConfig) -> BundleResult {
	let mut paths = vec![];
	collect_nymph_files(&config.root(db).join("src"), &mut paths);
	paths.sort();

	let project_root = config.root(db).clone();
	let output_dir = config.output_dir(db).clone();
	let implicit_prelude = config.implicit_prelude(db);
	let mut file_results = compiler_thread_pool().install(|| {
		paths
			.into_par_iter()
			.map(|path| {
				bundle_project_file(
					path,
					project_root.clone(),
					output_dir.clone(),
					implicit_prelude,
				)
			})
			.collect::<Vec<_>>()
	});
	file_results.sort_by(|left, right| left.source_path.cmp(&right.source_path));

	let mut seen_diagnostics = HashSet::new();
	let mut seen_type_errors = HashSet::new();
	let mut emitted_modules = Vec::new();
	let mut copied_assets = Vec::new();

	for result in file_results {
		for diagnostic in result.diagnostics {
			if seen_diagnostics.insert(diagnostic.clone()) {
				Diagnostics(diagnostic).accumulate(db);
			}
		}

		for type_error in result.type_errors {
			if seen_type_errors.insert(type_error.clone()) {
				TypeErrors(type_error).accumulate(db);
			}
		}

		if let Some(module) = result.emitted_module {
			emitted_modules.push(module);
		}

		if let Some(asset) = result.copied_asset {
			copied_assets.push(asset);
		}
	}

	emitted_modules.sort_by(|left, right| left.source_path.cmp(&right.source_path));
	copied_assets.sort_by(|left, right| left.source_path.cmp(&right.source_path));

	compiler_thread_pool().install(|| {
		emitted_modules.par_iter().for_each(|module| {
			let _ = write_if_changed(&module.output_path, module.code.as_bytes());
		});
	});

	compiler_thread_pool().install(|| {
		copied_assets.par_iter().for_each(|asset| {
			if let Ok(contents) = fs::read(&asset.source_path) {
				let _ = write_if_changed(&asset.output_path, &contents);
			}
		});
	});

	BundleResult {
		emitted_modules,
		copied_assets,
	}
}

fn bundle_project_file(
	source_path: PathBuf,
	project_root: PathBuf,
	output_dir: PathBuf,
	implicit_prelude: bool,
) -> FileBundleResult {
	let db = crate::db::NymphDatabase::default();
	let file = load_source_file(&db, source_path.to_string_lossy().to_string());
	let config = ProjectConfig::new(&db, project_root, output_dir, implicit_prelude);
	let emitted_module = transpile_project_file(&db, file, config);
	let copied_asset = external_project_asset(&db, file, config);
	let diagnostics = transpile_project_file::accumulated::<Diagnostics>(&db, file, config)
		.into_iter()
		.map(|diagnostic| diagnostic.0.clone())
		.collect();
	let type_errors = transpile_project_file::accumulated::<TypeErrors>(&db, file, config)
		.into_iter()
		.map(|type_error| type_error.0.clone())
		.collect();

	FileBundleResult {
		source_path,
		emitted_module,
		copied_asset,
		diagnostics,
		type_errors,
	}
}

fn compiler_thread_pool() -> &'static rayon::ThreadPool {
	static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
	POOL.get_or_init(|| {
		rayon::ThreadPoolBuilder::new()
			.stack_size(64 * 1024 * 1024)
			.build()
			.expect("compiler rayon thread pool should build")
	})
}

fn collect_nymph_files(dir: &Path, files: &mut Vec<PathBuf>) {
	let Ok(entries) = fs::read_dir(dir) else {
		return;
	};

	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			collect_nymph_files(&path, files);
		} else if path.extension().is_some_and(|ext| ext == "nym") {
			files.push(path);
		}
	}
}

fn output_root(db: &dyn Db, config: ProjectConfig) -> PathBuf {
	let output_dir = config.output_dir(db);
	if output_dir.is_absolute() {
		output_dir.clone()
	} else {
		config.root(db).join(output_dir)
	}
}

fn output_path_for_source(db: &dyn Db, config: ProjectConfig, source_path: &Path) -> PathBuf {
	let relative = source_relative_path(db, config, source_path);
	output_root(db, config).join(relative).with_extension("js")
}

fn output_path_for_asset(db: &dyn Db, config: ProjectConfig, asset_path: &Path) -> PathBuf {
	let relative = source_relative_path(db, config, asset_path);
	let mut output_path = output_root(db, config).join(relative);
	if let Some(file_name) = bundled_external_module_name(asset_path) {
		output_path.set_file_name(file_name);
	}
	output_path
}

fn source_relative_path(db: &dyn Db, config: ProjectConfig, path: &Path) -> PathBuf {
	path
		.strip_prefix(config.root(db).join("src"))
		.map(PathBuf::from)
		.unwrap_or_else(|_| path.file_name().map(PathBuf::from).unwrap_or_default())
}

fn write_if_changed(path: &Path, contents: &[u8]) -> std::io::Result<()> {
	if let Ok(existing) = fs::read(path)
		&& existing == contents
	{
		return Ok(());
	}

	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}

	fs::write(path, contents)
}

#[salsa::tracked]
pub fn module_items(db: &dyn Db, file: SourceFile) -> Vec<DefId<'_>> {
	let result = parse_file(db, file);
	let Some(module) = result.module else {
		return vec![];
	};

	(0..module.0.members.len())
		.map(|i| DefId::new(db, file, i as u32))
		.collect()
}

#[salsa::tracked]
pub fn def_ast(db: &dyn Db, def: DefId<'_>) -> Declaration {
	let file = def.file(db);
	let index = def.index(db) as usize;
	let result = parse_file(db, file);
	result.module.unwrap().0.members[index].clone()
}

#[salsa::tracked]
pub fn context_after(db: &dyn Db, file: SourceFile, config: ProjectConfig, n: u32) -> Context {
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
		config.implicit_prelude(db),
	);

	let result = match &decl {
		Declaration::Import { root, path, idents } => checker.check_import_salsa(
			db,
			file,
			config,
			root,
			path,
			idents.as_ref().map(|it| it.as_slice()),
			&prev_ctx,
		),
		_ => checker.check_declaration(&decl, &prev_ctx),
	};

	match result {
		Ok(mut ctx) => {
			ctx.next_type_var_id = checker.next_type_var_id;
			ctx
		}
		Err(err) => {
			let err_span = err.span.clone();
			let err_file = err
				.file_path()
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
	let mut checker = TypeChecker::with_salsa(
		PathBuf::from(file.path(db).as_str()),
		config.root(db).clone(),
		0,
		config.implicit_prelude(db),
	);
	let ctx = checker.check_file_salsa(db, file, config);
	TypecheckResult { ctx }
}
