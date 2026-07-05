//! The surface *type* grammar — what the programmer writes in annotations. This is
//! distinct from the semantic types the checker infers (those live in `nymph-sema`).

use crate::{Ident, Spanned};

#[derive(Debug, Eq, PartialEq, Hash, Clone, salsa::Update)]
pub enum Type {
	/// `int`
	Int,
	/// `uint`
	UInt,
	/// `float`
	Float,
	/// `char`
	Char,
	/// `string`
	String,
	/// `boolean`
	Boolean,
	/// `void` — equivalent to the empty tuple `#()`.
	Void,
	/// `never` — the type of an expression that diverges.
	Never,
	/// `self` — the type currently being declared or implemented.
	SelfType,
	/// `_` — an inference hole the checker must fill.
	Infer,
	/// `A + B` — the intersection of two interfaces.
	Intersection(Box<Spanned<Self>>, Box<Spanned<Self>>),
	/// `#[A]` — a list.
	List(Box<Spanned<Self>>),
	/// `#(A, B, C)` — a tuple.
	Tuple(Vec<Spanned<Self>>),
	/// `#{ A: B }` — a map.
	Map(Box<Spanned<Self>>, Box<Spanned<Self>>),
	/// `(A, b: B) -> C` — a function type.
	Function {
		params: Vec<FunctionTypeParam>,
		return_type: Box<Spanned<Self>>,
	},
	/// `A<B, C>` — a reference to a named type with optional (possibly labelled)
	/// generic arguments.
	Reference {
		name: Ident,
		generics: Vec<Spanned<GenericArg>>,
	},
	/// `(A)` — a parenthesised type, kept in the tree to preserve precedence intent.
	Grouped(Box<Spanned<Self>>),
}

/// A parameter in a function *type*: an optional label plus the parameter type.
pub type FunctionTypeParam = (Option<Ident>, Spanned<Type>);

/// A single generic argument, optionally labelled (`Output = T`).
#[derive(Debug, PartialEq, Hash, Clone, Eq, salsa::Update)]
pub struct GenericArg {
	pub value: Spanned<Type>,
	pub name: Option<Ident>,
}

/// A declared generic parameter, with an optional interface constraint and default.
#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub struct GenericParam {
	pub name: Ident,
	pub constraint: Option<Spanned<Type>>,
	pub default: Option<Spanned<Type>>,
}
