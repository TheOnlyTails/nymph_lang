//! Top-level and member declarations: the things a Nymph module is made of.

use ecow::EcoString;

use crate::{
	Ident, Spanned,
	expr::{Expr, Pattern},
	ty::{GenericArg, GenericParam, Type},
};

/// A parsed source file: an ordered list of declarations plus its module path.
#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub struct Module {
	pub members: Vec<Declaration>,
	pub path: EcoString,
}

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub enum Declaration {
	/// `import @/math`, `import @/math as m`, `import @/math with (sin as sine, cos)`
	Import {
		root: ImportRoot,
		path: Vec<Ident>,
		alias: Option<Ident>,
		idents: Option<Vec<(Ident, Option<Ident>)>>,
	},
	/// `let x = 1`, `public let mut count: int = 0`
	Let {
		visibility: Option<Visibility>,
		meta: LetDeclaration,
		value: Expr,
	},
	/// `external(js_name) let max_float: float`
	ExternalLet(Option<Visibility>, EcoString, LetDeclaration),
	/// `func add(a: int, b: int): int = a + b`
	Func {
		visibility: Option<Visibility>,
		meta: FuncDeclaration,
		body: Expr,
	},
	/// `external(char_at) func char_at(index: int): char`
	ExternalFunc(Option<Visibility>, EcoString, FuncDeclaration),
	/// `type TupleList<K, V> = #[#(K, V)]`
	TypeAlias {
		visibility: Option<Visibility>,
		meta: TypeAliasDeclaration,
		value: Spanned<Type>,
	},
	/// A product type. Its body splits into flat [`ImplMember`]s (instance /
	/// `mut` / `namespace` funcs and lets) and nested interface [`StructImpl`]s.
	Struct {
		visibility: Option<Visibility>,
		name: Ident,
		generics: Vec<Spanned<GenericParam>>,
		fields: Vec<Spanned<StructField>>,
		members: Vec<Spanned<ImplMember>>,
		impls: Vec<Spanned<StructImpl>>,
	},
	/// A sum type.
	Enum {
		visibility: Option<Visibility>,
		name: Ident,
		generics: Vec<Spanned<GenericParam>>,
		variants: Vec<Spanned<EnumVariant>>,
		members: Vec<Spanned<ImplMember>>,
		impls: Vec<Spanned<StructImpl>>,
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

#[derive(Debug, Copy, Eq, Clone, PartialEq, Hash, salsa::SalsaValue)]
pub enum Visibility {
	Public,
	Internal,
	Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
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

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub struct TypeAliasDeclaration {
	pub name: Ident,
	pub generics: Vec<Spanned<GenericParam>>,
}

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub struct LetDeclaration {
	pub kind: LetKind,
	pub name: Spanned<Pattern>,
	pub type_: Option<Spanned<Type>>,
}

/// How a `let` binding is introduced. `Mut` (mutable, `let mut`) and `Namespace`
/// (a static `namespace let`) are mutually exclusive by construction — a
/// namespaced binding can never be mutable, so `namespace let mut` is
/// unrepresentable. Mirrors [`FuncKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub enum LetKind {
	/// `let x = …` — an immutable per-instance / local binding.
	Instance,
	/// `let mut x = …` — a mutable per-instance / local binding.
	Mut,
	/// `namespace let x = …` — an immutable static (type-level) binding.
	Namespace,
}

impl LetDeclaration {
	/// Whether the binding may be reassigned (`let mut`).
	pub fn is_mutable(&self) -> bool {
		matches!(self.kind, LetKind::Mut)
	}

	/// Whether the binding is a static `namespace let`.
	pub fn is_namespaced(&self) -> bool {
		matches!(self.kind, LetKind::Namespace)
	}
}

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub struct FuncDeclaration {
	pub name: Ident,
	pub kind: FuncKind,
	pub generics: Vec<Spanned<GenericParam>>,
	pub params: Vec<Spanned<FuncParam>>,
	pub return_type: Option<Spanned<Type>>,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum FuncKind {
	Instance,
	Mut,
	Namespace,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct FuncParam {
	pub name: Spanned<Pattern>,
	pub type_: Spanned<Type>,
	pub mutable: bool,
	pub spread: bool,
}

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub struct StructField {
	pub visibility: Option<Visibility>,
	pub name: Ident,
	pub type_: Spanned<Type>,
	pub default: Option<Expr>,
}

/// A nested interface impl inside a `struct`/`enum` body:
/// `impl Plus<Other = Self> { func plus(...) = ... }`.
#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub struct StructImpl {
	pub interface: (Ident, Vec<Spanned<GenericArg>>),
	pub generics: Vec<Spanned<GenericParam>>,
	pub members: Vec<Spanned<ImplMember>>,
}

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub enum ImplMember {
	Let {
		visibility: Option<Visibility>,
		meta: LetDeclaration,
		value: Expr,
	},
	ExternalLet(Option<Visibility>, EcoString, LetDeclaration),
	Func {
		visibility: Option<Visibility>,
		meta: FuncDeclaration,
		body: Expr,
	},
	ExternalFunc(Option<Visibility>, EcoString, FuncDeclaration),
}

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub enum InterfaceMember {
	Element(Box<Spanned<InterfaceElement>>),
	Impl {
		interface: (Ident, Vec<Spanned<GenericArg>>),
		generics: Vec<Spanned<GenericParam>>,
		members: Vec<Spanned<ImplMember>>,
	},
}

/// A member of an interface: a `let` or `func` signature, each optionally with a
/// default body/value.
#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub enum InterfaceElement {
	Let {
		meta: LetDeclaration,
		value: Option<Expr>,
	},
	Func {
		meta: FuncDeclaration,
		body: Option<Expr>,
	},
}

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub struct EnumVariant {
	pub name: Ident,
	pub fields: Vec<Spanned<StructField>>,
}
