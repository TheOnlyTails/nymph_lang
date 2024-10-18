use super::{Ident, Spanned};
use crate::ast::expr::Pattern;
use std::collections::BTreeSet;

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub(crate) enum Type {
	// Type declarations
	/// `int`
	Int,
	/// `float`
	Float,
	/// `char`
	Char,
	/// `string`
	String,
	/// `boolean`
	Boolean,
	/// `void`
	Void,
	/// `never`
	Never,
	/// `self`
	Self_,
	/// `_`
	Infer,
	/// `A + B`
	Intersection(Box<Spanned<Self>>, Box<Spanned<Self>>),
	/// `#[A]`
	List(Box<Spanned<Self>>),
	/// `#(A, B, C)`
	Tuple(Vec<Spanned<Self>>),
	/// `#{ A: B }`
	Map(Box<Spanned<Self>>, Box<Spanned<Self>>),
	/// `(A, B) -> C`
	Function {
		params: Vec<Spanned<Self>>,
		return_type: Box<Spanned<Self>>,
	},
	/// `A<B>`
	Reference {
		name: Ident,
		generics: Vec<Spanned<GenericArg>>,
	},
	/// `int is 1..=6`, `#[T] is #[_, ...]`
	Pattern(Box<Spanned<Self>>, Spanned<Pattern>),
	/// `int !is 0`
	NotPattern(Box<Spanned<Self>>, Spanned<Pattern>),
	/// `(A)`
	Grouped(Box<Spanned<Self>>),

	// Internal types for resolving into
	Struct {
		members: BTreeSet<StructTypeMember>,
		impls: Vec<StructImplId>,
	},
	Generic {
		constraint: Option<Box<Self>>,
		default: Option<Box<Self>>,
	},
}

impl Type {
	pub(crate) fn simplify(&self) -> Self {
		match self {
			Type::Intersection(left, right) => {
				if left.0 == right.0 {
					left.0.simplify()
				} else {
					Type::Intersection(
						left.map(Type::simplify).into(),
						right.map(Type::simplify).into(),
					)
				}
			}
			Type::List(item) => Type::List(item.map(Type::simplify).into()),
			Type::Tuple(members) => {
				Type::Tuple(members.iter().map(|it| it.map(Type::simplify)).collect())
			}
			Type::Map(key, value) => Type::Map(
				key.map(Type::simplify).into(),
				value.map(Type::simplify).into(),
			),
			Type::Function {
				params,
				return_type,
			} => Type::Function {
				params: params.iter().map(|p| p.map(Type::simplify)).collect(),
				return_type: return_type.map(Type::simplify).into(),
			},
			Type::Generic {
				constraint,
				default,
			} => Type::Generic {
				constraint: constraint.as_ref().map(|it| it.simplify().into()),
				default: default.as_ref().map(|it| it.simplify().into()),
			},
			Type::Grouped(value) => value.0.simplify(),
			_ => self.clone(),
		}
	}

	pub(crate) fn assignable_to(&self, to: &Type) -> bool {
		match (self, to) {
			(a, b) if a == b => true,
			(Type::Int, Type::Float) | (_, Type::Infer) => true,
			(Type::Grouped(a), b) => a.0.assignable_to(b),
			(a, Type::Grouped(b)) => a.assignable_to(&b.0),
			(Type::Generic { constraint, .. }, other) => constraint
				.as_ref()
				.map(|it| it.assignable_to(other))
				.unwrap_or(false),
			_ => false,
		}
	}
}

pub(crate) type StructTypeMember = (Ident, Type);

#[derive(Debug, PartialEq, Hash, Clone, Eq)]
pub(crate) struct StructImplId {
	name: Ident,
	generics: Vec<GenericArg>,
}

#[derive(Debug, PartialEq, Hash, Clone, Eq)]
pub(crate) struct GenericArg {
	pub(crate) value: Spanned<Type>,
	pub(crate) name: Option<Ident>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GenericParam {
	pub(crate) name: Ident,
	pub(crate) constraint: Option<Spanned<Type>>,
	pub(crate) default: Option<Spanned<Type>>,
}
