//! JavaScript code generation from the Nymph HIR, via oxc's AST builder + codegen.

#![warn(clippy::all)]

mod box_rt;
mod emit;
mod strip;

use nymph_diagnostics::Diagnostic;
use nymph_hir::hir::HirModule;

pub use box_rt::{
	BOX_MODULE_DECLARATIONS, BOX_MODULE_KEY, box_module_declarations, box_module_source, box_preamble,
};
pub use strip::strip_ts_to_js;

/// Emit an ES module string for `module`.
pub fn emit(module: &HirModule) -> String {
	emit::Emitter::new().emit_module(module)
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
