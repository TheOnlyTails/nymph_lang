use super::{
	Ident, Spanned,
	declaration::LetDeclaration,
	ops::{AssignOperator, BinaryOperator, PostfixOperator, PrefixOperator, TypeOperator},
	types::{GenericArg, GenericParam, Type},
};
use crate::ast::ops::PatternOperator;
use ordered_float::OrderedFloat;
use strum::Display;

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Statement {
	Expr(Spanned<Expr>),
	Let {
		meta: LetDeclaration,
		value: Spanned<Expr>,
	},
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
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
	For {
		variable: Spanned<Pattern>,
		iterable: Box<Spanned<Self>>,
		body: Box<Spanned<Self>>,
		label: Option<Ident>,
	},
	While {
		condition: Box<Spanned<Self>>,
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

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StringPart {
	Char(char),
	EscapeSequence(StringEscape),
	InterpolatedExpr(Spanned<Expr>),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, Display)]
pub(crate) enum CharEscape {
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

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, Display)]
pub(crate) enum StringEscape {
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ListItem {
	Expr(Spanned<Expr>),
	Spread(Spanned<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MapEntry {
	Expr(Spanned<Expr>, Spanned<Expr>),
	Spread(Spanned<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StructLiteralField {
	Named { name: Ident, value: Expr },
	Shorthand(Ident),
	Spread(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RangeKind {
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClosureParam {
	pub(crate) name: Spanned<Pattern>,
	pub(crate) type_: Option<Spanned<Type>>,
	pub(crate) mutable: bool,
	pub(crate) spread: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CallArg {
	pub(crate) value: Spanned<Expr>,
	pub(crate) name: Option<Ident>,
	pub(crate) spread: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct MatchArm {
	pub(crate) pattern: Spanned<Pattern>,
	pub(crate) guard: Option<Spanned<Expr>>,
	pub(crate) body: Spanned<Expr>,
}

#[derive(Clone, PartialEq, Debug, Eq, Hash)]
pub(crate) enum Pattern {
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
		name: Ident,
		fields: Vec<Spanned<StructPatternField>>,
	},
	Placeholder,
	Union(Box<Spanned<Self>>, Box<Spanned<Self>>),
	Grouped(Box<Spanned<Self>>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum StringPatternPart {
	Char(char),
	EscapeSequence(StringEscape),
}

#[derive(PartialEq, Clone, Debug, Eq, Hash)]
pub(crate) enum RangePatternKind {
	/// `1..`
	ExclusiveMin(Box<Spanned<Pattern>>),
	/// `1..3`
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

#[derive(PartialEq, Clone, Debug, Eq, Hash)]
pub(crate) enum StructPatternField {
	Value {
		name: Ident,
		value: Spanned<Pattern>,
	},
	Named(Ident),
	Rest,
}

#[derive(PartialEq, Clone, Debug, Eq, Hash)]
pub(crate) enum ListPatternEntry {
	Item(Spanned<Pattern>),
	Rest(Option<Ident>),
}

#[derive(PartialEq, Clone, Debug, Eq, Hash)]
pub(crate) enum MapPatternEntry {
	Entry(Spanned<Pattern>, Spanned<Pattern>),
	Rest(Option<Ident>),
}
