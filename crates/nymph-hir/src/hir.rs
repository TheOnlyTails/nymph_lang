//! The mid-level IR consumed by code generation. It carries the representation
//! choices emission cannot recover, including exact fixed-width integers,
//! built-in operator results, and external marshalling plans.

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

/// A top-level `let` binding → a module-scope `const` declaration. Kept in source
/// order relative to other top-level lets; emitted
/// after classes/enums (so a let constructing/referencing one is safe) and before
/// functions (whose JS `function` declarations hoist regardless of placement).
#[derive(Clone, Debug, PartialEq)]
pub struct HirLet {
	pub name: EcoString,
	pub value: HirExpr,
}

/// A `struct` declaration → a JS class. Fields are stored in declaration order;
/// the emitted constructor takes one object argument and assigns each field.
/// Inherent instance methods are emitted into the class body.
#[derive(Clone, Debug, PartialEq)]
pub struct HirClass {
	pub name: EcoString,
	pub fields: Vec<EcoString>,
	/// Owner-defined field defaults, in declaration order. Constructors apply
	/// these only when the incoming object does not own the field.
	pub defaults: Vec<(EcoString, HirExpr)>,
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
		call_mode: ExternalCallMode,
	},
}

#[derive(Clone, Debug, PartialEq)]
// Statements keep expressions inline because they are traversed pervasively
// throughout lowering and codegen; boxing one variant would add broad churn.
#[allow(clippy::large_enum_variant)]
pub enum HirStmt {
	/// An immutable `let` binding.
	Let {
		name: EcoString,
		value: HirExpr,
		/// Exact checker-selected `Close.close` call for `let use`.
		cleanup: Option<HirExpr>,
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

/// How a generated Nymph call advances the current activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HirCallMode {
	/// Push a frame and resume the caller with the result.
	Push,
	/// Unwind the current frame and replace it without growing the logical stack.
	Tail,
}

/// Whether a cold task recipe inherits its driving context or establishes a
/// nested structured join scope. This is execution policy, not a host-backend
/// representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HirTaskContext {
	Inherited,
	Nested,
}

/// Backend-neutral operations on cold task recipes and execution handles.
/// Promise scheduling, cancellation controllers, and host adapter arguments
/// remain private to the selected runtime backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HirTaskOperation {
	Drive,
	Spawn,
	Observe,
	Cancel,
	Checkpoint,
	Select,
	Race,
}

impl HirTaskOperation {
	pub const fn suspends(self) -> bool {
		matches!(self, Self::Drive | Self::Observe | Self::Checkpoint)
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HirOptionAbi {
	pub enum_name: EcoString,
	pub some: EcoString,
	pub some_value: EcoString,
	pub none: EcoString,
}

/// Canonical nominal ABI used by persistent iterator steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HirIterationAbi {
	pub enum_name: EcoString,
	pub done: EcoString,
	pub yield_: EcoString,
	pub item: EcoString,
	pub next: EcoString,
}

/// Whether an `f64` HIR value is a boxed Nymph `float` or a compiler-internal
/// raw JavaScript number. Integers have separate exact [`HirExpr::Int`] and
/// [`HirExpr::UInt`] representations and never use this enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumKind {
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
	/// Compiler-generated arithmetic and predicates used by desugarings.
	Raw,
}

/// Whether codegen must retain a runtime guard or may emit the direct operation
/// selected by sema's body-local proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OperationMode {
	Checked,
	Direct,
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
	/// A live host reference boxed with its compiler-owned nominal identity.
	/// The identity is part of the stable ABI plan; backends must reject a box
	/// minted for any other external type instead of inspecting or repairing it.
	Opaque(u64),
}

/// Backend-neutral external invocation mode. A backend may translate the
/// execution signal into its native cancellation primitive, but only for the
/// explicitly cancellable ABI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExternalCallMode {
	#[default]
	Ordinary,
	Cancellable,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirExpr {
	/// An exact fixed-width integer literal. Keeping these separate from
	/// [`Self::Num`] prevents an integer from travelling through `f64` on its way
	/// from stable facts to the JavaScript BigInt runtime.
	Int(i64),
	UInt(u64),
	/// A floating-point literal or compiler-internal raw JS number.
	Num(f64, NumKind),
	Str(EcoString),
	/// Cooked string segments and Display-rendered interpolands, concatenated as
	/// raw JS strings and boxed once by codegen.
	InterpolatedString(Vec<HirExpr>),
	/// Render one value through its explicit Display implementation or the
	/// backend's structural fallback.
	ProtocolDisplay(Box<HirExpr>),
	Bool(bool),
	Char(char),
	/// Compiler-only placeholder for an erased hidden ABI slot.
	Undefined,
	/// An identifier or parameter reference.
	Local(EcoString),
	/// A privileged development observation. Release emission replaces this
	/// node with its operand and retains none of the site data.
	Echo {
		operand: Box<HirExpr>,
		site: EchoSite,
	},
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
	/// A call whose target uses the generated-Nymph activation ABI. External
	/// adapters deliberately remain [`Self::ExternCall`] and never enter this
	/// machine. `source` is the stable body-node anchor of the source call.
	ActivationCall {
		callee: Box<HirExpr>,
		args: Vec<HirExpr>,
		mode: HirCallMode,
		source: u32,
	},
	/// Construct a reusable cold recipe around one generated activation body.
	/// The runtime supplies the hidden execution frame when an execution starts.
	TaskRecipe {
		body: Box<HirExpr>,
		context: HirTaskContext,
	},
	/// Operate on recipes or handles without exposing backend scheduling values
	/// in HIR. Driving, observing, and checkpoints are explicit suspension
	/// points; the remaining operations complete synchronously.
	TaskOperation {
		operation: HirTaskOperation,
		operands: Vec<HirExpr>,
	},
	/// Invoke an enum method through the receiver's compile-time enum view.
	/// The value remains the source variant object; only method selection uses
	/// the view's canonical prototype.
	StaticEnumDispatch {
		owner: EcoString,
		method: EcoString,
		receiver: Box<HirExpr>,
		args: Vec<HirExpr>,
		mode: HirCallMode,
		source: u32,
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
		/// Ordinary adapters receive exactly `args`. Cancellable adapters receive
		/// one backend-provided execution signal after those arguments.
		call_mode: ExternalCallMode,
		/// Signature-directed unboxing for each argument. Integer entries cross
		/// the trusted JavaScript ABI as raw BigInt values; `None` preserves the
		/// existing boxed Nymph ABI for every other type.
		argument_marshals: Vec<Option<MarshalKind>>,
		/// Signature-directed reboxing for a direct integer result.
		return_marshal: Option<MarshalKind>,
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
		mode: HirCallMode,
		source: u32,
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
		mode: HirCallMode,
		source: u32,
	},
	/// Persistent list construction. Spread elements retain source evaluation
	/// order, while the runtime may use one private transient before freezing.
	ListConstruct(Vec<HirArrayElem>),
	/// Semantic persistent-list read; trie nodes remain runtime-private.
	ListRead {
		recv: Box<HirExpr>,
		index: Box<HirExpr>,
		mode: OperationMode,
	},
	/// Return a list with one item appended, preserving the receiver.
	ListAppend {
		recv: Box<HirExpr>,
		value: Box<HirExpr>,
	},
	/// Return a list with one item replaced, preserving the receiver.
	ListReplace {
		recv: Box<HirExpr>,
		index: Box<HirExpr>,
		value: Box<HirExpr>,
	},
	/// Return a rebased, trimmed structural-sharing slice.
	ListSlice {
		recv: Box<HirExpr>,
		start: Box<HirExpr>,
		end: Box<HirExpr>,
	},
	/// A tuple or compiler-internal raw array.
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
	/// A subscript into a list, tuple, or Unicode string — dispatches through its boxed wrapper.
	Index {
		recv: Box<HirExpr>,
		index: Box<HirExpr>,
		mode: OperationMode,
	},
	/// A homogeneous list or Unicode string range index.
	Slice {
		recv: Box<HirExpr>,
		start: Option<Box<HirExpr>>,
		end: Option<Box<HirExpr>>,
		inclusive: bool,
		string: bool,
		mode: OperationMode,
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
	StructFresh {
		class: EcoString,
		fields: Vec<(EcoString, HirExpr)>,
		prototype: Option<Box<HirExpr>>,
	},
	StructCloneUpdate {
		class: EcoString,
		source: Box<HirExpr>,
		replacements: Vec<(EcoString, HirExpr)>,
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
		mode: OperationMode,
		lhs: Box<HirExpr>,
		rhs: Box<HirExpr>,
	},
	Unary {
		op: UnOp,
		result: BuiltinResult,
		operand: Box<HirExpr>,
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
	StateLoop {
		target: LoopTarget,
		bindings: Vec<HirStateBinding>,
		body: Box<HirExpr>,
	},
	/// Persistent iteration remains explicit until the backend selects its loop
	/// representation. `next` references `iterator_name`; a yielded successor is
	/// stored into that binding before `body` starts.
	For {
		target: LoopTarget,
		source: u32,
		iterator_name: EcoString,
		successor_name: EcoString,
		iterator: Box<HirExpr>,
		next: Box<HirExpr>,
		pat: HirPat,
		body: Box<HirExpr>,
		iteration: HirIterationAbi,
		option: Option<HirOptionAbi>,
	},
	Break {
		target: LoopTarget,
		value: Box<HirExpr>,
	},
	Continue {
		target: LoopTarget,
	},
	ContinueTransition {
		target: LoopTarget,
		replacements: Vec<(EcoString, HirExpr)>,
	},
	/// `match <scrutinee> { <arms> }` — compiled to an if/else-if chain.
	Match {
		scrutinee: Box<HirExpr>,
		arms: Vec<HirArm>,
	},
	/// A built-in `as` scalar conversion that needs a runtime operation. Keeping
	/// it as a dedicated node prevents user bindings named `Math`, `String`, or
	/// `Number` from shadowing conversion helpers emitted by codegen.
	ScalarCast {
		kind: ScalarCastKind,
		operand: Box<HirExpr>,
		mode: OperationMode,
	},
	/// A closure expression (`(x, y) -> x + y`, `x -> x * 2`) — emits as a JS
	/// arrow function. Captures are free: JS arrows close over their enclosing
	/// scope by reference, which already matches the checker's own capture
	/// semantics (Slice 4L), so no explicit capture list is carried here.
	/// This is a real callable boundary: a `return` in `body` exits this closure,
	/// including when synthetic expression IIFEs occur inside it.
	Closure {
		params: Vec<EcoString>,
		body: Box<HirExpr>,
	},
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirStateBinding {
	pub name: EcoString,
	pub value: HirExpr,
	pub cleanup: Option<HirExpr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EchoSite {
	pub module: EcoString,
	pub start: u32,
	pub end: u32,
}

impl HirExpr {
	fn collect_runtime_type_references(&self, references: &mut FxHashSet<EcoString>) {
		match self {
			Self::Int(_)
			| Self::UInt(_)
			| Self::Num(..)
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
			Self::Echo { operand, .. } => operand.collect_runtime_type_references(references),
			Self::TaskRecipe { body, .. } => body.collect_runtime_type_references(references),
			Self::TaskOperation { operands, .. } => {
				for operand in operands {
					operand.collect_runtime_type_references(references);
				}
			}
			Self::InterpolatedString(items) => collect_exprs(items, references),
			Self::ProtocolDisplay(value) => value.collect_runtime_type_references(references),
			Self::Call { callee, args } | Self::ActivationCall { callee, args, .. } => {
				callee.collect_runtime_type_references(references);
				collect_exprs(args, references);
			}
			Self::StaticEnumDispatch {
				owner,
				receiver,
				args,
				..
			} => {
				references.insert(owner.clone());
				receiver.collect_runtime_type_references(references);
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
			Self::ListConstruct(elems) => {
				for elem in elems {
					match elem {
						HirArrayElem::Item(expr) | HirArrayElem::Spread(expr) => {
							expr.collect_runtime_type_references(references)
						}
					}
				}
			}
			Self::ListRead { recv, index, .. } => {
				recv.collect_runtime_type_references(references);
				index.collect_runtime_type_references(references);
			}
			Self::ListAppend { recv, value } => {
				recv.collect_runtime_type_references(references);
				value.collect_runtime_type_references(references);
			}
			Self::ListReplace { recv, index, value } => {
				recv.collect_runtime_type_references(references);
				index.collect_runtime_type_references(references);
				value.collect_runtime_type_references(references);
			}
			Self::ListSlice { recv, start, end } => {
				recv.collect_runtime_type_references(references);
				start.collect_runtime_type_references(references);
				end.collect_runtime_type_references(references);
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
			Self::Index { recv, index, .. } => {
				recv.collect_runtime_type_references(references);
				index.collect_runtime_type_references(references);
			}
			Self::Slice {
				recv, start, end, ..
			} => {
				recv.collect_runtime_type_references(references);
				if let Some(start) = start {
					start.collect_runtime_type_references(references);
				}
				if let Some(end) = end {
					end.collect_runtime_type_references(references);
				}
			}
			Self::MapGet { recv, key } => {
				recv.collect_runtime_type_references(references);
				key.collect_runtime_type_references(references);
			}
			Self::New {
				class,
				fields,
				prototype,
			}
			| Self::StructFresh {
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
			Self::StructCloneUpdate {
				class,
				source,
				replacements,
				prototype,
			} => {
				references.insert(class.clone());
				source.collect_runtime_type_references(references);
				collect_named(replacements, references);
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
			Self::StateLoop { bindings, body, .. } => {
				for binding in bindings {
					binding.value.collect_runtime_type_references(references);
					if let Some(cleanup) = &binding.cleanup {
						cleanup.collect_runtime_type_references(references);
					}
				}
				body.collect_runtime_type_references(references);
			}
			Self::For {
				iterator,
				next,
				pat,
				body,
				iteration,
				option,
				..
			} => {
				iterator.collect_runtime_type_references(references);
				next.collect_runtime_type_references(references);
				pat.collect_runtime_type_references(references);
				body.collect_runtime_type_references(references);
				references.insert(iteration.enum_name.clone());
				if let Some(option) = option {
					references.insert(option.enum_name.clone());
				}
			}
			Self::Break { value, .. } => value.collect_runtime_type_references(references),
			Self::Continue { .. } => {}
			Self::ContinueTransition { replacements, .. } => {
				for (_, value) in replacements {
					value.collect_runtime_type_references(references);
				}
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
// Map entries intentionally keep both expressions inline, matching ordinary
// map literals and avoiding an allocation for the common non-spread case.
#[allow(clippy::large_enum_variant)]
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
	IntToUInt,
	ToFloat,
	/// Checked `float as int`: truncate a finite value toward zero, then reject
	/// anything outside the signed 64-bit range.
	CheckedToInt,
	/// Checked `float as uint`: reject negative, non-finite, fractional, and
	/// out-of-range values rather than silently wrapping or saturating.
	CheckedToUInt,
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
	Int(i64),
	UInt(u64),
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
