use super::{
	Ident, Spanned,
	declaration::LetDeclaration,
	ops::{AssignOperator, BinaryOperator, PostfixOperator, PrefixOperator, TypeOperator},
	types::{GenericArg, GenericParam, Type},
};
use crate::ast::ops::PatternOperator;
use ecow::EcoString;
use ordered_float::OrderedFloat;
use strum::Display;

#[derive(Clone, PartialEq, Debug, salsa::Update)]
pub enum Statement {
	Expr(Spanned<Expr>),
	Let {
		meta: LetDeclaration,
		value: Spanned<Expr>,
	},
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub enum Expr {
	/// `1`, `0b010001`, `0xDEADF00D`
	Int(Spanned<u64>),
	/// `0.1`, 2f`, `6.02e23`
	Float(Spanned<OrderedFloat<f64>>),
	/// `'a'`, `'\n'`, `'\u0A'`
	Char(Spanned<char>),
	/// `"Hello, world!"`, `"The value is ${value}"`
	String(Vec<Spanned<StringPart>>),
	/// `true`, `false`
	Boolean(Spanned<bool>),
	/// `a`, `$meta`, `_unused`
	Identifier(Ident),
	/// `#[]`, `#[1, 2, 3, ...rest]`
	List(Vec<Spanned<ListItem>>),
	/// `#()`, `#(1, true, 'a', ...other)`
	Tuple(Vec<Spanned<ListItem>>),
	/// `#{'a': 1, 'b': 2, ...c_to_z}`
	Map(Vec<Spanned<MapEntry>>),
	Range(RangeKind),
	Call {
		func: Box<Spanned<Self>>,
		generics: Vec<Spanned<GenericArg>>,
		args: Vec<Spanned<CallArg>>,
	},
	MemberAccess {
		parent: Box<Spanned<Self>>,
		member: Ident,
		/// the `?.` operator
		optional: bool,
	},
	IndexAccess {
		parent: Box<Spanned<Self>>,
		index: Box<Spanned<Self>>,
		/// access via the `?.[i]` operator
		optional: bool,
	},
	Closure {
		params: Vec<Spanned<ClosureParam>>,
		generics: Vec<Spanned<GenericParam>>,
		return_type: Option<Spanned<Type>>,
		body: Box<Spanned<Self>>,
	},
	PrefixOp {
		op: PrefixOperator,
		value: Box<Spanned<Self>>,
	},
	PostfixOp {
		op: PostfixOperator,
		value: Box<Spanned<Self>>,
	},
	BinaryOp {
		lhs: Box<Spanned<Self>>,
		op: BinaryOperator,
		rhs: Box<Spanned<Self>>,
	},
	TypeOp {
		lhs: Box<Spanned<Self>>,
		op: TypeOperator,
		rhs: Spanned<Type>,
	},
	PatternOp {
		lhs: Box<Spanned<Self>>,
		op: PatternOperator,
		rhs: Spanned<Pattern>,
	},
	AssignOp {
		lhs: Box<Spanned<Self>>,
		op: AssignOperator,
		rhs: Box<Spanned<Self>>,
	},
	Return {
		value: Option<Box<Spanned<Self>>>,
		label: Option<Ident>,
	},
	Break {
		value: Option<Box<Spanned<Self>>>,
		label: Option<Ident>,
	},
	Continue {
		label: Option<Ident>,
	},
	While {
		condition: Box<Spanned<Self>>,
		body: Box<Spanned<Self>>,
		label: Option<Ident>,
	},
	For {
		variable: Spanned<Pattern>,
		iterable: Box<Spanned<Self>>,
		body: Box<Spanned<Self>>,
		label: Option<Ident>,
	},
	If {
		condition: Box<Spanned<Self>>,
		then: Box<Spanned<Self>>,
		otherwise: Option<Box<Spanned<Self>>>,
	},
	Match {
		value: Box<Spanned<Self>>,
		arms: Vec<MatchArm>,
	},
	This,
	Placeholder,
	Block {
		body: Vec<Spanned<Statement>>,
		label: Option<Ident>,
	},
	Grouped(Box<Spanned<Self>>),
}

#[derive(Debug, Clone, PartialEq, salsa::Update)]
pub enum StringPart {
	Text(EcoString),
	EscapeSequence(StringEscape),
	InterpolatedExpr(Spanned<Expr>),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, Display, salsa::Update)]
pub enum CharEscape {
	Backslash,
	Newline,
	Carriage,
	Tab,
	Apostrophe,
	Unicode(char),
}

impl From<CharEscape> for char {
	fn from(val: CharEscape) -> Self {
		match val {
			CharEscape::Backslash => '\\',
			CharEscape::Newline => '\n',
			CharEscape::Carriage => '\r',
			CharEscape::Tab => '\t',
			CharEscape::Apostrophe => '\'',
			CharEscape::Unicode(c) => c,
		}
	}
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, Display, salsa::Update)]
pub enum StringEscape {
	#[strum(to_string = r"\\")]
	Backslash,
	#[strum(to_string = r"\n")]
	Newline,
	#[strum(to_string = r"\r")]
	Carriage,
	#[strum(to_string = r"\t")]
	Tab,
	#[strum(to_string = r"\${")]
	Interpolation,
	#[strum(to_string = r#"\""#)]
	Quote,
	#[strum(to_string = "{0}")]
	Unicode(char),
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub enum ListItem {
	Expr(Spanned<Expr>),
	Spread(Spanned<Expr>),
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub enum MapEntry {
	Expr(Spanned<Expr>, Spanned<Expr>),
	Spread(Spanned<Expr>),
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub enum RangeKind {
	From(Box<Spanned<Expr>>),
	To(Box<Spanned<Expr>>),
	Exclusive {
		min: Box<Spanned<Expr>>,
		max: Box<Spanned<Expr>>,
	},
	ToInclusive(Box<Spanned<Expr>>),
	Inclusive {
		min: Box<Spanned<Expr>>,
		max: Box<Spanned<Expr>>,
	},
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub struct ClosureParam {
	pub name: Spanned<Pattern>,
	pub type_: Option<Spanned<Type>>,
	pub mutable: bool,
	pub spread: bool,
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub struct CallArg {
	pub value: Spanned<Expr>,
	pub name: Option<Ident>,
	pub spread: bool,
}

#[derive(Clone, PartialEq, Debug, salsa::Update)]
pub struct MatchArm {
	pub pattern: Spanned<Pattern>,
	pub guard: Option<Spanned<Expr>>,
	pub body: Spanned<Expr>,
}

#[derive(Clone, PartialEq, Debug, Eq, Hash, salsa::Update)]
pub enum Pattern {
	Int(Spanned<i64>),
	Float(Spanned<OrderedFloat<f64>>),
	Char(Spanned<char>),
	String(Vec<Spanned<StringPatternPart>>),
	Boolean(Spanned<bool>),
	Binding {
		name: Ident,
		inner: Box<Spanned<Self>>,
	},
	List(Vec<Spanned<ListPatternEntry>>),
	Tuple(Vec<Spanned<ListPatternEntry>>),
	Map(Vec<Spanned<MapPatternEntry>>),
	Range(RangePatternKind),
	Struct {
		path: Vec<Ident>,
		fields: Vec<Spanned<StructPatternField>>,
	},
	Placeholder,
	Union(Box<Spanned<Self>>, Box<Spanned<Self>>),
	Grouped(Box<Spanned<Self>>),
}

impl Pattern {
	/// Extract the main identifier from a pattern, if it's a simple binding
	pub fn as_binding(&self) -> Option<&Ident> {
		match self {
			Pattern::Binding { name, inner: _ } => Some(name),
			_ => None,
		}
	}
	pub(crate) fn is_constant(&self) -> bool {
		match self {
			Pattern::Int(_)
			| Pattern::Float(_)
			| Pattern::Char(_)
			| Pattern::String(_)
			| Pattern::Boolean(_) => true,
			Self::Range(_) | Self::Placeholder => false,
			Self::Binding { inner, .. } => inner.0.is_constant(),
			Self::List(items) | Self::Tuple(items) => items.iter().all(|it| match &it.0 {
				ListPatternEntry::Item(pat) => pat.0.is_constant(),
				ListPatternEntry::Rest(_) => false,
			}),
			Self::Map(items) => items.iter().all(|it| match &it.0 {
				MapPatternEntry::Entry(key, value) => key.0.is_constant() && value.0.is_constant(),
				MapPatternEntry::Rest(_) => false,
			}),
			Self::Struct { fields, .. } => fields.iter().all(|field| match &field.0 {
				StructPatternField::Value { name: _, value } => value.0.is_constant(),
				StructPatternField::Named(_) | StructPatternField::Rest => false,
			}),
			Self::Union(first, second) => first.0.is_constant() && second.0.is_constant(),
			Self::Grouped(inner) => inner.0.is_constant(),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub enum StringPatternPart {
	Text(EcoString),
	EscapeSequence(StringEscape),
}

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::Update)]
pub enum RangePatternKind {
	/// `1..<`
	ExclusiveMin(Box<Spanned<Pattern>>),
	/// `1..<3`
	ExclusiveBoth {
		min: Box<Spanned<Pattern>>,
		max: Box<Spanned<Pattern>>,
	},
	/// `..=2`
	InclusiveMax(Box<Spanned<Pattern>>),
	/// `1..=2`
	InclusiveBoth {
		min: Box<Spanned<Pattern>>,
		max: Box<Spanned<Pattern>>,
	},
}

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::Update)]
pub enum StructPatternField {
	Value {
		name: Ident,
		value: Spanned<Pattern>,
	},
	Named(Ident),
	Rest,
}

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::Update)]
pub enum ListPatternEntry {
	Item(Spanned<Pattern>),
	Rest(Option<Ident>),
}

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::Update)]
pub enum MapPatternEntry {
	Entry(Spanned<Pattern>, Spanned<Pattern>),
	Rest(Option<Ident>),
}
