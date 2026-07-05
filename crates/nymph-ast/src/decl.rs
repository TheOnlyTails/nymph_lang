//! Top-level and member declarations: the things a Nymph module is made of.

use ecow::EcoString;

use crate::{
	Ident, Spanned,
	expr::{Expr, Pattern},
	ty::{GenericArg, GenericParam, Type},
};

/// A parsed source file: an ordered list of declarations plus its module path.
#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct Module {
	pub members: Vec<Declaration>,
	pub path: EcoString,
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub enum Declaration {
	/// `import @/math`, `import @/math with (sin as sine, cos)`
	Import {
		root: ImportRoot,
		path: Vec<Ident>,
		idents: Option<Vec<(Ident, Option<Ident>)>>,
	},
	/// `let x = 1`, `public let mut count: int = 0`
	Let {
		visibility: Option<Visibility>,
		meta: LetDeclaration,
		value: Spanned<Expr>,
	},
	/// `external(js_name) let max_float: float`
	ExternalLet(Option<Visibility>, EcoString, LetDeclaration),
	/// `func add(a: int, b: int): int = a + b`
	Func {
		visibility: Option<Visibility>,
		meta: FuncDeclaration,
		body: Spanned<Expr>,
	},
	/// `external(char_at) func char_at(index: int): char`
	ExternalFunc(Option<Visibility>, EcoString, FuncDeclaration),
	/// `type TupleList<K, V> = #[#(K, V)]`
	TypeAlias {
		visibility: Option<Visibility>,
		meta: TypeAliasDeclaration,
		value: Spanned<Type>,
	},
	/// A product type. See [`StructInnerMember`] for the body forms.
	Struct {
		visibility: Option<Visibility>,
		name: Ident,
		generics: Vec<Spanned<GenericParam>>,
		fields: Vec<Spanned<StructField>>,
		members: Vec<Spanned<StructInnerMember>>,
	},
	/// A sum type.
	Enum {
		visibility: Option<Visibility>,
		name: Ident,
		generics: Vec<Spanned<GenericParam>>,
		variants: Vec<Spanned<EnumVariant>>,
		members: Vec<Spanned<StructInnerMember>>,
	},
	/// A `namespace` of type-level (static) members.
	Namespace {
		visibility: Option<Visibility>,
		name: Ident,
		members: Vec<Spanned<ImplMember>>,
	},
	/// An interface (trait), possibly with super-interfaces and default members.
	Interface {
		visibility: Option<Visibility>,
		name: Ident,
		generics: Vec<Spanned<GenericParam>>,
		super_interfaces: Vec<Spanned<(Ident, Vec<Spanned<GenericArg>>)>>,
		members: Vec<Spanned<InterfaceMember>>,
	},
	/// An inherent impl: `impl<T> Option<T> { ... }`, `impl<T> mut #[T] { ... }`.
	Impl {
		visibility: Option<Visibility>,
		generics: Vec<Spanned<GenericParam>>,
		mutable: bool,
		type_: Spanned<Type>,
		members: Vec<Spanned<ImplMember>>,
	},
	/// An interface impl: `impl<T> Unwrap<Output = T> for Option<T> { ... }`.
	ImplFor {
		visibility: Option<Visibility>,
		generics: Vec<Spanned<GenericParam>>,
		mutable: bool,
		type_: Spanned<Type>,
		for_interface: (Ident, Vec<Spanned<GenericArg>>),
		members: Vec<Spanned<ImplMember>>,
	},
}

#[derive(Debug, Copy, Eq, Clone, PartialEq, Hash, salsa::Update)]
pub enum Visibility {
	Public,
	Internal,
	Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub enum ImportRoot {
	/// `pkg/...` — a published package by name.
	Package(Ident),
	/// `@/...` — the project root.
	Project,
	/// `./...` — the current directory.
	Current,
	/// `../...` — the parent directory.
	Parent,
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct TypeAliasDeclaration {
	pub name: Ident,
	pub generics: Vec<Spanned<GenericParam>>,
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct LetDeclaration {
	pub mutable: bool,
	pub name: Spanned<Pattern>,
	pub type_: Option<Spanned<Type>>,
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct FuncDeclaration {
	pub name: Ident,
	pub generics: Vec<Spanned<GenericParam>>,
	pub params: Vec<Spanned<FuncParam>>,
	pub return_type: Option<Spanned<Type>>,
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub struct FuncParam {
	pub name: Spanned<Pattern>,
	pub type_: Spanned<Type>,
	pub mutable: bool,
	pub spread: bool,
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct StructField {
	pub visibility: Option<Visibility>,
	pub name: Ident,
	pub type_: Spanned<Type>,
	pub default: Option<Spanned<Expr>>,
}

/// The forms that can appear inside a `struct`/`enum` body.
#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub enum StructInnerMember {
	/// A derived `let` or instance `func`.
	Member(Box<Spanned<ImplMember>>),
	/// A `namespace { ... }` block of static members.
	Namespace(Vec<Spanned<ImplMember>>),
	/// A nested interface impl: `impl Plus<...> { ... }`.
	Impl {
		interface: (Ident, Vec<Spanned<GenericArg>>),
		generics: Vec<Spanned<GenericParam>>,
		members: Vec<Spanned<ImplMember>>,
	},
	/// `impl mut { ... }` — methods that mutate the receiver.
	ImplMut(Vec<Spanned<ImplMember>>),
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub enum ImplMember {
	Let {
		visibility: Option<Visibility>,
		meta: LetDeclaration,
		value: Spanned<Expr>,
	},
	ExternalLet(Option<Visibility>, EcoString, LetDeclaration),
	Func {
		visibility: Option<Visibility>,
		meta: FuncDeclaration,
		body: Spanned<Expr>,
	},
	ExternalFunc(Option<Visibility>, EcoString, FuncDeclaration),
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub enum InterfaceMember {
	Element(Box<Spanned<InterfaceElement>>),
	Namespace(Vec<Spanned<ImplMember>>),
	ImplMut(Vec<Spanned<InterfaceElement>>),
	Impl {
		interface: (Ident, Vec<Spanned<GenericArg>>),
		generics: Vec<Spanned<GenericParam>>,
		members: Vec<Spanned<ImplMember>>,
	},
}

/// A member of an interface: a `let` or `func` signature, each optionally with a
/// default body/value.
#[derive(Debug, Clone, PartialEq, salsa::Update)]
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

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub struct EnumVariant {
	pub name: Ident,
	pub fields: Vec<Spanned<StructField>>,
}
