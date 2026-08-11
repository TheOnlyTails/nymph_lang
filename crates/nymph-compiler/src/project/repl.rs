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
use nymph_sema::query::ImportedNameKind;

use super::{CompilerSession, ModulePath, ProjectDiagnostic, ProjectId};

const REPL_ROOT: &str = "__nymph_repl";
const REPL_RESERVED_PREFIX: &str = "__nymph_repl_";

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
	wildcard_names: BTreeSet<String>,
	visible_names: BTreeSet<String>,
	hidden_suffix: String,
}

struct ResolvedImportNames {
	visible: BTreeSet<String>,
	all_visible: BTreeSet<String>,
	module_namespaces: BTreeSet<String>,
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
			wildcard_names: BTreeSet::new(),
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
		let wildcard_idents;
		let idents = if let Some(idents) = &self.idents {
			Some(idents)
		} else if !self.wildcard_names.is_empty() {
			wildcard_idents = self
				.wildcard_names
				.iter()
				.cloned()
				.map(|name| (name, None))
				.collect::<Vec<_>>();
			Some(&wildcard_idents)
		} else {
			None
		};
		if let Some(idents) = idents {
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
	modules: BTreeMap<String, String>,
	dependency_sources: BTreeMap<String, String>,
	entry: String,
	render_symbol: Option<String>,
}

impl StagedReplSubmission {
	/// Exact compiler-emitted ES modules for this candidate. The persistent REPL
	/// worker loads only modules absent from its committed module registry.
	#[must_use]
	pub fn modules(&self) -> &BTreeMap<String, String> {
		&self.modules
	}

	#[must_use]
	pub fn entry(&self) -> &str {
		&self.entry
	}

	#[must_use]
	pub fn render_symbol(&self) -> Option<&str> {
		self.render_symbol.as_deref()
	}

	#[must_use]
	pub fn renders_value(&self) -> bool {
		self.render_symbol.is_some()
	}
}

/// Persistent, project-aware REPL compilation state.
pub struct ReplSession {
	project: ProjectId,
	load: Arc<SourceLoader>,
	committed: Vec<CommittedSubmission>,
	imports: Vec<ReplImport>,
	visible: BTreeMap<String, String>,
	loaded_modules: BTreeMap<String, String>,
	dependency_sources: BTreeMap<String, String>,
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
			loaded_modules: BTreeMap::new(),
			dependency_sources: BTreeMap::new(),
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
		let dependency_sources = std::cell::RefCell::new(BTreeMap::new());
		let load = |key: &str| {
			sources.get(key).cloned().or_else(|| {
				let source = disk(key)?;
				dependency_sources
					.borrow_mut()
					.insert(key.to_string(), source.clone());
				Some(source)
			})
		};
		let session = CompilerSession::from_source_loaders(
			self.project.clone(),
			&module,
			&load,
			&crate::embedded_std_provider,
		);
		let (modules, _entry_tag) = session
			.emit_transactional_repl_project(
				self.project.clone(),
				ModulePath::new(&module).expect("REPL module keys are canonical"),
			)
			.map_err(|diagnostics| ReplStageError::Diagnostics {
				diagnostics: diagnostics.iter().cloned().collect(),
				module: module.clone(),
				source: source.clone(),
			})?;
		let dependency_sources = dependency_sources.into_inner();
		if let Some((key, _)) = dependency_sources.iter().find(|(key, source)| {
			self
				.dependency_sources
				.get(*key)
				.is_some_and(|committed| committed != *source)
		}) {
			return Err(self.local_error(
				generation,
				input,
				&format!("loaded project module `{key}` changed during this REPL session"),
			));
		}

		let mut modules = version_runtime_modules(modules);
		modules.retain(|key, _| !self.loaded_modules.contains_key(key));
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
		let render_symbol = prepared
			.render_function
			.map(|render| format!("$m{}${render}", super::queries::repl_module_tag(&module)));
		Ok(StagedReplSubmission {
			generation,
			committed: CommittedSubmission { module, source },
			imports: prepared.imports,
			visible,
			modules: modules.into_iter().collect(),
			dependency_sources,
			entry: format!("{REPL_ROOT}/submission_{generation}"),
			render_symbol,
		})
	}

	/// Commit a candidate after successful runtime execution.
	pub fn commit(&mut self, staged: StagedReplSubmission, retained_modules: &[String]) {
		assert_eq!(
			staged.generation,
			self.committed.len(),
			"staged REPL submission is stale"
		);
		self.committed.push(staged.committed);
		self.imports = staged.imports;
		self.visible = staged.visible;
		self
			.loaded_modules
			.extend(retained_modules.iter().filter_map(|key| {
				staged
					.modules
					.get(key)
					.map(|source| (key.clone(), source.clone()))
			}));
		self.dependency_sources.extend(
			staged
				.dependency_sources
				.into_iter()
				.filter(|(key, _)| retained_modules.contains(key)),
		);
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
				let mut import = ReplImport::new(declaration, format!("{generation}_{index}"));
				let resolved = self
					.resolved_import_names(&import, generation)
					.ok_or_else(|| {
						self.local_error(
							generation,
							input,
							"could not resolve the imported binding inventory",
						)
					})?;
				import.visible_names = resolved.visible;
				if let Some(name) = resolved
					.all_visible
					.iter()
					.find(|name| name.starts_with(REPL_RESERVED_PREFIX))
				{
					return Err(self.local_error(
						generation,
						input,
						&format!("`{name}` uses the reserved REPL identifier prefix"),
					));
				}
				if import.idents.is_none() {
					import.wildcard_names = import.visible_names.clone();
					for namespace in resolved.module_namespaces {
						import.wildcard_names.remove(&namespace);
					}
				}
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
		let mut reserved_names = declared.clone();
		if let Declaration::Enum { variants, .. } = declaration {
			reserved_names.extend(variants.iter().map(|variant| variant.0.name.0.to_string()));
		}
		if let Some(name) = reserved_names
			.iter()
			.find(|name| name.starts_with(REPL_RESERVED_PREFIX))
		{
			return Err(self.local_error(
				generation,
				input,
				&format!("`{name}` uses the reserved REPL identifier prefix"),
			));
		}
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

	fn resolved_import_names(
		&self,
		import: &ReplImport,
		generation: usize,
	) -> Option<ResolvedImportNames> {
		let module = format!("{REPL_ROOT}/submission_{generation}");
		let committed_sources: BTreeMap<String, String> = self
			.committed
			.iter()
			.map(|submission| (submission.module.clone(), submission.source.clone()))
			.collect();
		let disk = self.load.clone();
		(0..1024)
			.find_map(|index| {
				let source = format!(
					"{}\npublic let __nymph_repl_import_probe_{index} = #()\n",
					import.source()
				);
				let mut sources = committed_sources.clone();
				sources.insert(module.clone(), source);
				let load = |key: &str| sources.get(key).cloned().or_else(|| disk(key));
				let session = CompilerSession::from_source_loaders(
					self.project.clone(),
					&module,
					&load,
					&crate::embedded_std_provider,
				);
				let path = ModulePath::new(&module).expect("REPL module keys are canonical");
				let diagnostics = session.tooling_diagnostics(self.project.clone(), path.clone(), true);
				if diagnostics
					.iter()
					.any(|diagnostic| diagnostic.diag.code == "IMPORT-NAME-COLLISION")
				{
					return None;
				}
				if diagnostics
					.iter()
					.any(|diagnostic| diagnostic.diag.severity == nymph_diagnostics::Severity::Error)
				{
					return Some(None);
				}
				Some(
					session
						.tooling_completion_analysis(self.project.clone(), path.clone(), path, true)
						.map(|analysis| {
							let imported = analysis.imported_names.iter();
							ResolvedImportNames {
								all_visible: imported.clone().map(|name| name.name.clone()).collect(),
								visible: imported
									.clone()
									.filter(|name| name.kind != ImportedNameKind::Variant)
									.map(|name| name.name.clone())
									.collect(),
								module_namespaces: imported
									.filter(|name| name.kind == ImportedNameKind::ModuleNamespace)
									.map(|name| name.name.clone())
									.collect(),
							}
						}),
				)
			})
			.flatten()
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

/// Runtime assembly modules can gain newly demanded attachments as later REPL
/// submissions are compiled. Content-addressing those exact compiler outputs
/// lets the persistent worker retain old evaluated modules while loading only
/// the new runtime delta. Project/source module identities stay canonical.
fn version_runtime_modules(
	mut modules: std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
	let mut graph = modules
		.iter()
		.filter(|(key, _)| key.starts_with("@nymph/runtime/"))
		.collect::<Vec<_>>();
	graph.sort_unstable_by_key(|(key, _)| *key);
	let mut graph_source = String::new();
	for (key, source) in graph {
		graph_source.push_str(key);
		graph_source.push('\0');
		graph_source.push_str(source);
		graph_source.push('\0');
	}
	let graph_version = blake3::hash(graph_source.as_bytes()).to_hex();
	let versions = modules
		.iter()
		.filter(|(key, _)| key.starts_with("@nymph/runtime/"))
		.map(|(key, _)| (key.clone(), format!("{key}?repl={graph_version}")))
		.collect::<BTreeMap<_, _>>();
	for source in modules.values_mut() {
		for (original, versioned) in &versions {
			*source = source.replace(
				&format!("from \"{original}\""),
				&format!("from \"{versioned}\""),
			);
		}
	}
	for (original, versioned) in versions {
		if let Some(source) = modules.remove(&original) {
			modules.insert(versioned, source);
		}
	}
	modules
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
