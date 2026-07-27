//! The Nymph compiler pipeline: a thin facade over the individual compiler
//! crates (`nymph-syntax`, `nymph-sema`, `nymph-codegen`) that exposes the
//! top-level API for parsing, checking, and compiling Nymph source.
//!
//! The public standalone and project facades both construct a project-backed
//! [`CompilerSession`] and run the stable Salsa semantic, lowering, and
//! emission queries. Standalone source is represented as a single virtual
//! module while its caller-supplied path remains a diagnostic anchor.
//!
//! [`compile`] runs the full pipeline and only lowers/emits when parsing and
//! checking are error-free. [`check`] runs just the parse and check stages
//! and returns every diagnostic (errors and warnings alike), which is the
//! entry point tooling such as an LSP should use.
//!
//! [`compile_entry`] and [`check_entry`] are additive entry-mode counterparts
//! (GG1): identical to [`compile`]/[`check`], except the module is also
//! required to declare a valid top-level `main` — the program's entry point —
//! via [`nymph_sema::check_module_entry_with_prelude`]. Plain
//! [`compile`]/[`check`] never require a `main`, so every existing
//! library-mode caller is unaffected.
//!
//! Because the prelude is flattened ahead of every checked module, a user
//! program can implement (and use) `Plus`/`Comparable`/… without declaring
//! them locally, and `compile` now lowers and emits that program too. A
//! dispatch into a prelude-owned body still panics loudly at lowering time
//! when codegen genuinely cannot materialize it (`external`/intrinsic
//! markers — every primitive arithmetic op, `compare_to_int`/`_float`/
//! `_char`/`_string`, the `Equals`/`Contains` blanket externals — or a
//! still-generic bound satisfied only through the prelude); silent wrong JS
//! is never an acceptable alternative to that loud deferral. Compiling
//! `stdlib/src/ops/mod.nym` itself through this facade is out of scope — the
//! prelude would collide with itself (KK2) — real stdlib compilation arrives
//! with import binding.

mod intrinsics;
mod prelude;
pub mod project;
mod std_source;

use std::path::{Path, PathBuf};

pub use nymph_diagnostics::{Diagnostic, Severity};
pub use project::{
	AmbientCoreModuleKey, BuiltinRuntimeOwnerArtifact, BuiltinRuntimeOwnerShape, CompiledProject,
	CompilerSession, ModuleAnalysis, ModulePath, ProjectDiagnostic, ProjectId, SourceVersion,
	check_project, check_project_library, check_project_library_with_std, check_project_with_std,
	compile_project, compile_project_library, compile_project_library_with_std,
	compile_project_with_std,
};
pub use std_source::embedded_std_provider;

/// Whether a compile/check pass should additionally require a valid
/// top-level `main` entry point ([`nymph_sema::check_module_entry`]) or run
/// as a plain library module ([`nymph_sema::check_module`]).
use nymph_sema::EntryMode;

/// Compile Nymph `source` to a JavaScript module string.
///
/// `path` is the module path used to anchor diagnostics (e.g. for rendering
/// or LSP URIs) — it does not need to correspond to a real file.
///
/// Runs the full pipeline: parse → check → lower → emit. If parsing or
/// checking produces any error diagnostics, lowering and emission are
/// skipped and this returns `Err` with those diagnostics (parse errors
/// followed by check errors). Warnings do not prevent compilation and are
/// discarded here — use [`check`] to observe them.
///
/// # Errors
///
/// Returns `Err` with all error-severity diagnostics from parsing and
/// checking if the source fails to parse or type-check.
pub fn compile(source: &str, path: &str) -> Result<String, Vec<Diagnostic>> {
	compile_impl(source, path, EntryMode::Library)
}

/// Compile Nymph `source` as the program's *entry module*, to a JavaScript
/// module string.
///
/// Identical to [`compile`], except `source` is additionally required to
/// declare a valid top-level `main` (see [`nymph_sema::check_module_entry`]);
/// a missing or mis-shaped `main` is reported as an ordinary error diagnostic
/// alongside any other parse/type errors, and lowering/emission are skipped
/// just as for any other error.
///
/// # Errors
///
/// Returns `Err` with all error-severity diagnostics from parsing and
/// checking if the source fails to parse, fails to type-check, or has no
/// valid top-level `main`.
pub fn compile_entry(source: &str, path: &str) -> Result<String, Vec<Diagnostic>> {
	compile_impl(source, path, EntryMode::Entry)
}

fn compile_impl(source: &str, path: &str, entry: EntryMode) -> Result<String, Vec<Diagnostic>> {
	project::compile_standalone(source, path, entry)
}

/// Parse and check Nymph `source`, returning every diagnostic produced.
///
/// `path` is the module path used to anchor diagnostics (e.g. for rendering
/// or LSP URIs) — it does not need to correspond to a real file.
///
/// Unlike [`compile`], this does not filter by severity and does not lower
/// or emit: it runs parse and check only, and returns all diagnostics from
/// both stages (parse diagnostics followed by check diagnostics), including
/// warnings. This is the entry point tooling and language servers should use
/// to surface the full diagnostic picture for a source file.
pub fn check(source: &str, path: &str) -> Vec<Diagnostic> {
	check_impl(source, path, EntryMode::Library)
}

/// Parse and check Nymph `source` as the program's *entry module*, returning
/// every diagnostic produced.
///
/// Identical to [`check`], except `source` is additionally required to
/// declare a valid top-level `main` (see [`nymph_sema::check_module_entry`]).
pub fn check_entry(source: &str, path: &str) -> Vec<Diagnostic> {
	check_impl(source, path, EntryMode::Entry)
}

fn check_impl(source: &str, path: &str, entry: EntryMode) -> Vec<Diagnostic> {
	project::check_standalone(source, path, entry, true)
}

/// Parse and check Nymph `source` with **no ambient prelude** — the `core`
/// module's own operator interfaces, `Option`/`Result`, `Iterator`/`Iterable`,
/// etc. are *not* injected.
///
/// [`check`] always flattens [`prelude::core_prelude`] ahead of the checked
/// module, which is exactly wrong for a **stdlib source file itself**: opening
/// `stdlib/src/ops/mod.nym` (say) through [`check`] injects a second copy of
/// `std/ops` right next to the real one, so every declaration in it collides
/// with its own ambient copy — a flood of spurious duplicate-declaration
/// errors that has nothing to do with the file's actual content. This entry
/// point checks `source` in isolation instead, exactly like a normal
/// `nymph_sema::check_module` library-mode module with no injected sources.
///
/// This trades self-duplication for a different, honest limitation: a stdlib
/// file that imports siblings (e.g. `option.nym` importing `@/default`) will
/// report those siblings as unresolved, since a prelude-free, project-free
/// check only ever sees the one file. That's an inherent consequence of
/// checking one file in isolation, not a regression — see
/// [`is_stdlib_source_path`] for how callers (the LSP) decide when to use this
/// instead of [`check`].
///
/// `path` is used only to anchor diagnostics, exactly as in [`check`].
pub fn check_without_prelude(source: &str, path: &str) -> Vec<Diagnostic> {
	project::check_standalone(source, path, EntryMode::Library, false)
}

/// The filesystem root of the `stdlib/src` tree embedded (via `include_str!`)
/// into [`prelude::core_prelude`], canonicalized. `None` if it can't be
/// resolved (e.g. the embedding `stdlib/` directory has been moved or deleted
/// out from under a built binary) — callers should treat that as "not a
/// stdlib path".
pub fn stdlib_source_root() -> Option<PathBuf> {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("../../stdlib/src")
		.canonicalize()
		.ok()
}

/// Whether `path` names a file inside the `stdlib/src` tree this compiler
/// embeds as the ambient `core` prelude (see [`stdlib_source_root`]) — the
/// principled signal for "this file IS (part of) the prelude", as opposed to
/// a brittle path/substring guess. A normal user file, anywhere outside that
/// tree, is never a stdlib source path.
///
/// Used by callers (the LSP) to decide between [`check`] (ambient prelude —
/// every ordinary user file) and [`check_without_prelude`] (no ambient
/// prelude — a stdlib source file, which would otherwise duplicate itself
/// against its own injected copy).
pub fn is_stdlib_source_path(path: &Path) -> bool {
	matches!(
		(stdlib_source_root(), path.canonicalize()),
		(Some(root), Ok(p)) if p.starts_with(&root)
	)
}
