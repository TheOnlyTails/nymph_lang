//! The mid-level typed IR consumed by code generation. Most nodes are deliberately
//! type-free because JavaScript has a single numeric representation; nodes retain
//! type information only where runtime representation or dispatch requires it.

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

/// A top-level `let`/`let mut` binding → a module-scope `const`/`let` declaration.
/// Kept in source order relative to other top-level lets; emitted
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
	/// `namespace func` static functions → JS `static` class methods.
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
	/// `namespace func` static functions. Unlike a struct's
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
	/// `return <value>;` (`None` for a bare `return`). Source returns remain
	/// statement-flavored in HIR: expression-position returns lower to a
	/// one-statement `HirExpr::Block`. Codegen carries them across synthetic
	/// expression IIFEs to the nearest real callable boundary.
	Return {
		value: Option<HirExpr>,
		target: HirReturnTarget,
	},
}

pub type BlockTarget = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HirReturnTarget {
	Callable,
	Block(BlockTarget),
}

pub type LoopTarget = u32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HirOptionAbi {
	pub enum_name: EcoString,
	pub some: EcoString,
	pub some_value: EcoString,
	pub none: EcoString,
}

/// The runtime numeric type a boxed numeric value carries — the one piece of
/// type information codegen needs to pick the right box wrapper class (`NInt` /
/// `NUint` / `NFloat`) for an otherwise type-free numeric HIR node. HIR is
/// deliberately type-free (JS has one `number`),
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
	/// desugarings, so they stay raw.
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
	/// `==`/`!=` on non-primitives compares object identity directly, then boxes
	/// the raw comparison result as `NBool`.
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
	/// Compiler-only placeholder for an erased hidden ABI slot.
	Undefined,
	/// An identifier or parameter reference.
	Local(EcoString),
	/// Compiler-only canonical runtime type object. This is never a Nymph value:
	/// it is calling-convention data used by receiverless generic dispatch.
	RuntimeTypeObject {
		binding: EcoString,
		box_runtime: bool,
		is_enum: bool,
		arguments: Vec<HirExpr>,
	},
	/// Compiler-only projection from a receiver's canonical runtime type object.
	RuntimeTypeProjection {
		receiver: Box<HirExpr>,
		path: Vec<usize>,
	},
	WithPrototype {
		value: Box<HirExpr>,
		prototype: Box<HirExpr>,
	},
	RuntimeTypeAttachment {
		object: Box<HirExpr>,
		method: Box<HirMethod>,
	},
	/// The method receiver — emits as the JS `this` keyword.
	This,
	Call {
		callee: Box<HirExpr>,
		args: Vec<HirExpr>,
	},
	/// A call to a linked external — a method call that
	/// resolved through a prelude `external(name)` marker present in
	/// [`nymph_hir::linkage::REGISTRY`], instead of the loud "prelude-only
	/// impl" defer every other `external`/transitively-external body still
	/// gets. `module`/`symbol` are the ALREADY-RESOLVED [`crate::linkage::Linked`]
	/// fields — not the bare `external(name)` marker — because `get` is
	/// an AMBIGUOUS marker shared by `List` and `Map` with DIFFERENT JS
	/// implementations: the only place that knows which receiver's `impl`
	/// block resolved this call (and can therefore compute the receiver tag
	/// [`crate::linkage::lookup`] needs to disambiguate) is lowering itself,
	/// at the point it decides to build this variant — re-deriving that tag
	/// from a bare marker at emit time, with only `args[0]`'s already-erased
	/// HIR to go on, isn't possible. Baking the resolved pair into HIR (rather
	/// than re-`lookup`-ing by marker in codegen) keeps codegen a
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
		hidden_arguments: Vec<HirExpr>,
		cases: Vec<HirBoundDispatchCase>,
	},
	/// A zero-argument method selected through a still-generic interface bound.
	/// Like `BoundDispatch`, but dispatch depends only on the receiver's boxed
	/// runtime tag.
	UnaryBoundDispatch {
		interface: EcoString,
		method: EcoString,
		receiver: Box<HirExpr>,
		hidden_arguments: Vec<HirExpr>,
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
	/// A map literal containing at least one spread entry
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
		prototype: Option<Box<HirExpr>>,
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
		prototype: Option<Box<HirExpr>>,
	},
	/// Nullary variant reference — emits as `<enum>.<variant>` (frozen singleton).
	VariantRef {
		enum_name: EcoString,
		variant: EcoString,
		prototype: Option<Box<HirExpr>>,
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
	LabeledBlock {
		target: BlockTarget,
		body: Box<HirExpr>,
	},
	If {
		cond: Box<HirExpr>,
		then: Box<HirExpr>,
		otherwise: Option<Box<HirExpr>>,
	},
	While {
		target: LoopTarget,
		cond: Box<HirExpr>,
		body: Box<HirExpr>,
		/// Compiler-generated work that must run before a `continue` resumes this
		/// loop (for example advancing a lowered bounded-range counter).
		continue_epilogue: Option<Box<HirExpr>>,
		option: Option<HirOptionAbi>,
	},
	Break {
		target: LoopTarget,
		value: Box<HirExpr>,
	},
	Continue {
		target: LoopTarget,
	},
	/// `match <scrutinee> { <arms> }` — compiled to an if/else-if chain.
	Match {
		scrutinee: Box<HirExpr>,
		arms: Vec<HirArm>,
	},
	/// A built-in `as` scalar conversion that needs an actual JS runtime operation,
	/// not just a value
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
	/// scope by reference, which matches the checker's capture semantics, so no
	/// explicit capture list is carried here.
	/// This is a real callable boundary: a `return` in `body` exits this closure,
	/// including when synthetic expression IIFEs occur inside it.
	Closure {
		params: Vec<EcoString>,
		body: Box<HirExpr>,
	},
}

impl HirExpr {
	fn collect_runtime_type_references(&self, references: &mut FxHashSet<EcoString>) {
		match self {
			Self::Num(..)
			| Self::Str(_)
			| Self::Bool(_)
			| Self::Char(_)
			| Self::Undefined
			| Self::This => {}
			Self::RuntimeTypeObject {
				binding, arguments, ..
			} => {
				references.insert(binding.clone());
				collect_exprs(arguments, references);
			}
			Self::RuntimeTypeProjection { receiver, .. } => {
				receiver.collect_runtime_type_references(references);
			}
			Self::WithPrototype { value, prototype } => {
				value.collect_runtime_type_references(references);
				prototype.collect_runtime_type_references(references);
			}
			Self::RuntimeTypeAttachment { object, method } => {
				object.collect_runtime_type_references(references);
				method.body.collect_runtime_type_references(references);
			}
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
				receiver,
				argument,
				hidden_arguments,
				..
			} => {
				receiver.collect_runtime_type_references(references);
				argument.collect_runtime_type_references(references);
				collect_exprs(hidden_arguments, references);
			}
			Self::UnaryBoundDispatch {
				receiver,
				hidden_arguments,
				..
			} => {
				receiver.collect_runtime_type_references(references);
				collect_exprs(hidden_arguments, references);
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
			Self::New {
				class,
				fields,
				prototype,
			} => {
				references.insert(class.clone());
				collect_named(fields, references);
				if let Some(prototype) = prototype {
					prototype.collect_runtime_type_references(references);
				}
			}
			Self::Field { recv, .. } => recv.collect_runtime_type_references(references),
			Self::VariantNew {
				enum_name,
				fields,
				prototype,
				..
			} => {
				references.insert(enum_name.clone());
				collect_named(fields, references);
				if let Some(prototype) = prototype {
					prototype.collect_runtime_type_references(references);
				}
			}
			Self::VariantRef {
				enum_name,
				prototype,
				..
			} => {
				references.insert(enum_name.clone());
				if let Some(prototype) = prototype {
					prototype.collect_runtime_type_references(references);
				}
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
						HirStmt::Return { value, .. } => {
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
			Self::LabeledBlock { body, .. } => body.collect_runtime_type_references(references),
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
			Self::While {
				cond,
				body,
				continue_epilogue,
				option,
				..
			} => {
				cond.collect_runtime_type_references(references);
				body.collect_runtime_type_references(references);
				if let Some(epilogue) = continue_epilogue {
					epilogue.collect_runtime_type_references(references);
				}
				if let Some(option) = option {
					references.insert(option.enum_name.clone());
				}
			}
			Self::Break { value, .. } => value.collect_runtime_type_references(references),
			Self::Continue { .. } => {}
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
	/// onto the same `Infinity → i64::MAX` branch as `+Infinity`.
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
