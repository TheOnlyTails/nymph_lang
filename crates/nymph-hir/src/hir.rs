//! The mid-level typed IR consumed by code generation. Slice 1 covers the
//! scalar/control-flow core and is deliberately *type-free*: JS has a single
//! `number` type and primitive operators map 1:1 to JS operators, so no type
//! information is needed to emit correct JS. Type-carrying fields arrive in later
//! slices, where value-copy and operator-overload dispatch first need them.

use ecow::EcoString;

#[derive(Clone, Debug, PartialEq)]
pub struct HirModule {
	pub funcs: Vec<HirFunc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirFunc {
	pub name: EcoString,
	pub params: Vec<EcoString>,
	pub body: HirExpr,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirStmt {
	/// `let`/`mut` binding. `mutable` selects JS `let` vs `const`.
	Let {
		name: EcoString,
		mutable: bool,
		value: HirExpr,
	},
	/// A bare expression evaluated for its effect.
	Expr(HirExpr),
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirExpr {
	/// Any numeric literal (int/uint/float) — all are JS `number`.
	Num(f64),
	Str(EcoString),
	Bool(bool),
	Char(char),
	/// An identifier or parameter reference.
	Local(EcoString),
	Call {
		callee: Box<HirExpr>,
		args: Vec<HirExpr>,
	},
	/// A tuple or list literal — both emit as a JS array.
	Array(Vec<HirExpr>),
	/// A map literal — emits as `new Map([[k, v], …])`.
	MapLit(Vec<(HirExpr, HirExpr)>),
	/// A subscript into a list/tuple — emits as `recv[index]`.
	Index {
		recv: Box<HirExpr>,
		index: Box<HirExpr>,
	},
	/// A map lookup — emits as `recv.get(key)`.
	MapGet {
		recv: Box<HirExpr>,
		key: Box<HirExpr>,
	},
	Binary {
		op: BinOp,
		lhs: Box<HirExpr>,
		rhs: Box<HirExpr>,
	},
	Unary {
		op: UnOp,
		operand: Box<HirExpr>,
	},
	/// An assignment `target = value`. Compound assignments (`+=`, …) are desugared
	/// by lowering into `target = target <op> value`, so this is always a plain `=`.
	Assign {
		target: Box<HirExpr>,
		value: Box<HirExpr>,
	},
	/// A block: statements then an optional trailing expression (the block's value).
	Block {
		stmts: Vec<HirStmt>,
		tail: Option<Box<HirExpr>>,
	},
	If {
		cond: Box<HirExpr>,
		then: Box<HirExpr>,
		otherwise: Option<Box<HirExpr>>,
	},
	While {
		cond: Box<HirExpr>,
		body: Box<HirExpr>,
	},
}

/// Binary operators that map directly to a JS operator (primitive fast-path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
	Add,
	Sub,
	Mul,
	Div,
	Rem,
	Pow,
	Eq,
	Ne,
	Lt,
	Le,
	Gt,
	Ge,
	And,
	Or,
	BitAnd,
	BitOr,
	BitXor,
	Shl,
	Shr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
	Neg,
	Not,
}
