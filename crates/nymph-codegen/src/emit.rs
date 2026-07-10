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

use nymph_hir::hir::{BinOp, HirExpr, HirFunc, HirModule, UnOp};

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
		let mut js_params = self.ast.vec();
		for param in &func.params {
			let binding_pattern = self
				.ast
				.binding_pattern_binding_identifier(SPAN, self.ast.allocator.alloc_str(param));
			js_params.push(self.ast.plain_formal_parameter(SPAN, binding_pattern));
		}
		let params = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::FormalParameter,
			js_params,
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
			HirExpr::Str(s) => {
				self
					.ast
					.expression_string_literal(SPAN, self.ast.allocator.alloc_str(s), None)
			}
			HirExpr::Bool(b) => self.ast.expression_boolean_literal(SPAN, *b),
			HirExpr::Char(c) => {
				// A Nymph char is a single-character JS string.
				let s = self.ast.allocator.alloc_str(&c.to_string());
				self.ast.expression_string_literal(SPAN, s, None)
			}
			HirExpr::Local(name) => self
				.ast
				.expression_identifier(SPAN, self.ast.allocator.alloc_str(name)),
			HirExpr::Binary { op, lhs, rhs } => {
				let left = self.emit_expr(lhs);
				let right = self.emit_expr(rhs);
				self.emit_binary(*op, left, right)
			}
			HirExpr::Unary { op, operand } => {
				let inner = self.emit_expr(operand);
				let operator = match op {
					UnOp::Neg => UnaryOperator::UnaryNegation,
					UnOp::Not => UnaryOperator::LogicalNot,
				};
				self.ast.expression_unary(SPAN, operator, inner)
			}
			HirExpr::Call { callee, args } => {
				let callee = self.emit_expr(callee);
				let mut arguments = self.ast.vec();
				for arg in args {
					arguments.push(Argument::from(self.emit_expr(arg)));
				}
				self
					.ast
					.expression_call(SPAN, callee, oxc::ast::NONE, arguments, false)
			}
			HirExpr::Block { .. } | HirExpr::If { .. } | HirExpr::While { .. } => {
				unreachable!("control-flow expressions are handled in Task 5/6")
			}
		}
	}

	fn emit_binary(&self, op: BinOp, left: Expression<'a>, right: Expression<'a>) -> Expression<'a> {
		// Logical operators are a distinct oxc node from binary operators.
		if let BinOp::And | BinOp::Or = op {
			let operator = if op == BinOp::And {
				LogicalOperator::And
			} else {
				LogicalOperator::Or
			};
			return self.ast.expression_logical(SPAN, left, operator, right);
		}
		let operator = match op {
			BinOp::Add => BinaryOperator::Addition,
			BinOp::Sub => BinaryOperator::Subtraction,
			BinOp::Mul => BinaryOperator::Multiplication,
			BinOp::Div => BinaryOperator::Division,
			BinOp::Rem => BinaryOperator::Remainder,
			BinOp::Pow => BinaryOperator::Exponential,
			BinOp::Eq => BinaryOperator::StrictEquality,
			BinOp::Ne => BinaryOperator::StrictInequality,
			BinOp::Lt => BinaryOperator::LessThan,
			BinOp::Le => BinaryOperator::LessEqualThan,
			BinOp::Gt => BinaryOperator::GreaterThan,
			BinOp::Ge => BinaryOperator::GreaterEqualThan,
			BinOp::BitAnd => BinaryOperator::BitwiseAnd,
			BinOp::BitOr => BinaryOperator::BitwiseOR,
			BinOp::BitXor => BinaryOperator::BitwiseXOR,
			BinOp::Shl => BinaryOperator::ShiftLeft,
			BinOp::Shr => BinaryOperator::ShiftRight,
			BinOp::And | BinOp::Or => unreachable!("handled above"),
		};
		self.ast.expression_binary(SPAN, left, operator, right)
	}
}
