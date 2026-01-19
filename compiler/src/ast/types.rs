use super::{Ident, Spanned};

#[derive(Debug, Eq, PartialEq, Hash, Clone, salsa::Update)]
pub enum Type {
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
	/// `(A, b: B) -> C`
	Function {
		params: Vec<FunctionTypeParam>,
		return_type: Box<Spanned<Self>>,
	},
	/// `A<B, C>`
	Reference {
		name: Ident,
		generics: Vec<Spanned<GenericArg>>,
	},
	/// `(A)`
	Grouped(Box<Spanned<Self>>),
}

pub type FunctionTypeParam = (Option<Ident>, Spanned<Type>);

#[derive(Debug, PartialEq, Hash, Clone, Eq, salsa::Update)]
pub struct GenericArg {
	pub value: Spanned<Type>,
	pub name: Option<Ident>,
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub struct GenericParam {
	pub name: Ident,
	pub constraint: Option<Spanned<Type>>,
	pub default: Option<Spanned<Type>>,
}
