//! JavaScript code generation from the Nymph HIR, via oxc's AST builder + codegen.

#![warn(clippy::all)]

mod box_rt;
mod emit;
mod strip;

use nymph_diagnostics::Diagnostic;
use nymph_hir::hir::HirModule;
use oxc::allocator::Allocator;

pub use box_rt::{
	BOX_MODULE_DECLARATIONS, BOX_MODULE_KEY, box_module_declarations, box_module_source,
	box_module_source_with_option_enum, box_preamble,
};
pub use strip::{EmbeddedModuleInspection, inspect_embedded_module, strip_ts_to_js};

/// Emit an ES module string for `module`.
pub fn emit(module: &HirModule) -> String {
	let allocator = Allocator::default();
	emit::Emitter::new(&allocator).emit_module(module)
}

/// Emit an ES module with awareness of its canonical module key. Linked
/// external calls targeting that same key import from its intrinsic backing
/// module, while ordinary canonical self-references remain local identifiers.
#[must_use]
pub fn emit_for_module(module: &HirModule, module_key: &str) -> String {
	let allocator = Allocator::default();
	emit::Emitter::for_module(&allocator, module_key).emit_module(module)
}

/// Emit a module for inclusion in a project bundle. Unlike standalone
/// emission, generated box values import the shared `std/box` runtime instead
/// of embedding a private copy in every source module.
#[must_use]
pub fn emit_for_project_module(module: &HirModule, module_key: &str) -> String {
	let allocator = Allocator::default();
	emit::Emitter::for_project_module(&allocator, module_key).emit_module(module)
}

/// Emit a project module while coalescing exact imports already required by the
/// project assembler with imports discovered structurally during HIR emission.
#[must_use]
pub fn emit_for_project_module_with_imports(
	module: &HirModule,
	module_key: &str,
	imports: &[(String, String, String)],
) -> String {
	let allocator = Allocator::default();
	emit::Emitter::for_project_module(&allocator, module_key)
		.with_needed_imports(imports)
		.emit_module(module)
}

/// Compile Nymph source to a JS module string, or return the diagnostics that
/// prevented it. Runs the full pipeline — parse → check → lower → emit — and only
/// lowers/emits when parsing and checking are error-free.
pub fn compile(source: &str, path: &str) -> Result<String, Vec<Diagnostic>> {
	let parsed = nymph_syntax::parse_module(source, path);
	let mut diags: Vec<Diagnostic> = parsed
		.diagnostics
		.iter()
		.filter(|d| d.is_error())
		.cloned()
		.collect();

	let checked = nymph_sema::check_module(&parsed.tree);
	diags.extend(checked.diags.iter().filter(|d| d.is_error()).cloned());

	if !diags.is_empty() {
		return Err(diags);
	}
	Ok(emit(&nymph_sema::lower_hir(&parsed.tree, &checked)))
}
