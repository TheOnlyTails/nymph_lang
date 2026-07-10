// oxc 0.138 deprecates most `AstBuilder` node-construction methods in favor of a
// "new AstBuilder interface" (oxc-project/oxc#23043) that is still landing upstream
// and is not yet present in this crate version. The deprecated methods below are the
// only usable construction path in 0.138 and are what the reference (oxc 0.123)
// transpiler also relies on; re-evaluate this `allow` when upgrading oxc.
#![allow(deprecated)]

use oxc::{
	allocator::{Allocator, Vec as OxcVec},
	ast::{AstBuilder, ast::*},
	codegen::Codegen,
	span::SPAN,
};

use nymph_hir::hir::{BinOp, HirExpr, HirFunc, HirModule, HirStmt, UnOp};

/// Intermediate representation for expression-valued code.
///
/// In Nymph, blocks (and eventually `if`/`while` in value position) are
/// expressions. When emitting to JS we may need to wrap them in an IIFE.
/// `JsValue` keeps the leading statements separate from the final expression
/// so the common case (no statements) can emit the expression directly.
struct JsValue<'a> {
	stmts: OxcVec<'a, Statement<'a>>,
	expr: Expression<'a>,
}

impl<'a> JsValue<'a> {
	/// Collapse into a single JS expression.
	/// If there are leading statements, wrap in an IIFE:
	/// `(() => { ...stmts; return expr; })()`
	fn into_expression(self, ast: AstBuilder<'a>) -> Expression<'a> {
		if self.stmts.is_empty() {
			return self.expr;
		}

		let mut body_stmts = self.stmts;
		body_stmts.push(ast.statement_return(SPAN, Some(self.expr)));

		let body = ast.function_body(SPAN, ast.vec(), body_stmts);
		let params = ast.formal_parameters(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			ast.vec(),
			oxc::ast::NONE,
		);
		let arrow = ast.expression_arrow_function(
			SPAN,
			false,
			false,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			body,
		);

		ast.expression_call(SPAN, arrow, oxc::ast::NONE, ast.vec(), false)
	}
}

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
		//
		// When the body is itself a `Block`, emit its statements directly into the
		// function body (followed by `return <tail>;`) instead of wrapping them in a
		// needless IIFE via `emit_expr`/`into_expression`.
		let mut body_stmts = self.ast.vec();
		match &func.body {
			HirExpr::Block { .. } => {
				let value = self.emit_value(&func.body);
				body_stmts.extend(value.stmts);
				body_stmts.push(self.ast.statement_return(SPAN, Some(value.expr)));
			}
			other => {
				let body_expr = self.emit_expr(other);
				body_stmts.push(self.ast.statement_return(SPAN, Some(body_expr)));
			}
		}
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
			HirExpr::Block { .. } => self.emit_value(expr).into_expression(self.ast),
			HirExpr::If { .. } | HirExpr::While { .. } => {
				unreachable!("value-position if/while are handled in Task 6")
			}
		}
	}

	/// Emit a single HIR statement as a JS statement.
	fn emit_stmt(&self, stmt: &HirStmt) -> Statement<'a> {
		match stmt {
			HirStmt::Let {
				name,
				mutable,
				value,
			} => {
				let kind = if *mutable {
					VariableDeclarationKind::Let
				} else {
					VariableDeclarationKind::Const
				};
				let init = self.emit_expr(value);
				let pat = self
					.ast
					.binding_pattern_binding_identifier(SPAN, self.ast.allocator.alloc_str(name));
				let declarator =
					self
						.ast
						.variable_declarator(SPAN, kind, pat, oxc::ast::NONE, Some(init), false);
				let decl = self
					.ast
					.variable_declaration(SPAN, kind, self.ast.vec1(declarator), false);
				Statement::from(Declaration::VariableDeclaration(self.ast.alloc(decl)))
			}
			HirStmt::Expr(e) => {
				let expr = self.emit_expr(e);
				self.ast.statement_expression(SPAN, expr)
			}
		}
	}

	/// Emit an expression as a `JsValue`: leading statements plus a final expression.
	///
	/// For `Block { stmts, tail }`, each statement is emitted in order and the tail
	/// (or `undefined` if absent) becomes the final expression. Any other expression
	/// has no leading statements.
	fn emit_value(&self, expr: &HirExpr) -> JsValue<'a> {
		match expr {
			HirExpr::Block { stmts, tail } => {
				let mut js_stmts = self.ast.vec();
				for stmt in stmts {
					js_stmts.push(self.emit_stmt(stmt));
				}
				let tail_expr = match tail {
					Some(tail) => self.emit_expr(tail),
					None => self.ast.expression_identifier(SPAN, "undefined"),
				};
				JsValue {
					stmts: js_stmts,
					expr: tail_expr,
				}
			}
			other => JsValue {
				stmts: self.ast.vec(),
				expr: self.emit_expr(other),
			},
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
