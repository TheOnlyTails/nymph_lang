use std::{fmt::Display, ops::Range};

use ecow::EcoString;

use crate::{ast::expr::Pattern, types::Type};

#[derive(Debug, Clone)]
pub enum TypeError {
	UnknownIdentifier(EcoString, Range<usize>),
	UnknownType(EcoString, Range<usize>),
	TypeMismatch {
		expected: Box<Type>,
		found: Box<Type>,
		span: Range<usize>,
	},
	NotCallable(Range<usize>),
	NotIndexable(Range<usize>),
	NotAccessible(Range<usize>),
	UnknownMember {
		type_: Box<Type>,
		member: EcoString,
		span: Range<usize>,
	},
	SpreadNonFinalParam(Range<usize>),
	SelfTypeInGlobalScope(Range<usize>),
	ThisOutsideStruct(Range<usize>),
	InvalidUnaryOp(Range<usize>),
	InvalidBinaryOp(Range<usize>),
	InfiniteTypeInstantiation {
		var: EcoString,
		ty: Box<Type>,
		span: Range<usize>,
	},
	PatternTypeMismatch {
		pattern: Pattern,
		scrutinee: Box<Type>,
		span: Range<usize>,
	},
	RestPatternNotAtEnd {
		pattern: Pattern,
		span: Range<usize>,
	},
	TuplePatternTooLong {
		pattern: Pattern,
		tuple_items: Vec<Type>,
		span: Range<usize>,
	},
	DuplicatePatternIdentifier {
		pattern: Pattern,
		identifier: EcoString,
		span: Range<usize>,
	},
	NonConstantMapPatternKey {
		key_pattern: Pattern,
		pattern: Pattern,
		span: Range<usize>,
	},
	ConflictingUnionPatternIdentifiers {
		identifier: EcoString,
		first_type: Box<Type>,
		second_type: Box<Type>,
		span: Range<usize>,
	},
	UnresolvedPath {
		path: Vec<EcoString>,
		index: usize,
		span: Range<usize>,
	},
	GenericArgumentMismatch {
		expected: usize,
		found: usize,
		span: Range<usize>,
	},
	ConstraintViolation {
		type_: Box<Type>,
		constraint: Box<Type>,
		span: Range<usize>,
	},
	ImplNotFound {
		type_: Box<Type>,
		interface: Box<Type>,
		span: Range<usize>,
	},
	IncompatibleImplMember {
		member: EcoString,
		expected: Box<Type>,
		found: Box<Type>,
		span: Range<usize>,
	},
	UnificationFailed {
		type1: Box<Type>,
		type2: Box<Type>,
		span: Range<usize>,
	},
	ExternalDependencyNotSupported {
		package: EcoString,
		span: Range<usize>,
	},
	ProjectRootNotFound {
		searched_from: EcoString,
		span: Range<usize>,
	},
	ModuleNotFound {
		path: EcoString,
		span: Range<usize>,
	},
	AmbiguousModule {
		path: EcoString,
		file_path: EcoString,
		dir_path: EcoString,
		span: Range<usize>,
	},
	ImportedItemNotFound {
		item: EcoString,
		module: EcoString,
		span: Range<usize>,
	},
	ModuleParseError {
		path: EcoString,
		message: EcoString,
		span: Range<usize>,
	},
	ExternalDeclarationMissingType(Range<usize>),
	ModuleTypeError {
		module_path: EcoString,
		error: Box<TypeError>,
	},
}

impl TypeError {
	/// Get the span associated with this error
	pub fn span(&self) -> Range<usize> {
		match self {
			TypeError::UnknownIdentifier(_, span) => span.clone(),
			TypeError::UnknownType(_, span) => span.clone(),
			TypeError::TypeMismatch { span, .. } => span.clone(),
			TypeError::NotCallable(span) => span.clone(),
			TypeError::NotIndexable(span) => span.clone(),
			TypeError::NotAccessible(span) => span.clone(),
			TypeError::UnknownMember { span, .. } => span.clone(),
			TypeError::SpreadNonFinalParam(span) => span.clone(),
			TypeError::SelfTypeInGlobalScope(span) => span.clone(),
			TypeError::ThisOutsideStruct(span) => span.clone(),
			TypeError::InvalidUnaryOp(span) => span.clone(),
			TypeError::InvalidBinaryOp(span) => span.clone(),
			TypeError::InfiniteTypeInstantiation { span, .. } => span.clone(),
			TypeError::PatternTypeMismatch { span, .. } => span.clone(),
			TypeError::RestPatternNotAtEnd { span, .. } => span.clone(),
			TypeError::TuplePatternTooLong { span, .. } => span.clone(),
			TypeError::DuplicatePatternIdentifier { span, .. } => span.clone(),
			TypeError::NonConstantMapPatternKey { span, .. } => span.clone(),
			TypeError::ConflictingUnionPatternIdentifiers { span, .. } => span.clone(),
			TypeError::UnresolvedPath { span, .. } => span.clone(),
			TypeError::GenericArgumentMismatch { span, .. } => span.clone(),
			TypeError::ConstraintViolation { span, .. } => span.clone(),
			TypeError::ImplNotFound { span, .. } => span.clone(),
			TypeError::IncompatibleImplMember { span, .. } => span.clone(),
			TypeError::UnificationFailed { span, .. } => span.clone(),
			TypeError::ExternalDependencyNotSupported { span, .. } => span.clone(),
			TypeError::ProjectRootNotFound { span, .. } => span.clone(),
			TypeError::ModuleNotFound { span, .. } => span.clone(),
			TypeError::AmbiguousModule { span, .. } => span.clone(),
			TypeError::ImportedItemNotFound { span, .. } => span.clone(),
			TypeError::ModuleParseError { span, .. } => span.clone(),
			TypeError::ExternalDeclarationMissingType(span) => span.clone(),
			TypeError::ModuleTypeError { error, .. } => error.span(),
		}
	}

	/// Get the file path associated with this error, if it originated from a different module
	pub fn file_path(&self) -> Option<EcoString> {
		match self {
			TypeError::ModuleTypeError { module_path, error } => {
				error.file_path().or_else(|| Some(module_path.clone()))
			}
			_ => None,
		}
	}
}

impl Display for TypeError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			TypeError::UnknownIdentifier(id, _) => write!(f, "Unknown identifier: {}", id),
			TypeError::UnknownType(id, _) => write!(f, "Unknown type: {}", id),
			TypeError::TypeMismatch {
				expected, found, ..
			} => {
				write!(f, "Type mismatch: expected {}, found {}", expected, found)
			}
			TypeError::NotCallable(_) => write!(f, "Cannot call non-function type"),
			TypeError::NotIndexable(_) => write!(f, "Cannot index non-indexable type"),
			TypeError::NotAccessible(_) => write!(f, "Cannot access members of non-accessible type"),
			TypeError::UnknownMember {
				type_: _, member, ..
			} => {
				write!(f, "Unknown member '{}' in type", member)
			}
			TypeError::SpreadNonFinalParam(_) => {
				write!(f, "Spread operator can only be used on the final parameter")
			}
			TypeError::SelfTypeInGlobalScope(_) => {
				write!(f, "`self` type cannot be used in global scope")
			}
			TypeError::ThisOutsideStruct(_) => {
				write!(f, "`this` can only be used inside struct/enum members")
			}
			TypeError::InvalidUnaryOp(_) => write!(f, "Invalid unary operation"),
			TypeError::InvalidBinaryOp(_) => write!(f, "Invalid binary operation"),
			TypeError::InfiniteTypeInstantiation { var, ty, .. } => {
				write!(
					f,
					"Infinite type instantiation: variable {} appears in its own bound: {}",
					var, ty
				)
			}
			TypeError::PatternTypeMismatch {
				pattern: _,
				scrutinee,
				..
			} => {
				write!(
					f,
					"Pattern type mismatch: pattern does not match scrutinee type {}",
					scrutinee
				)
			}
			TypeError::RestPatternNotAtEnd { .. } => write!(f, "Rest pattern must be at the end"),
			TypeError::TuplePatternTooLong { .. } => {
				write!(f, "Tuple pattern is too long for the tuple type")
			}
			TypeError::DuplicatePatternIdentifier { identifier, .. } => {
				write!(f, "Duplicate pattern identifier: {}", identifier)
			}
			TypeError::NonConstantMapPatternKey { .. } => {
				write!(f, "Non-constant key in map pattern")
			}
			TypeError::ConflictingUnionPatternIdentifiers { identifier, .. } => {
				write!(
					f,
					"Conflicting union pattern identifiers for {}",
					identifier
				)
			}
			TypeError::UnresolvedPath { path, index, .. } => {
				write!(f, "Unresolved path at index {}: {:?}", index, path)
			}
			TypeError::GenericArgumentMismatch {
				expected, found, ..
			} => {
				write!(
					f,
					"Generic argument mismatch: expected {} arguments, found {}",
					expected, found
				)
			}
			TypeError::ConstraintViolation {
				type_, constraint, ..
			} => {
				write!(f, "Type {} violates constraint {}", type_, constraint)
			}
			TypeError::ImplNotFound {
				type_, interface, ..
			} => {
				write!(
					f,
					"Type {} does not implement interface {}",
					type_, interface
				)
			}
			TypeError::IncompatibleImplMember {
				member,
				expected,
				found,
				..
			} => {
				write!(
					f,
					"Impl member '{}' has incompatible type: expected {}, found {}",
					member, expected, found
				)
			}
			TypeError::UnificationFailed { type1, type2, .. } => {
				write!(f, "Cannot unify types {} and {}", type1, type2)
			}
			TypeError::ExternalDependencyNotSupported { package, .. } => {
				write!(
					f,
					"External dependencies are not yet supported: '{}'",
					package
				)
			}
			TypeError::ProjectRootNotFound { searched_from, .. } => {
				write!(
					f,
					"Could not find project root (nymph.toml) searching from '{}'",
					searched_from
				)
			}
			TypeError::ModuleNotFound { path, .. } => {
				write!(f, "Module not found: '{}'", path)
			}
			TypeError::AmbiguousModule {
				path,
				file_path,
				dir_path,
				..
			} => {
				write!(
					f,
					"Ambiguous module '{}': both '{}' and '{}' exist",
					path, file_path, dir_path
				)
			}
			TypeError::ImportedItemNotFound { item, module, .. } => {
				write!(f, "Item '{}' not found in module '{}'", item, module)
			}
			TypeError::ModuleParseError { path, message, .. } => {
				write!(f, "Error parsing module '{}': {}", path, message)
			}
			TypeError::ExternalDeclarationMissingType(_) => {
				write!(
					f,
					"External declarations require an explicit type annotation"
				)
			}
			TypeError::ModuleTypeError { error, .. } => write!(f, "{error}"),
		}
	}
}
