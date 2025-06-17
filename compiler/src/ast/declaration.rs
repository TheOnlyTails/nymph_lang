use std::collections::HashMap;

use crate::ast::{
	Ident, Spanned,
	expr::{Expr, Pattern},
	types::{GenericArg, GenericParam, Type},
};

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
	pub members: Vec<Declaration>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Declaration {
	/// An `import` declaration imports an external module into the current module,
	/// either from inside the project or from a published package.
	/// An optional `with` clause can be used to only import specific items from the module,
	/// and make them available without a module qualifier, or under different names.
	///
	/// ```nym
	/// import std/math
	/// import std/math with (sin as sine, cos as cosine, tan as tangent)
	/// ```
	Import {
		root: ImportRoot,
		path: Vec<Ident>,
		idents: Option<HashMap<Ident, Option<Ident>>>,
	},
	Let {
		visibility: Option<Visibility>,
		meta: LetDeclaration,
		value: Spanned<Expr>,
	},
	ExternalLet(Option<Visibility>, LetDeclaration),
	Func {
		visibility: Option<Visibility>,
		meta: FuncDeclaration,
		body: Spanned<Expr>,
	},
	ExternalFunc(Option<Visibility>, FuncDeclaration),
	/// Redefines a type with a new name.
	/// ```nym
	/// type VeryVeryNested = #[#(#{#[int]: #(string, float)}, #[boolean)]
	/// type TupleList<K, V> = #[#(K, V)]
	/// ```
	TypeAlias {
		visibility: Option<Visibility>,
		meta: TypeAliasDeclaration,
		value: Spanned<Type>,
	},
	/// An algebraic product type with multiple named fields.
	/// After the fields, you may define variables and functions that can access those fields via the `this` object.
	///
	/// The struct definition may also include a `namespace` block, which includes variables, functions, and type aliases,
	/// available from the type directly rather than an instance of the struct.
	///
	/// You can implement interfaces for the struct by using an `impl` block,
	/// which also marks the struct as a subtype of the implemented interface.
	///
	/// Two instances of an object are considered equal if:
	/// - They are both instances of the same struct.
	/// - The values of each of their fields are the same.
	///
	/// ```nym
	/// struct Person(name: string, age: int) {
	///   let first_name = name.split()[0]
	///   let last_name = name.split()[1]
	///
	///   func is_minor() -> this.age < Person.age_of_majority
	///
	///   namespace {
	///     let age_of_majority = 18
	///   }
	/// }
	/// ```
	Struct {
		visibility: Option<Visibility>,
		name: Ident,
		generics: Vec<Spanned<GenericParam>>,
		fields: Vec<Spanned<StructField>>,
		members: Vec<Spanned<StructInnerMember>>,
	},
	/// An algebraic sum type, containing multiple named variants,
	/// each having an associated constructor and fields.
	///
	/// ```nym
	/// enum Option<T> {
	///   Some(T),
	///   None
	///
	///   func map<R>(f: (T) -> R) -> match this {
	///     Some(it) -> Some(f(it)),
	///     None -> None
	///   }
	/// }
	/// ```
	Enum {
		visibility: Option<Visibility>,
		name: Ident,
		generics: Vec<Spanned<GenericParam>>,
		variants: Vec<Spanned<EnumVariant>>,
		members: Vec<Spanned<StructInnerMember>>,
	},
	Namespace {
		visibility: Option<Visibility>,
		name: Ident,
		members: Vec<Spanned<ImplMember>>,
	},
	Interface {
		visibility: Option<Visibility>,
		mutable: bool,
		name: Ident,
		generics: Vec<Spanned<GenericParam>>,
		super_interfaces: Vec<Spanned<(Ident, Vec<Spanned<GenericArg>>)>>,
		members: Vec<Spanned<InterfaceMember>>,
	},
	/// An `impl` block extends a declaration with custom variables, functions, and types.
	///
	/// ```nym
	/// impl<T> Option<T> {
	/// 	func maybe_print() -> match (this) {
	/// 		Option.Some(value) -> io.println(value),
	/// 		Option.None -> {}
	/// 	}
	/// }
	/// ```
	Impl {
		visibility: Option<Visibility>,
		generics: Vec<Spanned<GenericParam>>,
		mutable: bool,
		type_: Spanned<Type>,
		members: Vec<Spanned<ImplMember>>,
	},
	/// An `impl for` block extends a declaration using an interface.
	/// For example:
	/// ```nym
	/// impl Comparable<Person> for Person {
	///   func compare(other: Person) -> this.age.compare(other.age)
	/// }
	/// ```
	ImplFor {
		visibility: Option<Visibility>,
		generics: Vec<Spanned<GenericParam>>,
		mutable: bool,
		type_: Spanned<Type>,
		for_interface: (Ident, Vec<Spanned<GenericArg>>),
		members: Vec<Spanned<ImplMember>>,
	},
}

#[derive(Debug, Clone, PartialEq)]
pub enum Visibility {
	Public,
	Internal,
	Private,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportRoot {
	PackageRoot,
	Current,
	Parent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeAliasDeclaration {
	pub name: Ident,
	pub generics: Vec<Spanned<GenericParam>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LetDeclaration {
	pub mutable: bool,
	pub name: Spanned<Pattern>,
	pub type_: Option<Spanned<Type>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncDeclaration {
	pub name: Ident,
	pub generics: Vec<Spanned<GenericParam>>,
	pub params: Vec<Spanned<FuncParam>>,
	pub return_type: Option<Spanned<Type>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FuncParam {
	pub name: Spanned<Pattern>,
	pub type_: Spanned<Type>,
	pub default: Option<Spanned<Expr>>,
	pub mutable: bool,
	pub spread: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
	pub visibility: Option<Visibility>,
	pub name: Ident,
	pub type_: Spanned<Type>,
	pub default: Option<Spanned<Expr>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StructInnerMember {
	Member(Spanned<ImplMember>),
	Namespace(Vec<Spanned<ImplMember>>),
	Impl {
		interface: (Ident, Vec<Spanned<GenericArg>>),
		generics: Vec<Spanned<GenericParam>>,
		members: Vec<Spanned<ImplMember>>,
	},
	ImplMut(Vec<Spanned<ImplMember>>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImplMember {
	Let {
		visibility: Option<Visibility>,
		meta: LetDeclaration,
		value: Spanned<Expr>,
	},
	ExternalLet(Option<Visibility>, LetDeclaration),
	Func {
		visibility: Option<Visibility>,
		meta: FuncDeclaration,
		body: Spanned<Expr>,
	},
	ExternalFunc(Option<Visibility>, FuncDeclaration),
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterfaceMember {
	Element(Spanned<InterfaceElement>),
	Namespace(Vec<Spanned<ImplMember>>),
	ImplMut(Vec<Spanned<InterfaceElement>>),
	Impl {
		interface: (Ident, Vec<Spanned<GenericArg>>),
		generics: Vec<Spanned<GenericParam>>,
		members: Vec<Spanned<ImplMember>>,
	},
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterfaceElement {
	Let {
		meta: LetDeclaration,
		value: Option<Spanned<Expr>>,
	},
	Func {
		meta: FuncDeclaration,
		body: Option<Spanned<Expr>>,
	},
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
	pub name: Ident,
	pub fields: Vec<Spanned<StructField>>,
}
