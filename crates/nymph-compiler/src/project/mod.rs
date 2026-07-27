//! The multi-module project driver (Slice IB1: import binding).
//!
//! A Nymph *project* is many `.nym` files reachable from one entry module via
//! `import`. This module is the whole-program compiler for that shape,
//! layered entirely on top of the existing single-module facade
//! (`nymph_sema::check_module_with_prelude`/`lower_hir_with_prelude`,
//! `nymph_codegen::emit`) — no change to the type checker or unifier was
//! needed (see `rewrite.rs`'s doc comment for why).
//!
//! The driver is deliberately filesystem-agnostic: every entry point here
//! takes a `load: &dyn Fn(&str) -> Option<String>` closure keyed by a
//! canonical, `/`-separated, extension-less module path relative to the
//! project's source root (e.g. `"main"`, `"geometry/vec"`). A test builds
//! this over a virtual `FxHashMap<String, String>`; the CLI builds it over
//! `std::fs::read_to_string` plus its own `<src-root>/<key>.nym` mapping —
//! this module never touches the filesystem or knows what a source root is.
//!
//! ## Pipeline
//! 1. **Resolve + parse** ([`resolve`]): depth-first from the entry, every
//!    transitively-imported module is resolved (`@/`/`./`/`../`), parsed, and
//!    ordered dependency-first (topological). A cycle, an unresolved import,
//!    or a parse error anywhere aborts here with file-aware diagnostics —
//!    nothing downstream ever sees a broken graph.
//! 2. **Bind + rewrite** ([`rewrite`]): every module gets a stable per-project
//!    tag; its own top-level names are renamed `$m{tag}$name` (globally
//!    unique) and every reference — to its own siblings, an import's
//!    namespace (`math.sin`), or an import's `with`-list name — is rewritten
//!    to the target's mangled name. Visibility (`private` not crossing a
//!    module boundary) and name collisions are enforced here.
//! 3. **Check + lower + emit** (this file): each module is checked with the
//!    stdlib operator prelude PLUS every module it transitively imports
//!    flattened alongside it as more "prelude" slice entries — reusing
//!    `nymph-sema`'s existing per-module NodeId/Span offsetting untouched
//!    (see `nymph-sema/src/prelude.rs`) — then lowered and emitted the same
//!    way. Every module's own top-level names are globally unique (step 2).
//! 4. **Bundle** ([`bundle`], Slice IB2): each module's flat emitted JS is
//!    wrapped with a synthesized `import { ... } from "<dep>";` line per
//!    direct dependency and a trailing `export { ... };` line naming its own
//!    non-private (mangled) declarations — real ES module syntax layered on
//!    top of the SAME globally-unique mangled names, so rolldown links the
//!    graph with zero renaming of its own. `rolldown` (an in-process, pure-
//!    Rust bundler — no node/npm at build time) then bundles + tree-shakes
//!    the graph into one flat script. The mangling from step 2 stays: it is
//!    what lets [`Driver::prelude_slice`] flatten a module's transitive
//!    dependencies into one shared checker scope without name collisions,
//!    which real ES modules alone would not fix without a much bigger,
//!    fenced-off change to per-module name resolution in `nymph-sema`.

mod bundle;
mod compat;
mod emission;
mod metrics;
mod queries;
mod resolve;
mod rewrite;
mod session;

pub use session::{
	AmbientCoreModuleKey, BuiltinRuntimeOwnerArtifact, BuiltinRuntimeOwnerShape, CompilerSession,
	ModuleAnalysis, ModulePath, ProjectDiagnostics, ProjectId, RuntimeDefinitionError, SourceVersion,
};

#[cfg(feature = "test-support")]
pub use emission::StableEmittedProject;

#[cfg(feature = "test-support")]
pub use session::{SemanticPipeline, SemanticQueryEvent};

#[cfg(feature = "test-support")]
pub use metrics::{PhaseCounts, with_phase_counts};

#[cfg(feature = "test-support")]
pub use test_support::{GraphFixture, GraphShape};

#[cfg(test)]
use ecow::EcoString;
#[cfg(test)]
use nymph_ast::decl::{Module, Visibility};
use nymph_diagnostics::Diagnostic;
#[cfg(test)]
use nymph_diagnostics::Label;
#[cfg(test)]
use nymph_hir::hir::{HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirModule};
#[cfg(test)]
use rayon::prelude::*;
#[cfg(test)]
use rustc_hash::{FxHashMap, FxHashSet};

#[cfg(test)]
use metrics::{CompilerPhase, capture_collector, install_collector, record_phase};
use resolve::GraphBuilder;
#[cfg(test)]
use resolve::RawModule;
#[cfg(test)]
use rewrite::{DeclaredName, NsInfo, RewriteCtx, declared_names, rewrite_module};

#[cfg(feature = "test-support")]
mod test_support {
	use std::collections::BTreeMap;

	#[derive(Clone, Copy, Debug)]
	pub enum GraphShape {
		Wide { leaves: usize },
		Deep { depth: usize },
		Mixed { width: usize, depth: usize },
	}

	#[derive(Clone, Debug)]
	pub struct GraphFixture {
		entry: String,
		sources: BTreeMap<String, String>,
		leaves: Vec<String>,
	}

	impl GraphShape {
		#[must_use]
		pub fn generate(self) -> GraphFixture {
			let mut fixture = GraphFixture {
				entry: "main".to_string(),
				sources: BTreeMap::new(),
				leaves: Vec::new(),
			};
			let entry_imports: Vec<String> = match self {
				Self::Wide { leaves } => (0..leaves)
					.map(|index| {
						let key = format!("wide/leaf_{index:03}");
						fixture.add_module(&key, None, index);
						fixture.leaves.push(key.clone());
						key
					})
					.collect(),
				Self::Deep { depth } => {
					let mut parent = None;
					for index in (0..depth).rev() {
						let key = format!("deep/level_{index:03}");
						fixture.add_module(&key, parent.as_deref(), index);
						if parent.is_none() {
							fixture.leaves.push(key.clone());
						}
						parent = Some(key);
					}
					parent.into_iter().collect()
				}
				Self::Mixed { width, depth } => (0..width)
					.map(|branch| {
						let mut child = None;
						for level in (1..depth).rev() {
							let key = format!("mixed/branch_{branch:03}/level_{level:03}");
							fixture.add_module(&key, child.as_deref(), branch * depth + level);
							if child.is_none() {
								fixture.leaves.push(key.clone());
							}
							child = Some(key);
						}
						let head = format!("mixed/branch_{branch:03}");
						fixture.add_module(&head, child.as_deref(), branch * depth);
						head
					})
					.collect(),
			};
			let imports = entry_imports
				.iter()
				.map(|key| format!("import @/{key}"))
				.collect::<Vec<_>>()
				.join("\n");
			fixture.sources.insert(
				"main".to_string(),
				format!("{imports}\npublic func root_value(): int = 0\n"),
			);
			fixture
		}
	}

	impl GraphFixture {
		fn add_module(&mut self, key: &str, child: Option<&str>, value: usize) {
			let import = child.map_or_else(String::new, |child| format!("import @/{child}\n"));
			self.sources.insert(
				key.to_string(),
				format!("{import}public func value_{value}(): int = {value}\n"),
			);
		}

		#[must_use]
		pub fn entry(&self) -> &str {
			&self.entry
		}
		#[must_use]
		pub fn sources(&self) -> &BTreeMap<String, String> {
			&self.sources
		}
		#[must_use]
		pub fn load(&self, key: &str) -> Option<String> {
			self.sources.get(key).cloned()
		}
		#[must_use]
		pub fn unresolved_imports(&self) -> Vec<String> {
			self
				.sources
				.iter()
				.flat_map(|(_, source)| source.lines())
				.filter_map(|line| line.strip_prefix("import @/"))
				.filter(|target| !self.sources.contains_key(*target))
				.map(str::to_string)
				.collect()
		}
		#[must_use]
		pub fn retained_bytes(&self) -> usize {
			self
				.sources
				.iter()
				.map(|(key, source)| key.len() + source.len())
				.sum()
		}
		pub fn replace_private_leaf_body(&mut self) {
			let leaf = &self.leaves[0];
			self
				.sources
				.get_mut(leaf)
				.expect("leaf exists")
				.push_str("let private_change = 1\n");
		}
		pub fn replace_public_leaf_signature(&mut self) {
			let leaf = &self.leaves[0];
			let source = self.sources.get_mut(leaf).expect("leaf exists");
			*source =
				source
					.replacen("(): int", "(input: int): int", 1)
					.replacen(" = ", " = input + ", 1);
		}
	}
}

/// A diagnostic attributed to one module of the project, keyed by its
/// canonical path (e.g. `"main"`, `"geometry/vec"`) — the same key `load` was
/// asked for. Render against `"<module>.nym"` and that module's own source.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectDiagnostic {
	pub module: String,
	pub diag: Diagnostic,
}

#[cfg(test)]
fn merge_canonical_enum(target: &mut nymph_hir::hir::HirEnum, incoming: nymph_hir::hir::HirEnum) {
	assert_eq!(target.name, incoming.name);
	assert_eq!(target.variants, incoming.variants);
	merge_canonical_methods(&mut target.methods, incoming.methods);
	merge_canonical_methods(&mut target.statics, incoming.statics);
}

#[cfg(test)]
fn merge_canonical_class(
	target: &mut nymph_hir::hir::HirClass,
	incoming: nymph_hir::hir::HirClass,
) {
	assert_eq!(target.name, incoming.name);
	assert_eq!(target.fields, incoming.fields);
	merge_canonical_methods(&mut target.methods, incoming.methods);
	merge_canonical_methods(&mut target.statics, incoming.statics);
}

#[cfg(test)]
fn merge_canonical_methods(
	target: &mut Vec<nymph_hir::hir::HirMethod>,
	incoming: Vec<nymph_hir::hir::HirMethod>,
) {
	for method in incoming {
		// Prelude methods are lowered independently in each consumer. Their
		// generated local names can differ with that consumer's rename counters,
		// even though they came from the same checked declaration. The ambient
		// owner has already enforced method-name uniqueness, so retain the first
		// alpha-equivalent materialization in stable project traversal order.
		if !target.iter().any(|item| item.name == method.name) {
			target.push(method);
		}
	}
}

#[cfg(test)]
fn runtime_import_lines(imports: &[(String, Vec<String>)]) -> String {
	let mut out = String::new();
	for (specifier, names) in imports {
		out.push_str(&format!(
			"import {{ {} }} from \"{specifier}\";\n",
			names.join(", ")
		));
	}
	out
}

#[cfg(test)]
fn insert_runtime_module(
	sources: &mut FxHashMap<String, String>,
	key: String,
	source: String,
) -> Result<(), Vec<ProjectDiagnostic>> {
	if sources.contains_key(&key) {
		return Err(vec![ProjectDiagnostic {
			module: key.clone(),
			diag: Diagnostic::error(
				"PROJECT-RUNTIME-MODULE-COLLISION".into(),
				format!("project module `{key}` conflicts with a compiler runtime module"),
				nymph_ast::Span::new(0, 0),
			),
		}]);
	}
	sources.insert(key, source);
	Ok(())
}

/// The result of a successful [`compile_project`]: the whole program as one
/// runnable JS string, plus the entry module's `main` — every OTHER
/// top-level name in the project is renamed (step 2 above) to stay globally
/// unique, but the entry module's own `main` is deliberately exempted (see
/// [`Self::entry_symbol`]), so `entry_main` is always the literal `"main"`
/// and a caller can append `main();` exactly like the single-module facade
/// (`compile`/`compile_entry`) already does.
#[derive(Clone)]
pub struct CompiledProject {
	pub js: String,
	pub entry_main: String,
	/// The entry module's own per-project tag. Every top-level name the entry
	/// module declares OTHER than `main` is mangled `$m{entry_tag}$<name>`
	/// (see [`Self::entry_symbol`]) — exposed so a caller (chiefly tests)
	/// that wants to reach some entry-module symbol other than `main`
	/// doesn't need to know the mangling scheme itself.
	pub entry_tag: usize,
}

impl CompiledProject {
	/// The mangled name of `name` as declared by the entry module (`main`
	/// itself is never mangled — see [`Self::entry_main`]).
	#[must_use]
	pub fn entry_symbol(&self, name: &str) -> String {
		if name == "main" {
			"main".to_string()
		} else {
			format!("$m{}${name}", self.entry_tag)
		}
	}
}

const FACADE_PROJECT: &str = "__nymph_internal_facade_project__";

fn facade_session(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	std_provider: &dyn Fn(&str) -> Option<String>,
) -> (CompilerSession, ProjectId, ModulePath) {
	use std::{cell::RefCell, collections::BTreeMap};

	let project_sources = RefCell::new(BTreeMap::new());
	let builtin_sources = RefCell::new(BTreeMap::new());
	let capture_project = |key: &str| {
		load(key).inspect(|source| {
			project_sources
				.borrow_mut()
				.entry(key.to_string())
				.or_insert_with(|| source.clone());
		})
	};
	let capture_builtin = |key: &str| {
		std_provider(key).inspect(|source| {
			builtin_sources
				.borrow_mut()
				.entry(key.to_string())
				.or_insert_with(|| source.clone());
		})
	};
	GraphBuilder::new(&capture_project, &capture_builtin).visit(entry);

	let project = ProjectId::new(FACADE_PROJECT);
	let mut session = CompilerSession::from_builtin_sources(builtin_sources.into_inner());
	for (path, source) in project_sources.into_inner() {
		session.set_source(
			project.clone(),
			ModulePath::new(path).expect("legacy discovery produced a canonical module path"),
			source,
			SourceVersion(1),
		);
	}
	let entry = ModulePath::new(entry).expect("project entry must be a canonical module path");
	(session, project, entry)
}

/// Resolve, parse, bind, and type-check every module reachable from `entry`,
/// requiring `entry` to declare a valid top-level `main`, and returning every
/// diagnostic produced (empty ⇒ the whole project is clean). Does not lower
/// or emit — mirrors [`crate::check_entry`] one level up (whole-program, not
/// single-module). See [`check_project_library`] for the non-entry-mode
/// counterpart, and [`check_project_with_std`] for a caller with real
/// `import std/…` support to offer. A stray `import std/…` still surfaces
/// `IMPORT-UNRESOLVED` here, exactly like any other unresolved import.
pub fn check_project(entry: &str, load: &dyn Fn(&str) -> Option<String>) -> Vec<ProjectDiagnostic> {
	check_project_with_std(entry, load, &|_| None)
}

/// [`check_project`], with a pluggable `std_provider`: given an `import
/// std/…`'s path with its `std/` root and `std::` key-prefix already stripped
/// (e.g. `collections/tree`, for `import std/collections/tree`), it returns
/// that module's source, or `None` if no such std module exists.
pub fn check_project_with_std(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	std_provider: &dyn Fn(&str) -> Option<String>,
) -> Vec<ProjectDiagnostic> {
	let (session, project, entry) = facade_session(entry, load, std_provider);
	session
		.check_project(project, entry, nymph_sema::EntryMode::Entry)
		.iter()
		.cloned()
		.collect()
}

/// Library-mode counterpart of [`check_project`]: `entry` is not required to
/// declare a `main` — mirrors [`crate::check`] one level up. Used to check a
/// project graph rooted at a non-entry module (e.g. `nymph build` on a file
/// that isn't the project's `main.nym`). See [`check_project_library_with_std`]
/// for a caller with real `import std/…` support to offer.
pub fn check_project_library(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
) -> Vec<ProjectDiagnostic> {
	check_project_library_with_std(entry, load, &|_| None)
}

/// [`check_project_library`], with a pluggable `std_provider` — see
/// [`check_project_with_std`].
pub fn check_project_library_with_std(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	std_provider: &dyn Fn(&str) -> Option<String>,
) -> Vec<ProjectDiagnostic> {
	let (session, project, entry) = facade_session(entry, load, std_provider);
	session
		.check_project(project, entry, nymph_sema::EntryMode::Library)
		.iter()
		.cloned()
		.collect()
}

/// Compile the whole project reachable from `entry` to one runnable JS
/// string, requiring `entry` to declare a valid top-level `main`. Returns
/// every diagnostic (from every module) if any module fails to resolve,
/// bind, or type-check — mirrors [`crate::compile_entry`] one level up. See
/// [`compile_project_library`] for the non-entry-mode counterpart, and
/// [`compile_project_with_std`] for a caller with real `import std/…` support
/// to offer.
///
/// # Errors
/// Returns `Err` with every project diagnostic if resolution, binding, or
/// type-checking fails anywhere in the graph.
pub fn compile_project(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
	compile_project_with_std(entry, load, &|_| None)
}

/// [`compile_project`], with a pluggable `std_provider` — see
/// [`check_project_with_std`].
///
/// # Errors
/// Returns `Err` with every project diagnostic if resolution, binding, or
/// type-checking fails anywhere in the graph.
pub fn compile_project_with_std(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	std_provider: &dyn Fn(&str) -> Option<String>,
) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
	let (session, project, entry) = facade_session(entry, load, std_provider);
	session
		.compile_project(project, entry, nymph_sema::EntryMode::Entry)
		.map(|compiled| compiled.as_ref().clone())
		.map_err(|diagnostics| diagnostics.iter().cloned().collect())
}

/// Compile one standalone source through the canonical virtual-module
/// assembly while retaining the facade's unmangled top-level names.
pub(crate) fn compile_standalone(
	source: &str,
	_path: &str,
	entry_mode: bool,
) -> Result<String, Vec<Diagnostic>> {
	const STANDALONE_ENTRY: &str = "__nymph_internal_standalone_entry__";
	let project = ProjectId::new(FACADE_PROJECT);
	let path = ModulePath::new(STANDALONE_ENTRY).expect("standalone key is canonical");
	let mut session = CompilerSession::from_builtin_sources(Default::default());
	session.set_source(
		project.clone(),
		path.clone(),
		source.to_string(),
		SourceVersion(1),
	);
	session
		.compile_project_with_options(
			project,
			path,
			if entry_mode {
				nymph_sema::EntryMode::Entry
			} else {
				nymph_sema::EntryMode::Library
			},
			true,
		)
		.map(|compiled| compiled.js.clone())
		.map_err(|diags| diags.iter().map(|item| item.diag.clone()).collect())
}

/// Internal inspection seam for regressions that must assert the exact ES
/// module graph assembled before rolldown transforms it.
#[doc(hidden)]
pub fn compile_project_module_sources_with_std(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	std_provider: &dyn Fn(&str) -> Option<String>,
) -> Result<std::collections::HashMap<String, String>, Vec<ProjectDiagnostic>> {
	let (session, project, entry) = facade_session(entry, load, std_provider);
	session
		.inspect_emitted_project(project, entry, nymph_sema::EntryMode::Entry)
		.map(|(sources, _)| sources)
		.map_err(|diagnostics| diagnostics.iter().cloned().collect())
}

/// Library-mode counterpart of [`compile_project`]: `entry` is not required
/// to declare a `main` — mirrors [`crate::compile`] one level up. See
/// [`compile_project_library_with_std`] for a caller with real `import
/// std/…` support to offer.
///
/// # Errors
/// Returns `Err` with every project diagnostic if resolution, binding, or
/// type-checking fails anywhere in the graph.
pub fn compile_project_library(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
	compile_project_library_with_std(entry, load, &|_| None)
}

/// [`compile_project_library`], with a pluggable `std_provider` — see
/// [`check_project_with_std`].
///
/// # Errors
/// Returns `Err` with every project diagnostic if resolution, binding, or
/// type-checking fails anywhere in the graph.
pub fn compile_project_library_with_std(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	std_provider: &dyn Fn(&str) -> Option<String>,
) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
	let (session, project, entry) = facade_session(entry, load, std_provider);
	session
		.compile_project(project, entry, nymph_sema::EntryMode::Library)
		.map(|compiled| compiled.as_ref().clone())
		.map_err(|diagnostics| diagnostics.iter().cloned().collect())
}

/// Owns everything phase 2/3 need once the module graph is resolved: the raw
/// parsed modules, their dependency-first order, per-module stable tags, and
/// every module's declared-name table (for cross-module visibility checks).
#[cfg(test)]
struct Driver {
	entry: String,
	/// Whether `entry` must declare a valid top-level `main` — see
	/// [`check_project`] vs [`check_project_library`].
	entry_mode: bool,
	preserve_entry_names: bool,
	order: Vec<String>,
	modules: FxHashMap<String, RawModule>,
	tags: FxHashMap<String, usize>,
	processed: FxHashMap<String, Module>,
	/// Every module's declared top-level names (by canonical key), computed
	/// once from the raw (pre-rewrite) AST — reused in [`Self::compile_all`]
	/// to synthesize each module's `import`/`export` wrapping (bundling,
	/// Slice IB2) from the same table phase 2 used for visibility checks.
	declared: FxHashMap<String, Vec<DeclaredName>>,
}

#[cfg(test)]
impl Driver {
	/// Phase 1 (resolve + parse) and phase 2 (bind + rewrite). Returns every
	/// diagnostic collected in either phase, aborting before any type
	/// checking — a project with a broken import graph is never partially
	/// checked.
	fn resolve_and_bind(
		entry: &str,
		load: &dyn Fn(&str) -> Option<String>,
		std_provider: &dyn Fn(&str) -> Option<String>,
		entry_mode: bool,
		preserve_entry_names: bool,
	) -> Result<Self, Vec<ProjectDiagnostic>> {
		let mut builder = GraphBuilder::new(load, std_provider);
		builder.visit(entry);
		if !builder.diags.is_empty() {
			return Err(builder.diags);
		}

		let order = builder.order;
		let modules = builder.modules;
		for _ in &modules {
			record_phase(CompilerPhase::Parse);
		}
		record_phase(CompilerPhase::Graph);
		let tags: FxHashMap<String, usize> = order
			.iter()
			.enumerate()
			.map(|(i, k)| (k.clone(), i))
			.collect();
		let declared: FxHashMap<String, Vec<DeclaredName>> = order
			.iter()
			.map(|k| (k.clone(), declared_names(&modules[k].tree)))
			.collect();

		let mut diags: Vec<ProjectDiagnostic> = Vec::new();
		let mut processed: FxHashMap<String, Module> = FxHashMap::default();
		let mut static_attachments: FxHashMap<(String, EcoString, EcoString), nymph_ast::Span> =
			FxHashMap::default();

		for key in &order {
			let raw = &modules[key];
			let own_tag = tags[key];
			let own_names: FxHashSet<_> = declared[key].iter().map(|d| d.name.clone()).collect();
			let owned_types: FxHashSet<_> = raw
				.tree
				.members
				.iter()
				.filter_map(|decl| match decl {
					nymph_ast::decl::Declaration::Struct { name, .. }
					| nymph_ast::decl::Declaration::Enum { name, .. } => Some(name.0.clone()),
					_ => None,
				})
				.collect();
			for decl in &raw.tree.members {
				let nymph_ast::decl::Declaration::Impl { type_, members, .. } = decl else {
					continue;
				};
				let nymph_ast::ty::Type::Reference { name, .. } = &type_.0 else {
					continue;
				};
				if !owned_types.contains(&name.0) {
					diags.push(ProjectDiagnostic {
						module: key.clone(),
						diag: Diagnostic::error(
							"INHERENT-IMPL-OWNER".into(),
							format!(
								"inherent impl for `{}` must be declared in the module that owns the type; extension attachments are not allowed",
								name.0
							),
							name.1,
						),
					});
				}
				let canonical_owner = if owned_types.contains(&name.0) {
					Some(key.clone())
				} else {
					raw.imports.iter().find_map(|import| {
						import
							.with_idents
							.iter()
							.any(|(imported, alias)| alias.as_ref().unwrap_or(imported).0 == name.0)
							.then(|| import.target_key.clone())
					})
				};
				let Some(canonical_owner) = canonical_owner else {
					continue;
				};
				for member in members {
					let nymph_ast::decl::ImplMember::Func { meta, .. } = &member.0 else {
						continue;
					};
					if meta.kind != nymph_ast::decl::FuncKind::Namespace {
						continue;
					}
					let identity = (canonical_owner.clone(), name.0.clone(), meta.name.0.clone());
					if let Some(previous) = static_attachments.insert(identity, meta.name.1) {
						diags.push(ProjectDiagnostic {
							module: key.clone(),
							diag: Diagnostic::error(
								"2045".into(),
								format!(
									"`{}` is defined more than once on `{}`",
									meta.name.0, name.0
								),
								meta.name.1,
							)
							.with_label(Label::new(previous, "previously defined here")),
						});
					}
				}
			}

			// The entry module's own `main` is exempted from mangling: the checker's
			// entry-point validation (`check_module_entry_with_prelude`) and the
			// CLI/test `run` convention both look for a literally-named top-level
			// `main` — nothing else needs a stable unmangled name, since every other
			// cross-module reference goes through this driver's own rewrite, not
			// the checker's name resolution.
			let is_entry = entry_mode && *key == entry;
			let mut renames: FxHashMap<_, _> = declared[key]
				.iter()
				.filter(|d| !((preserve_entry_names && *key == entry) || (is_entry && d.name == "main")))
				.map(|d| (d.name.clone(), format!("$m{own_tag}${}", d.name).into()))
				.collect();
			let mut namespaces: FxHashMap<_, NsInfo> = FxHashMap::default();

			for imp in &raw.imports {
				let target_tag = tags[&imp.target_key];
				// Cross-check against `renames` too (a prior import's `with`-bound
				// name), not just own decls / other namespaces — a namespace name
				// and a `with`-name sharing an identifier must collide, not silently
				// bind to two different modules.
				if own_names.contains(&imp.ns_name)
					|| namespaces.contains_key(&imp.ns_name)
					|| renames.contains_key(&imp.ns_name)
				{
					diags.push(ProjectDiagnostic {
						module: key.clone(),
						diag: Diagnostic::error(
							"IMPORT-NAME-COLLISION".into(),
							format!(
								"import namespace `{}` collides with another name in this module",
								imp.ns_name
							),
							imp.ns_span,
						),
					});
				} else {
					namespaces.insert(
						imp.ns_name.clone(),
						NsInfo {
							target_key: imp.target_key.clone(),
							target_tag,
						},
					);
				}

				for (name, alias) in &imp.with_idents {
					let effective = alias.clone().unwrap_or_else(|| name.clone());
					match declared[&imp.target_key].iter().find(|d| d.name == name.0) {
						None => diags.push(ProjectDiagnostic {
							module: key.clone(),
							diag: Diagnostic::error(
								"IMPORT-UNRESOLVED-NAME".into(),
								format!("module `{}` has no member `{}`", imp.target_key, name.0),
								name.1,
							),
						}),
						Some(d) if d.vis == nymph_ast::decl::Visibility::Private => {
							diags.push(ProjectDiagnostic {
								module: key.clone(),
								diag: Diagnostic::error(
									"IMPORT-PRIVATE-NAME".into(),
									format!(
										"`{}` is private to module `{}` and cannot be imported",
										name.0, imp.target_key
									),
									name.1,
								),
							})
						}
						Some(_) => {
							// Cross-check against `namespaces` too — a `with`-name colliding
							// with an import's namespace name is a collision, not a silent
							// double-bind of the same identifier to two modules.
							if own_names.contains(&effective.0)
								|| renames.contains_key(&effective.0)
								|| namespaces.contains_key(&effective.0)
							{
								diags.push(ProjectDiagnostic {
									module: key.clone(),
									diag: Diagnostic::error(
										"IMPORT-NAME-COLLISION".into(),
										format!(
											"imported name `{}` collides with another name in this module",
											effective.0
										),
										effective.1,
									),
								});
							} else {
								renames.insert(
									effective.0.clone(),
									format!("$m{target_tag}${}", name.0).into(),
								);
							}
						}
					}
				}
			}

			let ctx = RewriteCtx {
				renames,
				namespaces,
				declared: &declared,
				diags: std::cell::RefCell::new(Vec::new()),
			};
			record_phase(CompilerPhase::Rewrite);
			let rewritten = rewrite_module(&raw.tree, &ctx);
			diags.extend(
				ctx
					.diags
					.into_inner()
					.into_iter()
					.map(|d| ProjectDiagnostic {
						module: key.clone(),
						diag: d,
					}),
			);
			processed.insert(key.clone(), rewritten);
		}

		if !diags.is_empty() {
			return Err(diags);
		}

		Ok(Self {
			entry: entry.to_string(),
			entry_mode,
			preserve_entry_names,
			order,
			modules,
			tags,
			processed,
			declared,
		})
	}

	/// Every module transitively reachable from `key` via `import` (excluding
	/// `key` itself), in a stable (source import-statement) order — the
	/// prelude-slice entries a check/lower of `key` needs alongside the real
	/// operator prelude.
	fn transitive_deps(&self, key: &str) -> Vec<String> {
		let mut seen = FxHashSet::default();
		let mut out = Vec::new();
		self.collect_deps(key, &mut seen, &mut out);
		out
	}

	fn collect_deps(&self, key: &str, seen: &mut FxHashSet<String>, out: &mut Vec<String>) {
		for imp in &self.modules[key].imports {
			if seen.insert(imp.target_key.clone()) {
				out.push(imp.target_key.clone());
				self.collect_deps(&imp.target_key, seen, out);
			}
		}
	}

	fn prelude_slice(&self, key: &str) -> Vec<Module> {
		crate::prelude::core_prelude()
			.iter()
			.cloned()
			.chain(
				self
					.transitive_deps(key)
					.iter()
					.map(|d| self.processed[d].clone()),
			)
			.collect()
	}

	/// `check_all`/`compile_all` both check every module in `self.order` on
	/// its OWN turn — including a `std::`-keyed module reached via `import
	/// std/…` (Slice B). When a std module is instead reached as some OTHER
	/// module's *dependency*, its diagnostics are already dropped by
	/// [`nymph_sema::check_module_with_prelude`]'s span-offset filter (it's
	/// flattened into that caller's `prelude` slice — see
	/// [`Self::prelude_slice`]); that filter has no bearing here, though — on
	/// a std module's OWN turn it IS the `module` argument (not a prelude
	/// entry), so its own diagnostics carry their natural, un-offset span
	/// against its own real source, exactly like an ordinary project module's
	/// own-turn diagnostics. A GENUINE bug in a std module (as opposed to core,
	/// which never gets an own turn at all — it's only ever flattened into
	/// another module's prelude slice, see [`Self::prelude_slice`]) must be
	/// reported like any other module's, never silently swallowed: "loud
	/// panics/errors over silent wrong-JS" — a std module is treated exactly
	/// like an ordinary project module for diagnostic purposes, with no
	/// `std::`-key special-casing at all.
	fn check_all(&self) -> Vec<ProjectDiagnostic> {
		let collector = capture_collector();
		self
			.order
			.par_iter()
			.map(|key| {
				let collector = collector.clone();
				install_collector(collector, || {
					record_phase(CompilerPhase::Check);
					let prelude = self.prelude_slice(key);
					let module = &self.processed[key];
					let checked = if self.entry_mode && *key == self.entry {
						nymph_sema::check_module_entry_with_prelude(module, &prelude)
					} else {
						nymph_sema::check_module_with_prelude(module, &prelude)
					};
					checked
						.diags
						.into_iter()
						.map(|diag| ProjectDiagnostic {
							module: key.clone(),
							diag,
						})
						.collect::<Vec<_>>()
				})
			})
			.flatten()
			.collect()
	}

	fn assemble_module_sources(
		&self,
	) -> Result<(FxHashMap<String, String>, usize), Vec<ProjectDiagnostic>> {
		let collector = capture_collector();
		let checked_modules = self.order.par_iter().map(|key| {
			let collector = collector.clone();
			install_collector(collector, || {
				record_phase(CompilerPhase::Check);
				let prelude = self.prelude_slice(key);
				let module = &self.processed[key];
				let checked = if self.entry_mode && *key == self.entry {
					nymph_sema::check_module_entry_with_prelude(module, &prelude)
				} else {
					nymph_sema::check_module_with_prelude(module, &prelude)
				};
				let diags = checked
					.diags
					.iter()
					.filter(|d| d.is_error())
					.cloned()
					.map(|diag| ProjectDiagnostic {
						module: key.clone(),
						diag,
					})
					.collect::<Vec<_>>();
				if !diags.is_empty() {
					return (diags, None);
				}
				// `prelude` is `core_prelude() ++ transitive_deps` (see `prelude_slice`),
				// so the ambient `core` modules occupy the leading `core_prelude().len()`
				// slots and everything after is an EMITTED dependency module — a call to
				// one of those dep structs' methods lowers as a real class-method call
				// rather than a (struct-unsupported) materialization.
				let prelude_owners = crate::prelude::core_runtime_module_owners()
					.map(nymph_sema::RuntimeOwner::Compiler)
					.chain(
						self
							.transitive_deps(key)
							.iter()
							.cloned()
							.map(Into::into)
							.map(nymph_sema::RuntimeOwner::Project),
					)
					.collect::<Vec<_>>();
				let hir = nymph_sema::lower_hir_with_prelude_runtime_and_deps_with_owners(
					module,
					&prelude,
					&prelude_owners,
					crate::prelude::core_prelude().len(),
					&checked,
				);
				record_phase(CompilerPhase::Lower);
				(Vec::new(), Some((key.clone(), hir)))
			})
		});
		let mut diags = Vec::new();
		let mut lowered_modules = Vec::new();
		for (module_diags, lowered) in checked_modules.collect::<Vec<_>>() {
			diags.extend(module_diags);
			lowered_modules.extend(lowered);
		}
		if !diags.is_empty() {
			return Err(diags);
		}

		let owners = crate::prelude::core_runtime_type_owners();
		let intrinsic_sources = crate::intrinsics::intrinsic_module_sources();
		let intrinsic_type_demands =
			crate::intrinsics::runtime_type_imports(&intrinsic_sources, owners);
		let declaration_seeds = crate::prelude::core_runtime_declaration_seeds();
		let mut runtime_enums: FxHashMap<String, Vec<HirEnum>> = FxHashMap::default();
		let mut runtime_classes: FxHashMap<String, Vec<HirClass>> = FxHashMap::default();
		let mut runtime_funcs: FxHashMap<String, Vec<HirFunc>> = FxHashMap::default();
		let mut proven_project_runtime_owners = FxHashSet::default();
		for seed in &declaration_seeds.enums {
			if intrinsic_type_demands.contains(&seed.name) {
				runtime_enums
					.entry(owners[&seed.name].to_string())
					.or_default()
					.push(seed.clone());
			}
		}
		for seed in &declaration_seeds.classes {
			if intrinsic_type_demands.contains(&seed.name) {
				runtime_classes
					.entry(owners[&seed.name].to_string())
					.or_default()
					.push(seed.clone());
			}
		}
		for (_, lowered) in &mut lowered_modules {
			lowered
				.prelude_runtime
				.lets
				.append(&mut lowered.module.lets);
			lowered.module.lets = std::mem::take(&mut lowered.prelude_runtime.lets);
			for func in lowered.prelude_runtime.funcs.drain(..) {
				let owner = lowered
					.runtime_func_owners
					.get(&func.name)
					.unwrap_or_else(|| {
						panic!(
							"ambient runtime function `{}` has no canonical owner",
							func.name
						)
					});
				{
					if let nymph_sema::RuntimeOwner::Project(owner) = owner {
						proven_project_runtime_owners.insert(owner.to_string());
					}
					let funcs = runtime_funcs.entry(owner.key().to_string()).or_default();
					if let Some(existing) = funcs.iter().find(|item| item.name == func.name) {
						assert_eq!(
							existing, &func,
							"conflicting ambient runtime function `{}`",
							func.name
						);
					} else {
						funcs.push(func);
					}
				}
			}
			for class in lowered.prelude_runtime.classes.drain(..) {
				let owner = owners
					.get(&class.name)
					.unwrap_or_else(|| panic!("ambient class `{}` has no runtime owner", class.name));
				let classes = runtime_classes.entry((*owner).to_string()).or_default();
				if let Some(canonical) = classes.iter_mut().find(|item| item.name == class.name) {
					merge_canonical_class(canonical, class);
				} else {
					classes.push(class);
				}
			}
			for enum_ in lowered.prelude_runtime.enums.drain(..) {
				let owner = owners
					.get(&enum_.name)
					.unwrap_or_else(|| panic!("ambient enum `{}` has no runtime owner", enum_.name));
				let enums = runtime_enums.entry((*owner).to_string()).or_default();
				if let Some(canonical) = enums.iter_mut().find(|item| item.name == enum_.name) {
					merge_canonical_enum(canonical, enum_);
				} else {
					enums.push(enum_);
				}
			}
		}
		// External host snapshots are owned once by the assembled project, not by
		// whichever consumer happened to demand their ambient declarations.
		let mut identities = std::collections::BTreeSet::new();
		for (_, lowered) in &lowered_modules {
			for let_ in &lowered.module.lets {
				if let HirExpr::ExternValue {
					module,
					symbol,
					marshal,
				} = let_.value
				{
					identities.insert((module, symbol, marshal));
				}
			}
		}
		let canonical_names: std::collections::BTreeMap<_, _> = identities
			.iter()
			.enumerate()
			.map(|(index, identity)| (*identity, format!("$nymph_external_value${index}")))
			.collect();
		let mut external_imports: FxHashMap<String, Vec<String>> = FxHashMap::default();
		for (key, lowered) in &mut lowered_modules {
			for let_ in &mut lowered.module.lets {
				if let HirExpr::ExternValue {
					module,
					symbol,
					marshal,
				} = let_.value
				{
					let canonical = canonical_names[&(module, symbol, marshal)].clone();
					let_.value = HirExpr::Local(canonical.clone().into());
					external_imports
						.entry(key.clone())
						.or_default()
						.push(canonical);
				}
			}
		}
		let runtime_names: FxHashSet<_> = runtime_enums
			.values()
			.flat_map(|enums| enums.iter().map(|enum_| enum_.name.clone()))
			.chain(
				runtime_classes
					.values()
					.flat_map(|classes| classes.iter().map(|class| class.name.clone())),
			)
			.chain(
				runtime_funcs
					.values()
					.flat_map(|funcs| funcs.iter().map(|func| func.name.clone())),
			)
			.collect();
		let mut runtime_symbol_owners: FxHashMap<_, String> = owners
			.iter()
			.map(|(name, owner)| (name.clone(), (*owner).to_string()))
			.collect();
		for (owner, funcs) in &runtime_funcs {
			for func in funcs {
				if let Some(previous) = runtime_symbol_owners.insert(func.name.clone(), owner.clone()) {
					assert_eq!(
						previous,
						owner.as_str(),
						"conflicting canonical owners for `{}`",
						func.name
					);
				}
			}
		}
		let imports_for = |hir: &HirModule, own_owner: Option<&str>| {
			let mut imports: FxHashMap<String, Vec<String>> = FxHashMap::default();
			for name in hir.runtime_type_references() {
				if !runtime_names.contains(&name) {
					continue;
				}
				let owner = &runtime_symbol_owners[&name];
				if own_owner == Some(owner.as_str()) {
					continue;
				}
				imports
					.entry(owner.to_string())
					.or_default()
					.push(name.to_string());
			}
			let mut imports: Vec<_> = imports.into_iter().collect();
			for (_, names) in &mut imports {
				names.sort_unstable();
				names.dedup();
			}
			imports.sort_unstable_by(|a, b| a.0.cmp(&b.0));
			imports
		};

		// Demand-generated declarations whose canonical owner is already a real
		// project/dependency module belong to that HIR module. Emitting a second
		// virtual module under the same key would collide rather than establish
		// ownership.
		let mut merged_runtime_exports: FxHashMap<String, Vec<String>> = FxHashMap::default();
		for (key, lowered) in &mut lowered_modules {
			if !proven_project_runtime_owners.contains(key) {
				continue;
			}
			let funcs = runtime_funcs.remove(key).unwrap_or_default();
			let classes = runtime_classes.remove(key).unwrap_or_default();
			let enums = runtime_enums.remove(key).unwrap_or_default();
			let exports = merged_runtime_exports.entry(key.clone()).or_default();
			exports.extend(funcs.iter().map(|item| item.name.to_string()));
			exports.extend(classes.iter().map(|item| item.name.to_string()));
			exports.extend(enums.iter().map(|item| item.name.to_string()));
			for func in funcs {
				if let Some(existing) = lowered
					.module
					.funcs
					.iter()
					.find(|item| item.name == func.name)
				{
					assert_eq!(
						existing, &func,
						"conflicting canonical function `{}`",
						func.name
					);
				} else {
					lowered.module.funcs.push(func);
				}
			}
			for class in classes {
				if let Some(existing) = lowered
					.module
					.classes
					.iter_mut()
					.find(|item| item.name == class.name)
				{
					merge_canonical_class(existing, class);
				} else {
					lowered.module.classes.push(class);
				}
			}
			for enum_ in enums {
				if let Some(existing) = lowered
					.module
					.enums
					.iter_mut()
					.find(|item| item.name == enum_.name)
				{
					merge_canonical_enum(existing, enum_);
				} else {
					lowered.module.enums.push(enum_);
				}
			}
		}

		let mut module_sources: FxHashMap<String, String> = FxHashMap::default();
		for (key, lowered) in lowered_modules {
			record_phase(CompilerPhase::Emit);
			let mut imports = imports_for(&lowered.module, Some(&key));
			if let Some(mut names) = external_imports.remove(&key) {
				names.sort_unstable();
				names.dedup();
				imports.push(("std/nymph/external-values".to_string(), names));
			}
			let body = nymph_codegen::emit_for_project_module(&lowered.module, &key);
			let mut source = self.wrap_module_js(&key, &body, &imports);
			if let Some(exports) = merged_runtime_exports.get_mut(&key)
				&& !exports.is_empty()
			{
				exports.sort_unstable();
				exports.dedup();
				source.push_str(&format!("export {{ {} }};\n", exports.join(", ")));
			}
			module_sources.insert(key.clone(), source);
		}
		if !canonical_names.is_empty() {
			let lets = canonical_names
				.iter()
				.map(|(&(module, symbol, marshal), name)| HirLet {
					name: name.clone().into(),
					mutable: false,
					value: HirExpr::ExternValue {
						module,
						symbol,
						marshal,
					},
				})
				.collect();
			let hir = HirModule {
				lets,
				funcs: Vec::new(),
				classes: Vec::new(),
				enums: Vec::new(),
			};
			let mut source = nymph_codegen::emit(&hir);
			source.push_str(&format!(
				"export {{ {} }};\n",
				canonical_names
					.values()
					.cloned()
					.collect::<Vec<_>>()
					.join(", ")
			));
			insert_runtime_module(
				&mut module_sources,
				"std/nymph/external-values".to_string(),
				source,
			)?;
		}
		let runtime_owners: FxHashSet<_> = runtime_enums
			.keys()
			.chain(runtime_classes.keys())
			.chain(runtime_funcs.keys())
			.cloned()
			.collect();
		for owner in runtime_owners.clone() {
			let mut enums = runtime_enums.remove(&owner).unwrap_or_default();
			let mut classes = runtime_classes.remove(&owner).unwrap_or_default();
			let mut funcs = runtime_funcs.remove(&owner).unwrap_or_default();
			enums.sort_unstable_by(|a, b| a.name.cmp(&b.name));
			classes.sort_unstable_by(|a, b| a.name.cmp(&b.name));
			funcs.sort_unstable_by(|a, b| a.name.cmp(&b.name));
			let hir = HirModule {
				lets: Vec::new(),
				funcs,
				classes,
				enums,
			};
			let imports = imports_for(&hir, Some(&owner));
			let mut source = runtime_import_lines(&imports);
			source.push_str(&nymph_codegen::emit_for_project_module(&hir, &owner));
			let names = hir
				.classes
				.iter()
				.map(|item| item.name.as_str())
				.chain(hir.enums.iter().map(|item| item.name.as_str()))
				.chain(hir.funcs.iter().map(|item| item.name.as_str()))
				.collect::<Vec<_>>()
				.join(", ");
			source.push_str(&format!("export {{ {names} }};\n"));
			insert_runtime_module(&mut module_sources, owner, source)?;
		}
		// Gap 3 (L0): inject one virtual intrinsic module per distinct
		// LINKED-external registry module (today, just
		// `"std/collections/list"`, seeded with `length`) — the stripped
		// `.ts` runtime source an emitted `import { length } from
		// "std/collections/list"` (see `nymph-codegen`'s `HirExpr::ExternCall`
		// emit arm) actually resolves against. Unconditional per compile (not
		// gated on whether any emitted module actually references it):
		// `VirtualFsPlugin::load` is only invoked on demand, so an unused
		// intrinsic costs one un-consulted map entry, and rolldown
		// tree-shakes it away regardless.
		for (key, source) in intrinsic_sources {
			assert_ne!(
				key, "std/option",
				"intrinsics must not replace the canonical Option module"
			);
			if module_sources.contains_key(&key)
				&& (runtime_owners.contains(&key) || proven_project_runtime_owners.contains(&key))
			{
				let backing = format!("{key}$intrinsics");
				insert_runtime_module(&mut module_sources, backing.clone(), source)?;
				let public = module_sources
					.get_mut(&key)
					.expect("canonical owner exists");
				public.push_str(&format!("export * from \"{backing}\";\n"));
			} else {
				insert_runtime_module(&mut module_sources, key, source)?;
			}
		}
		Ok((module_sources, self.tags[&self.entry]))
	}

	fn compile_all(&self) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
		let (module_sources, entry_tag) = self.assemble_module_sources()?;
		record_phase(CompilerPhase::Bundle);
		let js = bundle::bundle(&self.entry, module_sources).map_err(|msg| {
			vec![ProjectDiagnostic {
				module: self.entry.clone(),
				diag: Diagnostic::error(
					"BUNDLE-FAILED".into(),
					format!("bundling the project failed: {msg}"),
					nymph_ast::Span::new(0, 0),
				),
			}]
		})?;
		Ok(CompiledProject {
			js,
			entry_main: "main".to_string(),
			entry_tag,
		})
	}

	/// Wrap one module's flat emitted JS (`body`, over the `$m{tag}$`-mangled
	/// names IB1 already rewrote every declaration/reference to) into a real
	/// ES module: an `import { ... } from "<dep>";` line per direct
	/// dependency naming that dependency's own non-private mangled surface,
	/// and a trailing `export { ... };` line naming this module's own
	/// non-private mangled surface (see [`bundle::bundle`]'s doc comment for
	/// why this needs no renaming of its own). Over-importing/exporting the
	/// full non-private surface (rather than only what's referenced) is
	/// deliberate — simplest to synthesize, and rolldown's tree-shaking prunes
	/// whatever ends up unused.
	///
	/// The entry module's own literal `main` (never mangled — see
	/// [`CompiledProject::entry_symbol`]) is exported unmangled alongside its
	/// other exports, so it survives tree-shaking even though nothing in the
	/// module graph itself calls it (only a caller appending `main();`
	/// outside the bundle, exactly like the single-module facade).
	fn wrap_module_js(
		&self,
		key: &str,
		body: &str,
		runtime_imports: &[(String, Vec<String>)],
	) -> String {
		let own_tag = self.tags[key];
		let is_entry = self.entry_mode && key == self.entry;
		let preserve_names = self.preserve_entry_names && key == self.entry;

		let mut seen_deps = FxHashSet::default();
		let mut import_lines = Vec::new();
		for imp in &self.modules[key].imports {
			if !seen_deps.insert(imp.target_key.clone()) {
				continue;
			}
			let dep_tag = self.tags[&imp.target_key];
			let mut names: Vec<String> = self.declared[&imp.target_key]
				.iter()
				.filter(|d| d.vis != Visibility::Private && d.has_runtime_binding)
				.map(|d| format!("$m{dep_tag}${}", d.name))
				.collect();
			names.sort_unstable();
			if !names.is_empty() {
				import_lines.push(format!(
					"import {{ {} }} from \"{}\";",
					names.join(", "),
					imp.target_key
				));
			}
		}

		let mut export_names: Vec<String> = self.declared[key]
			.iter()
			.filter(|d| d.has_runtime_binding && (preserve_names || d.vis != Visibility::Private))
			.map(|d| {
				if preserve_names || (is_entry && d.name == "main") {
					d.name.to_string()
				} else {
					format!("$m{own_tag}${}", d.name)
				}
			})
			.collect();
		export_names.sort_unstable();

		let mut out = String::new();
		let runtime_imports = runtime_imports
			.iter()
			.filter_map(|(module, names)| {
				let names = names
					.iter()
					.filter(|name| !body.contains(&format!("import {{ {name} }} from \"{module}\";")))
					.cloned()
					.collect::<Vec<_>>();
				(!names.is_empty()).then(|| (module.clone(), names))
			})
			.collect::<Vec<_>>();
		out.push_str(&runtime_import_lines(&runtime_imports));
		for line in &import_lines {
			out.push_str(line);
			out.push('\n');
		}
		// A module that MATCHES or constructs an IMPORTED enum references the
		// shared `TAG` discriminant (`x[TAG]`) but — unlike the enum's own
		// declaring module — never emits `const TAG` itself. In IB1's flat
		// concatenation that was fine (one shared scope); across rolldown's
		// per-module ES scopes it is a `ReferenceError: TAG is not defined`.
		// Provide it here when used-but-undeclared. `Symbol.for("nymph.tag")` is
		// globally idempotent, so every module's `TAG` is the same symbol.
		if body.contains("[TAG]") && !body.contains("const TAG") {
			out.push_str("const TAG = Symbol.for(\"nymph.tag\");\n");
		}
		out.push_str(body);
		if !export_names.is_empty() {
			if !out.ends_with('\n') {
				out.push('\n');
			}
			out.push_str(&format!("export {{ {} }};\n", export_names.join(", ")));
		}
		out
	}
}
