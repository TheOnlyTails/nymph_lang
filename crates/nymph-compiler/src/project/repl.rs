//! Transactional compiler state for a persistent REPL.
//!
//! Each successful declaration is compiled as a small project module. Later
//! submissions import the currently visible declarations from those modules,
//! which preserves the lexical meaning of older declarations while allowing a
//! newer submission to shadow a name. A candidate is compiled through the
//! ordinary project pipeline and returned as staged state; the caller commits
//! it only after its JavaScript has also executed successfully.

use std::{
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
};

use nymph_ast::{
	decl::{Declaration, ImportRoot, Visibility},
	expr::{ListPatternEntry, MapPatternEntry, Pattern, StructPatternField},
};
use nymph_diagnostics::Diagnostic;
use nymph_sema::EntryMode;

use super::{CompiledProject, CompilerSession, ModulePath, ProjectDiagnostic, ProjectId};

const REPL_ROOT: &str = "__nymph_repl";

type SourceLoader = dyn Fn(&str) -> Option<String> + Send + Sync;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplInputStatus {
	Complete,
	Incomplete,
}

/// Classify whether a REPL buffer needs another line. This consumes the
/// lexer's/parser's typed end-of-input flags rather than inspecting blank lines
/// or rendered diagnostic strings.
#[must_use]
pub fn repl_input_status(source: &str) -> ReplInputStatus {
	if source.trim().is_empty() {
		return ReplInputStatus::Complete;
	}
	let module = nymph_syntax::parse_module(source, "<repl>");
	if module.diagnostics.is_empty() {
		return ReplInputStatus::Complete;
	}
	let expression = nymph_syntax::parse_expression(source);
	if expression.diagnostics.is_empty() {
		return ReplInputStatus::Complete;
	}
	if module.incomplete || expression.incomplete {
		ReplInputStatus::Incomplete
	} else {
		ReplInputStatus::Complete
	}
}

#[derive(Debug)]
pub enum ReplStageError {
	Incomplete,
	Diagnostics {
		diagnostics: Vec<ProjectDiagnostic>,
		module: String,
		source: String,
	},
}

impl ReplStageError {
	#[must_use]
	pub fn diagnostics(&self) -> Option<(&[ProjectDiagnostic], &str, &str)> {
		match self {
			Self::Incomplete => None,
			Self::Diagnostics {
				diagnostics,
				module,
				source,
			} => Some((diagnostics, module, source)),
		}
	}
}

#[derive(Clone)]
struct CommittedSubmission {
	module: String,
	source: String,
}

#[derive(Clone)]
struct ReplImport {
	root: ImportRoot,
	path: Vec<String>,
	alias: Option<String>,
	idents: Option<Vec<(String, Option<String>)>>,
	visible_names: BTreeSet<String>,
	hidden_suffix: String,
}

impl ReplImport {
	fn new(declaration: &Declaration, hidden_suffix: String) -> Self {
		let Declaration::Import {
			root,
			path,
			alias,
			idents,
		} = declaration
		else {
			unreachable!("REPL imports are constructed from import declarations")
		};
		let mut visible_names = BTreeSet::new();
		import_names(declaration, &mut visible_names);
		Self {
			root: root.clone(),
			path: path.iter().map(|part| part.0.to_string()).collect(),
			alias: alias.as_ref().map(|alias| alias.0.to_string()),
			idents: idents.as_ref().map(|idents| {
				idents
					.iter()
					.map(|(name, alias)| {
						(
							name.0.to_string(),
							alias.as_ref().map(|alias| alias.0.to_string()),
						)
					})
					.collect()
			}),
			visible_names,
			hidden_suffix,
		}
	}

	fn source(&self) -> String {
		let mut source = String::from("import ");
		match &self.root {
			ImportRoot::Project => source.push_str("@/"),
			ImportRoot::Current => source.push_str("./"),
			ImportRoot::Parent => source.push_str("../"),
			ImportRoot::Package(package) => {
				source.push_str(&package.0);
				if !self.path.is_empty() {
					source.push('/');
				}
			}
		}
		source.push_str(&self.path.join("/"));
		let namespace = self.alias.clone().or_else(|| {
			self.path.last().cloned().or_else(|| match &self.root {
				ImportRoot::Package(package) => Some(package.0.to_string()),
				ImportRoot::Project | ImportRoot::Current | ImportRoot::Parent => None,
			})
		});
		if let Some(namespace) = namespace {
			source.push_str(" as ");
			if self.visible_names.contains(&namespace) {
				source.push_str(&namespace);
			} else {
				source.push_str(&format!(
					"__nymph_repl_import_namespace_{}",
					self.hidden_suffix
				));
			}
		}
		if let Some(idents) = &self.idents {
			source.push_str(" with (");
			for (index, (name, alias)) in idents.iter().enumerate() {
				if index > 0 {
					source.push_str(", ");
				}
				source.push_str(name);
				let local = alias.as_ref().unwrap_or(name);
				if alias.is_some() || !self.visible_names.contains(local) {
					source.push_str(" as ");
					if self.visible_names.contains(local) {
						source.push_str(local);
					} else {
						source.push_str(&format!(
							"__nymph_repl_import_{index}_{}",
							self.hidden_suffix
						));
					}
				}
			}
			source.push(')');
		}
		source
	}
}

struct PreparedSubmission {
	body: String,
	declared: BTreeSet<String>,
	imports: Vec<ReplImport>,
	render_function: Option<String>,
}

/// A compiled candidate. Dropping it leaves the session unchanged. Call
/// [`ReplSession::commit`] only after [`Self::execution_js`] exits normally.
pub struct StagedReplSubmission {
	generation: usize,
	committed: CommittedSubmission,
	imports: Vec<ReplImport>,
	visible: BTreeMap<String, String>,
	compiled: CompiledProject,
	render_function: Option<String>,
}

impl StagedReplSubmission {
	/// Runnable JavaScript for this candidate. Expression results pass through a
	/// Nymph function constrained by `Debug`; JavaScript only prints the returned
	/// Nymph string payload.
	#[must_use]
	pub fn execution_js(&self) -> String {
		let mut js = self.compiled.js.clone();
		if let Some(render_function) = &self.render_function {
			let symbol = self.compiled.entry_symbol(render_function);
			js.push_str(&format!("\nconsole.log({symbol}().v);\n"));
		}
		js
	}

	#[must_use]
	pub fn renders_value(&self) -> bool {
		self.render_function.is_some()
	}
}

/// Persistent, project-aware REPL compilation state.
pub struct ReplSession {
	project: ProjectId,
	load: Arc<SourceLoader>,
	committed: Vec<CommittedSubmission>,
	imports: Vec<ReplImport>,
	visible: BTreeMap<String, String>,
}

impl ReplSession {
	#[must_use]
	pub fn new(load: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
		Self {
			project: ProjectId::new(REPL_ROOT),
			load: Arc::new(load),
			committed: Vec::new(),
			imports: Vec::new(),
			visible: BTreeMap::new(),
		}
	}

	#[must_use]
	pub fn loose() -> Self {
		Self::new(|_| None)
	}

	/// Parse and compile a candidate without changing committed state.
	pub fn stage(&self, input: &str) -> Result<StagedReplSubmission, ReplStageError> {
		if repl_input_status(input) == ReplInputStatus::Incomplete {
			return Err(ReplStageError::Incomplete);
		}

		let generation = self.committed.len();
		let module = format!("{REPL_ROOT}/submission_{generation}");
		let parsed_module = nymph_syntax::parse_module(input, &module);
		let parsed_expression = nymph_syntax::parse_expression(input);

		let prepared = if parsed_module.diagnostics.is_empty() && !parsed_module.tree.members.is_empty()
		{
			self.prepare_declarations(input, &parsed_module.tree.members, generation)?
		} else if parsed_expression.diagnostics.is_empty() {
			let render = format!("__nymph_repl_render_{generation}");
			PreparedSubmission {
				body: format!("public func {render}(): string = ({input}).debug()\n"),
				declared: BTreeSet::new(),
				imports: self.imports.clone(),
				render_function: Some(render),
			}
		} else {
			let diagnostics = parsed_module
				.diagnostics
				.into_iter()
				.map(|diag| ProjectDiagnostic {
					module: module.clone(),
					diag,
				})
				.collect();
			return Err(ReplStageError::Diagnostics {
				diagnostics,
				module,
				source: input.to_string(),
			});
		};

		let mut shadowed = prepared.declared.clone();
		shadowed.extend(
			prepared
				.imports
				.iter()
				.flat_map(|import| import.visible_names.iter().cloned()),
		);
		let source = self.module_source(generation, &prepared.imports, &shadowed, &prepared.body);
		let mut sources: BTreeMap<String, String> = self
			.committed
			.iter()
			.map(|submission| (submission.module.clone(), submission.source.clone()))
			.collect();
		sources.insert(module.clone(), source.clone());
		let disk = self.load.clone();
		let load = |key: &str| sources.get(key).cloned().or_else(|| disk(key));
		let session = CompilerSession::from_source_loaders(
			self.project.clone(),
			&module,
			&load,
			&crate::embedded_std_provider,
		);
		let compiled = session
			.compile_project(
				self.project.clone(),
				ModulePath::new(&module).expect("REPL module keys are canonical"),
				EntryMode::Library,
			)
			.map_err(|diagnostics| ReplStageError::Diagnostics {
				diagnostics: diagnostics.iter().cloned().collect(),
				module: module.clone(),
				source: source.clone(),
			})?;

		let mut visible = self.visible.clone();
		let imported_names: BTreeSet<_> = prepared
			.imports
			.iter()
			.flat_map(|import| import.visible_names.iter().cloned())
			.collect();
		visible.retain(|name, _| !imported_names.contains(name));
		for name in prepared.declared {
			visible.insert(name, module.clone());
		}
		Ok(StagedReplSubmission {
			generation,
			committed: CommittedSubmission { module, source },
			imports: prepared.imports,
			visible,
			compiled: compiled.as_ref().clone(),
			render_function: prepared.render_function,
		})
	}

	/// Commit a candidate after successful runtime execution.
	pub fn commit(&mut self, staged: StagedReplSubmission) {
		assert_eq!(
			staged.generation,
			self.committed.len(),
			"staged REPL submission is stale"
		);
		self.committed.push(staged.committed);
		self.imports = staged.imports;
		self.visible = staged.visible;
	}

	#[must_use]
	pub fn committed_submissions(&self) -> usize {
		self.committed.len()
	}

	fn prepare_declarations(
		&self,
		input: &str,
		declarations: &[Declaration],
		generation: usize,
	) -> Result<PreparedSubmission, ReplStageError> {
		if declarations
			.iter()
			.all(|decl| matches!(decl, Declaration::Import { .. }))
		{
			let mut imports = self.imports.clone();
			for (index, declaration) in declarations.iter().enumerate() {
				let import = ReplImport::new(declaration, format!("{generation}_{index}"));
				for previous in &mut imports {
					previous
						.visible_names
						.retain(|name| !import.visible_names.contains(name));
				}
				imports.push(import);
			}
			return Ok(PreparedSubmission {
				body: String::new(),
				declared: BTreeSet::new(),
				imports,
				render_function: None,
			});
		}
		if declarations.len() != 1 {
			return Err(self.local_error(
				generation,
				input,
				"enter one declaration per REPL submission",
			));
		}
		let declaration = &declarations[0];
		let mut declared = BTreeSet::new();
		declaration_names(declaration, &mut declared);
		let mut imports = self.imports.clone();
		for import in &mut imports {
			import.visible_names.retain(|name| !declared.contains(name));
		}
		let trimmed = input.trim();
		let body = match declaration_visibility(declaration) {
			None => format!("public {trimmed}\n"),
			Some(Visibility::Private) => {
				format!(
					"public {}\n",
					trimmed
						.strip_prefix("private")
						.unwrap_or(trimmed)
						.trim_start()
				)
			}
			Some(Visibility::Internal) => {
				format!(
					"public {}\n",
					trimmed
						.strip_prefix("internal")
						.unwrap_or(trimmed)
						.trim_start()
				)
			}
			Some(Visibility::Public) => format!("{trimmed}\n"),
		};
		Ok(PreparedSubmission {
			body,
			declared,
			imports,
			render_function: None,
		})
	}

	fn local_error(&self, generation: usize, source: &str, message: &str) -> ReplStageError {
		let module = format!("{REPL_ROOT}/submission_{generation}");
		ReplStageError::Diagnostics {
			diagnostics: vec![ProjectDiagnostic {
				module: module.clone(),
				diag: Diagnostic::error("REPL-INPUT".into(), message, 0..source.len()),
			}],
			module,
			source: source.to_string(),
		}
	}

	fn module_source(
		&self,
		generation: usize,
		imports: &[ReplImport],
		declared: &BTreeSet<String>,
		body: &str,
	) -> String {
		let mut source = String::new();
		for import in imports {
			source.push_str(&import.source());
			source.push('\n');
		}
		let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
		if generation > 0 {
			owners.insert(
				format!("{REPL_ROOT}/submission_{}", generation - 1),
				vec![format!("__nymph_repl_marker_{}", generation - 1)],
			);
		}
		for (name, owner) in &self.visible {
			if !declared.contains(name) {
				owners.entry(owner.clone()).or_default().push(name.clone());
			}
		}
		for (owner, names) in owners {
			source.push_str(&format!("import @/{owner} with ({})\n", names.join(", ")));
		}
		source.push_str(body);
		source.push_str(&format!(
			"public let __nymph_repl_marker_{generation} = #()\n"
		));
		source
	}
}

fn declaration_visibility(declaration: &Declaration) -> Option<Visibility> {
	match declaration {
		Declaration::Import { .. } => None,
		Declaration::Let { visibility, .. }
		| Declaration::Func { visibility, .. }
		| Declaration::TypeAlias { visibility, .. }
		| Declaration::Struct { visibility, .. }
		| Declaration::Enum { visibility, .. }
		| Declaration::Namespace { visibility, .. }
		| Declaration::Interface { visibility, .. }
		| Declaration::Impl { visibility, .. }
		| Declaration::ImplFor { visibility, .. } => *visibility,
		Declaration::ExternalLet(visibility, ..) | Declaration::ExternalFunc(visibility, ..) => {
			*visibility
		}
	}
}

fn declaration_names(declaration: &Declaration, names: &mut BTreeSet<String>) {
	match declaration {
		Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
			pattern_names(&meta.name.0, names);
		}
		Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
			names.insert(meta.name.0.to_string());
		}
		Declaration::TypeAlias { meta, .. } => {
			names.insert(meta.name.0.to_string());
		}
		Declaration::Struct { name, .. }
		| Declaration::Enum { name, .. }
		| Declaration::Namespace { name, .. }
		| Declaration::Interface { name, .. } => {
			names.insert(name.0.to_string());
		}
		Declaration::Import { .. } | Declaration::Impl { .. } | Declaration::ImplFor { .. } => {}
	}
}

fn import_names(declaration: &Declaration, names: &mut BTreeSet<String>) {
	let Declaration::Import {
		root,
		path,
		alias,
		idents,
	} = declaration
	else {
		return;
	};
	if let Some(alias) = alias {
		names.insert(alias.0.to_string());
	} else if let Some(last) = path.last() {
		names.insert(last.0.to_string());
	} else if let nymph_ast::decl::ImportRoot::Package(package) = root {
		names.insert(package.0.to_string());
	}
	if let Some(idents) = idents {
		for (name, alias) in idents {
			names.insert(alias.as_ref().unwrap_or(name).0.to_string());
		}
	}
}

fn pattern_names(pattern: &Pattern, names: &mut BTreeSet<String>) {
	match pattern {
		Pattern::Binding { name, inner } => {
			names.insert(name.0.to_string());
			pattern_names(&inner.0, names);
		}
		Pattern::List(entries) | Pattern::Tuple(entries) => {
			for entry in entries {
				match &entry.0 {
					ListPatternEntry::Item(pattern) => pattern_names(&pattern.0, names),
					ListPatternEntry::Rest(Some(name)) => {
						names.insert(name.0.to_string());
					}
					ListPatternEntry::Rest(None) => {}
				}
			}
		}
		Pattern::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapPatternEntry::Entry(key, value) => {
						pattern_names(&key.0, names);
						pattern_names(&value.0, names);
					}
					MapPatternEntry::Rest(Some(name)) => {
						names.insert(name.0.to_string());
					}
					MapPatternEntry::Rest(None) => {}
				}
			}
		}
		Pattern::Struct { fields, .. } => {
			for field in fields {
				match &field.0 {
					StructPatternField::Value { value, .. } | StructPatternField::Positional(value) => {
						pattern_names(&value.0, names)
					}
					StructPatternField::Named(name) => {
						names.insert(name.0.to_string());
					}
					StructPatternField::Rest => {}
				}
			}
		}
		Pattern::Union(left, right) => {
			pattern_names(&left.0, names);
			pattern_names(&right.0, names);
		}
		Pattern::Grouped(pattern) => pattern_names(&pattern.0, names),
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Range(_)
		| Pattern::Placeholder => {}
	}
}
