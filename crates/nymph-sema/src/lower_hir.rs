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
	expr::{CallArg, Expr, ExprKind, ListItem, MapEntry, Statement},
	ops::{AssignOperator, BinaryOperator, PrefixOperator},
};
use nymph_hir::hir::{
	BinOp, HirArm, HirClass, HirEnum, HirExpr, HirFunc, HirLit, HirModule, HirPat, HirRange, HirStmt,
	HirVariant, UnOp,
};
use nymph_hir::ty::{Interner, TyKind};
use rustc_hash::FxHashSet;

use crate::{Annotations, Checked};

/// Lower a checked module into the code-generation HIR, consulting `checked`'s
/// annotations/interner for type-directed decisions (e.g. index-access dispatch).
pub fn lower_hir(module: &Module, checked: &Checked) -> HirModule {
	// A call whose callee names a struct is construction, not an ordinary call.
	// Collect the module's struct names up front so `lower_expr` can dispatch on
	// them. This mirrors the checker's own dispatch: `infer_call` treats *any*
	// identifier resolving to a struct def as construction, before trying variant/
	// method/function resolution — so lowering stays consistent with checking.
	// ASSUMPTION: every constructible struct is declared in this module. That holds
	// for the current single-module pipeline; when cross-module imports are wired,
	// this set must also include imported struct names (otherwise an imported
	// `Point(…)` would lower to a plain call instead of `New`).
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
		let mut enums = Vec::new();
		for decl in &module.members {
			match decl {
				Declaration::Func { meta, body, .. } => funcs.push(self.lower_func(meta, body)),
				Declaration::Struct { name, fields, .. } => classes.push(HirClass {
					name: name.0.clone(),
					fields: fields.iter().map(|f| f.0.name.0.clone()).collect(),
					methods: Vec::new(),
				}),
				Declaration::Enum { name, variants, .. } => enums.push(HirEnum {
					name: name.0.clone(),
					variants: variants
						.iter()
						.map(|v| HirVariant {
							name: v.0.name.0.clone(),
							fields: v.0.fields.iter().map(|f| f.0.name.0.clone()).collect(),
						})
						.collect(),
				}),
				_ => {}
			}
		}
		HirModule {
			funcs,
			classes,
			enums,
		}
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
			ExprKind::Identifier(name) => match self.annotations.variant_of(expr.id) {
				// A bare name resolving to a variant (`None`, or `Some` as a value) →
				// the variant binding `Enum.Variant`.
				Some(res) => HirExpr::VariantRef {
					enum_name: res.enum_name.clone(),
					variant: res.variant.clone(),
				},
				None => HirExpr::Local(name.0.clone()),
			},
			ExprKind::Grouped(inner) => self.lower_expr(inner),
			ExprKind::Call { func, args, .. } => {
				// A call the checker resolved to a variant is variant construction →
				// `VariantNew` (bare `Some(…)` or qualified `Opt.Some(…)`).
				if let Some(variant_new) = self.variant_new(expr.id, args) {
					variant_new
				}
				// A call whose callee names a struct is construction → `New`. 2B supports
				// labeled fields only; positional construction is deferred.
				else if let ExprKind::Identifier(name) = &func.kind
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
			ExprKind::MemberAccess { parent, member, .. } => {
				match self.annotations.variant_of(expr.id) {
					// A qualified nullary reference `Opt.None` → the variant binding.
					Some(res) => HirExpr::VariantRef {
						enum_name: res.enum_name.clone(),
						variant: res.variant.clone(),
					},
					None => HirExpr::Field {
						recv: Box::new(self.lower_expr(parent)),
						name: member.0.clone(),
					},
				}
			}
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
			ExprKind::Match { value, arms } => {
				let arms = arms
					.iter()
					.map(|arm| HirArm {
						pat: self.lower_pattern(&arm.pattern),
						guard: arm.guard.as_ref().map(|g| self.lower_expr(g)),
						body: self.lower_expr(&arm.body),
					})
					.collect();
				HirExpr::Match {
					scrutinee: Box::new(self.lower_expr(value)),
					arms,
				}
			}
			other => panic!("slice-2a lowering does not yet handle {other:?}"),
		}
	}

	/// Lower an AST pattern into a `HirPat`. 3B handles the full pattern surface:
	/// scalar/string literals, bindings, placeholders, variant/struct/tuple/list/map/
	/// range/union patterns. Deferred edges panic loudly: map-rest, non-literal map
	/// keys, interpolated/escaped string patterns.
	fn lower_pattern(&self, pat: &nymph_ast::Spanned<nymph_ast::expr::Pattern>) -> HirPat {
		use nymph_ast::expr::{ListPatternEntry, Pattern};
		match &pat.0 {
			Pattern::Placeholder => HirPat::Wildcard,
			Pattern::Int(v) => HirPat::Lit(HirLit::Num(v.0 as f64)),
			Pattern::UInt(v) => HirPat::Lit(HirLit::Num(v.0 as f64)),
			Pattern::Float(v) => HirPat::Lit(HirLit::Num(v.0.into_inner())),
			Pattern::Boolean(b) => HirPat::Lit(HirLit::Bool(b.0)),
			Pattern::Char(c) => HirPat::Lit(HirLit::Char(c.0)),
			Pattern::String(parts) => HirPat::Lit(HirLit::Str(lower_string_pattern(parts))),
			Pattern::Grouped(inner) => self.lower_pattern(inner),
			Pattern::Binding { name, inner } => {
				// A bare name recorded as a variant is a nullary variant pattern; else a
				// binding, optionally with a sub-pattern.
				if let Some(res) = self.annotations.pattern_variant_of(pat.1) {
					HirPat::Variant {
						enum_name: res.enum_name.clone(),
						variant: res.variant.clone(),
						fields: Vec::new(),
					}
				} else {
					let sub = match &inner.0 {
						Pattern::Placeholder => None,
						_ => Some(Box::new(self.lower_pattern(inner))),
					};
					HirPat::Binding {
						name: name.0.clone(),
						sub,
					}
				}
			}
			Pattern::Struct { fields, .. } => {
				let lowered = self.lower_struct_fields(fields);
				// A `Pattern::Struct` recorded as a variant is a variant pattern; otherwise
				// it is a struct pattern (irrefutable, binds fields only).
				match self.annotations.pattern_variant_of(pat.1) {
					Some(res) => HirPat::Variant {
						enum_name: res.enum_name.clone(),
						variant: res.variant.clone(),
						fields: lowered,
					},
					None => HirPat::Struct { fields: lowered },
				}
			}
			Pattern::Tuple(entries) => HirPat::Tuple(self.lower_pattern_items(entries)),
			Pattern::List(entries) => {
				let mut prefix = Vec::new();
				let mut suffix = Vec::new();
				let mut rest: Option<Option<ecow::EcoString>> = None;
				for entry in entries {
					match &entry.0 {
						ListPatternEntry::Item(p) => {
							if rest.is_none() {
								prefix.push(self.lower_pattern(p));
							} else {
								suffix.push(self.lower_pattern(p));
							}
						}
						ListPatternEntry::Rest(name) => {
							assert!(rest.is_none(), "list pattern has at most one `...` rest");
							rest = Some(name.as_ref().map(|n| n.0.clone()));
						}
					}
				}
				HirPat::List {
					prefix,
					rest,
					suffix,
				}
			}
			Pattern::Map(entries) => {
				use nymph_ast::expr::MapPatternEntry;
				let lowered = entries
					.iter()
					.map(|entry| match &entry.0 {
						MapPatternEntry::Entry(k, v) => (lower_lit_pattern(k), self.lower_pattern(v)),
						MapPatternEntry::Rest(_) => {
							panic!("slice-3b lowering does not yet handle map-pattern rest")
						}
					})
					.collect();
				HirPat::Map(lowered)
			}
			Pattern::Range(kind) => HirPat::Range(lower_range_pattern(kind)),
			Pattern::Union(a, b) => {
				// A union whose sides bind would need cross-branch consistent-name analysis
				// (which the checker doesn't yet do); 3B rejects it here rather than in
				// codegen so the failure is a clear lowering panic like every other deferral.
				let a = self.lower_pattern(a);
				let b = self.lower_pattern(b);
				assert!(
					!pat_binds(&a) && !pat_binds(&b),
					"slice-3b lowering does not yet handle union patterns that bind"
				);
				HirPat::Or(Box::new(a), Box::new(b))
			}
		}
	}

	/// Lower a struct/variant pattern's fields into `(name, sub-pattern)` pairs.
	fn lower_struct_fields(
		&self,
		fields: &[nymph_ast::Spanned<nymph_ast::expr::StructPatternField>],
	) -> Vec<(ecow::EcoString, HirPat)> {
		use nymph_ast::expr::StructPatternField;
		fields
			.iter()
			.filter_map(|f| match &f.0 {
				StructPatternField::Value { name, value } => {
					Some((name.0.clone(), self.lower_pattern(value)))
				}
				StructPatternField::Named(name) => Some((
					name.0.clone(),
					HirPat::Binding {
						name: name.0.clone(),
						sub: None,
					},
				)),
				StructPatternField::Rest => None,
			})
			.collect()
	}

	/// Lower tuple-pattern items (no rest allowed in a tuple).
	fn lower_pattern_items(
		&self,
		entries: &[nymph_ast::Spanned<nymph_ast::expr::ListPatternEntry>],
	) -> Vec<HirPat> {
		use nymph_ast::expr::ListPatternEntry;
		entries
			.iter()
			.map(|entry| match &entry.0 {
				ListPatternEntry::Item(p) => self.lower_pattern(p),
				ListPatternEntry::Rest(_) => panic!("slice-3b lowering does not handle tuple rest"),
			})
			.collect()
	}

	/// If the checker resolved node `id` to a variant, lower a construction call to
	/// `VariantNew`. 2C supports labeled fields only (positional deferred). Returns
	/// `None` when the node is not a variant construction (an ordinary call/struct).
	fn variant_new(
		&self,
		id: nymph_ast::NodeId,
		args: &[nymph_ast::Spanned<CallArg>],
	) -> Option<HirExpr> {
		let res = self.annotations.variant_of(id)?;
		let fields = args
			.iter()
			.map(|a| {
				let label = a
					.0
					.name
					.as_ref()
					.unwrap_or_else(|| panic!("slice-2c variant construction requires labeled fields"));
				(label.0.clone(), self.lower_expr(&a.0.value))
			})
			.collect();
		Some(HirExpr::VariantNew {
			enum_name: res.enum_name.clone(),
			variant: res.variant.clone(),
			fields,
		})
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

/// Lower a literal pattern to a `HirLit` (for map keys and range bounds). Panics on
/// a non-literal pattern (3B only supports literal keys/bounds).
fn lower_lit_pattern(pat: &nymph_ast::Spanned<nymph_ast::expr::Pattern>) -> HirLit {
	use nymph_ast::expr::Pattern;
	match &pat.0 {
		Pattern::Int(v) => HirLit::Num(v.0 as f64),
		Pattern::UInt(v) => HirLit::Num(v.0 as f64),
		Pattern::Float(v) => HirLit::Num(v.0.into_inner()),
		Pattern::Boolean(b) => HirLit::Bool(b.0),
		Pattern::Char(c) => HirLit::Char(c.0),
		Pattern::String(parts) => HirLit::Str(lower_string_pattern(parts)),
		Pattern::Grouped(inner) => lower_lit_pattern(inner),
		other => panic!("slice-3b expects a literal pattern (map key / range bound), got {other:?}"),
	}
}

/// Whether a lowered pattern introduces any binding — used to reject binding
/// unions, which 3B does not support.
fn pat_binds(pat: &HirPat) -> bool {
	match pat {
		HirPat::Wildcard | HirPat::Lit(_) | HirPat::Range(_) => false,
		HirPat::Binding { .. } => true,
		HirPat::Variant { fields, .. } | HirPat::Struct { fields } => {
			fields.iter().any(|(_, p)| pat_binds(p))
		}
		HirPat::Tuple(ps) => ps.iter().any(pat_binds),
		HirPat::List {
			prefix,
			rest,
			suffix,
		} => matches!(rest, Some(Some(_))) || prefix.iter().chain(suffix).any(pat_binds),
		HirPat::Map(entries) => entries.iter().any(|(_, p)| pat_binds(p)),
		HirPat::Or(a, b) => pat_binds(a) || pat_binds(b),
	}
}

/// Concatenate a string pattern's text parts. 3B string patterns are text-only.
fn lower_string_pattern(
	parts: &[nymph_ast::Spanned<nymph_ast::expr::StringPatternPart>],
) -> ecow::EcoString {
	use nymph_ast::expr::StringPatternPart;
	let mut s = ecow::EcoString::new();
	for part in parts {
		match &part.0 {
			StringPatternPart::Text(t) => s.push_str(t),
			StringPatternPart::EscapeSequence(_) => {
				panic!("slice-3b string patterns are text-only (escapes not yet lowered)")
			}
		}
	}
	s
}

/// Lower a range pattern's bounds into a `HirRange`.
fn lower_range_pattern(kind: &nymph_ast::expr::RangePatternKind) -> HirRange {
	use nymph_ast::expr::RangePatternKind as R;
	match kind {
		R::From(p) => HirRange::From(lower_lit_pattern(p)),
		R::To(p) => HirRange::To(lower_lit_pattern(p)),
		R::ToInclusive(p) => HirRange::ToInclusive(lower_lit_pattern(p)),
		R::Exclusive { min, max } => HirRange::Exclusive {
			min: lower_lit_pattern(min),
			max: lower_lit_pattern(max),
		},
		R::Inclusive { min, max } => HirRange::Inclusive {
			min: lower_lit_pattern(min),
			max: lower_lit_pattern(max),
		},
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
