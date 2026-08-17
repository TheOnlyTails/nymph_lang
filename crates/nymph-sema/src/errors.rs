//! The type checker's diagnostics as a single typed catalog.
//!
//! Every error and warning the checker can emit is one variant of [`TypeError`],
//! carrying only its semantic data; the [`IntoDiagnostic`] impl is the one place
//! that turns a variant into a rendered message, severity, labels, and help. The
//! primary span is supplied at the emit site (`Checker::emit`). Error *codes* are
//! not assigned yet — `code` inherits the trait default (`None`) until the code
//! scheme lands.

use ecow::EcoString;
use nymph_ast::Span;
use nymph_diagnostics::{ErrorCode, IntoDiagnostic, Label, Severity};
use nymph_errorcode::ErrorCode;

/// A diagnostic produced by name resolution, type checking, or the interface solver.
#[derive(Clone, Debug, PartialEq, ErrorCode)]
#[error_code(2)]
pub enum TypeError {
	// ── Names & resolution ───────────────────────────────────────────────────
	/// A name was referenced that isn't bound in the current scope.
	CannotFind {
		name: EcoString,
	},
	/// A type name was referenced that isn't in scope.
	CannotFindType {
		name: EcoString,
	},
	/// A pattern referenced an enum that isn't in scope.
	CannotFindEnum {
		name: EcoString,
	},
	/// A pattern referenced a constructor that couldn't be resolved.
	CannotFindConstructor {
		name: EcoString,
	},
	/// A bare variant name matched more than one enum; it must be qualified.
	AmbiguousVariant {
		name: EcoString,
	},
	/// A name used in type position doesn't denote a type.
	NotAType {
		name: EcoString,
	},
	/// A name used as an interface doesn't denote one.
	NotAnInterface {
		name: EcoString,
	},
	/// A generic parameter was given type arguments (it takes none).
	GenericParamWithArgs {
		name: EcoString,
	},
	/// A top-level name was defined more than once.
	Redefinition {
		name: EcoString,
		redefined_span: Span,
		prev: Span,
	},
	/// A type alias expands into itself without termination.
	RecursiveTypeAlias,
	/// Inference produced a type that contains itself.
	InfiniteType {
		ty: String,
	},

	// ── Values & access ──────────────────────────────────────────────────────
	/// `this` was used outside any method body.
	ThisOutsideMethod,
	/// A struct type name was used where a value is expected.
	StructTypeAsValue,
	/// A type was used where a value is expected.
	TypeAsValue,
	/// A non-callable expression was called.
	NotCallable,
	/// A field was named that the type does not have.
	UnknownField {
		field: EcoString,
	},
	/// A struct literal supplied more fields than the type declares.
	TooManyFields,
	/// A field access named a field the receiver type lacks.
	NoField {
		field: EcoString,
		ty: String,
	},
	/// An enum was accessed with a variant name it doesn't declare.
	EnumHasNoVariant {
		enum_name: EcoString,
		variant: EcoString,
	},
	/// A namespaced access found neither a variant nor a namespaced function.
	NoVariantOrNamespacedFn {
		ty: EcoString,
		name: EcoString,
	},
	/// A namespaced access found no namespaced function of that name.
	NoNamespacedFn {
		ty: EcoString,
		name: EcoString,
	},
	/// No method of that name resolves for the receiver type.
	NoMethod {
		method: EcoString,
		ty: String,
	},
	/// A member access could not be resolved on the receiver type.
	CannotAccess {
		member: EcoString,
		ty: String,
	},
	/// Optional chaining was used on a value other than canonical `Option`/`Result`.
	OptionalChainReceiver {
		ty: String,
	},
	/// A namespaced function was called through a type parameter that lacks it.
	NoNamespacedFnOnParam {
		name: EcoString,
	},

	// ── Operators, casts, impls ──────────────────────────────────────────────
	/// A required interface method isn't implemented for the operand type.
	NotImplemented {
		method: EcoString,
		ty: String,
	},
	/// An `as` cast has no corresponding `Into` implementation.
	CannotCast {
		from: String,
		to: String,
	},
	/// Two types that were required to match did not.
	MismatchedTypes {
		expected: String,
		found: String,
	},
	/// Two impls of one interface overlap for the same receiver.
	ConflictingImpls {
		iface: EcoString,
	},
	/// No overload of a function matches the given arguments.
	NoMatchingOverload {
		name: EcoString,
	},
	/// Multiple impls apply to a call, and none is more specific.
	AmbiguousCall {
		name: EcoString,
	},

	// ── Arguments ────────────────────────────────────────────────────────────
	/// A call supplied the wrong number of arguments.
	WrongArgCount {
		expected: usize,
		found: usize,
	},
	/// A named function/method was called with the wrong number of arguments.
	NamedWrongArgCount {
		name: EcoString,
		expected: usize,
		found: usize,
	},

	// ── Assignment ───────────────────────────────────────────────────────────
	/// Assignment to an immutable binding.
	AssignToImmutable {
		name: EcoString,
	},
	/// Assignment to something that isn't an assignable place.
	CannotAssign {
		name: EcoString,
	},

	// ── Patterns ─────────────────────────────────────────────────────────────
	/// A constructor pattern used an unsupported path form.
	UnsupportedConstructorPath,

	// ── Not yet supported ────────────────────────────────────────────────────
	/// An anonymous closure parameter (`$`/`$0`/`$1`, …) that no candidate
	/// boundary expression could resolve into a well-typed closure — either
	/// every candidate boundary up to the enclosing slot still mismatched, or
	/// (rarer) `$` was used with no enclosing slot at all. See
	/// `crate::anon_closure` for the type-directed boundary search this
	/// diagnostic is the loud fallback for.
	AnonymousParamUnsupported,
	/// Method-call syntax is unsupported.
	MethodCallsUnsupported,

	// ── Exhaustiveness (warnings + errors) ───────────────────────────────────
	/// A `match` does not cover a constructible value (witness rendered).
	NonExhaustiveMatch {
		witness: String,
	},
	/// A `match` over `int` leaves some values uncovered.
	NonExhaustiveInt,
	/// A `match` needs a `_` arm to cover its remaining cases.
	NonExhaustiveNeedsWildcard,
	/// A `match` arm can never be reached. **Warning.**
	UnreachableArm,

	// ── Codegen-ABI limitations ──────────────────────────────────────────────
	/// A field-carrying variant was used as a first-class value (e.g. `let g = Some`).
	/// Its constructor is not yet expressible in the value ABI (the emitted factory
	/// takes an object, not positional args), so this is rejected rather than
	/// silently miscompiled. Call it to construct instead.
	FieldVariantAsValue {
		variant: EcoString,
	},

	// ── Operators (late finalization) ────────────────────────────────────────
	/// A binary operator's operand type was still an unresolved inference variable
	/// after the whole module was checked — genuinely under-determined, so an
	/// explicit type annotation is needed rather than lowering guessing at it.
	CannotInferOperandType,

	// ── Generics (call-site bound enforcement) ──────────────────────────────
	/// A call site instantiated a generic parameter (declared `T: Interface` or
	/// the `impl Interface` param sugar) with a type that does not implement the
	/// required interface — e.g. `measure(3)` against `measure<T: Area>(shape: T)`.
	/// Without this check the call type-checks and then crashes at JS runtime.
	BoundNotSatisfied {
		ty: String,
		interface: EcoString,
	},

	// ── Inner members ────────────────────────────────────────────────────────
	/// A struct, enum, or namespace member was declared more than once under the
	/// same name on the same owner. The member collectors keep a single signature
	/// per name, so silently accepting a later declaration would leave an earlier
	/// body unchecked even though runtime extraction still walks the raw AST.
	/// This diagnostic keeps that collision from reaching lowering. `ty` names
	/// the owning type or namespace.
	DuplicateMember {
		name: EcoString,
		ty: String,
		redefined_span: Span,
		prev: Span,
	},

	// ── Entry point (`main`) validation ─────────────────────────────────────
	// `ErrorCode` derives codes as `2{variant_index:03}` from declaration order,
	// so variants must remain in place and additions must go at the end.
	/// Entry mode (`check_module_entry`) found no top-level `func main` in the
	/// module. Library mode (`check_module`) never emits this.
	MainMissing,
	/// The entry module's top-level `main` declares one or more generic
	/// parameters; the entry point cannot be generic.
	MainGeneric,
	/// The entry module's top-level `main` declares one or more parameters;
	/// the entry point is invoked with no arguments.
	MainHasParams,
	/// The entry module's top-level `main` declares an explicit return type
	/// other than `void`; the entry point's result is discarded.
	MainNonVoidReturn,

	// ── Casts ────────────────────────────────────────────────────────────────
	/// An `as` cast is neither identity/scalar-builtin nor resolvable through an
	/// `Into` impl because no `Into` interface is in scope. This differs from
	/// [`TypeError::CannotCast`], which fires when `Into` is in scope but no impl
	/// satisfies it.
	CastRequiresInto {
		from: String,
		to: String,
	},

	/// An `as` cast resolved a `holds`-satisfying `Into`-named interface (`self.defs
	///.get("Into")` found one, and some impl satisfies it for `src`/`target`), but
	/// the interface itself doesn't declare exactly one zero-arg method — the shape
	/// `check_cast` needs to know WHICH method `as` should call. `holds` only checks
	/// the interface's generic args (`Other`/whatever it's named), never method
	/// names or arity, so a local `interface Into<Other> { .. }` with zero, two, or
	/// only non-zero-arg methods satisfies `holds` just as readily as the canonical
	/// single-zero-arg-method shape — this diagnostic closes the gap so lowering
	/// never has to guess (or fall back to a hardcoded name that might not exist on
	/// the class at all).
	IntoInterfaceMalformed {
		from: String,
		to: String,
	},
	/// A literal numeric-to-`char` cast truncates to a value outside the Unicode
	/// scalar-value range (including the surrogate interval).
	InvalidCharCastLiteral,

	/// A `&&`/`||` operand was not `boolean`. Logical operators are never
	/// overloadable (copying Rust's design): whether the right-hand operand
	/// evaluates must never depend on operand types, so both sides always unify
	/// with `boolean` and the builtin always short-circuits — there is no
	/// `And`/`Or` interface to dispatch through any more. This variant exists
	/// (rather than reusing `MismatchedTypes`) purely so the diagnostic can
	/// carry a dedicated help hint explaining *why* there's no overload to reach
	/// for, instead of reading like an ordinary type-mismatch bug.
	LogicalOperandNotBoolean {
		found: String,
	},

	/// A source `int`/`uint` literal whose magnitude exceeds `2^53 - 1`
	/// (`Number.MAX_SAFE_INTEGER`) — Nymph's `int`/`uint` are JS doubles at
	/// runtime, and a magnitude past this bound can't round-trip through one
	/// exactly, so the literal as written and the value the program actually
	/// runs with can silently diverge. A warning, not an error (precedent:
	/// `UnreachableArm`'s `Severity::Warning` arm below) — the program still
	/// runs, just with the nearest representable `f64` in place of the exact
	/// literal. `int`/`uint` literals store their magnitude as `u64` with the
	/// sign (for `int`) as a separate `PrefixOperator::Negate` wrapping the
	/// literal node, so this also fires for a negative literal like
	/// `-9007199254740992`: inferring the `Negate` operand infers the inner
	/// literal expression first, which is where this warning is emitted — no
	/// special-casing of the surrounding `Negate` is needed.
	IntLiteralUnsafe {
		value: u64,
	},

	/// A field's SLOT was reassigned (`p.field = v`) through a receiver whose
	/// type is not `mut` — mutable-types enforcement. The field's own
	/// declared type is irrelevant here; what's gating is whether `p` itself is
	/// a `mut` view.
	AssignFieldThroughImmutable {
		field: EcoString,
		ty: String,
	},

	// ── Mutable types, interfaces  ──────────────────────────────────────
	/// A `mut func` interface method (the interface's declared kind is the
	/// source of truth) was called on a receiver that isn't `mut` — a plain
	/// value only has the interface's non-`mut` methods available. Reached
	/// uniformly through `resolve_method`'s single gate for a concrete `mut B`
	/// receiver, an interface default body, and a `T: A` bound's `mut T`
	/// requirement alike.
	MutMethodNeedsMutReceiver {
		method: EcoString,
	},

	/// An `impl A for B` (or nested `impl A { .. }`) restated a method's
	/// `mut func`/`func` kind differently from what interface `A` itself
	/// declares — e.g. the interface says `mut func push`, the impl says
	/// plain `func push`. The interface is the source of truth every call-site
	/// gate reads, so a mismatch here would silently desync what the impl body
	/// requires from what callers are checked against. `expected_mut` is the
	/// interface's own declared kind.
	MethodMutMismatch {
		name: EcoString,
		ty: String,
		expected_mut: bool,
	},

	/// A generic parameter `T: A` was instantiated, across the arguments of one
	/// call, by BOTH a `mut` and a non-`mut` value at positions sharing that
	/// same `T`  — e.g. `f<T: A>(x: T, y: T)` called `f(mut_b, b)`. No
	/// single type for `T` can be correct for both call sites when `A` is
	/// implemented only for `mut B` (`impl A for mut B` / `impl mut A for B`).
	MixedMutabilityForBound {
		interface: EcoString,
	},

	/// A `T: A` bound obligation failed for a plain type `ty`, but the `mut`
	/// version of `ty` WOULD satisfy it (`A` is implemented only for
	/// `mut ty`, via `impl A for mut ty` / `impl mut A for ty`) — a more
	/// specific diagnostic than [`TypeError::BoundNotSatisfied`], hinting the
	/// fix directly (pass a `mut` value) rather than leaving the caller to
	/// guess.
	BoundSatisfiedOnlyByMut {
		ty: String,
		interface: EcoString,
	},

	/// A `for` loop's source implements neither `Iterator` nor `Iterable` (the
	/// only two shapes `infer_iterable_element` accepts once the syntactic-range
	/// and list fast paths are ruled out). Accepting the source with `self.fresh`
	/// leaves the loop pattern unconstrained and lets invalid bodies reach
	/// lowering.
	NotIterable {
		ty: String,
	},

	/// A `match` over `uint` leaves some values uncovered — the unsigned-domain
	/// counterpart of [`TypeError::NonExhaustiveInt`], worded for `uint` so the
	/// message never claims the in `int` values.
	NonExhaustiveUInt,

	/// An operator has no implementation for its operand type(s). The user-facing
	/// counterpart of [`TypeError::NotImplemented`] for operator syntax: names the
	/// operator symbol, both operands, and the interface to implement — rather than
	/// leaking the internal desugared method name and only one operand type. `rhs` is
	/// `None` for a unary operator.
	OperatorNotImplemented {
		operator: EcoString,
		interface: EcoString,
		lhs: String,
		rhs: Option<String>,
	},

	/// A positional (unnamed) sub-pattern was used on a constructor that does not have
	/// exactly one field, so there is no single field for it to bind to. `fields` is
	/// the constructor's actual field count.
	PositionalPatternArity {
		fields: usize,
	},

	/// Range iteration advances by one, so its bounds must be discrete integer
	/// values. Floating-point and other `Comparable` values do not define that
	/// progression.
	InvalidRangeBound {
		ty: String,
	},

	/// One destructuring pattern binds the same local more than once.
	DuplicatePatternBinding {
		name: EcoString,
	},
	/// The alternatives of a union pattern introduce different locals.
	InconsistentUnionBindings,
	/// A tuple spread operand does not have a concrete, statically known tuple
	/// shape, so its elements cannot be incorporated into the result type.
	TupleSpreadRequiresStaticTuple {
		ty: String,
	},
	/// An external let marker has no immutable-value registry entry.
	ExternalValueLinkageMissing {
		marker: EcoString,
	},
	/// An external let marker is registered for a callable external instead.
	ExternalLinkageWrongKind {
		marker: EcoString,
	},
	/// An external function marker is registered for an immutable value instead.
	ExternalFunctionLinkageWrongKind {
		marker: EcoString,
	},
	/// The declared type has no raw-host marshalling ABI.
	ExternalValueTypeUnsupported,
	/// The declaration's marshal ABI differs from the registry contract.
	ExternalValueTypeMismatch {
		marker: EcoString,
	},
	/// External host values are snapshots and cannot be mutable bindings.
	ExternalValueMutable,
	/// A loop-control expression has no lexically enclosing loop. Appended so
	/// existing numeric diagnostic codes remain stable.
	LoopControlOutsideLoop {
		keyword: &'static str,
	},
	/// One loop contains both `break` and `break value`. Appended so existing
	/// numeric diagnostic codes remain stable.
	MixedBreakForms,
	UnknownControlLabel {
		name: EcoString,
	},
	WrongControlLabelKind {
		name: EcoString,
		keyword: &'static str,
	},
	DuplicateControlLabel {
		name: EcoString,
		previous: Span,
	},
	/// The same local has incompatible inferred types in two union alternatives.
	/// Appended so existing numeric diagnostic codes remain stable.
	InconsistentUnionBindingType {
		name: EcoString,
		left: String,
		right: String,
	},
	/// The same local has different binding mutability in two union alternatives.
	/// Appended so existing numeric diagnostic codes remain stable.
	InconsistentUnionBindingMutability {
		name: EcoString,
	},
	QuestionOperand {
		found: String,
	},
	QuestionTarget {
		family: &'static str,
		found: String,
	},
	QuestionOutsideCallable,
}

impl IntoDiagnostic for TypeError {
	fn message(&self) -> EcoString {
		use TypeError as E;
		match self {
			E::CannotFind { name } => format!("cannot find `{name}` in this scope").into(),
			E::CannotFindType { name } => format!("cannot find type `{name}` in this scope").into(),
			E::CannotFindEnum { name } => format!("cannot find enum `{name}`").into(),
			E::CannotFindConstructor { name } => format!("cannot find constructor `{name}`").into(),
			E::AmbiguousVariant { name } => {
				format!("ambiguous variant `{name}`; qualify it as `Enum.{name}`").into()
			}
			E::NotAType { name } => format!("`{name}` is not a type").into(),
			E::NotAnInterface { name } => format!("`{name}` is not an interface").into(),
			E::GenericParamWithArgs { name } => {
				format!("generic parameter `{name}` cannot take type arguments").into()
			}
			E::Redefinition { name, .. } => format!("`{name}` is defined more than once").into(),
			E::RecursiveTypeAlias => "type alias expands recursively without end".into(),
			E::InfiniteType { ty } => format!("this expression has an infinite type `{ty}`").into(),
			E::LoopControlOutsideLoop { keyword } => {
				format!("`{keyword}` is only valid inside a loop").into()
			}
			E::MixedBreakForms => {
				"a loop cannot mix bare `break` with `break value`".into()
			}
			E::UnknownControlLabel { name } => format!("unknown control label `{name}`").into(),
			E::WrongControlLabelKind { name, keyword } => format!("`{keyword}` cannot target `{name}` because it has the wrong kind").into(),
			E::DuplicateControlLabel { name, .. } => format!("control label `{name}` is already active").into(),
			E::QuestionOperand { found } => {
				format!("the `?` operand must be `Option` or `Result`, found `{found}`").into()
			}
			E::QuestionTarget { family, found } => format!(
				"cannot propagate `{family}` into a target returning `{found}`"
			)
			.into(),
			E::QuestionOutsideCallable => {
				"unlabelled `?` is only valid inside a callable".into()
			}

			E::ThisOutsideMethod => "`this` is only valid inside a method".into(),
			E::StructTypeAsValue => "a struct type cannot be used as a value directly".into(),
			E::FieldVariantAsValue { variant } => format!(
				"variant `{variant}` carries fields and cannot be used as a value; call it to construct, e.g. `{variant}(field = …)`"
			)
			.into(),
			E::TypeAsValue => "a type cannot be used as a value".into(),
			E::NotCallable => "this expression is not callable".into(),
			E::UnknownField { field } => format!("unknown field `{field}`").into(),
			E::TooManyFields => "too many fields supplied".into(),
			E::NoField { field, ty } => format!("no field `{field}` on `{ty}`").into(),
			E::EnumHasNoVariant { enum_name, variant } => {
				format!("enum `{enum_name}` has no variant `{variant}`").into()
			}
			E::NoVariantOrNamespacedFn { ty, name } => {
				format!("`{ty}` has no variant or namespaced function `{name}`").into()
			}
			E::NoNamespacedFn { ty, name } => {
				format!("`{ty}` has no namespaced function `{name}`").into()
			}
			E::NoMethod { method, ty } => format!("no method `{method}` found for `{ty}`").into(),
			E::CannotAccess { member, ty } => format!("cannot access `{member}` on `{ty}`").into(),
			E::OptionalChainReceiver { ty } => format!(
				"optional chaining requires canonical `Option` or `Result`, found `{ty}`"
			)
			.into(),
			E::NoNamespacedFnOnParam { name } => {
				format!("no namespaced function `{name}` found on this type parameter").into()
			}

			E::NotImplemented { method, ty } => {
				format!("`{method}` is not implemented for `{ty}`").into()
			}
			E::OperatorNotImplemented {
				operator,
				interface,
				lhs,
				rhs,
			} => match rhs {
				Some(rhs) => format!(
					"the `{operator}` operator is not implemented for `{lhs}` and `{rhs}`; \
					 implement `{interface}` to support it"
				)
				.into(),
				None => format!(
					"the `{operator}` operator is not implemented for `{lhs}`; \
					 implement `{interface}` to support it"
				)
				.into(),
			},
			E::PositionalPatternArity { fields } => format!(
				"a positional sub-pattern is only allowed on a constructor with exactly one field, \
				 but this one has {fields}; name the fields (`field = pattern`) instead"
			)
			.into(),
			E::CannotCast { from, to } => {
				format!("cannot cast `{from}` to `{to}`: no `Into` implementation").into()
			}
			E::MismatchedTypes { expected, found } => {
				format!("mismatched types: expected `{expected}`, found `{found}`").into()
			}
			E::ConflictingImpls { iface } => {
				format!("conflicting implementations of interface `{iface}`").into()
			}
			E::NoMatchingOverload { name } => {
				format!("no overload of `{name}` matches these arguments").into()
			}
			E::AmbiguousCall { name } => {
				format!("ambiguous call to `{name}`: multiple impls apply").into()
			}

			E::WrongArgCount { expected, found } => {
				format!("expected {expected} argument(s), found {found}").into()
			}
			E::NamedWrongArgCount {
				name,
				expected,
				found,
			} => format!("`{name}` expects {expected} argument(s), found {found}").into(),

			E::AssignToImmutable { name } => format!("cannot assign to immutable `{name}`").into(),
			E::CannotAssign { name } => format!("cannot assign to `{name}`").into(),

			E::UnsupportedConstructorPath => "unsupported constructor path".into(),

			E::AnonymousParamUnsupported => {
				"no enclosing closure boundary type-checks for this anonymous parameter (`$`)".into()
			}
			E::MethodCallsUnsupported => "method calls are not supported yet".into(),

			E::NonExhaustiveMatch { witness } => {
				format!("non-exhaustive match: `{witness}` is not covered").into()
			}
			E::NonExhaustiveInt => {
				"non-exhaustive match: some `int` values are not covered — add a `_` arm".into()
			}
			E::NonExhaustiveUInt => {
				"non-exhaustive match: some `uint` values are not covered — add a `_` arm".into()
			}
			E::NonExhaustiveNeedsWildcard => {
				"non-exhaustive match: add a `_` arm to cover the remaining cases".into()
			}
			E::UnreachableArm => "unreachable match arm".into(),

			E::CannotInferOperandType => {
				"cannot infer the operand type of this operator; add a type annotation".into()
			}

			E::BoundNotSatisfied { ty, interface } => {
				format!("`{ty}` does not implement `{interface}`").into()
			}

			E::DuplicateMember { name, ty, .. } => {
				format!("`{name}` is defined more than once on `{ty}`").into()
			}
			E::DuplicatePatternBinding { name } => {
				format!("`{name}` is bound more than once in this pattern").into()
			}
			E::InconsistentUnionBindings => {
				"both sides of a union pattern must bind the same names".into()
			}
			E::InconsistentUnionBindingType { name, left, right } => format!(
				"mismatched types for union pattern binding `{name}`: left alternative has `{left}`, right alternative has `{right}`"
			)
			.into(),
			E::InconsistentUnionBindingMutability { name } => {
				format!("`{name}` has different mutability across union pattern alternatives").into()
			}
			E::ExternalValueLinkageMissing { marker } => {
				format!("external value marker `{marker}` is not registered").into()
			}
			E::ExternalLinkageWrongKind { marker } => {
				format!("external marker `{marker}` is registered as a function, not a value").into()
			}
			E::ExternalFunctionLinkageWrongKind { marker } => {
				format!("external marker `{marker}` is registered as a value, not a function").into()
			}
			E::ExternalValueTypeUnsupported => {
				"external let type has no raw host-value marshalling ABI".into()
			}
			E::ExternalValueTypeMismatch { marker } => {
				format!("external value marker `{marker}` has an incompatible declared type").into()
			}
			E::ExternalValueMutable => "external lets are immutable; remove `mut`".into(),

			E::MainMissing => "no `main` function found".into(),
			E::MainGeneric => "`main` cannot declare generic parameters".into(),
			E::MainHasParams => "`main` must not declare any parameters".into(),
			E::MainNonVoidReturn => {
				"`main` must not declare a return type other than `void`".into()
			}

			E::CastRequiresInto { from, to } => {
				format!("cannot cast `{from}` to `{to}`: no `Into` interface is in scope").into()
			}
			E::IntoInterfaceMalformed { from, to } => format!(
				"cannot cast `{from}` to `{to}`: the resolved `Into` interface must declare exactly \
				 one zero-argument method"
			)
			.into(),
			E::InvalidCharCastLiteral => {
				"numeric literal is not a valid Unicode scalar value after truncation".into()
			}
			E::LogicalOperandNotBoolean { found } => {
				format!("mismatched types: expected `boolean`, found `{found}`").into()
			}
			E::IntLiteralUnsafe { value } => format!(
				"integer literal `{value}` exceeds `Number.MAX_SAFE_INTEGER` (2^53 - 1) and will lose \
				 precision"
			)
			.into(),
			E::AssignFieldThroughImmutable { field, ty } => {
				format!("cannot assign to field `{field}` through immutable `{ty}`").into()
			}

			E::MutMethodNeedsMutReceiver { method } => {
				format!("`{method}` requires a `mut` receiver").into()
			}
			E::MethodMutMismatch {
				name, expected_mut, ..
			} => {
				let (declared, restated) = if *expected_mut {
					("mut func", "func")
				} else {
					("func", "mut func")
				};
				format!(
					"`{name}` is declared `{declared}` on the interface but restated `{restated}` here"
				)
				.into()
			}
			E::MixedMutabilityForBound { interface } => format!(
				"mixed `mut`/non-`mut` arguments for one type parameter bounded by `{interface}`"
			)
			.into(),
			E::BoundSatisfiedOnlyByMut { ty, interface } => {
				format!("`{ty}` does not implement `{interface}`; `mut {ty}` does").into()
			}
			E::NotIterable { ty } => {
				format!("`{ty}` is not iterable; it implements neither `Iterator` nor `Iterable`")
					.into()
			}
			E::InvalidRangeBound { ty } => {
				format!("range bounds must be `int` or `uint`, not `{ty}`").into()
			}
			E::TupleSpreadRequiresStaticTuple { ty } => {
				format!("tuple spread requires a statically shaped tuple, found `{ty}`").into()
			}
		}
	}

	fn severity(&self) -> Severity {
		match self {
			TypeError::UnreachableArm | TypeError::IntLiteralUnsafe { .. } => Severity::Warning,
			_ => Severity::Error,
		}
	}

	fn labels(&self) -> Vec<Label> {
		match self {
			TypeError::Redefinition {
				redefined_span,
				prev,
				..
			}
			| TypeError::DuplicateMember {
				redefined_span,
				prev,
				..
			} => vec![
				Label::new(*redefined_span, "redefined here"),
				Label::new(*prev, "first defined here"),
			],
			TypeError::DuplicateControlLabel { previous, .. } => {
				vec![Label::new(*previous, "previous label is here")]
			}
			_ => Vec::new(),
		}
	}

	fn help(&self) -> Option<EcoString> {
		match self {
			TypeError::UnreachableArm => Some("a previous arm already covers this case".into()),
			TypeError::DuplicateMember { name, .. } => {
				Some(format!("rename one of the `{name}` members to remove the collision").into())
			}
			TypeError::MainMissing => Some(
				"the entry module must declare a top-level `func main` taking no parameters and \
				 declaring no return type other than `void`"
					.into(),
			),
			TypeError::MainGeneric => Some(
				"remove the generic parameters — `main` is the program's entry point and is \
				 invoked directly, with no type arguments"
					.into(),
			),
			TypeError::MainHasParams => {
				Some("`main` is called with no arguments — remove the parameters".into())
			}
			TypeError::MainNonVoidReturn => {
				Some("`main` returns nothing — remove the return type".into())
			}
			TypeError::CastRequiresInto { .. } => Some(
				"declare or import an `Into` interface and implement it for the source type, or cast \
				 between built-in scalar types instead"
					.into(),
			),
			TypeError::IntoInterfaceMalformed { .. } => Some(
				"give the `Into` interface exactly one zero-argument method (its conversion entry \
				 point) — `as` dispatches to whichever one it declares"
					.into(),
			),
			TypeError::LogicalOperandNotBoolean { .. } => Some(
				"logical operators are not overloadable — `&&`/`||` always take booleans and \
				 short-circuit"
					.into(),
			),
			TypeError::IntLiteralUnsafe { .. } => Some(
				"Nymph's `int`/`uint` are JS doubles at runtime — integers beyond ±(2^53 - 1) can't \
				 be represented exactly and will be rounded to the nearest representable value"
					.into(),
			),
			TypeError::AssignFieldThroughImmutable { ty, .. } => Some(
				format!("make the receiver a mutable view, e.g. `let mut` or a `mut {ty}` annotation")
					.into(),
			),
			TypeError::MutMethodNeedsMutReceiver { .. } => Some(
				"bind or annotate the receiver as `mut`, e.g. `let mut` or a `mut` type annotation".into(),
			),
			TypeError::MethodMutMismatch {
				name, expected_mut, ..
			} => Some(if *expected_mut {
				format!("declare `{name}` as `mut func` here to match the interface").into()
			} else {
				format!("declare `{name}` as `func` here to match the interface").into()
			}),
			TypeError::MixedMutabilityForBound { .. } => Some(
				"pass `mut` values at every use of this type parameter, or non-`mut` values at none".into(),
			),
			TypeError::BoundSatisfiedOnlyByMut { ty, .. } => {
				Some(format!("pass a `mut {ty}` instead").into())
			}
			_ => None,
		}
	}
}
