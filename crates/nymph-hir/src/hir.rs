//! The mid-level typed IR consumed by code generation. Slice 1 covers the
//! scalar/control-flow core and is deliberately *type-free*: JS has a single
//! `number` type and primitive operators map 1:1 to JS operators, so no type
//! information is needed to emit correct JS. Type-carrying fields arrive in later
//! slices, where value-copy and operator-overload dispatch first need them.

use ecow::EcoString;

#[derive(Clone, Debug, PartialEq)]
pub struct HirModule {
	pub funcs: Vec<HirFunc>,
	pub classes: Vec<HirClass>,
	pub enums: Vec<HirEnum>,
}

/// A `struct` declaration → a JS class. Fields are stored in declaration order;
/// the emitted constructor takes one object argument and assigns each field.
/// Inherent instance methods are emitted into the class body.
#[derive(Clone, Debug, PartialEq)]
pub struct HirClass {
	pub name: EcoString,
	pub fields: Vec<EcoString>,
	pub methods: Vec<HirMethod>,
}

/// An inherent instance method → a JS class method. `this` in the body refers to
/// the receiver instance.
#[derive(Clone, Debug, PartialEq)]
pub struct HirMethod {
	pub name: EcoString,
	pub params: Vec<EcoString>,
	pub body: HirExpr,
}

/// An `enum` declaration → the Symbol-tag ABI object. Each variant becomes a
/// factory (fields) or a frozen singleton (nullary).
#[derive(Clone, Debug, PartialEq)]
pub struct HirEnum {
	pub name: EcoString,
	pub variants: Vec<HirVariant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirVariant {
	pub name: EcoString,
	/// Field names in declaration order; empty ⇒ nullary singleton variant.
	pub fields: Vec<EcoString>,
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
	/// The method receiver — emits as the JS `this` keyword.
	This,
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
	/// Struct construction — emits as `new <class>({ field: value, … })`.
	New {
		class: EcoString,
		fields: Vec<(EcoString, HirExpr)>,
	},
	/// Field access — emits as `recv.name`.
	Field {
		recv: Box<HirExpr>,
		name: EcoString,
	},
	/// Variant construction — emits as `<enum>.<variant>({ field: value, … })`.
	VariantNew {
		enum_name: EcoString,
		variant: EcoString,
		fields: Vec<(EcoString, HirExpr)>,
	},
	/// Nullary variant reference — emits as `<enum>.<variant>` (frozen singleton).
	VariantRef {
		enum_name: EcoString,
		variant: EcoString,
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
	/// `match <scrutinee> { <arms> }` — compiled to an if/else-if chain.
	Match {
		scrutinee: Box<HirExpr>,
		arms: Vec<HirArm>,
	},
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirArm {
	pub pat: HirPat,
	/// A `pattern if <cond>` guard — the arm matches only when this is truthy. A
	/// matched-but-guard-failed arm falls through to the next arm.
	pub guard: Option<HirExpr>,
	pub body: HirExpr,
}

/// A compiled pattern. Codegen turns each into a test expression plus a binding
/// sequence against a subject expression.
#[derive(Clone, Debug, PartialEq)]
pub enum HirPat {
	/// `_` — always matches, binds nothing.
	Wildcard,
	/// Bind the subject to `name`, then match `sub` against it (if present).
	Binding {
		name: EcoString,
		sub: Option<Box<HirPat>>,
	},
	/// A scalar literal — matches by `===`.
	Lit(HirLit),
	/// A variant — matches by tag identity, then matches each field sub-pattern
	/// against the corresponding field of the subject.
	Variant {
		enum_name: EcoString,
		variant: EcoString,
		fields: Vec<(EcoString, HirPat)>,
	},
	/// A struct pattern — irrefutable (the nominal type guarantees the shape); binds
	/// each named field (a field sub-pattern may still be refutable).
	Struct { fields: Vec<(EcoString, HirPat)> },
	/// A tuple pattern — irrefutable, binds each element by index.
	Tuple(Vec<HirPat>),
	/// A list pattern `#[<prefix>, ...rest, <suffix>]`. `rest` present ⇒ a spread
	/// (with an optional binding) and a `length >=` test; absent ⇒ an exact-length test.
	List {
		prefix: Vec<HirPat>,
		rest: Option<Option<EcoString>>,
		suffix: Vec<HirPat>,
	},
	/// A map pattern — tests `.has(key)` and matches the value pattern against `.get(key)`.
	Map(Vec<(HirLit, HirPat)>),
	/// A range pattern over scalar bounds.
	Range(HirRange),
	/// `A | B` — matches if either side matches (3B: neither side binds).
	Or(Box<HirPat>, Box<HirPat>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirLit {
	Num(f64),
	Bool(bool),
	Char(char),
	Str(EcoString),
}

/// A range pattern's bounds (scalar literals).
#[derive(Clone, Debug, PartialEq)]
pub enum HirRange {
	/// `min..`
	From(HirLit),
	/// `..max`
	To(HirLit),
	/// `..=max`
	ToInclusive(HirLit),
	/// `min..max`
	Exclusive { min: HirLit, max: HirLit },
	/// `min..=max`
	Inclusive { min: HirLit, max: HirLit },
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
	BitNot,
}
