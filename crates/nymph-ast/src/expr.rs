//! Expressions, statements, and patterns.
//!
//! Nymph is expression-oriented: `if`, `match`, `while`, `for`, and blocks all *are*
//! expressions that produce values, which is why they live here rather than in a
//! separate statement grammar. The only statements are a bare expression and a `let`.

use ecow::EcoString;
use ordered_float::OrderedFloat;
use strum::Display;

use crate::{
	Ident, Spanned,
	decl::LetDeclaration,
	ops::{
		AssignOperator, BinaryOperator, PatternOperator, PostfixOperator, PrefixOperator, TypeOperator,
	},
	ty::{GenericArg, GenericParam, Type},
};

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
	/// `1u`, `0xDEADF00Du`
	UInt(Spanned<u64>),
	/// `0.1`, `2f`, `6.02e23`
	Float(Spanned<OrderedFloat<f64>>),
	/// `'a'`, `'\n'`
	Char(Spanned<char>),
	/// `"Hello, ${name}!"`
	String(Vec<Spanned<StringPart>>),
	/// `true`, `false`
	Boolean(Spanned<bool>),
	/// `a`, `my_var`
	Identifier(Ident),
	/// `$`, `$0`, `$1` — a positional parameter of the enclosing implicit closure.
	AnonymousParam(Option<u32>),
	/// `#[]`, `#[1, 2, ...rest]`
	List(Vec<Spanned<ListItem>>),
	/// `#()`, `#(1, true, ...other)`
	Tuple(Vec<Spanned<ListItem>>),
	/// `#{ "a": 1, ...rest }`
	Map(Vec<Spanned<MapEntry>>),
	/// `1..10`, `1..=10`, `1..`, `..10`, `..=10`
	Range(RangeKind),
	Call {
		func: Box<Spanned<Self>>,
		generics: Vec<Spanned<GenericArg>>,
		args: Vec<Spanned<CallArg>>,
	},
	MemberAccess {
		parent: Box<Spanned<Self>>,
		member: Ident,
		/// The `?.` optional-chaining form.
		optional: bool,
	},
	IndexAccess {
		parent: Box<Spanned<Self>>,
		index: Box<Spanned<Self>>,
		/// The `?.[i]` optional-chaining form.
		optional: bool,
	},
	/// `(x, y) -> x + y`, `x -> x * 2`
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
	/// `value as Type`
	TypeOp {
		lhs: Box<Spanned<Self>>,
		op: TypeOperator,
		rhs: Spanned<Type>,
	},
	/// `value is Pattern`, `value !is Pattern`
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
	/// `this` — the current instance.
	This,
	Block {
		body: Vec<Spanned<Statement>>,
		label: Option<Ident>,
	},
	/// `(expr)` — kept to preserve grouping intent.
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

impl StringEscape {
	/// The concrete character this escape expands to, if it maps to a single char.
	pub fn to_char(self) -> Option<char> {
		match self {
			StringEscape::Backslash => Some('\\'),
			StringEscape::Newline => Some('\n'),
			StringEscape::Carriage => Some('\r'),
			StringEscape::Tab => Some('\t'),
			StringEscape::Quote => Some('"'),
			StringEscape::Unicode(c) => Some(c),
			StringEscape::Interpolation => None,
		}
	}
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub enum ListItem {
	Expr(Spanned<Expr>),
	Spread(Spanned<Expr>),
}

#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub enum MapEntry {
	Entry(Spanned<Expr>, Spanned<Expr>),
	Spread(Spanned<Expr>),
}

/// The five forms a range expression can take. All use `..` (exclusive) or `..=`
/// (inclusive); the previous `..<` pattern-only form has been removed.
#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub enum RangeKind {
	/// `1..` — from a lower bound, unbounded above (iterable, infinite).
	From(Box<Spanned<Expr>>),
	/// `..10` — up to an exclusive upper bound (inclusion-test only).
	To(Box<Spanned<Expr>>),
	/// `1..10`
	Exclusive {
		min: Box<Spanned<Expr>>,
		max: Box<Spanned<Expr>>,
	},
	/// `..=10`
	ToInclusive(Box<Spanned<Expr>>),
	/// `1..=10`
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
	UInt(Spanned<u64>),
	Float(Spanned<OrderedFloat<f64>>),
	Char(Spanned<char>),
	String(Vec<Spanned<StringPatternPart>>),
	Boolean(Spanned<bool>),
	/// `name` or `name = inner` — binds a name, optionally to a sub-pattern.
	Binding {
		name: Ident,
		inner: Box<Spanned<Self>>,
	},
	List(Vec<Spanned<ListPatternEntry>>),
	Tuple(Vec<Spanned<ListPatternEntry>>),
	Map(Vec<Spanned<MapPatternEntry>>),
	Range(RangePatternKind),
	/// `Some(value)`, `Ok(value = inner)`, `Color.Red`
	Struct {
		path: Vec<Ident>,
		fields: Vec<Spanned<StructPatternField>>,
	},
	/// `_`
	Placeholder,
	/// `A | B`
	Union(Box<Spanned<Self>>, Box<Spanned<Self>>),
	Grouped(Box<Spanned<Self>>),
}

impl Pattern {
	/// The bound identifier, if this pattern is a plain binding.
	pub fn as_binding(&self) -> Option<&Ident> {
		match self {
			Pattern::Binding { name, .. } => Some(name),
			_ => None,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub enum StringPatternPart {
	Text(EcoString),
	EscapeSequence(StringEscape),
}

/// Range patterns mirror [`RangeKind`], using unified `..` / `..=` bounds.
#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::Update)]
pub enum RangePatternKind {
	/// `1..` — matches values `>= min`.
	From(Box<Spanned<Pattern>>),
	/// `..10` — matches values `< max`.
	To(Box<Spanned<Pattern>>),
	/// `1..10`
	Exclusive {
		min: Box<Spanned<Pattern>>,
		max: Box<Spanned<Pattern>>,
	},
	/// `..=10`
	ToInclusive(Box<Spanned<Pattern>>),
	/// `1..=10`
	Inclusive {
		min: Box<Spanned<Pattern>>,
		max: Box<Spanned<Pattern>>,
	},
}

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::Update)]
pub enum StructPatternField {
	/// `field = pattern`
	Value {
		name: Ident,
		value: Spanned<Pattern>,
	},
	/// `field` — shorthand binding the field to a same-named variable.
	Named(Ident),
	/// `...`
	Rest,
}

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::Update)]
pub enum ListPatternEntry {
	Item(Spanned<Pattern>),
	/// `...` or `...rest`
	Rest(Option<Ident>),
}

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::Update)]
pub enum MapPatternEntry {
	Entry(Spanned<Pattern>, Spanned<Pattern>),
	Rest(Option<Ident>),
}
