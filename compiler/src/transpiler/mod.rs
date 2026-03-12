pub mod emit;
pub mod external;
pub mod operators;

#[cfg(test)]
mod tests;

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::AstBuilder;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_span::{SPAN, SourceType};

use crate::{ast::declaration::Module, types::Context};

use emit::Emitter;

/// Result of transpiling a Nymph module to JavaScript.
pub struct TranspileResult {
	/// The generated ES6 JavaScript source code.
	pub code: String,
}

/// Transpile a type-checked Nymph module to ES6 JavaScript.
///
/// `module` is the parsed AST (from the parser).
/// `ctx` is the type-checking context (from the type checker).
/// `source_path` is the path to the `.nym` file being compiled,
/// used for resolving external declarations.
pub fn transpile(module: &Module, ctx: &Context, source_path: Option<&Path>) -> TranspileResult {
	let allocator = Allocator::default();
	let ast = AstBuilder::new(&allocator);

	let mut emitter = Emitter::new(&allocator, ctx, source_path);
	let js_stmts = emitter.emit_module(module);

	// Build a JS Program node
	let program = ast.program(
		SPAN,
		SourceType::mjs(),
		"",
		ast.vec(),
		None,
		ast.vec(),
		js_stmts,
	);

	// Use OXC Codegen to print the JS AST
	let codegen_result = Codegen::new()
		.with_options(CodegenOptions {
			single_quote: true,
			..CodegenOptions::default()
		})
		.build(&program);

	TranspileResult {
		code: codegen_result.code,
	}
}
