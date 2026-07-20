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
mod resolve;
mod rewrite;

use nymph_ast::decl::{Module, Visibility};
use nymph_diagnostics::Diagnostic;
use rustc_hash::{FxHashMap, FxHashSet};

use resolve::{GraphBuilder, RawModule};
use rewrite::{DeclaredName, NsInfo, RewriteCtx, declared_names, rewrite_module};

/// A diagnostic attributed to one module of the project, keyed by its
/// canonical path (e.g. `"main"`, `"geometry/vec"`) — the same key `load` was
/// asked for. Render against `"<module>.nym"` and that module's own source.
#[derive(Debug, Clone)]
pub struct ProjectDiagnostic {
	pub module: String,
	pub diag: Diagnostic,
}

/// The result of a successful [`compile_project`]: the whole program as one
/// runnable JS string, plus the entry module's `main` — every OTHER
/// top-level name in the project is renamed (step 2 above) to stay globally
/// unique, but the entry module's own `main` is deliberately exempted (see
/// [`Self::entry_symbol`]), so `entry_main` is always the literal `"main"`
/// and a caller can append `main();` exactly like the single-module facade
/// (`compile`/`compile_entry`) already does.
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
	match Driver::resolve_and_bind(entry, load, std_provider, true) {
		Err(diags) => diags,
		Ok(driver) => driver.check_all(),
	}
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
	match Driver::resolve_and_bind(entry, load, std_provider, false) {
		Err(diags) => diags,
		Ok(driver) => driver.check_all(),
	}
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
	let driver = Driver::resolve_and_bind(entry, load, std_provider, true)?;
	driver.compile_all()
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
	let driver = Driver::resolve_and_bind(entry, load, std_provider, false)?;
	driver.compile_all()
}

/// Owns everything phase 2/3 need once the module graph is resolved: the raw
/// parsed modules, their dependency-first order, per-module stable tags, and
/// every module's declared-name table (for cross-module visibility checks).
struct Driver {
	entry: String,
	/// Whether `entry` must declare a valid top-level `main` — see
	/// [`check_project`] vs [`check_project_library`].
	entry_mode: bool,
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
	) -> Result<Self, Vec<ProjectDiagnostic>> {
		let mut builder = GraphBuilder::new(load, std_provider);
		builder.visit(entry);
		if !builder.diags.is_empty() {
			return Err(builder.diags);
		}

		let order = builder.order;
		let modules = builder.modules;
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

		for key in &order {
			let raw = &modules[key];
			let own_tag = tags[key];
			let own_names: FxHashSet<_> = declared[key].iter().map(|d| d.name.clone()).collect();

			// The entry module's own `main` is exempted from mangling: the checker's
			// entry-point validation (`check_module_entry_with_prelude`) and the
			// CLI/test `run` convention both look for a literally-named top-level
			// `main` — nothing else needs a stable unmangled name, since every other
			// cross-module reference goes through this driver's own rewrite, not
			// the checker's name resolution.
			let is_entry = entry_mode && *key == entry;
			let mut renames: FxHashMap<_, _> = declared[key]
				.iter()
				.filter(|d| !(is_entry && d.name == "main"))
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
		let mut diags = Vec::new();
		for key in &self.order {
			let prelude = self.prelude_slice(key);
			let module = &self.processed[key];
			let checked = if self.entry_mode && *key == self.entry {
				nymph_sema::check_module_entry_with_prelude(module, &prelude)
			} else {
				nymph_sema::check_module_with_prelude(module, &prelude)
			};
			diags.extend(checked.diags.into_iter().map(|d| ProjectDiagnostic {
				module: key.clone(),
				diag: d,
			}));
		}
		diags
	}

	fn compile_all(&self) -> Result<CompiledProject, Vec<ProjectDiagnostic>> {
		let mut diags = Vec::new();
		let mut module_sources: FxHashMap<String, String> = FxHashMap::default();
		for key in &self.order {
			let prelude = self.prelude_slice(key);
			let module = &self.processed[key];
			let checked = if self.entry_mode && *key == self.entry {
				nymph_sema::check_module_entry_with_prelude(module, &prelude)
			} else {
				nymph_sema::check_module_with_prelude(module, &prelude)
			};
			let has_errors = checked.diags.iter().any(Diagnostic::is_error);
			diags.extend(
				checked
					.diags
					.iter()
					.filter(|d| d.is_error())
					.cloned()
					.map(|d| ProjectDiagnostic {
						module: key.clone(),
						diag: d,
					}),
			);
			if has_errors {
				continue;
			}
			// `prelude` is `core_prelude() ++ transitive_deps` (see `prelude_slice`),
			// so the ambient `core` modules occupy the leading `core_prelude().len()`
			// slots and everything after is an EMITTED dependency module — a call to
			// one of those dep structs' methods lowers as a real class-method call
			// rather than a (struct-unsupported) materialization.
			let hir = nymph_sema::lower_hir_with_prelude_and_deps(
				module,
				&prelude,
				crate::prelude::core_prelude().len(),
				&checked,
			);
			let body = nymph_codegen::emit(&hir);
			module_sources.insert(key.clone(), self.wrap_module_js(key, &body));
		}
		if !diags.is_empty() {
			return Err(diags);
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
		module_sources.extend(crate::intrinsics::intrinsic_module_sources());
		let entry_tag = self.tags[&self.entry];
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
	fn wrap_module_js(&self, key: &str, body: &str) -> String {
		let own_tag = self.tags[key];
		let is_entry = self.entry_mode && key == self.entry;

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
			.filter(|d| d.vis != Visibility::Private && d.has_runtime_binding)
			.map(|d| {
				if is_entry && d.name == "main" {
					"main".to_string()
				} else {
					format!("$m{own_tag}${}", d.name)
				}
			})
			.collect();
		export_names.sort_unstable();

		let mut out = String::new();
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
