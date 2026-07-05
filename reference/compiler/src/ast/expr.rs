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
	/// `1u`, `0b010001u`, `0xDEADF00Du`
	UInt(Spanned<u64>),
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
	/// `$`, `$0`, `$1`
	AnonymousParam(Option<u32>),
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
	UInt(Spanned<u64>),
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
			| Pattern::UInt(_)
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

fn semantic_anonymous_param_index(index: Option<u32>) -> usize {
	index.unwrap_or(0) as usize
}

fn visit_anonymous_params(expr: &Spanned<Expr>, visit: &mut impl FnMut(Option<u32>, super::Span)) {
	match &expr.0 {
		Expr::AnonymousParam(index) => visit(*index, expr.1),
		Expr::String(parts) => {
			for part in parts {
				if let StringPart::InterpolatedExpr(inner) = &part.0 {
					visit_anonymous_params(inner, visit);
				}
			}
		}
		Expr::List(items) | Expr::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(inner) | ListItem::Spread(inner) => {
						visit_anonymous_params(inner, visit);
					}
				}
			}
		}
		Expr::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapEntry::Expr(key, value) => {
						visit_anonymous_params(key, visit);
						visit_anonymous_params(value, visit);
					}
					MapEntry::Spread(value) => visit_anonymous_params(value, visit),
				}
			}
		}
		Expr::Range(kind) => match kind {
			RangeKind::From(value) | RangeKind::To(value) | RangeKind::ToInclusive(value) => {
				visit_anonymous_params(value, visit);
			}
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				visit_anonymous_params(min, visit);
				visit_anonymous_params(max, visit);
			}
		},
		Expr::Call { func, args, .. } => {
			visit_anonymous_params(func, visit);
			for arg in args {
				visit_anonymous_params(&arg.0.value, visit);
			}
		}
		Expr::MemberAccess { parent, .. }
		| Expr::Grouped(parent)
		| Expr::PrefixOp { value: parent, .. }
		| Expr::PostfixOp { value: parent, .. } => visit_anonymous_params(parent, visit),
		Expr::IndexAccess { parent, index, .. }
		| Expr::BinaryOp {
			lhs: parent,
			rhs: index,
			..
		}
		| Expr::AssignOp {
			lhs: parent,
			rhs: index,
			..
		} => {
			visit_anonymous_params(parent, visit);
			visit_anonymous_params(index, visit);
		}
		Expr::TypeOp { lhs, .. } | Expr::PatternOp { lhs, .. } => {
			visit_anonymous_params(lhs, visit);
		}
		Expr::Return { value, .. } | Expr::Break { value, .. } => {
			if let Some(value) = value {
				visit_anonymous_params(value, visit);
			}
		}
		Expr::While {
			condition, body, ..
		}
		| Expr::If {
			condition,
			then: body,
			otherwise: None,
		} => {
			visit_anonymous_params(condition, visit);
			visit_anonymous_params(body, visit);
		}
		Expr::If {
			condition,
			then,
			otherwise: Some(otherwise),
		} => {
			visit_anonymous_params(condition, visit);
			visit_anonymous_params(then, visit);
			visit_anonymous_params(otherwise, visit);
		}
		Expr::For { iterable, body, .. } => {
			visit_anonymous_params(iterable, visit);
			visit_anonymous_params(body, visit);
		}
		Expr::Match { value, arms } => {
			visit_anonymous_params(value, visit);
			for arm in arms {
				if let Some(guard) = &arm.guard {
					visit_anonymous_params(guard, visit);
				}
				visit_anonymous_params(&arm.body, visit);
			}
		}
		Expr::Block { body, .. } => {
			for statement in body {
				match &statement.0 {
					Statement::Expr(inner) => visit_anonymous_params(inner, visit),
					Statement::Let { value, .. } => visit_anonymous_params(value, visit),
				}
			}
		}
		Expr::Closure { .. }
		| Expr::Int(_)
		| Expr::UInt(_)
		| Expr::Float(_)
		| Expr::Char(_)
		| Expr::Boolean(_)
		| Expr::Identifier(_)
		| Expr::This
		| Expr::Placeholder
		| Expr::Continue { .. } => {}
	}
}

pub fn anonymous_params(expr: &Spanned<Expr>) -> std::collections::BTreeMap<usize, super::Span> {
	let mut params = std::collections::BTreeMap::new();
	visit_anonymous_params(expr, &mut |index, span| {
		params
			.entry(semantic_anonymous_param_index(index))
			.or_insert(span);
	});
	params
}

pub fn anonymous_param_syntaxes(expr: &Spanned<Expr>) -> Vec<Option<u32>> {
	let mut params = std::collections::BTreeSet::new();
	visit_anonymous_params(expr, &mut |index, _| {
		params.insert(index);
	});
	params.into_iter().collect()
}

pub fn rewrite_anonymous_params(
	expr: &Spanned<Expr>,
	names: &std::collections::BTreeMap<usize, ecow::EcoString>,
) -> Spanned<Expr> {
	fn rewrite(
		expr: &Spanned<Expr>,
		names: &std::collections::BTreeMap<usize, ecow::EcoString>,
	) -> Spanned<Expr> {
		let span = expr.1;
		match &expr.0 {
			Expr::AnonymousParam(index) => names
				.get(&semantic_anonymous_param_index(*index))
				.map_or_else(
					|| expr.clone(),
					|name| Spanned(Expr::Identifier(Spanned(name.clone(), span)), span),
				),
			Expr::String(parts) => Spanned(
				Expr::String(
					parts
						.iter()
						.map(|part| {
							Spanned(
								match &part.0 {
									StringPart::InterpolatedExpr(inner) => {
										StringPart::InterpolatedExpr(rewrite(inner, names))
									}
									other => other.clone(),
								},
								part.1,
							)
						})
						.collect(),
				),
				span,
			),
			Expr::List(items) => Spanned(
				Expr::List(
					items
						.iter()
						.map(|item| {
							Spanned(
								match &item.0 {
									ListItem::Expr(inner) => ListItem::Expr(rewrite(inner, names)),
									ListItem::Spread(inner) => ListItem::Spread(rewrite(inner, names)),
								},
								item.1,
							)
						})
						.collect(),
				),
				span,
			),
			Expr::Tuple(items) => Spanned(
				Expr::Tuple(
					items
						.iter()
						.map(|item| {
							Spanned(
								match &item.0 {
									ListItem::Expr(inner) => ListItem::Expr(rewrite(inner, names)),
									ListItem::Spread(inner) => ListItem::Spread(rewrite(inner, names)),
								},
								item.1,
							)
						})
						.collect(),
				),
				span,
			),
			Expr::Map(entries) => Spanned(
				Expr::Map(
					entries
						.iter()
						.map(|entry| {
							Spanned(
								match &entry.0 {
									MapEntry::Expr(key, value) => {
										MapEntry::Expr(rewrite(key, names), rewrite(value, names))
									}
									MapEntry::Spread(value) => MapEntry::Spread(rewrite(value, names)),
								},
								entry.1,
							)
						})
						.collect(),
				),
				span,
			),
			Expr::Range(kind) => Spanned(
				Expr::Range(match kind {
					RangeKind::From(value) => RangeKind::From(Box::new(rewrite(value, names))),
					RangeKind::To(value) => RangeKind::To(Box::new(rewrite(value, names))),
					RangeKind::ToInclusive(value) => RangeKind::ToInclusive(Box::new(rewrite(value, names))),
					RangeKind::Exclusive { min, max } => RangeKind::Exclusive {
						min: Box::new(rewrite(min, names)),
						max: Box::new(rewrite(max, names)),
					},
					RangeKind::Inclusive { min, max } => RangeKind::Inclusive {
						min: Box::new(rewrite(min, names)),
						max: Box::new(rewrite(max, names)),
					},
				}),
				span,
			),
			Expr::Call {
				func,
				generics,
				args,
			} => Spanned(
				Expr::Call {
					func: Box::new(rewrite(func, names)),
					generics: generics.clone(),
					args: args
						.iter()
						.map(|arg| {
							Spanned(
								CallArg {
									value: rewrite(&arg.0.value, names),
									name: arg.0.name.clone(),
									spread: arg.0.spread,
								},
								arg.1,
							)
						})
						.collect(),
				},
				span,
			),
			Expr::MemberAccess {
				parent,
				member,
				optional,
			} => Spanned(
				Expr::MemberAccess {
					parent: Box::new(rewrite(parent, names)),
					member: member.clone(),
					optional: *optional,
				},
				span,
			),
			Expr::IndexAccess {
				parent,
				index,
				optional,
			} => Spanned(
				Expr::IndexAccess {
					parent: Box::new(rewrite(parent, names)),
					index: Box::new(rewrite(index, names)),
					optional: *optional,
				},
				span,
			),
			Expr::Closure { .. } => expr.clone(),
			Expr::PrefixOp { op, value } => Spanned(
				Expr::PrefixOp {
					op: *op,
					value: Box::new(rewrite(value, names)),
				},
				span,
			),
			Expr::PostfixOp { op, value } => Spanned(
				Expr::PostfixOp {
					op: *op,
					value: Box::new(rewrite(value, names)),
				},
				span,
			),
			Expr::BinaryOp { lhs, op, rhs } => Spanned(
				Expr::BinaryOp {
					lhs: Box::new(rewrite(lhs, names)),
					op: *op,
					rhs: Box::new(rewrite(rhs, names)),
				},
				span,
			),
			Expr::TypeOp { lhs, op, rhs } => Spanned(
				Expr::TypeOp {
					lhs: Box::new(rewrite(lhs, names)),
					op: *op,
					rhs: rhs.clone(),
				},
				span,
			),
			Expr::PatternOp { lhs, op, rhs } => Spanned(
				Expr::PatternOp {
					lhs: Box::new(rewrite(lhs, names)),
					op: *op,
					rhs: rhs.clone(),
				},
				span,
			),
			Expr::AssignOp { lhs, op, rhs } => Spanned(
				Expr::AssignOp {
					lhs: Box::new(rewrite(lhs, names)),
					op: *op,
					rhs: Box::new(rewrite(rhs, names)),
				},
				span,
			),
			Expr::Return { value, label } => Spanned(
				Expr::Return {
					value: value.as_ref().map(|value| Box::new(rewrite(value, names))),
					label: label.clone(),
				},
				span,
			),
			Expr::Break { value, label } => Spanned(
				Expr::Break {
					value: value.as_ref().map(|value| Box::new(rewrite(value, names))),
					label: label.clone(),
				},
				span,
			),
			Expr::Continue { .. }
			| Expr::Int(_)
			| Expr::UInt(_)
			| Expr::Float(_)
			| Expr::Char(_)
			| Expr::Boolean(_)
			| Expr::Identifier(_)
			| Expr::This
			| Expr::Placeholder => expr.clone(),
			Expr::While {
				condition,
				body,
				label,
			} => Spanned(
				Expr::While {
					condition: Box::new(rewrite(condition, names)),
					body: Box::new(rewrite(body, names)),
					label: label.clone(),
				},
				span,
			),
			Expr::For {
				variable,
				iterable,
				body,
				label,
			} => Spanned(
				Expr::For {
					variable: variable.clone(),
					iterable: Box::new(rewrite(iterable, names)),
					body: Box::new(rewrite(body, names)),
					label: label.clone(),
				},
				span,
			),
			Expr::If {
				condition,
				then,
				otherwise,
			} => Spanned(
				Expr::If {
					condition: Box::new(rewrite(condition, names)),
					then: Box::new(rewrite(then, names)),
					otherwise: otherwise
						.as_ref()
						.map(|otherwise| Box::new(rewrite(otherwise, names))),
				},
				span,
			),
			Expr::Match { value, arms } => Spanned(
				Expr::Match {
					value: Box::new(rewrite(value, names)),
					arms: arms
						.iter()
						.map(|arm| MatchArm {
							pattern: arm.pattern.clone(),
							guard: arm.guard.as_ref().map(|guard| rewrite(guard, names)),
							body: rewrite(&arm.body, names),
						})
						.collect(),
				},
				span,
			),
			Expr::Block { body, label } => Spanned(
				Expr::Block {
					body: body
						.iter()
						.map(|statement| {
							Spanned(
								match &statement.0 {
									Statement::Expr(inner) => Statement::Expr(rewrite(inner, names)),
									Statement::Let { meta, value } => Statement::Let {
										meta: meta.clone(),
										value: rewrite(value, names),
									},
								},
								statement.1,
							)
						})
						.collect(),
					label: label.clone(),
				},
				span,
			),
			Expr::Grouped(inner) => Spanned(Expr::Grouped(Box::new(rewrite(inner, names))), span),
		}
	}

	rewrite(expr, names)
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
