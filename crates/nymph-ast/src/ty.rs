//! The surface *type* grammar — what the programmer writes in annotations. This is
//! distinct from the semantic types the checker infers (those live in `nymph-sema`).

use crate::{Ident, Spanned};

#[derive(Debug, Eq, PartialEq, Hash, Clone, salsa::SalsaValue)]
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
		/// The callable's checked effects. Omission means the pure row.
		effects: Option<Spanned<EffectRow>>,
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

/// A checked-effect atom after its leading `!`.
#[derive(Debug, PartialEq, Hash, Clone, Eq, salsa::SalsaValue)]
pub enum Effect {
	/// `!Name`; semantic analysis decides whether `Name` is a nominal effect or
	/// an in-scope effect parameter.
	Named(Ident),
	/// `!_`, requesting the least row inferred from the available body/context.
	Infer,
	/// Parser recovery for a malformed atom. This can reach recovered facts but
	/// never complete semantic facts or lowering.
	Error,
}

/// `!()`, `!Database + !Network`, `!E`, or a known row plus `!_`.
#[derive(Debug, Default, PartialEq, Hash, Clone, Eq, salsa::SalsaValue)]
pub struct EffectRow {
	pub effects: Vec<Spanned<Effect>>,
}

impl EffectRow {
	pub fn requests_inference(&self) -> bool {
		self.effects.iter().any(|effect| effect.0 == Effect::Infer)
	}

	pub fn contains_error(&self) -> bool {
		self.effects.iter().any(|effect| effect.0 == Effect::Error)
	}
}

/// The value of a generic argument.
#[derive(Debug, PartialEq, Hash, Clone, Eq, salsa::SalsaValue)]
pub enum GenericArgValue {
	Type(Spanned<Type>),
	Effect(Spanned<EffectRow>),
}

impl GenericArgValue {
	pub fn as_type(&self) -> Option<&Spanned<Type>> {
		match self {
			Self::Type(ty) => Some(ty),
			Self::Effect(_) => None,
		}
	}

	pub fn as_effect(&self) -> Option<&Spanned<EffectRow>> {
		match self {
			Self::Type(_) => None,
			Self::Effect(effect) => Some(effect),
		}
	}
}

/// A single generic argument, optionally labelled (`Output = T`, `E = !Io`).
#[derive(Debug, PartialEq, Hash, Clone, Eq, salsa::SalsaValue)]
pub struct GenericArg {
	pub value: GenericArgValue,
	pub name: Option<Ident>,
}

/// A declared generic parameter, with an optional interface constraint and default.
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct GenericParam {
	pub name: Ident,
	pub kind: GenericParamKind,
	pub constraint: Option<Spanned<Type>>,
	pub default: Option<Spanned<Type>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum GenericParamKind {
	Type,
	Effect,
}
