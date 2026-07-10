//! JavaScript code generation from the Nymph HIR, via oxc's AST builder + codegen.

mod emit;

use nymph_hir::hir::HirModule;

/// Emit an ES module string for `module`.
pub fn emit(module: &HirModule) -> String {
	emit::Emitter::new().emit_module(module)
}
