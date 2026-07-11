//! Structural lowering of the AST into the code-generation HIR.
//!
//! Slice 1 was a pure syntactic walk that consumed neither type annotations nor
//! the interner, because JS needs no type information to emit correct
//! scalar/control-flow code (see the slice-1 plan's Design Decisions). Slice 2A
//! starts consuming the checker's output: index-access lowering must know whether
//! the receiver is a `Map` (→ `HirExpr::MapGet`) or a list/tuple (→ `HirExpr::Index`),
//! which is only recorded in the checker's `Annotations` side-table. `lower_hir` now
//! takes the full `Checked` result and threads `&Annotations`/`&Interner` down through
//! a `Lowerer` so later slices can add further type-directed lowering without another
//! signature change.

use ecow::EcoString;
use nymph_ast::{
	decl::{Declaration, FuncDeclaration, Module},
	expr::{Expr, ExprKind, ListItem, MapEntry, Statement},
	ops::{AssignOperator, BinaryOperator, PrefixOperator},
};
use nymph_hir::hir::{BinOp, HirClass, HirExpr, HirFunc, HirModule, HirStmt, UnOp};
use nymph_hir::ty::{Interner, TyKind};
use rustc_hash::FxHashSet;

use crate::{Annotations, Checked};

/// Lower a checked module into the code-generation HIR, consulting `checked`'s
/// annotations/interner for type-directed decisions (e.g. index-access dispatch).
pub fn lower_hir(module: &Module, checked: &Checked) -> HirModule {
	// A call whose callee names a struct is construction, not an ordinary call.
	// Collect the module's struct names up front so `lower_expr` can dispatch on
	// them. Sound because lowering runs only on error-free programs, where a struct
	// and a function cannot share a name.
	let struct_names = module
		.members
		.iter()
		.filter_map(|decl| match decl {
			Declaration::Struct { name, .. } => Some(name.0.clone()),
			_ => None,
		})
		.collect();
	let lowerer = Lowerer {
		annotations: &checked.annotations,
		interner: &checked.interner,
		struct_names,
	};
	lowerer.lower_module(module)
}

/// Carries the checker's output through the recursive lowering walk.
struct Lowerer<'a> {
	annotations: &'a Annotations,
	interner: &'a Interner,
	struct_names: FxHashSet<EcoString>,
}

impl Lowerer<'_> {
	fn lower_module(&self, module: &Module) -> HirModule {
		let mut funcs = Vec::new();
		let mut classes = Vec::new();
		for decl in &module.members {
			match decl {
				Declaration::Func { meta, body, .. } => funcs.push(self.lower_func(meta, body)),
				Declaration::Struct { name, fields, .. } => classes.push(HirClass {
					name: name.0.clone(),
					fields: fields.iter().map(|f| f.0.name.0.clone()).collect(),
				}),
				_ => {}
			}
		}
		HirModule { funcs, classes }
	}

	fn lower_func(&self, meta: &FuncDeclaration, body: &Expr) -> HirFunc {
		let params = meta.params.iter().map(|p| param_name(&p.0.name)).collect();
		HirFunc {
			name: meta.name.0.clone(),
			params,
			body: self.lower_expr(body),
		}
	}

	fn lower_expr(&self, expr: &Expr) -> HirExpr {
		match &expr.kind {
			ExprKind::Int(v) => HirExpr::Num(v.0 as f64),
			ExprKind::UInt(v) => HirExpr::Num(v.0 as f64),
			ExprKind::Float(v) => HirExpr::Num(v.0.into_inner()),
			ExprKind::Boolean(b) => HirExpr::Bool(b.0),
			ExprKind::Char(c) => HirExpr::Char(c.0),
			ExprKind::Identifier(name) => HirExpr::Local(name.0.clone()),
			ExprKind::Grouped(inner) => self.lower_expr(inner),
			ExprKind::Call { func, args, .. } => {
				// A call whose callee names a struct is construction → `New`. 2B supports
				// labeled fields only; positional construction is deferred.
				if let ExprKind::Identifier(name) = &func.kind
					&& self.struct_names.contains(&name.0)
				{
					let fields = args
						.iter()
						.map(|a| {
							let label =
								a.0.name.as_ref().unwrap_or_else(|| {
									panic!("slice-2b struct construction requires labeled fields")
								});
							(label.0.clone(), self.lower_expr(&a.0.value))
						})
						.collect();
					HirExpr::New {
						class: name.0.clone(),
						fields,
					}
				} else {
					HirExpr::Call {
						callee: Box::new(self.lower_expr(func)),
						args: args.iter().map(|a| self.lower_expr(&a.0.value)).collect(),
					}
				}
			}
			ExprKind::MemberAccess { parent, member, .. } => HirExpr::Field {
				recv: Box::new(self.lower_expr(parent)),
				name: member.0.clone(),
			},
			ExprKind::Tuple(items) => HirExpr::Array(self.lower_items(items)),
			ExprKind::List(items) => HirExpr::Array(self.lower_items(items)),
			ExprKind::Map(entries) => HirExpr::MapLit(self.lower_map_entries(entries)),
			ExprKind::IndexAccess { parent, index, .. } => {
				// Dispatch on the receiver's recorded type: Map → get, else subscript.
				let recv = self.lower_expr(parent);
				let index = self.lower_expr(index);
				let recv_is_map = self
					.annotations
					.get(parent.id)
					.is_some_and(|info| matches!(self.interner.kind(info.ty), TyKind::Map(..)));
				if recv_is_map {
					HirExpr::MapGet {
						recv: Box::new(recv),
						key: Box::new(index),
					}
				} else {
					HirExpr::Index {
						recv: Box::new(recv),
						index: Box::new(index),
					}
				}
			}
			ExprKind::BinaryOp { lhs, op, rhs } => HirExpr::Binary {
				op: lower_binop(*op),
				lhs: Box::new(self.lower_expr(lhs)),
				rhs: Box::new(self.lower_expr(rhs)),
			},
			ExprKind::PrefixOp { op, value } => HirExpr::Unary {
				op: lower_prefix(*op),
				operand: Box::new(self.lower_expr(value)),
			},
			ExprKind::AssignOp { lhs, op, rhs } => {
				// A compound assignment `a op= b` desugars to `a = a op b`; a plain `=`
				// assigns the value directly.
				let value = match assign_binop(*op) {
					None => self.lower_expr(rhs),
					Some(binop) => HirExpr::Binary {
						op: binop,
						lhs: Box::new(self.lower_expr(lhs)),
						rhs: Box::new(self.lower_expr(rhs)),
					},
				};
				HirExpr::Assign {
					target: Box::new(self.lower_expr(lhs)),
					value: Box::new(value),
				}
			}
			ExprKind::Block { body, .. } => self.lower_block(body),
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => HirExpr::If {
				cond: Box::new(self.lower_expr(condition)),
				then: Box::new(self.lower_expr(then)),
				otherwise: otherwise.as_ref().map(|e| Box::new(self.lower_expr(e))),
			},
			ExprKind::While {
				condition, body, ..
			} => HirExpr::While {
				cond: Box::new(self.lower_expr(condition)),
				body: Box::new(self.lower_expr(body)),
			},
			other => panic!("slice-2a lowering does not yet handle {other:?}"),
		}
	}

	/// Lower a list/tuple literal's items. 2A does not yet support spread elements.
	fn lower_items(&self, items: &[nymph_ast::Spanned<ListItem>]) -> Vec<HirExpr> {
		items
			.iter()
			.map(|item| match &item.0 {
				ListItem::Expr(e) => self.lower_expr(e),
				ListItem::Spread(_) => panic!("slice-2a lowering does not yet handle spread list items"),
			})
			.collect()
	}

	/// Lower a map literal's entries. 2A does not yet support spread entries.
	fn lower_map_entries(&self, entries: &[nymph_ast::Spanned<MapEntry>]) -> Vec<(HirExpr, HirExpr)> {
		entries
			.iter()
			.map(|entry| match &entry.0 {
				MapEntry::Entry(k, v) => (self.lower_expr(k), self.lower_expr(v)),
				MapEntry::Spread(_) => panic!("slice-2a lowering does not yet handle spread map entries"),
			})
			.collect()
	}

	fn lower_block(&self, body: &[nymph_ast::Spanned<Statement>]) -> HirExpr {
		let mut stmts = Vec::new();
		let mut tail = None;
		for (i, stmt) in body.iter().enumerate() {
			let is_last = i + 1 == body.len();
			match &stmt.0 {
				Statement::Let { meta, value } => stmts.push(HirStmt::Let {
					name: param_name(&meta.name),
					mutable: meta.mutable,
					value: self.lower_expr(value),
				}),
				Statement::Expr(e) => {
					if is_last {
						tail = Some(Box::new(self.lower_expr(e)));
					} else {
						stmts.push(HirStmt::Expr(self.lower_expr(e)));
					}
				}
			}
		}
		HirExpr::Block { stmts, tail }
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
