//! Structural lowering of the AST into the code-generation HIR.
//!
//! Slice 1 is a pure syntactic walk: it consumes neither type annotations nor the
//! interner, because JS needs no type information to emit correct scalar/control-flow
//! code (see the slice-1 plan's Design Decisions). Later slices thread annotations
//! through here for value-copy insertion and operator-overload dispatch.

use nymph_ast::{
	decl::{Declaration, FuncDeclaration, Module},
	expr::{Expr, ExprKind, Statement},
	ops::{AssignOperator, BinaryOperator, PrefixOperator},
};
use nymph_hir::hir::{BinOp, HirExpr, HirFunc, HirModule, HirStmt, UnOp};

/// Lower a checked module into the code-generation HIR.
pub fn lower_hir(module: &Module) -> HirModule {
	let mut funcs = Vec::new();
	for decl in &module.members {
		if let Declaration::Func { meta, body, .. } = decl {
			funcs.push(lower_func(meta, body));
		}
	}
	HirModule { funcs }
}

fn lower_func(meta: &FuncDeclaration, body: &Expr) -> HirFunc {
	let params = meta.params.iter().map(|p| param_name(&p.0.name)).collect();
	HirFunc {
		name: meta.name.0.clone(),
		params,
		body: lower_expr(body),
	}
}

/// The bound name of a simple parameter pattern. Slice 1 supports plain-identifier
/// parameters; destructuring parameters arrive with pattern lowering (Slice 3).
fn param_name(pattern: &nymph_ast::Spanned<nymph_ast::expr::Pattern>) -> ecow::EcoString {
	match &pattern.0 {
		nymph_ast::expr::Pattern::Binding { name, .. } => name.0.clone(),
		other => panic!("slice-1 lowering supports only identifier params, got {other:?}"),
	}
}

fn lower_expr(expr: &Expr) -> HirExpr {
	match &expr.kind {
		ExprKind::Int(v) => HirExpr::Num(v.0 as f64),
		ExprKind::UInt(v) => HirExpr::Num(v.0 as f64),
		ExprKind::Float(v) => HirExpr::Num(v.0.into_inner()),
		ExprKind::Boolean(b) => HirExpr::Bool(b.0),
		ExprKind::Char(c) => HirExpr::Char(c.0),
		ExprKind::Identifier(name) => HirExpr::Local(name.0.clone()),
		ExprKind::Grouped(inner) => lower_expr(inner),
		ExprKind::Call { func, args, .. } => HirExpr::Call {
			callee: Box::new(lower_expr(func)),
			args: args.iter().map(|a| lower_expr(&a.0.value)).collect(),
		},
		ExprKind::BinaryOp { lhs, op, rhs } => HirExpr::Binary {
			op: lower_binop(*op),
			lhs: Box::new(lower_expr(lhs)),
			rhs: Box::new(lower_expr(rhs)),
		},
		ExprKind::PrefixOp { op, value } => HirExpr::Unary {
			op: lower_prefix(*op),
			operand: Box::new(lower_expr(value)),
		},
		ExprKind::AssignOp { lhs, op, rhs } => {
			// A compound assignment `a op= b` desugars to `a = a op b`; a plain `=`
			// assigns the value directly.
			let value = match assign_binop(*op) {
				None => lower_expr(rhs),
				Some(binop) => HirExpr::Binary {
					op: binop,
					lhs: Box::new(lower_expr(lhs)),
					rhs: Box::new(lower_expr(rhs)),
				},
			};
			HirExpr::Assign {
				target: Box::new(lower_expr(lhs)),
				value: Box::new(value),
			}
		}
		ExprKind::Block { body, .. } => lower_block(body),
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => HirExpr::If {
			cond: Box::new(lower_expr(condition)),
			then: Box::new(lower_expr(then)),
			otherwise: otherwise.as_ref().map(|e| Box::new(lower_expr(e))),
		},
		ExprKind::While {
			condition, body, ..
		} => HirExpr::While {
			cond: Box::new(lower_expr(condition)),
			body: Box::new(lower_expr(body)),
		},
		other => panic!("slice-1 lowering does not yet handle {other:?}"),
	}
}

fn lower_block(body: &[nymph_ast::Spanned<Statement>]) -> HirExpr {
	let mut stmts = Vec::new();
	let mut tail = None;
	for (i, stmt) in body.iter().enumerate() {
		let is_last = i + 1 == body.len();
		match &stmt.0 {
			Statement::Let { meta, value } => stmts.push(HirStmt::Let {
				name: param_name(&meta.name),
				mutable: meta.mutable,
				value: lower_expr(value),
			}),
			Statement::Expr(e) => {
				if is_last {
					tail = Some(Box::new(lower_expr(e)));
				} else {
					stmts.push(HirStmt::Expr(lower_expr(e)));
				}
			}
		}
	}
	HirExpr::Block { stmts, tail }
}

fn lower_binop(op: BinaryOperator) -> BinOp {
	use BinaryOperator as B;
	match op {
		B::Plus => BinOp::Add,
		B::Minus => BinOp::Sub,
		B::Times => BinOp::Mul,
		B::Divide => BinOp::Div,
		B::Remainder => BinOp::Rem,
		B::Power => BinOp::Pow,
		B::Equals => BinOp::Eq,
		B::NotEquals => BinOp::Ne,
		B::LessThan => BinOp::Lt,
		B::LessThanEquals => BinOp::Le,
		B::GreaterThan => BinOp::Gt,
		B::GreaterThanEquals => BinOp::Ge,
		B::BoolAnd => BinOp::And,
		B::BoolOr => BinOp::Or,
		B::BitAnd => BinOp::BitAnd,
		B::BitOr => BinOp::BitOr,
		B::BitXor => BinOp::BitXor,
		B::LeftShift => BinOp::Shl,
		B::RightShift => BinOp::Shr,
		other => panic!("slice-1 lowering does not yet handle operator {other:?}"),
	}
}

/// The binary operator a compound assignment desugars to, or `None` for a plain `=`.
fn assign_binop(op: AssignOperator) -> Option<BinOp> {
	use AssignOperator as A;
	Some(match op {
		A::Assign => return None,
		A::PlusAssign => BinOp::Add,
		A::MinusAssign => BinOp::Sub,
		A::TimesAssign => BinOp::Mul,
		A::DivideAssign => BinOp::Div,
		A::RemainderAssign => BinOp::Rem,
		A::PowerAssign => BinOp::Pow,
		A::LeftShiftAssign => BinOp::Shl,
		A::RightShiftAssign => BinOp::Shr,
		A::BitAndAssign => BinOp::BitAnd,
		A::BitXorAssign => BinOp::BitXor,
		A::BitOrAssign => BinOp::BitOr,
		A::BoolAndAssign => BinOp::And,
		A::BoolOrAssign => BinOp::Or,
		other => panic!("slice-1 lowering does not yet handle {other:?}"),
	})
}

fn lower_prefix(op: PrefixOperator) -> UnOp {
	match op {
		PrefixOperator::Negate => UnOp::Neg,
		PrefixOperator::BoolNot => UnOp::Not,
		other => panic!("slice-1 lowering does not yet handle prefix {other:?}"),
	}
}
