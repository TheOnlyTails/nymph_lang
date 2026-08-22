//! Project-oriented compiler facade and incremental compiler session.
//!
//! The public convenience functions discover sources through caller-provided
//! loaders, populate a [`CompilerSession`], and delegate checking, lowering,
//! emission, and inspection to the canonical query pipeline.
//!
//! `CompilerSession` is the sole owner of project/module identity, source
//! inputs, Salsa storage, and the semantic graph. A CLI facade creates one
//! session per invocation; long-lived tools retain one and update source
//! inputs. Forward dependencies come from tracked import resolution and the
//! reverse-importer relation is derived from that same graph.

mod assembly;
#[cfg(feature = "test-support")]
mod benchmark_support;
mod bundle;
pub mod documentation;
mod emission;
mod link_plan;
mod queries;
mod repl;
mod resolve;
mod session;

pub use repl::{
	ReplInputStatus, ReplSession, ReplStageError, StagedReplSubmission, repl_input_status,
};

pub use session::{
	AmbientCoreModuleKey, BuildProfile, BuiltinRuntimeOwnerArtifact, BuiltinRuntimeOwnerShape,
	CompilerSession, LintLevel, ModuleAnalysis, ModulePath, PackageGraphError, PackageId,
	ProjectDiagnostics, ProjectId, RuntimeDefinitionError, ToolingModuleDeclarations,
};

pub use nymph_diagnostics::SourceVersion;

#[cfg(feature = "test-support")]
pub use emission::StableEmittedProject;

#[cfg(feature = "test-support")]
pub use benchmark_support::{
	BenchmarkPhaseTiming, BenchmarkProfile, begin_benchmark_profile, finish_benchmark_profile,
};

#[cfg(feature = "test-support")]
pub use session::SemanticQueryEvent;

#[cfg(feature = "test-support")]
pub use test_support::{GraphFixture, GraphShape};

use nymph_diagnostics::Diagnostic;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompilerOptions {
	pub profile: BuildProfile,
	pub lints: std::collections::BTreeMap<String, LintLevel>,
}

#[cfg(feature = "test-support")]
mod test_support {
	use std::collections::BTreeMap;

	#[derive(Clone, Copy, Debug)]
	pub enum GraphShape {
		Single,
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
			if matches!(self, Self::Single) {
				fixture.sources.insert(
					"main".to_string(),
					"public func root_value(): int = 0".to_string(),
				);
				return fixture;
			}
			let entry_imports: Vec<String> = match self {
				Self::Single => unreachable!("single fixture returned above"),
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

/// Statically selected executable-root adapter and its exact canonical enum
/// binding when value classification is required.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompiledEntryRoot {
	Void,
	Option { binding: String },
	Result { binding: String },
	TaskVoid,
	TaskOption { binding: String },
	TaskResult { binding: String },
}

/// The result of a successful [`compile_project`]: the whole program as one
/// runnable JS string, plus the entry module's `main` — every OTHER
/// top-level name in the project is renamed (step 2 above) to stay globally
/// unique, but the entry module's own `main` is deliberately exempted (see
/// [`Self::entry_symbol`]), so `entry_main` is always the literal `"main"`
/// and a caller can append `main();` exactly like the single-module facade
/// (`compile`/`compile_entry`) already does.
#[derive(Clone, PartialEq, Eq)]
pub struct CompiledProject {
	pub js: String,
	pub entry_main: String,
	/// Present only when this project was compiled in entry mode. The emitted
	/// `js` remains inert; executable hosts consume this separate adapter fact.
	pub entry_root: Option<CompiledEntryRoot>,
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

pub(crate) const FACADE_PROJECT: &str = "__nymph_internal_facade_project__";

fn facade_session(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	std_provider: &dyn Fn(&str) -> Option<String>,
) -> (CompilerSession, ProjectId, ModulePath) {
	facade_session_with_options(entry, load, std_provider, &CompilerOptions::default())
}

fn facade_session_with_options(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	std_provider: &dyn Fn(&str) -> Option<String>,
	options: &CompilerOptions,
) -> (CompilerSession, ProjectId, ModulePath) {
	let project = ProjectId::new(FACADE_PROJECT);
	let mut session =
		CompilerSession::from_source_loaders(project.clone(), entry, load, std_provider);
	session.set_build_profile(options.profile);
	session.set_project_lints(project.clone(), options.lints.clone());
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

/// Check an entry-mode project against the compiler's embedded standard
/// library. This is the check-only counterpart to calling
/// [`compile_project_with_std`] with [`crate::embedded_std_provider`]: it uses
/// the same project graph and shipped std sources, but never lowers or emits.
pub fn check_project_with_embedded_std(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
) -> Vec<ProjectDiagnostic> {
	check_project_with_std(entry, load, &crate::embedded_std_provider)
}

pub fn check_project_with_embedded_std_and_options(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	options: &CompilerOptions,
) -> Vec<ProjectDiagnostic> {
	let (session, project, entry) =
		facade_session_with_options(entry, load, &crate::embedded_std_provider, options);
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

/// Library-mode counterpart of [`check_project_with_embedded_std`]. The root
/// module is not required to declare `main`, while imports and ambient core/std
/// availability remain identical to an embedded-std project compile.
pub fn check_project_library_with_embedded_std(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
) -> Vec<ProjectDiagnostic> {
	check_project_library_with_std(entry, load, &crate::embedded_std_provider)
}

pub fn check_project_library_with_embedded_std_and_options(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	options: &CompilerOptions,
) -> Vec<ProjectDiagnostic> {
	let (session, project, entry) =
		facade_session_with_options(entry, load, &crate::embedded_std_provider, options);
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

pub fn compile_project_with_embedded_std_and_options(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	options: &CompilerOptions,
) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
	let (session, project, entry) =
		facade_session_with_options(entry, load, &crate::embedded_std_provider, options);
	session
		.compile_project(project, entry, nymph_sema::EntryMode::Entry)
		.map(|compiled| compiled.as_ref().clone())
		.map_err(|diagnostics| diagnostics.iter().cloned().collect())
}

pub fn compile_project_with_embedded_std_options_and_source_uris(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	options: &CompilerOptions,
	uri_for_module: &dyn Fn(&str) -> Option<String>,
) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
	let (mut session, project, entry) =
		facade_session_with_options(entry, load, &crate::embedded_std_provider, options);
	session.set_project_source_uris(&project, uri_for_module);
	session
		.compile_project(project, entry, nymph_sema::EntryMode::Entry)
		.map(|compiled| compiled.as_ref().clone())
		.map_err(|diagnostics| diagnostics.iter().cloned().collect())
}

pub fn compile_project_library_with_embedded_std_options_and_source_uris(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	options: &CompilerOptions,
	uri_for_module: &dyn Fn(&str) -> Option<String>,
) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
	let (mut session, project, entry) =
		facade_session_with_options(entry, load, &crate::embedded_std_provider, options);
	session.set_project_source_uris(&project, uri_for_module);
	session
		.compile_project(project, entry, nymph_sema::EntryMode::Library)
		.map(|compiled| compiled.as_ref().clone())
		.map_err(|diagnostics| diagnostics.iter().cloned().collect())
}

/// Compile one standalone source through the canonical virtual-module
/// assembly while retaining the facade's unmangled top-level names.
fn standalone_session(source: &str, source_name: &str) -> (CompilerSession, ProjectId, ModulePath) {
	standalone_session_with_options(source, source_name, &CompilerOptions::default())
}

fn standalone_session_with_options(
	source: &str,
	source_name: &str,
	options: &CompilerOptions,
) -> (CompilerSession, ProjectId, ModulePath) {
	const STANDALONE_ENTRY: &str = "__nymph_internal_standalone_entry__";
	let project = ProjectId::new(FACADE_PROJECT);
	let path = ModulePath::new(STANDALONE_ENTRY).expect("standalone key is canonical");
	let mut session = CompilerSession::from_builtin_sources(Default::default());
	session.set_build_profile(options.profile);
	session.set_project_lints(project.clone(), options.lints.clone());
	session.set_source_with_location(
		project.clone(),
		path.clone(),
		source.to_string(),
		SourceVersion(1),
		source_name,
		None::<String>,
	);
	(session, project, path)
}

pub(crate) fn check_standalone(
	source: &str,
	path: &str,
	entry_mode: nymph_sema::EntryMode,
	ambient_prelude: bool,
) -> Vec<Diagnostic> {
	let (session, project, path) = standalone_session(source, path);
	let diagnostics = if ambient_prelude {
		session.check_project_with_options(project, path, entry_mode, true)
	} else {
		session.check_project_without_prelude(project, path, entry_mode)
	};
	diagnostics.iter().map(|item| item.diag.clone()).collect()
}

pub(crate) fn compile_standalone(
	source: &str,
	path: &str,
	entry_mode: nymph_sema::EntryMode,
) -> Result<String, Vec<Diagnostic>> {
	let (session, project, path) = standalone_session(source, path);
	session
		.compile_project_with_options(project, path, entry_mode, true)
		.map(|compiled| compiled.js.clone())
		.map_err(|diags| diags.iter().map(|item| item.diag.clone()).collect())
}

pub(crate) fn compile_standalone_with_options(
	source: &str,
	path: &str,
	entry_mode: nymph_sema::EntryMode,
	options: &CompilerOptions,
) -> Result<String, Vec<Diagnostic>> {
	let (session, project, path) = standalone_session_with_options(source, path, options);
	session
		.compile_project_with_options(project, path, entry_mode, true)
		.map(|compiled| compiled.js.clone())
		.map_err(|diags| diags.iter().map(|item| item.diag.clone()).collect())
}

pub(crate) fn compile_standalone_report(
	source: &str,
	path: &str,
	entry_mode: nymph_sema::EntryMode,
) -> crate::StandaloneCompileReport {
	let (session, project, path) = standalone_session(source, path);
	let mut diagnostics = session
		.check_project_with_options(project.clone(), path.clone(), entry_mode, true)
		.iter()
		.map(|item| item.diag.clone())
		.collect::<Vec<_>>();
	let js = if diagnostics.iter().any(Diagnostic::is_error) {
		None
	} else {
		match session.compile_project_with_options(project, path, entry_mode, true) {
			Ok(compiled) => Some(compiled.js.clone()),
			Err(failed) => {
				diagnostics.extend(failed.iter().map(|item| item.diag.clone()));
				None
			}
		}
	};
	crate::StandaloneCompileReport { js, diagnostics }
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

pub fn compile_project_library_with_embedded_std_and_options(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	options: &CompilerOptions,
) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
	let (session, project, entry) =
		facade_session_with_options(entry, load, &crate::embedded_std_provider, options);
	session
		.compile_project(project, entry, nymph_sema::EntryMode::Library)
		.map(|compiled| compiled.as_ref().clone())
		.map_err(|diagnostics| diagnostics.iter().cloned().collect())
}

#[cfg(test)]
mod stable_session_contracts {
	use super::*;
	use nymph_sema::EntryMode;

	#[test]
	fn one_shot_and_retained_sessions_have_exact_diagnostic_and_emission_parity() {
		let sources = [
			"public func answer(): int = 1",
			"public func answer(): int = 2",
			"public func answer(value: Missing): int = 2",
		];
		let (mut retained, project, path) = standalone_session(sources[0], "ignored.nym");

		for (index, source) in sources.into_iter().enumerate() {
			if index != 0 {
				retained.set_source(
					project.clone(),
					path.clone(),
					source.to_string(),
					SourceVersion(index as i64 + 1),
				);
			}
			let one_shot = compile_standalone_report(source, "ignored.nym", EntryMode::Library);
			let retained_diagnostics = retained
				.check_project_with_options(project.clone(), path.clone(), EntryMode::Library, true)
				.iter()
				.map(|diagnostic| diagnostic.diag.clone())
				.collect::<Vec<_>>();
			let retained_js = if retained_diagnostics.iter().any(Diagnostic::is_error) {
				None
			} else {
				Some(
					retained
						.compile_project_with_options(project.clone(), path.clone(), EntryMode::Library, true)
						.expect("checked retained source emits")
						.js
						.clone(),
				)
			};

			assert_eq!(one_shot.diagnostics, retained_diagnostics, "source {index}");
			assert_eq!(one_shot.js, retained_js, "source {index}");
		}
	}
}
