// oxc 0.138 deprecates most `AstBuilder` node-construction methods in favor of a
// "new AstBuilder interface" (oxc-project/oxc#23043) that is still landing upstream
// and is not yet present in this crate version. The deprecated methods below are the
// only usable construction path in 0.138 and are what the reference (oxc 0.123)
// transpiler also relies on; re-evaluate this `allow` when upgrading oxc.
#![allow(deprecated)]

use oxc::{
	allocator::Allocator,
	ast::{AstBuilder, ast::*},
	codegen::Codegen,
	span::SPAN,
};

use nymph_hir::hir::{HirExpr, HirFunc, HirModule};

pub struct Emitter<'a> {
	ast: AstBuilder<'a>,
	#[allow(dead_code)]
	alloc: &'a Allocator,
}

impl Default for Emitter<'_> {
	fn default() -> Self {
		Self::new()
	}
}

impl<'a> Emitter<'a> {
	pub fn new() -> Emitter<'static> {
		// Leak an allocator for the lifetime of the emit call; the returned String
		// outlives it. (A slice-1 simplification; a later slice can thread an
		// externally-owned Allocator if allocation pressure matters.)
		let alloc: &'static Allocator = Box::leak(Box::new(Allocator::default()));
		Emitter {
			ast: AstBuilder::new(alloc),
			alloc,
		}
	}

	pub fn emit_module(&self, module: &HirModule) -> String {
		let mut stmts = self.ast.vec();
		for func in &module.funcs {
			stmts.push(self.emit_func(func));
		}
		let program = self.ast.program(
			SPAN,
			SourceType::mjs(),
			"",
			self.ast.vec(),
			None,
			self.ast.vec(),
			stmts,
		);
		Codegen::new().build(&program).code
	}

	fn emit_func(&self, func: &HirFunc) -> Statement<'a> {
		// function <name>(<params>) { return <body>; }
		let body_expr = self.emit_expr(&func.body);
		let mut body_stmts = self.ast.vec();
		body_stmts.push(self.ast.statement_return(SPAN, Some(body_expr)));
		let params = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::FormalParameter,
			self.ast.vec(),
			oxc::ast::NONE,
		);
		let fn_body = self.ast.function_body(SPAN, self.ast.vec(), body_stmts);
		let function = self.ast.alloc_function(
			SPAN,
			FunctionType::FunctionDeclaration,
			Some(
				self
					.ast
					.binding_identifier(SPAN, self.ast.allocator.alloc_str(&func.name)),
			),
			false,
			false,
			false,
			oxc::ast::NONE,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			Some(fn_body),
		);
		Statement::FunctionDeclaration(function)
	}

	fn emit_expr(&self, expr: &HirExpr) -> Expression<'a> {
		match expr {
			HirExpr::Num(value) => {
				self
					.ast
					.expression_numeric_literal(SPAN, *value, None, NumberBase::Decimal)
			}
			_ => unreachable!("only Num is supported in the slice-1 spike"),
		}
	}
}
