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
	/// Anonymous closure parameters (`$0`) are not implemented yet.
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

			E::AnonymousParamUnsupported => "anonymous closure parameters are not supported yet".into(),
			E::MethodCallsUnsupported => "method calls are not supported yet (Milestone B)".into(),

			E::NonExhaustiveMatch { witness } => {
				format!("non-exhaustive match: `{witness}` is not covered").into()
			}
			E::NonExhaustiveInt => {
				"non-exhaustive match: some `int` values are not covered — add a `_` arm".into()
			}
			E::NonExhaustiveNeedsWildcard => {
				"non-exhaustive match: add a `_` arm to cover the remaining cases".into()
			}
			E::UnreachableArm => "unreachable match arm".into(),
		}
	}

	fn severity(&self) -> Severity {
		match self {
			TypeError::UnreachableArm => Severity::Warning,
			_ => Severity::Error,
		}
	}

	fn labels(&self) -> Vec<Label> {
		match self {
			TypeError::Redefinition {
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
			_ => None,
		}
	}
}
