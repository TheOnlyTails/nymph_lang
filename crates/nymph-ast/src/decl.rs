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
	/// `let x = 1`, `public let count: int = 0`
	Let {
		visibility: Option<Visibility>,
		meta: LetDeclaration,
		value: Expr,
	},
	/// `external(js_name) let max_float: float`
	ExternalLet(Option<Visibility>, EcoString, LetDeclaration),
	/// `effect Database`
	Effect {
		visibility: Option<Visibility>,
		name: Ident,
	},
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
	/// A product type. Its body splits into flat instance/namespace [`ImplMember`]s
	/// and nested interface [`StructImpl`]s.
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
		/// Source enum views embedded by `...Source`, or one selected source
		/// variant embedded by `Source.Variant`.
		embeddings: Vec<Spanned<EnumEmbedding>>,
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
	/// An inherent impl: `impl<T> Option<T> { ... }`.
	Impl {
		visibility: Option<Visibility>,
		generics: Vec<Spanned<GenericParam>>,
		type_: Spanned<Type>,
		members: Vec<Spanned<ImplMember>>,
	},
	/// An interface impl: `impl<T> Unwrap<Output = T> for Option<T> { ... }`.
	ImplFor {
		visibility: Option<Visibility>,
		generics: Vec<Spanned<GenericParam>>,
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

/// How a `let` binding is introduced. Managed bindings are local, while
/// namespaced bindings are static.
#[derive(Clone, Copy, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub enum LetKind {
	/// `let x = …` — an immutable per-instance / local binding.
	Instance,
	/// `let use x = …` — an immutable local with lexical cleanup.
	Use,
	/// `namespace let x = …` — an immutable static (type-level) binding.
	Namespace,
}

impl LetDeclaration {
	/// Whether this is a managed `let use` binding.
	pub fn is_managed(&self) -> bool {
		matches!(self.kind, LetKind::Use)
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
	/// Whether this callable constructs a cold task recipe.
	pub is_async: bool,
	pub generics: Vec<Spanned<GenericParam>>,
	pub params: Vec<Spanned<FuncParam>>,
	pub return_type: Option<Spanned<Type>>,
	/// An explicit checked-effect row. With an explicit return type, omission is
	/// pure; when the return type is also omitted, both value and effects infer.
	pub effects: Option<Spanned<crate::ty::EffectRow>>,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum FuncKind {
	Instance,
	Namespace,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct FuncParam {
	pub name: Spanned<Pattern>,
	pub type_: Spanned<Type>,
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

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub struct EnumEmbedding {
	pub source: Ident,
	pub variant: Option<Ident>,
}
