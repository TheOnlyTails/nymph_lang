use std::{fmt::Display, ops::Range};

use ecow::EcoString;

use crate::{ast::expr::Pattern, types::Type};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AmbiguousMemberCandidate {
	pub interface: Box<Type>,
	pub span: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeError {
	pub kind: TypeErrorKind,
	pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeErrorKind {
	UnknownIdentifier {
		name: EcoString,
		suggestion: Option<EcoString>,
	},
	UnknownType {
		name: EcoString,
		suggestion: Option<EcoString>,
	},
	TypeMismatch {
		expected: Box<Type>,
		found: Box<Type>,
	},
	NotCallable(Box<Type>),
	EmptyStruct {
		name: EcoString,
	},
	NotIndexable,
	NotAccessible,
	UnknownMember {
		type_: Box<Type>,
		member: EcoString,
		suggestion: Option<EcoString>,
	},
	AmbiguousMemberAccess {
		type_: Box<Type>,
		member: EcoString,
		candidates: Vec<AmbiguousMemberCandidate>,
	},
	SpreadNonFinalParam,
	SelfTypeInGlobalScope,
	ThisOutsideStruct,
	InvalidUnaryOp,
	InvalidBinaryOp,
	InfiniteTypeInstantiation {
		var: EcoString,
		ty: Box<Type>,
	},
	PatternTypeMismatch {
		pattern: Pattern,
		scrutinee: Box<Type>,
	},
	RestPatternNotAtEnd {
		pattern: Pattern,
	},
	TuplePatternTooLong {
		pattern: Pattern,
		tuple_items: Vec<Type>,
	},
	DuplicatePatternIdentifier {
		pattern: Pattern,
		identifier: EcoString,
	},
	NonConstantMapPatternKey {
		key_pattern: Pattern,
		pattern: Pattern,
	},
	ConflictingUnionPatternIdentifiers {
		identifier: EcoString,
		first_type: Box<Type>,
		second_type: Box<Type>,
	},
	UnresolvedPath {
		path: Vec<EcoString>,
		index: usize,
	},
	GenericArgumentMismatch {
		expected: usize,
		found: usize,
	},
	ConstraintViolation {
		type_: Box<Type>,
		constraint: Box<Type>,
	},
	ImplNotFound {
		type_: Box<Type>,
		interface: Box<Type>,
	},
	IncompatibleImplMember {
		member: EcoString,
		expected: Box<Type>,
		found: Box<Type>,
	},
	MissingImplMembers {
		type_: Box<Type>,
		interface: Box<Type>,
		members: Vec<EcoString>,
	},
	ConflictingImpls {
		receiver: Box<Type>,
		interface: Box<Type>,
		first_span: Range<usize>,
		second_span: Range<usize>,
	},
	UnificationFailed {
		type1: Box<Type>,
		type2: Box<Type>,
	},
	ExternalDependencyNotSupported {
		package: EcoString,
	},
	ProjectRootNotFound {
		searched_from: EcoString,
	},
	ModuleNotFound {
		path: EcoString,
	},
	AmbiguousModule {
		path: EcoString,
		file_path: EcoString,
		dir_path: EcoString,
	},
	ImportedItemNotFound {
		item: EcoString,
		module: EcoString,
		suggestion: Option<EcoString>,
	},
	ModuleParseError {
		module_path: EcoString,
		message: EcoString,
	},
	UnknownNamedArgument {
		name: EcoString,
		suggestion: Option<EcoString>,
	},
	CannotInferAnonymousFunction {
		placeholders: Vec<Option<u32>>,
	},
	ExternalDeclarationMissingType,
	ModuleTypeError {
		module_path: EcoString,
		error: Box<TypeError>,
	},
}

impl TypeError {
	/// Get the file path associated with this error, if it originated from a different module
	pub fn file_path(&self) -> Option<EcoString> {
		match &self.kind {
			TypeErrorKind::ModuleParseError { module_path, .. } => Some(module_path.clone()),
			TypeErrorKind::ModuleTypeError { module_path, error } => {
				error.file_path().or_else(|| Some(module_path.clone()))
			}
			_ => None,
		}
	}
}

impl Display for TypeError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.kind {
			TypeErrorKind::UnknownIdentifier { name, .. } => write!(f, "Unknown identifier: {}", name),
			TypeErrorKind::UnknownType { name, .. } => write!(f, "Unknown type: {}", name),
			TypeErrorKind::TypeMismatch { expected, found } => {
				write!(f, "Type mismatch: expected {}, found {}", expected, found)
			}
			TypeErrorKind::NotCallable(type_) => {
				write!(f, "Cannot call non-function type `{}`", type_)
			}
			TypeErrorKind::EmptyStruct { name } => {
				write!(
					f,
					"Struct `{}` has no fields; use a `namespace` instead",
					name
				)
			}
			TypeErrorKind::NotIndexable => write!(f, "Cannot index non-indexable type"),
			TypeErrorKind::NotAccessible => write!(f, "Cannot access members of non-accessible type"),
			TypeErrorKind::UnknownMember {
				type_: _, member, ..
			} => {
				write!(f, "Unknown member '{}' in type", member)
			}
			TypeErrorKind::AmbiguousMemberAccess {
				type_,
				member,
				candidates,
			} => {
				write!(
					f,
					"Multiple interface members named '{}' are available for type {} ({})",
					member,
					type_,
					candidates
						.iter()
						.map(|candidate| candidate.interface.to_string())
						.collect::<Vec<_>>()
						.join(", "),
				)
			}
			TypeErrorKind::SpreadNonFinalParam => {
				write!(f, "Spread operator can only be used on the final parameter")
			}
			TypeErrorKind::SelfTypeInGlobalScope => {
				write!(f, "`self` type cannot be used in global scope")
			}
			TypeErrorKind::ThisOutsideStruct => {
				write!(f, "`this` can only be used inside struct/enum members")
			}
			TypeErrorKind::InvalidUnaryOp => write!(f, "Invalid unary operation"),
			TypeErrorKind::InvalidBinaryOp => write!(f, "Invalid binary operation"),
			TypeErrorKind::InfiniteTypeInstantiation { var, ty } => {
				write!(
					f,
					"Infinite type instantiation: variable {} appears in its own bound: {}",
					var, ty
				)
			}
			TypeErrorKind::PatternTypeMismatch {
				pattern: _,
				scrutinee,
			} => {
				write!(
					f,
					"Pattern type mismatch: pattern does not match scrutinee type {}",
					scrutinee
				)
			}
			TypeErrorKind::RestPatternNotAtEnd { .. } => write!(f, "Rest pattern must be at the end"),
			TypeErrorKind::TuplePatternTooLong { .. } => {
				write!(f, "Tuple pattern is too long for the tuple type")
			}
			TypeErrorKind::DuplicatePatternIdentifier { identifier, .. } => {
				write!(f, "Duplicate pattern identifier: {}", identifier)
			}
			TypeErrorKind::NonConstantMapPatternKey { .. } => {
				write!(f, "Non-constant key in map pattern")
			}
			TypeErrorKind::ConflictingUnionPatternIdentifiers { identifier, .. } => {
				write!(
					f,
					"Conflicting union pattern identifiers for {}",
					identifier
				)
			}
			TypeErrorKind::UnresolvedPath { path, index } => {
				write!(f, "Unresolved path at index {}: {:?}", index, path)
			}
			TypeErrorKind::GenericArgumentMismatch {
				expected, found, ..
			} => {
				write!(
					f,
					"Generic argument mismatch: expected {} arguments, found {}",
					expected, found
				)
			}
			TypeErrorKind::ConstraintViolation {
				type_, constraint, ..
			} => {
				write!(f, "Type {} violates constraint {}", type_, constraint)
			}
			TypeErrorKind::ImplNotFound {
				type_, interface, ..
			} => {
				write!(
					f,
					"Type {} does not implement interface {}",
					type_, interface
				)
			}
			TypeErrorKind::IncompatibleImplMember {
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
			TypeErrorKind::MissingImplMembers {
				type_,
				interface,
				members,
			} => write!(
				f,
				"Type {} is missing required members for interface {}: {}",
				type_,
				interface,
				members.join(", ")
			),
			TypeErrorKind::ConflictingImpls {
				receiver,
				interface,
				..
			} => write!(
				f,
				"Conflicting implementations of interface {} for type {}",
				interface, receiver
			),
			TypeErrorKind::UnificationFailed { type1, type2 } => {
				write!(f, "Cannot unify types {} and {}", type1, type2)
			}
			TypeErrorKind::ExternalDependencyNotSupported { package } => {
				write!(
					f,
					"External dependencies are not yet supported: '{}'",
					package
				)
			}
			TypeErrorKind::ProjectRootNotFound { searched_from } => {
				write!(
					f,
					"Could not find project root (nymph.toml) searching from '{}'",
					searched_from
				)
			}
			TypeErrorKind::ModuleNotFound { path } => {
				write!(f, "Module not found: '{}'", path)
			}
			TypeErrorKind::AmbiguousModule {
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
			TypeErrorKind::ImportedItemNotFound { item, module, .. } => {
				write!(f, "Item '{}' not found in module '{}'", item, module)
			}
			TypeErrorKind::ModuleParseError { message, .. } => {
				write!(f, "{}", message)
			}
			TypeErrorKind::UnknownNamedArgument { name, .. } => {
				write!(f, "Unknown named argument '{}'", name)
			}
			TypeErrorKind::CannotInferAnonymousFunction { placeholders } => {
				let placeholders = placeholders
					.iter()
					.map(|index| match index {
						None => "$".to_string(),
						Some(index) => format!("${index}"),
					})
					.collect::<Vec<_>>()
					.join(", ");
				write!(
					f,
					"Cannot infer types for anonymous function parameters {placeholders}"
				)
			}
			TypeErrorKind::ExternalDeclarationMissingType => {
				write!(
					f,
					"External declarations require an explicit type annotation"
				)
			}
			TypeErrorKind::ModuleTypeError { error, .. } => write!(f, "{error}"),
		}
	}
}
