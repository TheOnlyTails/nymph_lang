//! The type checker's diagnostics as a single typed catalog.
//!
//! Every error and warning the checker can emit is one variant of [`TypeError`],
//! carrying only its semantic data; the [`IntoDiagnostic`] impl is the one place
//! that turns a variant into a rendered message, severity, labels, and help. The
//! primary span is supplied at the emit site (`Checker::emit`). Error *codes* are
//! not assigned yet — `code()` inherits the trait default (`None`) until the code
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
	CannotFind { name: EcoString },
	/// A type name was referenced that isn't in scope.
	CannotFindType { name: EcoString },
	/// A pattern referenced an enum that isn't in scope.
	CannotFindEnum { name: EcoString },
	/// A pattern referenced a constructor that couldn't be resolved.
	CannotFindConstructor { name: EcoString },
	/// A bare variant name matched more than one enum; it must be qualified.
	AmbiguousVariant { name: EcoString },
	/// A name used in type position doesn't denote a type.
	NotAType { name: EcoString },
	/// A name used as an interface doesn't denote one.
	NotAnInterface { name: EcoString },
	/// A generic parameter was given type arguments (it takes none).
	GenericParamWithArgs { name: EcoString },
	/// A top-level name was defined more than once.
	Redefinition {
		name: EcoString,
		redefined_span: Span,
		prev: Span,
	},
	/// A type alias expands into itself without termination.
	RecursiveTypeAlias,
	/// Inference produced a type that contains itself.
	InfiniteType { ty: String },

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
	UnknownField { field: EcoString },
	/// A struct literal supplied more fields than the type declares.
	TooManyFields,
	/// A field access named a field the receiver type lacks.
	NoField { field: EcoString, ty: String },
	/// An enum was accessed with a variant name it doesn't declare.
	EnumHasNoVariant {
		enum_name: EcoString,
		variant: EcoString,
	},
	/// A namespaced access found neither a variant nor a namespaced function.
	NoVariantOrNamespacedFn { ty: EcoString, name: EcoString },
	/// A namespaced access found no namespaced function of that name.
	NoNamespacedFn { ty: EcoString, name: EcoString },
	/// No method of that name resolves for the receiver type.
	NoMethod { method: EcoString, ty: String },
	/// A member access could not be resolved on the receiver type.
	CannotAccess { member: EcoString, ty: String },
	/// A namespaced function was called through a type parameter that lacks it.
	NoNamespacedFnOnParam { name: EcoString },

	// ── Operators, casts, impls ──────────────────────────────────────────────
	/// A required interface method isn't implemented for the operand type.
	NotImplemented { method: EcoString, ty: String },
	/// An `as` cast has no corresponding `Into` implementation.
	CannotCast { from: String, to: String },
	/// Two types that were required to match did not.
	MismatchedTypes { expected: String, found: String },
	/// Two impls of one interface overlap for the same receiver.
	ConflictingImpls { iface: EcoString },
	/// No overload of a function matches the given arguments.
	NoMatchingOverload { name: EcoString },
	/// Multiple impls apply to a call, and none is more specific.
	AmbiguousCall { name: EcoString },

	// ── Arguments ────────────────────────────────────────────────────────────
	/// A call supplied the wrong number of arguments.
	WrongArgCount { expected: usize, found: usize },
	/// A named function/method was called with the wrong number of arguments.
	NamedWrongArgCount {
		name: EcoString,
		expected: usize,
		found: usize,
	},

	// ── Assignment ───────────────────────────────────────────────────────────
	/// Assignment to an immutable binding.
	AssignToImmutable { name: EcoString },
	/// Assignment to something that isn't an assignable place.
	CannotAssign { name: EcoString },

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
	/// Method-call syntax is not implemented yet (Milestone B).
	MethodCallsUnsupported,

	// ── Exhaustiveness (warnings + errors) ───────────────────────────────────
	/// A `match` does not cover a constructible value (witness rendered).
	NonExhaustiveMatch { witness: String },
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
	FieldVariantAsValue { variant: EcoString },

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
	BoundNotSatisfied { ty: String, interface: EcoString },

	// ── Inner members ────────────────────────────────────────────────────────
	/// A struct/enum inner member — an instance `func`, a `namespace func`
	/// static, or a `mut func` method — was declared more than once under the
	/// same name on the same type. `collect_impl_member` (members.rs) used to
	/// collect all three kinds into one `FxHashMap` keyed only by name, letting a
	/// later member silently overwrite an earlier same-named one; the shadowed
	/// member's body was then never type-checked, yet the Slice 4J HIR lowering
	/// walks the raw AST and emits every member's body regardless — an
	/// unchecked-body-reaches-JS soundness hole. This diagnostic closes it at the
	/// root: a program with this collision never has zero diagnostics, so it
	/// never reaches lowering. `ty` names the owning struct/enum.
	DuplicateMember {
		name: EcoString,
		ty: String,
		redefined_span: Span,
		prev: Span,
	},

	// ── Entry point (`main`) validation ─────────────────────────────────────
	// New variants are appended at the END of the enum, never inserted earlier:
	// the `ErrorCode` derive assigns codes as `2{variant_index:03}` purely by
	// declaration order, so appending here preserves every existing code
	// (DuplicateMember above took 2045 on the main line; these mint 2046-2049).
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

	// ── Casts (Slice 4K) ─────────────────────────────────────────────────────
	/// An `as` cast is neither identity/scalar-builtin nor resolvable through an
	/// `Into` impl, because no `Into` interface is even in scope (`self.defs.get
	/// ("Into")` found nothing) — distinct from [`TypeError::CannotCast`], which
	/// fires when `Into` *is* in scope but no impl satisfies it. Without this,
	/// `check_cast` used to return silently whenever a module (e.g. one that
	/// doesn't link the stdlib) never declares `interface Into`, so a non-scalar
	/// `as` type-checked completely unchecked and only died later at lowering's
	/// unresolved-cast panic — a checker-bug-shaped hole on every real program,
	/// since `nymph-compiler::compile` checks a module standalone with no stdlib
	/// linkage. New variant appended at the enum's end per the `ErrorCode`
	/// derive's declaration-order codes (mints 2050; MainNonVoidReturn above kept
	/// its existing 2049).
	CastRequiresInto { from: String, to: String },

	/// An `as` cast resolved a `holds`-satisfying `Into`-named interface (`self.defs
	/// .get("Into")` found one, and some impl satisfies it for `src`/`target`), but
	/// the interface itself doesn't declare exactly one zero-arg method — the shape
	/// `check_cast` needs to know WHICH method `as` should call. `holds` only checks
	/// the interface's generic args (`Other`/whatever it's named), never method
	/// names or arity, so a local `interface Into<Other> { .. }` with zero, two, or
	/// only non-zero-arg methods satisfies `holds` just as readily as the canonical
	/// single-zero-arg-method shape — this diagnostic closes the gap so lowering
	/// never has to guess (or fall back to a hardcoded name that might not exist on
	/// the class at all — the exact silent-miscompile bug this fixes). New variant
	/// appended at the enum's end (mints 2051; CastRequiresInto above kept its
	/// existing 2050).
	IntoInterfaceMalformed { from: String, to: String },

	/// A `&&`/`||` operand was not `boolean`. Logical operators are never
	/// overloadable (copying Rust's design): whether the right-hand operand
	/// evaluates must never depend on operand types, so both sides always unify
	/// with `boolean` and the builtin always short-circuits — there is no
	/// `And`/`Or` interface to dispatch through any more. This variant exists
	/// (rather than reusing `MismatchedTypes`) purely so the diagnostic can
	/// carry a dedicated help hint explaining *why* there's no overload to reach
	/// for, instead of reading like an ordinary type-mismatch bug. New variant
	/// appended at the enum's end (mints 2052; IntoInterfaceMalformed above kept
	/// its existing 2051).
	LogicalOperandNotBoolean { found: String },

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
	/// special-casing of the surrounding `Negate` is needed. New variant
	/// appended at the enum's end (mints 2053; LogicalOperandNotBoolean above
	/// kept its existing 2052).
	IntLiteralUnsafe { value: u64 },

	/// A field's SLOT was reassigned (`p.field = v`) through a receiver whose
	/// type is not `mut` — mutable-types (MT1) enforcement. The field's own
	/// declared type is irrelevant here; what's gating is whether `p` itself is
	/// a `mut` view. New variant appended at the enum's end (mints 2054).
	AssignFieldThroughImmutable { field: EcoString, ty: String },

	// ── Mutable types, interfaces (MT2) ──────────────────────────────────────
	/// A `mut func` interface method (OO1: the interface's declared kind is the
	/// source of truth) was called on a receiver that isn't `mut` — a plain
	/// value only has the interface's non-`mut` methods available. Reached
	/// uniformly through `resolve_method`'s single gate for a concrete `mut B`
	/// receiver, an interface default body, and a `T: A` bound's `mut T`
	/// requirement (OO3) alike. New variant appended at the enum's end (mints
	/// 2055).
	MutMethodNeedsMutReceiver { method: EcoString },

	/// An `impl A for B` (or nested `impl A { .. }`) restated a method's
	/// `mut func`/`func` kind differently from what interface `A` itself
	/// declares (OO2) — e.g. the interface says `mut func push`, the impl says
	/// plain `func push`. The interface is the source of truth every call-site
	/// gate reads, so a mismatch here would silently desync what the impl body
	/// requires from what callers are checked against. `expected_mut` is the
	/// interface's own declared kind. New variant appended at the enum's end
	/// (mints 2056).
	MethodMutMismatch {
		name: EcoString,
		ty: String,
		expected_mut: bool,
	},

	/// A generic parameter `T: A` was instantiated, across the arguments of one
	/// call, by BOTH a `mut` and a non-`mut` value at positions sharing that
	/// same `T` (OO4) — e.g. `f<T: A>(x: T, y: T)` called `f(mut_b, b)`. No
	/// single type for `T` can be correct for both call sites when `A` is
	/// implemented only for `mut B` (`impl A for mut B` / `impl mut A for B`).
	/// New variant appended at the enum's end (mints 2057).
	MixedMutabilityForBound { interface: EcoString },

	/// A `T: A` bound obligation failed for a plain type `ty`, but the `mut`
	/// version of `ty` WOULD satisfy it (OO4: `A` is implemented only for
	/// `mut ty`, via `impl A for mut ty` / `impl mut A for ty`) — a more
	/// specific diagnostic than [`TypeError::BoundNotSatisfied`], hinting the
	/// fix directly (pass a `mut` value) rather than leaving the caller to
	/// guess. New variant appended at the enum's end (mints 2058).
	BoundSatisfiedOnlyByMut { ty: String, interface: EcoString },

	/// A `for` loop's source implements neither `Iterator` nor `Iterable` (the
	/// only two shapes `infer_iterable_element` accepts once the syntactic-range
	/// and list fast paths are ruled out). Replaces what used to be a silent
	/// `self.fresh()` accept — the loop pattern bound to an unconstrained
	/// inference variable that let the body typecheck against garbage, only to
	/// panic in lowering. New variant appended at the enum's end (mints 2059).
	NotIterable { ty: String },

	/// A `match` over `uint` leaves some values uncovered — the unsigned-domain
	/// counterpart of [`TypeError::NonExhaustiveInt`], worded for `uint` so the
	/// message never claims the gap is in `int` values. New variant appended at the
	/// enum's end (mints 2060).
	NonExhaustiveUInt,

	/// An operator has no implementation for its operand type(s). The user-facing
	/// counterpart of [`TypeError::NotImplemented`] for operator syntax: names the
	/// operator symbol, both operands, and the interface to implement — rather than
	/// leaking the internal desugared method name and only one operand type. `rhs` is
	/// `None` for a unary operator. New variant appended at the enum's end (mints 2061).
	OperatorNotImplemented {
		operator: EcoString,
		interface: EcoString,
		lhs: String,
		rhs: Option<String>,
	},

	/// A positional (unnamed) sub-pattern was used on a constructor that does not have
	/// exactly one field, so there is no single field for it to bind to. `fields` is
	/// the constructor's actual field count. New variant appended at the enum's end
	/// (mints 2062).
	PositionalPatternArity { fields: usize },

	/// Range iteration advances by one, so its bounds must be discrete integer
	/// values. Floating-point and other `Comparable` values do not define that
	/// progression. New variant appended at the enum's end (mints 2063).
	InvalidRangeBound { ty: String },

	/// One destructuring pattern binds the same local more than once.
	DuplicatePatternBinding { name: EcoString },
	/// The alternatives of a union pattern introduce different locals.
	InconsistentUnionBindings,
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
			E::MethodCallsUnsupported => "method calls are not supported yet (Milestone B)".into(),

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
