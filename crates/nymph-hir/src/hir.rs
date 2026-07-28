//! The mid-level typed IR consumed by code generation. Slice 1 covers the
//! scalar/control-flow core and is deliberately *type-free*: JS has a single
//! `number` type and primitive operators map 1:1 to JS operators, so no type
//! information is needed to emit correct JS. Type-carrying fields arrive in later
//! slices, where value-copy and operator-overload dispatch first need them.

use ecow::EcoString;
use rustc_hash::FxHashSet;

#[derive(Clone, Debug, PartialEq)]
pub struct HirModule {
	pub lets: Vec<HirLet>,
	pub funcs: Vec<HirFunc>,
	pub classes: Vec<HirClass>,
	pub enums: Vec<HirEnum>,
}

impl HirModule {
	/// Return every nominal runtime type referenced by executable HIR.
	///
	/// This deliberately walks declaration bodies as well as top-level values:
	/// canonical runtime declarations can themselves demand another canonical
	/// enum or struct, and project linking uses that edge to synthesize imports.
	pub fn runtime_type_references(&self) -> FxHashSet<EcoString> {
		let mut references = FxHashSet::default();
		for let_ in &self.lets {
			let_.value.collect_runtime_type_references(&mut references);
		}
		for func in &self.funcs {
			func.body.collect_runtime_type_references(&mut references);
		}
		for class in &self.classes {
			for method in class.methods.iter().chain(&class.statics) {
				method.body.collect_runtime_type_references(&mut references);
			}
		}
		for enum_ in &self.enums {
			for method in enum_.methods.iter().chain(&enum_.statics) {
				method.body.collect_runtime_type_references(&mut references);
			}
		}
		references
	}
}

/// A top-level `let`/`let mut` binding → a module-scope `const`/`let` declaration
/// (Slice 4E, Y3). Kept in source order relative to other top-level lets; emitted
/// after classes/enums (so a let constructing/referencing one is safe) and before
/// functions (whose JS `function` declarations hoist regardless of placement).
#[derive(Clone, Debug, PartialEq)]
pub struct HirLet {
	pub name: EcoString,
	pub mutable: bool,
	pub value: HirExpr,
}

/// A `struct` declaration → a JS class. Fields are stored in declaration order;
/// the emitted constructor takes one object argument and assigns each field.
/// Inherent instance methods are emitted into the class body.
#[derive(Clone, Debug, PartialEq)]
pub struct HirClass {
	pub name: EcoString,
	pub fields: Vec<EcoString>,
	pub methods: Vec<HirMethod>,
	/// `namespace func` static functions (Slice 4J) → JS `static` class methods.
	/// A separate list, not a flag on `HirMethod`: JS legally allows a static and
	/// an instance method sharing one name (they live in different tables), so
	/// keeping them in separate lists keeps `assert_no_duplicate_methods`'
	/// per-list "one name, one method" invariant meaningful for each.
	pub statics: Vec<HirMethod>,
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
/// factory (fields) or a frozen singleton (nullary). Instance methods (from
/// top-level `impl`/`impl … for` blocks and enum-body inherent funcs/nested
/// impls) share a per-enum prototype object every variant is created against.
#[derive(Clone, Debug, PartialEq)]
pub struct HirEnum {
	pub name: EcoString,
	pub variants: Vec<HirVariant>,
	pub methods: Vec<HirMethod>,
	/// `namespace func` static functions (Slice 4J). Unlike a struct's
	/// `statics`, these become OBJECT-level method properties on the IIFE's
	/// returned object (not `proto`-level): call sites emit `E.func(..)` against
	/// the object `E` itself, and `proto` is only reachable through a
	/// constructed variant instance, never through the enum name — a
	/// proto-level property would be unreachable from what call sites emit to.
	pub statics: Vec<HirMethod>,
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
pub struct HirBoundDispatchCase {
	pub receiver_tag: EcoString,
	pub argument_tag: EcoString,
	pub target: HirBoundDispatchTarget,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirBoundDispatchTarget {
	TopLevel {
		module: EcoString,
		name: EcoString,
	},
	Extern {
		module: &'static str,
		symbol: &'static str,
	},
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
	/// `return <value>;` (`None` for a bare `return`). Statement-flavored: valid
	/// only directly inside a `HirExpr::Block`'s `stmts` — never reachable as a
	/// subexpression. Lowering panics loudly if `return` appears anywhere else
	/// (an expression position, or with a label); emit panics loudly if a
	/// `Return` is reached while emitting an expression-position (IIFE-wrapped)
	/// block/if/match, where a JS `return` would target the IIFE rather than the
	/// enclosing function (Slice 4E, Y1).
	Return(Option<HirExpr>),
}

/// The runtime numeric type a boxed numeric value carries — the one piece of
/// type information codegen needs to pick the right box wrapper class (`NInt` /
/// `NUint` / `NFloat`) for an otherwise type-free numeric HIR node (uniform
/// value boxing, slice #2). HIR is deliberately type-free (JS has one `number`),
/// so lowering threads this on from the checker's inferred type at the point it
/// builds a numeric node; without it emit could not tell `5`, `5u` and `5.0`
/// apart, all of which lower to the same `f64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumKind {
	/// `int` → boxed as `new NInt(v)`.
	Int,
	/// `uint` → boxed as `new NUint(v)`.
	UInt,
	/// `float` → boxed as `new NFloat(v)`.
	Float,
	/// A compiler-internal raw JS number — NEVER produced from a user literal.
	/// Emitted as a bare numeric literal (no box), because it is scaffolding the
	/// desugared control-flow machinery operates on with native JS arithmetic
	/// (loop counters `i + 1`, list indices `arr[i]`, `i < arr.length`), not a
	/// user-visible Nymph value. Boxing these would break the emitted loop
	/// desugarings; they stay raw until a later slice reworks that machinery.
	Raw,
}

/// The checker-resolved result representation of a built-in operator. User
/// operators lower to method calls instead; this marker exists so codegen can
/// re-box a native-JS fast-path result without re-deriving type information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinResult {
	Int,
	UInt,
	Float,
	Char,
	String,
	Boolean,
	/// Transitional `==`/`!=` on non-primitives: compare object identity directly,
	/// then box the raw comparison result as `NBool`. Value equality replaces this
	/// in the later equality slice.
	IdentityBoolean,
	/// Compiler-generated arithmetic and predicates used by desugarings.
	Raw,
}

/// Raw-host-to-boxed-Nymph marshalling performed once for an external let.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MarshalKind {
	Int,
	UInt,
	Float,
	Char,
	String,
	Boolean,
	List,
	Tuple,
	Map,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirExpr {
	/// Any numeric literal (int/uint/float), tagged with the [`NumKind`] codegen
	/// boxes it as — all three are the same JS `number` at the payload level.
	Num(f64, NumKind),
	Str(EcoString),
	/// Cooked string segments and Display-rendered interpolands, concatenated as
	/// raw JS strings and boxed once by codegen.
	InterpolatedString(Vec<HirExpr>),
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
	/// A call to a LINKED external (Gap 3, L0/L1) — a method call that
	/// resolved through a prelude `external(name)` marker present in
	/// [`nymph_hir::linkage::REGISTRY`], instead of the loud "prelude-only
	/// impl" defer every other `external`/transitively-external body still
	/// gets. `module`/`symbol` are the ALREADY-RESOLVED [`crate::linkage::Linked`]
	/// fields — not the bare `external(name)` marker — because L1's `get` is
	/// an AMBIGUOUS marker shared by `List` and `Map` with DIFFERENT JS
	/// implementations: the only place that knows which receiver's `impl`
	/// block resolved this call (and can therefore compute the receiver tag
	/// [`crate::linkage::lookup`] needs to disambiguate) is lowering itself,
	/// at the point it decides to build this variant — re-deriving that tag
	/// from a bare marker at emit time, with only `args[0]`'s already-erased
	/// HIR to go on, isn't possible. Baking the resolved pair into HIR (rather
	/// than re-`lookup`-ing by marker in codegen, as L0 did) keeps codegen a
	/// dumb consumer instead of a second place that has to re-derive
	/// receiver-tag disambiguation. `args` is already in `$_this`-FIRST
	/// order: the receiver lowered first, then the call's own arguments,
	/// exactly the shape every `Linked` JS function expects (e.g.
	/// `xs.length()` → `args = [xs]` → emits `length(xs)`).
	ExternCall {
		module: &'static str,
		symbol: &'static str,
		args: Vec<HirExpr>,
	},
	/// A registry-resolved immutable host value. This expression occurs only as
	/// a canonical module `HirLet` initializer, never at each reference site.
	ExternValue {
		module: &'static str,
		symbol: &'static str,
		marshal: MarshalKind,
	},
	/// A binary operator selected through a still-generic interface bound.
	/// Canonical boxed tags select concrete prelude implementations; user
	/// classes fall back to their materialized method.
	BoundDispatch {
		interface: EcoString,
		method: EcoString,
		receiver: Box<HirExpr>,
		argument: Box<HirExpr>,
		cases: Vec<HirBoundDispatchCase>,
	},
	/// A tuple, list, or compiler-internal raw array.
	Array {
		kind: HirArrayKind,
		items: Vec<HirExpr>,
	},
	/// A list or tuple literal containing at least one spread element — emits as
	/// the collection selected by `kind`, carrying an array with the spread
	/// elements' JS `...` syntax preserved in position. A spread-free list
	/// still lowers to the plain [`HirExpr::Array`] above (zero behavior
	/// change for the common case).
	ArraySpread {
		kind: HirArrayKind,
		elems: Vec<HirArrayElem>,
	},
	/// A map literal — emits as a boxed value-equality HAMT.
	MapLit(Vec<(HirExpr, HirExpr)>),
	/// A map literal (SS1) containing at least one spread entry
	/// (`#{...m, k: v}`) — emits as an `NMap` with the spread entries'
	/// JS `...` syntax preserved in position (a Map merge, later-key-wins,
	/// since the `Map` constructor processes its entries array in order). A
	/// spread-free map still lowers to the plain [`HirExpr::MapLit`] above.
	MapSpread(Vec<HirMapElem>),
	/// A subscript into a list/tuple — dispatches through its boxed wrapper.
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
		result: BuiltinResult,
		lhs: Box<HirExpr>,
		rhs: Box<HirExpr>,
	},
	Unary {
		op: UnOp,
		result: BuiltinResult,
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
	/// A built-in `as` scalar conversion (Slice 4K, extended by the saturating-cast
	/// change below) that needs an actual JS runtime operation, not just a value
	/// pass-through. Identity casts and the remaining same-"JS number" numeric
	/// casts (`int`/`uint` → `float`, `uint` → `int`, plus `Foo as Foo` for any
	/// `Foo`) need no node at all — lowering just returns the operand unchanged —
	/// so this variant only ever wraps the `kind`s below. Kept as a dedicated node
	/// (rather than composing `Call`/`Field` onto `Local("Math")`/`Local("String")`/
	/// `Local("Number")`) so a user local of one of those names can never shadow
	/// the conversion codegen emits (see `emit.rs`).
	ScalarCast {
		kind: ScalarCastKind,
		operand: Box<HirExpr>,
	},
	/// A closure expression (`(x, y) -> x + y`, `x -> x * 2`) — emits as a JS
	/// arrow function. Captures are free: JS arrows close over their enclosing
	/// scope by reference, which already matches the checker's own capture
	/// semantics (Slice 4L), so no explicit capture list is carried here.
	/// `return` lexically inside `body` is rejected by lowering (never reaches
	/// this node) — see `lower_hir`'s closure-depth guard for why.
	Closure {
		params: Vec<EcoString>,
		body: Box<HirExpr>,
	},
}

impl HirExpr {
	fn collect_runtime_type_references(&self, references: &mut FxHashSet<EcoString>) {
		match self {
			Self::Num(..) | Self::Str(_) | Self::Bool(_) | Self::Char(_) | Self::This => {}
			Self::Local(name) => {
				references.insert(name.clone());
			}
			Self::InterpolatedString(items) => collect_exprs(items, references),
			Self::Call { callee, args } => {
				callee.collect_runtime_type_references(references);
				collect_exprs(args, references);
			}
			Self::ExternCall { args, .. } => collect_exprs(args, references),
			Self::ExternValue { .. } => {}
			Self::BoundDispatch {
				receiver, argument, ..
			} => {
				receiver.collect_runtime_type_references(references);
				argument.collect_runtime_type_references(references);
			}
			Self::Array { items, .. } => collect_exprs(items, references),
			Self::ArraySpread { elems, .. } => {
				for item in elems {
					match item {
						HirArrayElem::Item(expr) | HirArrayElem::Spread(expr) => {
							expr.collect_runtime_type_references(references)
						}
					}
				}
			}
			Self::MapLit(entries) => collect_pairs(entries, references),
			Self::MapSpread(entries) => {
				for entry in entries {
					match entry {
						HirMapElem::Entry(key, value) => {
							key.collect_runtime_type_references(references);
							value.collect_runtime_type_references(references);
						}
						HirMapElem::Spread(expr) => expr.collect_runtime_type_references(references),
					}
				}
			}
			Self::Index { recv, index } => {
				recv.collect_runtime_type_references(references);
				index.collect_runtime_type_references(references);
			}
			Self::MapGet { recv, key } => {
				recv.collect_runtime_type_references(references);
				key.collect_runtime_type_references(references);
			}
			Self::New { class, fields } => {
				references.insert(class.clone());
				collect_named(fields, references);
			}
			Self::Field { recv, .. } => recv.collect_runtime_type_references(references),
			Self::VariantNew {
				enum_name, fields, ..
			} => {
				references.insert(enum_name.clone());
				collect_named(fields, references);
			}
			Self::VariantRef { enum_name, .. } => {
				references.insert(enum_name.clone());
			}
			Self::Binary { lhs, rhs, .. } => {
				lhs.collect_runtime_type_references(references);
				rhs.collect_runtime_type_references(references);
			}
			Self::Unary { operand, .. } | Self::ScalarCast { operand, .. } => {
				operand.collect_runtime_type_references(references)
			}
			Self::Assign { target, value } => {
				target.collect_runtime_type_references(references);
				value.collect_runtime_type_references(references);
			}
			Self::Block { stmts, tail } => {
				for stmt in stmts {
					match stmt {
						HirStmt::Let { value, .. } | HirStmt::Expr(value) => {
							value.collect_runtime_type_references(references)
						}
						HirStmt::Return(value) => {
							if let Some(value) = value {
								value.collect_runtime_type_references(references);
							}
						}
					}
				}
				if let Some(tail) = tail {
					tail.collect_runtime_type_references(references);
				}
			}
			Self::If {
				cond,
				then,
				otherwise,
			} => {
				cond.collect_runtime_type_references(references);
				then.collect_runtime_type_references(references);
				if let Some(otherwise) = otherwise {
					otherwise.collect_runtime_type_references(references);
				}
			}
			Self::While { cond, body } => {
				cond.collect_runtime_type_references(references);
				body.collect_runtime_type_references(references);
			}
			Self::Match { scrutinee, arms } => {
				scrutinee.collect_runtime_type_references(references);
				for arm in arms {
					arm.pat.collect_runtime_type_references(references);
					if let Some(guard) = &arm.guard {
						guard.collect_runtime_type_references(references);
					}
					arm.body.collect_runtime_type_references(references);
				}
			}
			Self::Closure { body, .. } => body.collect_runtime_type_references(references),
		}
	}
}

fn collect_exprs(exprs: &[HirExpr], references: &mut FxHashSet<EcoString>) {
	for expr in exprs {
		expr.collect_runtime_type_references(references);
	}
}
fn collect_pairs(exprs: &[(HirExpr, HirExpr)], references: &mut FxHashSet<EcoString>) {
	for (left, right) in exprs {
		left.collect_runtime_type_references(references);
		right.collect_runtime_type_references(references);
	}
}
fn collect_named(exprs: &[(EcoString, HirExpr)], references: &mut FxHashSet<EcoString>) {
	for (_, expr) in exprs {
		expr.collect_runtime_type_references(references);
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HirArrayKind {
	List,
	Tuple,
	Raw,
}

/// One element of a spread-bearing list literal (see [`HirExpr::ArraySpread`]).
#[derive(Clone, Debug, PartialEq)]
pub enum HirArrayElem {
	/// An ordinary, non-spread item.
	Item(HirExpr),
	/// `...e` — `e` is already a JS-array-valued expression (either the
	/// lowered spread source directly, when it's natively a JS array, or a
	/// drain IIFE that collects a non-array `Iterator`/`Iterable` source into
	/// one — see `Lowerer::lower_spread_source`), so codegen always emits it
	/// with JS spread syntax.
	Spread(HirExpr),
}

/// One element of a spread-bearing map literal (see [`HirExpr::MapSpread`]).
#[derive(Clone, Debug, PartialEq)]
pub enum HirMapElem {
	/// An ordinary `k: v` entry.
	Entry(HirExpr, HirExpr),
	/// `...e` — `e` is already an array of `[k, v]` pairs (a native JS `Map`,
	/// spliceable directly since a JS `Map` iterates as `[k, v]` pairs, or a
	/// drain IIFE collecting a non-map `Iterator`/`Iterable<#(K, V)>` source
	/// into one), so codegen always emits it with JS spread syntax inside the
	/// `NMap` entries array.
	Spread(HirExpr),
}

/// Which JS runtime conversion a [`HirExpr::ScalarCast`] compiles to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarCastKind {
	/// Rebox a scalar identity cast as the canonical destination representation.
	IdentityInt,
	IdentityUInt,
	IdentityFloat,
	IdentityChar,
	/// Numeric widening/reinterpretation conversions that only change the box.
	ToInt,
	ToFloat,
	/// `float as int` — Nymph defines its own float→int semantics rather than
	/// inheriting `Math.trunc`'s JS passthrough: `NaN` saturates to `0`,
	/// `Infinity`/`-Infinity` saturate to `i64::MAX`/`i64::MIN` (JS stores the
	/// former as `2^63`, the nearest `f64` to `2^63 - 1`), and any other (finite)
	/// value truncates toward zero as before.
	SaturatingToInt,
	/// `float as uint` / `int as uint` — like `SaturatingToInt`, but the operand
	/// is `Math.abs`-ed first: a negative finite value (or a negative `int` being
	/// cast to `uint`) saturates to its absolute value, and `-Infinity` collapses
	/// onto the same `Infinity → i64::MAX` branch as `+Infinity` (`int as uint`
	/// previously had no runtime effect at all — this makes it a real operation).
	SaturatingToUInt,
	/// `char as int`/`char as uint`/`char as float` — `operand.codePointAt(0)`.
	CharToInt,
	CharToUInt,
	CharToFloat,
	/// `int as char`/`uint as char` — `String.fromCodePoint(operand)`.
	NumToChar,
	/// `float as char` — `String.fromCodePoint(Math.trunc(operand))`.
	FloatToChar,
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
		kind: HirArrayKind,
		prefix: Vec<HirPat>,
		rest: Option<Option<EcoString>>,
		suffix: Vec<HirPat>,
	},
	/// A map pattern — tests `.has(key)` and matches the value pattern against
	/// `.get(key)`. `rest` present ⇒ an optional binding to the rest-of-map (a
	/// shallow copy of the scrutinee minus the named `entries` keys); absent ⇒ no
	/// rest clause.
	Map {
		entries: Vec<(HirLit, HirPat)>,
		rest: Option<Option<EcoString>>,
	},
	/// A range pattern over scalar bounds.
	Range(HirRange),
	/// `A | B` — matches if either side matches. Both sides bind the same names.
	Or(Box<HirPat>, Box<HirPat>),
}

impl HirPat {
	fn collect_runtime_type_references(&self, references: &mut FxHashSet<EcoString>) {
		match self {
			Self::Wildcard | Self::Lit(_) | Self::Range(_) => {}
			Self::Binding { sub, .. } => {
				if let Some(sub) = sub {
					sub.collect_runtime_type_references(references);
				}
			}
			Self::Variant {
				enum_name, fields, ..
			} => {
				references.insert(enum_name.clone());
				for (_, pat) in fields {
					pat.collect_runtime_type_references(references);
				}
			}
			Self::Struct { fields } => {
				for (_, pat) in fields {
					pat.collect_runtime_type_references(references);
				}
			}
			Self::Tuple(items) => {
				for pat in items {
					pat.collect_runtime_type_references(references);
				}
			}
			Self::List { prefix, suffix, .. } => {
				for pat in prefix.iter().chain(suffix) {
					pat.collect_runtime_type_references(references);
				}
			}
			Self::Map { entries, .. } => {
				for (_, pat) in entries {
					pat.collect_runtime_type_references(references);
				}
			}
			Self::Or(left, right) => {
				left.collect_runtime_type_references(references);
				right.collect_runtime_type_references(references);
			}
		}
	}
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirLit {
	Num(f64, NumKind),
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
	Neg,
	Not,
	BitNot,
}
