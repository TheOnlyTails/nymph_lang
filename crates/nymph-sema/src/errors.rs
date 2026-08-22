//! The type checker's diagnostics as a single typed catalog.
//!
//! Every error and warning the checker can emit is one variant of [`TypeError`],
//! carrying only its semantic data; the [`IntoDiagnostic`] impl is the one place
//! that turns a variant into a rendered message, severity, labels, and help. The
//! primary span is supplied at the emit site (`Checker::emit`). Existing variants
//! remain in place and new variants are appended so their generated codes stay
//! stable.

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
	/// A `let use` initializer does not satisfy the canonical `Close` interface.
	ManagedResourceRequired {
		ty: EcoString,
	},

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

	/// A signed source integer literal outside `i64::MIN..=i64::MAX`.
	IntLiteralOutOfRange {
		value: u64,
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
	QuestionOperand {
		found: String,
	},
	QuestionTarget {
		family: &'static str,
		found: String,
	},
	QuestionOutsideCallable,
	/// `.await` was used without an enclosing async function or block.
	AwaitOutsideAsync,
	/// `.await` was applied to a value that is not a task or execution handle.
	AwaitOperand {
		found: String,
	},
	/// A fully constant fixed-width integer operation that would fail at runtime.
	/// Appended so existing numeric diagnostic codes remain stable.
	IntegerConstantInvalid {
		reason: EcoString,
	},
	RangeOperationInvalid {
		reason: EcoString,
	},
	TupleSliceUnsupported,
	/// An effect name was referenced that is not in scope.
	CannotFindEffect {
		name: EcoString,
	},
	/// A name used in an effect row does not denote an effect.
	NotAnEffect {
		name: EcoString,
	},
	/// An effect generic was used where a type was required, or vice versa.
	GenericKindMismatch {
		name: EcoString,
		expected: &'static str,
	},
	/// `!_` needs a body or initializer from which effects can be inferred.
	CannotInferEffectRow,
	/// A callable body requires effects outside its declared closed upper bound.
	EffectRowExceedsAnnotation,
	/// An interface implementation declares effects outside the interface contract.
	ImplementationEffectRowExceedsContract {
		method: EcoString,
	},
	PositionalStructField,
	InvalidStructSpread,
	DuplicateStructField {
		field: EcoString,
	},
	InaccessibleStructField {
		field: EcoString,
	},
	StructFreshUnavailable,
	MissingStructFields {
		fields: Vec<EcoString>,
	},
	StructPatternRestRequired,
	PositionalStructPattern,
	InvalidStructPatternRest,
	/// A nominal type directly stores a managed resource but does not define its
	/// own cleanup behavior. This warning is deliberately non-transitive.
	ManagedFieldWithoutClose {
		owner: EcoString,
		field: EcoString,
		owner_span: Span,
		field_span: Span,
	},
	/// A spawned child captures a managed local beyond that local's lexical
	/// cleanup boundary. This remains a warning rather than ownership checking.
	ManagedChildCapture {
		declaration: Span,
		close: Span,
		join: Span,
	},
	/// A state-loop header must declare a named immutable binding.
	InvalidStateLoopBinding,
	/// A named continue replacement does not match a state binding.
	UnknownStateReplacement {
		name: EcoString,
	},
	/// A named continue replaces the same state binding more than once.
	DuplicateStateReplacement {
		name: EcoString,
	},
	/// Argument-bearing continue can only target an immutable state loop.
	StateReplacementOutsideStateLoop,
	/// A destination enum was used as a runtime wrapper around a source enum.
	RetiredEnumWrapper,
	/// A destination enum qualified a source-owned variant pattern.
	RetiredEnumWrapperPattern,
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
			E::CannotFindEffect { name } => {
				format!("cannot find effect `{name}` in this scope").into()
			}
			E::NotAnEffect { name } => format!("`{name}` is not an effect").into(),
			E::GenericKindMismatch { name, expected } => {
				format!("generic parameter `{name}` is not a {expected} parameter").into()
			}
			E::CannotInferEffectRow => {
				"cannot infer this effect row without a callable body or initializer".into()
			}
			E::EffectRowExceedsAnnotation => {
				"callable body requires effects outside its declared effect row".into()
			}
			E::ImplementationEffectRowExceedsContract { method } => format!(
				"implementation method `{method}` requires effects outside the interface contract"
			)
			.into(),
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
			E::AwaitOutsideAsync => {
				"`.await` is only valid inside an async function or async block".into()
			}
			E::AwaitOperand { found } => {
				format!("the `.await` operand must be a task or execution handle, found `{found}`").into()
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
			E::ManagedResourceRequired { ty } => {
				format!("`let use` requires a value implementing `Close`, not `{ty}`").into()
			}

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

			E::MainMissing => "no `main` function found".into(),
			E::MainGeneric => "`main` cannot declare generic parameters".into(),
			E::MainHasParams => "`main` must not declare any parameters".into(),
			E::MainNonVoidReturn => {
				"`main` must return `void`, `Option<void>`, `Result<void, E>`, or a `Task` producing one of those types".into()
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
			E::IntLiteralOutOfRange { value } => {
				format!("integer literal `{value}` is out of range for `int`").into()
			}
			E::IntegerConstantInvalid { reason } => reason.clone(),
			E::RangeOperationInvalid { reason } => reason.clone(),
			E::TupleSliceUnsupported => "tuple slicing is unsupported".into(),
			E::NotIterable { ty } => {
				format!("`{ty}` is not iterable; it implements neither `Iterator` nor `Iterable`")
					.into()
			}
			E::InvalidRangeBound { ty } => {
				format!("range bounds must be `int` or `uint`, not `{ty}`").into()
			}
			E::InvalidStateLoopBinding => {
				"state-loop headers require named immutable `let` or `let use` bindings".into()
			}
			E::UnknownStateReplacement { name } => {
				format!("`{name}` is not a state binding of the target loop").into()
			}
			E::DuplicateStateReplacement { name } => {
				format!("state binding `{name}` is replaced more than once").into()
			}
			E::StateReplacementOutsideStateLoop => {
				"named `continue` replacements require a state loop target".into()
			}
			E::RetiredEnumWrapper => {
				"enum embedding changes the static view and does not construct a wrapper".into()
			}
			E::RetiredEnumWrapperPattern => {
				"embedded variants are matched through their qualified source enum".into()
			}
			E::PositionalStructField => "struct fields must be supplied by name (`field = value`)".into(),
			E::InvalidStructSpread => "a struct clone/update requires exactly one leading source spread".into(),
			E::DuplicateStructField { field } => format!("struct field `{field}` is supplied more than once").into(),
			E::InaccessibleStructField { field } => format!("struct field `{field}` is not available in this context").into(),
			E::StructFreshUnavailable => "this struct cannot be constructed fresh because it has hidden fields".into(),
			E::MissingStructFields { fields } => format!("missing required struct field(s): {}", fields.join(", ")).into(),
			E::StructPatternRestRequired => "a partial struct pattern must end with anonymous `...`".into(),
			E::PositionalStructPattern => "struct pattern fields must be named".into(),
			E::InvalidStructPatternRest => "anonymous `...` must occur once at the end of a struct pattern".into(),
			E::TupleSpreadRequiresStaticTuple { ty } => {
				format!("tuple spread requires a statically shaped tuple, found `{ty}`").into()
			}
			E::ManagedFieldWithoutClose { owner, field, .. } => format!(
				"`{owner}` directly stores managed field `{field}` but does not implement `Close`"
			)
			.into(),
			E::ManagedChildCapture { .. } => {
				"spawned child may use this managed resource after its lexical cleanup".into()
			}
		}
	}

	fn severity(&self) -> Severity {
		match self {
			TypeError::UnreachableArm
			| TypeError::ManagedFieldWithoutClose { .. }
			| TypeError::ManagedChildCapture { .. } => Severity::Warning,
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
			TypeError::ManagedFieldWithoutClose {
				owner_span,
				field_span,
				..
			} => vec![
				Label::new(*owner_span, "containing type declared here"),
				Label::new(*field_span, "managed field declared here"),
			],
			TypeError::ManagedChildCapture {
				declaration,
				close,
				join,
			} => vec![
				Label::new(*declaration, "managed resource declared here"),
				Label::new(*close, "resource closes at this lexical boundary"),
				Label::new(*join, "child is joined at this boundary"),
			],
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
				Some("use one of the supported resolved root result shapes".into())
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
			TypeError::IntLiteralOutOfRange { .. } => {
				Some("use a `uint` literal suffix (`u`) for values up to `u64::MAX`".into())
			}
			_ => None,
		}
	}
}
