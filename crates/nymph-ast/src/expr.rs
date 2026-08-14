//! Expressions, statements, and patterns.
//!
//! Nymph is expression-oriented: `if`, `match`, `while`, `for`, and blocks all *are*
//! expressions that produce values, which is why they live here rather than in a
//! separate statement grammar. The only statements are a bare expression and a `let`.

use ecow::EcoString;
use ordered_float::OrderedFloat;
use strum::Display;

use crate::{
	Ident, NodeId, Span, Spanned,
	decl::LetDeclaration,
	ops::{
		AssignOperator, BinaryOperator, PatternOperator, PostfixOperator, PrefixOperator, TypeOperator,
	},
	ty::{GenericArg, GenericParam, Type},
};

#[derive(Clone, PartialEq, Debug, salsa::SalsaValue)]
pub enum Statement {
	Expr(Expr),
	Let { meta: LetDeclaration, value: Expr },
}

/// A self-spanned expression node: its kind, the source span it covers, and a
/// stable [`NodeId`]. Expressions carry their own span (unlike other AST nodes,
/// which are wrapped in [`Spanned`]) so that identity, position, and shape travel
/// together — the shape the HIR and LSP both want.
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct Expr {
	pub kind: ExprKind,
	pub span: Span,
	pub id: NodeId,
}

impl Expr {
	pub fn new(kind: ExprKind, span: Span, id: NodeId) -> Self {
		Self { kind, span, id }
	}

	/// Calls `f` for each immediate child expression, in source order.
	///
	/// This is structural traversal only: recursion and scope boundaries remain
	/// the caller's responsibility.
	pub fn for_each_child<'a>(&'a self, mut f: impl FnMut(&'a Expr)) {
		match &self.kind {
			ExprKind::String(parts) => {
				for part in parts {
					if let StringPart::InterpolatedExpr(expr) = &part.0 {
						f(expr);
					}
				}
			}
			ExprKind::List(items) | ExprKind::Tuple(items) => {
				for item in items {
					match &item.0 {
						ListItem::Expr(expr) | ListItem::Spread(expr) => f(expr),
					}
				}
			}
			ExprKind::Map(entries) => {
				for entry in entries {
					match &entry.0 {
						MapEntry::Entry(key, value) => {
							f(key);
							f(value);
						}
						MapEntry::Spread(expr) => f(expr),
					}
				}
			}
			ExprKind::Range(range) => match range {
				RangeKind::From(expr) | RangeKind::To(expr) | RangeKind::ToInclusive(expr) => f(expr),
				RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
					f(min);
					f(max);
				}
			},
			ExprKind::Call { func, args, .. } => {
				f(func);
				for arg in args {
					f(&arg.0.value);
				}
			}
			ExprKind::MemberAccess { parent, .. } => f(parent),
			ExprKind::IndexAccess { parent, index, .. } => {
				f(parent);
				f(index);
			}
			ExprKind::Closure { body, .. }
			| ExprKind::PrefixOp { value: body, .. }
			| ExprKind::PostfixOp { value: body, .. }
			| ExprKind::TypeOp { lhs: body, .. }
			| ExprKind::PatternOp { lhs: body, .. }
			| ExprKind::Grouped(body) => f(body),
			ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
				f(lhs);
				f(rhs);
			}
			ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
				if let Some(value) = value {
					f(value);
				}
			}
			ExprKind::While {
				condition, body, ..
			} => {
				f(condition);
				f(body);
			}
			ExprKind::For { iterable, body, .. } => {
				f(iterable);
				f(body);
			}
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => {
				f(condition);
				f(then);
				if let Some(otherwise) = otherwise {
					f(otherwise);
				}
			}
			ExprKind::Match { value, arms } => {
				f(value);
				for arm in arms {
					if let Some(guard) = &arm.guard {
						f(guard);
					}
					f(&arm.body);
				}
			}
			ExprKind::Block { body, .. } => {
				for statement in body {
					match &statement.0 {
						Statement::Expr(expr) | Statement::Let { value: expr, .. } => f(expr),
					}
				}
			}
			ExprKind::Int(_)
			| ExprKind::UInt(_)
			| ExprKind::Float(_)
			| ExprKind::Char(_)
			| ExprKind::Boolean(_)
			| ExprKind::Identifier(_)
			| ExprKind::AnonymousParam(_)
			| ExprKind::This
			| ExprKind::Continue { .. } => {}
		}
	}
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum ExprKind {
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
	AnonymousParam(Option<u8>),
	/// `#[]`, `#[1, 2, ...rest]`
	List(Vec<Spanned<ListItem>>),
	/// `#()`, `#(1, true, ...other)`
	Tuple(Vec<Spanned<ListItem>>),
	/// `#{ "a": 1, ...rest }`
	Map(Vec<Spanned<MapEntry>>),
	/// `1..10`, `1..=10`, `1..`, `..10`, `..=10`
	Range(RangeKind),
	Call {
		func: Box<Expr>,
		generics: Vec<Spanned<GenericArg>>,
		args: Vec<Spanned<CallArg>>,
	},
	MemberAccess {
		parent: Box<Expr>,
		member: Ident,
		/// The `?.` optional-chaining form.
		optional: bool,
	},
	IndexAccess {
		parent: Box<Expr>,
		index: Box<Expr>,
		/// The `?.[i]` optional-chaining form.
		optional: bool,
	},
	/// `(x, y) -> x + y`, `x -> x * 2`
	Closure {
		/// Callable label written before the parameter list or on the body block.
		label: Option<Ident>,
		params: Vec<Spanned<ClosureParam>>,
		generics: Vec<Spanned<GenericParam>>,
		return_type: Option<Spanned<Type>>,
		body: Box<Expr>,
	},
	PrefixOp {
		op: PrefixOperator,
		value: Box<Expr>,
	},
	PostfixOp {
		op: PostfixOperator,
		value: Box<Expr>,
	},
	BinaryOp {
		lhs: Box<Expr>,
		op: BinaryOperator,
		rhs: Box<Expr>,
	},
	/// `value as Type`
	TypeOp {
		lhs: Box<Expr>,
		op: TypeOperator,
		rhs: Spanned<Type>,
	},
	/// `value is Pattern`, `value !is Pattern`
	PatternOp {
		lhs: Box<Expr>,
		op: PatternOperator,
		rhs: Spanned<Pattern>,
	},
	AssignOp {
		lhs: Box<Expr>,
		op: AssignOperator,
		rhs: Box<Expr>,
	},
	Return {
		value: Option<Box<Expr>>,
		label: Option<Ident>,
	},
	Break {
		value: Option<Box<Expr>>,
		label: Option<Ident>,
	},
	Continue {
		label: Option<Ident>,
	},
	While {
		condition: Box<Expr>,
		body: Box<Expr>,
		label: Option<Ident>,
	},
	For {
		variable: Spanned<Pattern>,
		iterable: Box<Expr>,
		body: Box<Expr>,
		label: Option<Ident>,
	},
	If {
		condition: Box<Expr>,
		then: Box<Expr>,
		otherwise: Option<Box<Expr>>,
	},
	Match {
		value: Box<Expr>,
		arms: Vec<MatchArm>,
	},
	/// `this` — the current instance.
	This,
	Block {
		body: Vec<Spanned<Statement>>,
		label: Option<Ident>,
	},
	/// `(expr)` — kept to preserve grouping intent.
	Grouped(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, salsa::SalsaValue)]
pub enum StringPart {
	Text(EcoString),
	EscapeSequence(StringEscape),
	InterpolatedExpr(Expr),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, Display, salsa::SalsaValue)]
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

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, Display, salsa::SalsaValue)]
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

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum ListItem {
	Expr(Expr),
	Spread(Expr),
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum MapEntry {
	Entry(Expr, Expr),
	Spread(Expr),
}

/// The five forms a range expression can take. All use `..` (exclusive) or `..=`
/// (inclusive); the previous `..<` pattern-only form has been removed.
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum RangeKind {
	/// `1..` — from a lower bound, unbounded above (iterable, infinite).
	From(Box<Expr>),
	/// `..10` — up to an exclusive upper bound (inclusion-test only).
	To(Box<Expr>),
	/// `1..10`
	Exclusive { min: Box<Expr>, max: Box<Expr> },
	/// `..=10`
	ToInclusive(Box<Expr>),
	/// `1..=10`
	Inclusive { min: Box<Expr>, max: Box<Expr> },
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct ClosureParam {
	pub name: Spanned<Pattern>,
	pub type_: Option<Spanned<Type>>,
	pub mutable: bool,
	pub spread: bool,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct CallArg {
	pub value: Expr,
	pub name: Option<Ident>,
	pub spread: bool,
}

#[derive(Clone, PartialEq, Debug, salsa::SalsaValue)]
pub struct MatchArm {
	pub pattern: Spanned<Pattern>,
	pub guard: Option<Expr>,
	pub body: Expr,
}

#[derive(Clone, PartialEq, Debug, Eq, Hash, salsa::SalsaValue)]
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum StringPatternPart {
	Text(EcoString),
	EscapeSequence(StringEscape),
}

/// Range patterns mirror [`RangeKind`], using unified `..` / `..=` bounds.
#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::SalsaValue)]
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

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::SalsaValue)]
pub enum StructPatternField {
	/// `field = pattern`
	Value {
		name: Ident,
		value: Spanned<Pattern>,
	},
	/// `field` — shorthand binding the field to a same-named variable.
	Named(Ident),
	/// A bare sub-pattern with no field name (`Ok(Add(title))`, `Some(3)`). Only
	/// valid when the struct/variant has exactly ONE field — then it unambiguously
	/// matches that field. The checker rejects it for a zero- or multi-field
	/// constructor (where there is no single field to bind it to).
	Positional(Spanned<Pattern>),
	/// `...`
	Rest,
}

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::SalsaValue)]
pub enum ListPatternEntry {
	Item(Spanned<Pattern>),
	/// `...` or `...rest`
	Rest(Option<Ident>),
}

#[derive(PartialEq, Clone, Debug, Eq, Hash, salsa::SalsaValue)]
pub enum MapPatternEntry {
	Entry(Spanned<Pattern>, Spanned<Pattern>),
	Rest(Option<Ident>),
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		decl::{LetDeclaration, LetKind},
		ops::{
			AssignOperator, BinaryOperator, PatternOperator, PostfixOperator, PrefixOperator,
			TypeOperator,
		},
		ty::Type,
	};

	const SPAN: Span = Span { start: 0, end: 0 };

	fn child(id: u32) -> Expr {
		Expr::new(ExprKind::This, SPAN, NodeId(id))
	}

	fn assert_children(kind: ExprKind, expected: &[u32]) {
		let expr = Expr::new(kind, SPAN, NodeId(0));
		let mut actual = Vec::new();
		expr.for_each_child(|child| actual.push(child.id.0));
		assert_eq!(actual, expected);
	}

	#[test]
	fn structural_children_cover_embedded_expression_collections_in_order() {
		assert_children(
			ExprKind::String(vec![Spanned::new(
				StringPart::InterpolatedExpr(child(1)),
				SPAN,
			)]),
			&[1],
		);
		assert_children(
			ExprKind::List(vec![
				Spanned::new(ListItem::Expr(child(1)), SPAN),
				Spanned::new(ListItem::Spread(child(2)), SPAN),
			]),
			&[1, 2],
		);
		assert_children(
			ExprKind::Tuple(vec![Spanned::new(ListItem::Expr(child(1)), SPAN)]),
			&[1],
		);
		assert_children(
			ExprKind::Map(vec![
				Spanned::new(MapEntry::Entry(child(1), child(2)), SPAN),
				Spanned::new(MapEntry::Spread(child(3)), SPAN),
			]),
			&[1, 2, 3],
		);
		assert_children(
			ExprKind::Range(RangeKind::Inclusive {
				min: Box::new(child(1)),
				max: Box::new(child(2)),
			}),
			&[1, 2],
		);
		assert_children(
			ExprKind::Call {
				func: Box::new(child(1)),
				generics: Vec::new(),
				args: vec![Spanned::new(
					CallArg {
						value: child(2),
						name: None,
						spread: false,
					},
					SPAN,
				)],
			},
			&[1, 2],
		);
		assert_children(
			ExprKind::Match {
				value: Box::new(child(1)),
				arms: vec![MatchArm {
					pattern: Spanned::new(Pattern::Placeholder, SPAN),
					guard: Some(child(2)),
					body: child(3),
				}],
			},
			&[1, 2, 3],
		);
		assert_children(
			ExprKind::Block {
				body: vec![
					Spanned::new(Statement::Expr(child(1)), SPAN),
					Spanned::new(
						Statement::Let {
							meta: LetDeclaration {
								kind: LetKind::Instance,
								name: Spanned::new(Pattern::Placeholder, SPAN),
								type_: None,
							},
							value: child(2),
						},
						SPAN,
					),
				],
				label: None,
			},
			&[1, 2],
		);
	}

	#[test]
	fn structural_children_cover_expression_operators_and_control_flow() {
		assert_children(
			ExprKind::MemberAccess {
				parent: Box::new(child(1)),
				member: Spanned::new("member".into(), SPAN),
				optional: false,
			},
			&[1],
		);
		assert_children(
			ExprKind::IndexAccess {
				parent: Box::new(child(1)),
				index: Box::new(child(2)),
				optional: false,
			},
			&[1, 2],
		);
		assert_children(
			ExprKind::Closure {
				label: None,
				params: Vec::new(),
				generics: Vec::new(),
				return_type: None,
				body: Box::new(child(1)),
			},
			&[1],
		);
		assert_children(
			ExprKind::PrefixOp {
				op: PrefixOperator::Negate,
				value: Box::new(child(1)),
			},
			&[1],
		);
		assert_children(
			ExprKind::PostfixOp {
				op: PostfixOperator::ErrorReturn,
				value: Box::new(child(1)),
			},
			&[1],
		);
		assert_children(
			ExprKind::BinaryOp {
				lhs: Box::new(child(1)),
				op: BinaryOperator::Plus,
				rhs: Box::new(child(2)),
			},
			&[1, 2],
		);
		assert_children(
			ExprKind::AssignOp {
				lhs: Box::new(child(1)),
				op: AssignOperator::Assign,
				rhs: Box::new(child(2)),
			},
			&[1, 2],
		);
		assert_children(
			ExprKind::TypeOp {
				lhs: Box::new(child(1)),
				op: TypeOperator::As,
				rhs: Spanned::new(Type::Infer, SPAN),
			},
			&[1],
		);
		assert_children(
			ExprKind::PatternOp {
				lhs: Box::new(child(1)),
				op: PatternOperator::Is,
				rhs: Spanned::new(Pattern::Placeholder, SPAN),
			},
			&[1],
		);
		assert_children(
			ExprKind::While {
				condition: Box::new(child(1)),
				body: Box::new(child(2)),
				label: None,
			},
			&[1, 2],
		);
		assert_children(
			ExprKind::For {
				variable: Spanned::new(Pattern::Placeholder, SPAN),
				iterable: Box::new(child(1)),
				body: Box::new(child(2)),
				label: None,
			},
			&[1, 2],
		);
		assert_children(
			ExprKind::If {
				condition: Box::new(child(1)),
				then: Box::new(child(2)),
				otherwise: Some(Box::new(child(3))),
			},
			&[1, 2, 3],
		);
		assert_children(
			ExprKind::Return {
				value: Some(Box::new(child(1))),
				label: None,
			},
			&[1],
		);
		assert_children(
			ExprKind::Break {
				value: Some(Box::new(child(1))),
				label: None,
			},
			&[1],
		);
		assert_children(ExprKind::Grouped(Box::new(child(1))), &[1]);
		assert_children(ExprKind::This, &[]);
	}
}
