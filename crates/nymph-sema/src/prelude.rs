//! Additive "stdlib prelude" injection (Slice: stdlib linkage groundwork).
//!
//! [`check_module_with_prelude`] flattens one or more *prelude* modules ahead of a
//! user module's own declarations and checks the combined program as one module —
//! the same flattening trick [`crate::check_program`] already uses for the stdlib's
//! own cross-file typecheck, but with two additions that make it safe to expose to
//! real user programs:
//!
//! 1. **NodeId/Span offsetting** (this module's main job): the prelude and the user
//!    module are parsed *separately*, so the parser's per-parse `NodeId`/`Span`
//!    counters both start at 0 and collide once flattened into one module —
//!    `Annotations` is keyed by `NodeId` (and by `Span` for pattern-variant
//!    resolutions), so an unmodified prelude clone would silently clobber the
//!    user's own recorded types/resolutions. [`offset_module`] clones a prelude
//!    module and shifts every `NodeId`/`Span` in it by an offset unique to that
//!    module's position in the `prelude` slice ([`NODE_BASE`]/[`SPAN_BASE`] plus a
//!    stride per index — see [`NODE_STRIDE`]/[`SPAN_STRIDE`]), chosen far above
//!    anything a real parse can produce, so prelude and user identities can never
//!    collide with each other *or* with any other prelude module flattened
//!    alongside them in the same call.
//! 2. **Diagnostic partitioning**: the same offset doubles as an origin marker —
//!    any diagnostic whose primary span falls at or above `SPAN_BASE` is
//!    prelude-internal by construction and must never reach a user program (a user
//!    must never see a stdlib source span). Such diagnostics are dropped; any
//!    *user*-anchored diagnostic that still carries a secondary label pointing into
//!    the prelude (e.g. `Redefinition`'s "first defined here" label, when a user
//!    shadows a prelude name) has that label replaced with a note instead, so the
//!    signal survives without leaking a prelude span.
//!
//! This is deliberately **not** wired into [`crate::check_module`] or
//! [`crate::check_program`] — both are completely unaffected, byte-for-byte. See
//! the design write-up in `docs/superpowers/plans/2026-07-14-nymph-stdlib-linkage-groundwork.md`
//! for the full investigation this landed from.

use std::cell::Cell;

use nymph_ast::{
	Ident, NodeId, Span, Spanned,
	decl::{
		Declaration, EnumVariant, FuncDeclaration, FuncParam, ImplMember, ImportRoot, InterfaceElement,
		InterfaceMember, LetDeclaration, Module, StructField, StructImpl, TypeAliasDeclaration,
	},
	expr::{
		CallArg, ClosureParam, Expr, ExprKind, ListItem, ListPatternEntry, MapEntry, MapPatternEntry,
		MatchArm, Pattern, RangeKind, RangePatternKind, Statement, StringPart, StructPatternField,
	},
	ty::{GenericArg, GenericParam, Type},
};
use nymph_diagnostics::Diagnostic;

use crate::annotate::Checked;
use crate::check::{EntryMode, check_module_impl};

/// The `NodeId` offset applied to the *first* (`index == 0`) prelude module an
/// [`offset_module`] call clones. Chosen well below `NodeId::DUMMY` (`u32::MAX`)
/// and well above anything a real parse could mint (the parser's `next_id` starts
/// at 0 per parse — see `nymph_syntax::parser::Parser::new`), so a prelude id and
/// a user id can never collide once flattened into one combined module. Every
/// subsequent prelude module (`index >= 1`) is offset by this plus `index *
/// NODE_STRIDE` (see [`node_base_for`]), so distinct prelude modules flattened
/// together in one [`check_module_with_prelude`] call get disjoint ranges too,
/// not just a range disjoint from the user module's.
pub const NODE_BASE: u32 = 1 << 30;

/// How many `NodeId`s each prelude module past the first reserves, so
/// `NODE_BASE + index * NODE_STRIDE` never runs into the next module's block.
/// Chosen far above any real single file's node count (the `ops/mod.nym`
/// prelude, ~330 lines, mints on the order of a few thousand) while still
/// leaving room for thousands of prelude modules before
/// `NODE_BASE + index * NODE_STRIDE` would overflow `u32` (see [`node_base_for`]'s
/// checked arithmetic, which panics rather than silently wrap if it ever would).
const NODE_STRIDE: u32 = 1 << 20;

/// The `Span` offset applied to the *first* (`index == 0`) prelude module an
/// [`offset_module`] call clones. Also doubles as the prelude-origin marker: any
/// diagnostic whose primary span is `>= SPAN_BASE` originated inside *some*
/// prelude clone, not the user's source, and must never be shown to the user
/// (see this module's doc comment). Every subsequent prelude module (`index >=
/// 1`) is offset by this plus `index * SPAN_STRIDE` (see [`span_base_for`]).
/// On 64-bit targets there is effectively unlimited headroom. On a 32-bit
/// target (e.g. `wasm32`, for the browser playground) `usize` is 32 bits, so
/// `1 << 32` / `1 << 40` would overflow at const-eval; there we use smaller
/// bases that still sit far above any realistic source span (256 MiB) with
/// ample per-module stride (16 MiB) — no snippet a playground compiles comes
/// anywhere near either bound.
#[cfg(target_pointer_width = "64")]
pub const SPAN_BASE: usize = 1 << 32;
#[cfg(not(target_pointer_width = "64"))]
pub const SPAN_BASE: usize = 1 << 28;

/// How many `Span` positions each prelude module past the first reserves. See
/// [`NODE_STRIDE`]'s doc comment for the same reasoning, one dimension up.
#[cfg(target_pointer_width = "64")]
const SPAN_STRIDE: usize = 1 << 40;
#[cfg(not(target_pointer_width = "64"))]
const SPAN_STRIDE: usize = 1 << 24;

/// The `NodeId` offset for the prelude module at `index` (its position in
/// [`check_module_with_prelude`]'s `prelude` slice).
fn node_base_for(index: usize) -> u32 {
	let index = u32::try_from(index).unwrap_or_else(|_| {
		panic!("stdlib linkage: prelude index {index} does not fit in a u32 NodeId offset")
	});
	let stride = NODE_STRIDE.checked_mul(index).unwrap_or_else(|| {
		panic!("stdlib linkage: too many prelude modules ({index}) to offset without overflowing NodeId's u32 range")
	});
	NODE_BASE.checked_add(stride).unwrap_or_else(|| {
		panic!("stdlib linkage: too many prelude modules ({index}) to offset without overflowing NodeId's u32 range")
	})
}

/// The `Span` offset for the prelude module at `index`. See [`node_base_for`];
/// `usize` overflowing here in practice never happens (see [`SPAN_STRIDE`]'s doc
/// comment), but the arithmetic is still checked so a future change can't
/// silently wrap into a colliding range instead of panicking loudly.
fn span_base_for(index: usize) -> usize {
	let stride = SPAN_STRIDE.checked_mul(index).unwrap_or_else(|| {
		panic!("stdlib linkage: too many prelude modules ({index}) to offset without overflowing Span's usize range")
	});
	SPAN_BASE.checked_add(stride).unwrap_or_else(|| {
		panic!("stdlib linkage: too many prelude modules ({index}) to offset without overflowing Span's usize range")
	})
}

thread_local! {
	/// The `NodeId`/`Span` offset the [`offset_module`] call currently walking a
	/// prelude module's tree is applying — read by [`node_id`]/[`span`]. Set once,
	/// synchronously, at the top of [`offset_module`] before it recurses through
	/// that module's declarations, so every recursive helper in this file (there
	/// are some sixty of them, one per AST node shape, none of which take an
	/// offset parameter) sees the right value for whichever module is currently
	/// being cloned — without threading an extra parameter through all of them.
	/// Thread-local rather than a shared global: [`check_module_with_prelude`]
	/// (and this whole facade) has no other shared mutable state, and a
	/// thread-local keeps concurrent callers on different threads fully
	/// independent, matching that fact.
	static CUR_NODE_BASE: Cell<u32> = const { Cell::new(NODE_BASE) };
	static CUR_SPAN_BASE: Cell<usize> = const { Cell::new(SPAN_BASE) };
}

fn span(s: Span) -> Span {
	let base = CUR_SPAN_BASE.with(Cell::get);
	Span::new(s.start + base, s.end + base)
}

fn spanned<T>(s: Spanned<T>, f: impl FnOnce(T) -> T) -> Spanned<T> {
	Spanned(f(s.0), span(s.1))
}

/// Offset a `Spanned` leaf whose payload needs no further recursion (a literal, a
/// `bool`, a char, …): only its span moves.
fn spanned_leaf<T>(s: Spanned<T>) -> Spanned<T> {
	Spanned(s.0, span(s.1))
}

fn ident(i: Ident) -> Ident {
	spanned_leaf(i)
}

/// Offset a `NodeId`, except [`NodeId::DUMMY`] (the sentinel for synthetic nodes
/// built outside the parser) which is left untouched — `Annotations::record`
/// already skips it by identity, and offsetting it would turn it into an
/// ordinary, colliding id instead of the one value guaranteed never to collide.
fn node_id(i: NodeId) -> NodeId {
	if i == NodeId::DUMMY {
		i
	} else {
		let base = CUR_NODE_BASE.with(Cell::get);
		NodeId(i.0 + base)
	}
}

fn opt_ident(i: Option<Ident>) -> Option<Ident> {
	i.map(ident)
}

// ── Types (ty.rs) ────────────────────────────────────────────────────────────

fn ty(t: Type) -> Type {
	match t {
		Type::Int
		| Type::UInt
		| Type::Float
		| Type::Char
		| Type::String
		| Type::Boolean
		| Type::Void
		| Type::Never
		| Type::SelfType
		| Type::Infer => t,
		Type::Intersection(a, b) => {
			Type::Intersection(Box::new(spanned_type(*a)), Box::new(spanned_type(*b)))
		}
		Type::List(inner) => Type::List(Box::new(spanned_type(*inner))),
		Type::Tuple(elems) => Type::Tuple(elems.into_iter().map(spanned_type).collect()),
		Type::Map(k, v) => Type::Map(Box::new(spanned_type(*k)), Box::new(spanned_type(*v))),
		Type::Function {
			params,
			return_type,
		} => Type::Function {
			params: params
				.into_iter()
				.map(|(name, t)| (opt_ident(name), spanned_type(t)))
				.collect(),
			return_type: Box::new(spanned_type(*return_type)),
		},
		Type::Reference { name, generics } => Type::Reference {
			name: ident(name),
			generics: generics.into_iter().map(generic_arg).collect(),
		},
		Type::Grouped(inner) => Type::Grouped(Box::new(spanned_type(*inner))),
		Type::Mut(inner) => Type::Mut(Box::new(spanned_type(*inner))),
	}
}

fn spanned_type(t: Spanned<Type>) -> Spanned<Type> {
	spanned(t, ty)
}

fn generic_arg(g: Spanned<GenericArg>) -> Spanned<GenericArg> {
	spanned(g, |g| GenericArg {
		value: spanned_type(g.value),
		name: opt_ident(g.name),
	})
}

fn generic_param(g: Spanned<GenericParam>) -> Spanned<GenericParam> {
	spanned(g, |g| GenericParam {
		name: ident(g.name),
		constraint: g.constraint.map(spanned_type),
		default: g.default.map(spanned_type),
	})
}

// ── Patterns (expr.rs) ───────────────────────────────────────────────────────

fn spanned_pattern(p: Spanned<Pattern>) -> Spanned<Pattern> {
	spanned(p, pattern)
}

fn pattern(p: Pattern) -> Pattern {
	match p {
		Pattern::Int(v) => Pattern::Int(spanned_leaf(v)),
		Pattern::UInt(v) => Pattern::UInt(spanned_leaf(v)),
		Pattern::Float(v) => Pattern::Float(spanned_leaf(v)),
		Pattern::Char(v) => Pattern::Char(spanned_leaf(v)),
		Pattern::String(parts) => Pattern::String(parts.into_iter().map(spanned_leaf).collect()),
		Pattern::Boolean(v) => Pattern::Boolean(spanned_leaf(v)),
		Pattern::Binding { name, inner } => Pattern::Binding {
			name: ident(name),
			inner: Box::new(spanned_pattern(*inner)),
		},
		Pattern::List(items) => Pattern::List(items.into_iter().map(list_pattern_entry).collect()),
		Pattern::Tuple(items) => Pattern::Tuple(items.into_iter().map(list_pattern_entry).collect()),
		Pattern::Map(entries) => Pattern::Map(entries.into_iter().map(map_pattern_entry).collect()),
		Pattern::Range(r) => Pattern::Range(range_pattern_kind(r)),
		Pattern::Struct { path, fields } => Pattern::Struct {
			path: path.into_iter().map(ident).collect(),
			fields: fields.into_iter().map(struct_pattern_field).collect(),
		},
		Pattern::Placeholder => Pattern::Placeholder,
		Pattern::Union(a, b) => {
			Pattern::Union(Box::new(spanned_pattern(*a)), Box::new(spanned_pattern(*b)))
		}
		Pattern::Grouped(inner) => Pattern::Grouped(Box::new(spanned_pattern(*inner))),
	}
}

fn list_pattern_entry(e: Spanned<ListPatternEntry>) -> Spanned<ListPatternEntry> {
	spanned(e, |e| match e {
		ListPatternEntry::Item(p) => ListPatternEntry::Item(spanned_pattern(p)),
		ListPatternEntry::Rest(name) => ListPatternEntry::Rest(opt_ident(name)),
	})
}

fn map_pattern_entry(e: Spanned<MapPatternEntry>) -> Spanned<MapPatternEntry> {
	spanned(e, |e| match e {
		MapPatternEntry::Entry(k, v) => MapPatternEntry::Entry(spanned_pattern(k), spanned_pattern(v)),
		MapPatternEntry::Rest(name) => MapPatternEntry::Rest(opt_ident(name)),
	})
}

fn range_pattern_kind(r: RangePatternKind) -> RangePatternKind {
	match r {
		RangePatternKind::From(p) => RangePatternKind::From(Box::new(spanned_pattern(*p))),
		RangePatternKind::To(p) => RangePatternKind::To(Box::new(spanned_pattern(*p))),
		RangePatternKind::Exclusive { min, max } => RangePatternKind::Exclusive {
			min: Box::new(spanned_pattern(*min)),
			max: Box::new(spanned_pattern(*max)),
		},
		RangePatternKind::ToInclusive(p) => {
			RangePatternKind::ToInclusive(Box::new(spanned_pattern(*p)))
		}
		RangePatternKind::Inclusive { min, max } => RangePatternKind::Inclusive {
			min: Box::new(spanned_pattern(*min)),
			max: Box::new(spanned_pattern(*max)),
		},
	}
}

fn struct_pattern_field(f: Spanned<StructPatternField>) -> Spanned<StructPatternField> {
	spanned(f, |f| match f {
		StructPatternField::Value { name, value } => StructPatternField::Value {
			name: ident(name),
			value: spanned_pattern(value),
		},
		StructPatternField::Named(name) => StructPatternField::Named(ident(name)),
		StructPatternField::Positional(value) => {
			StructPatternField::Positional(spanned_pattern(value))
		}
		StructPatternField::Rest => StructPatternField::Rest,
	})
}

// ── Expressions (expr.rs) ────────────────────────────────────────────────────

fn expr(e: Expr) -> Expr {
	Expr {
		kind: expr_kind(e.kind),
		span: span(e.span),
		id: node_id(e.id),
	}
}

// `e` is deliberately taken and returned boxed: every AST call site holds a
// `Box<Expr>` field (`lhs`/`rhs`/`body`, …), so unboxing to `Expr` would only
// push the (identical) allocate-transform-reallocate work onto each caller,
// not eliminate it. `offset_module` (this whole tree-clone) runs once per
// `check_module_with_prelude` call, over a small (~330-line) prelude — not a
// hot path — so the extra realloc clippy flags here is not perf-sensitive.
#[allow(clippy::boxed_local)]
fn box_expr(e: Box<Expr>) -> Box<Expr> {
	Box::new(expr(*e))
}

fn opt_box_expr(e: Option<Box<Expr>>) -> Option<Box<Expr>> {
	e.map(box_expr)
}

fn expr_kind(k: ExprKind) -> ExprKind {
	match k {
		ExprKind::Int(v) => ExprKind::Int(spanned_leaf(v)),
		ExprKind::UInt(v) => ExprKind::UInt(spanned_leaf(v)),
		ExprKind::Float(v) => ExprKind::Float(spanned_leaf(v)),
		ExprKind::Char(v) => ExprKind::Char(spanned_leaf(v)),
		ExprKind::String(parts) => {
			ExprKind::String(parts.into_iter().map(spanned_string_part).collect())
		}
		ExprKind::Boolean(v) => ExprKind::Boolean(spanned_leaf(v)),
		ExprKind::Identifier(i) => ExprKind::Identifier(ident(i)),
		ExprKind::AnonymousParam(p) => ExprKind::AnonymousParam(p),
		ExprKind::List(items) => ExprKind::List(items.into_iter().map(list_item).collect()),
		ExprKind::Tuple(items) => ExprKind::Tuple(items.into_iter().map(list_item).collect()),
		ExprKind::Map(entries) => ExprKind::Map(entries.into_iter().map(map_entry).collect()),
		ExprKind::Range(r) => ExprKind::Range(range_kind(r)),
		ExprKind::Call {
			func,
			generics,
			args,
		} => ExprKind::Call {
			func: box_expr(func),
			generics: generics.into_iter().map(generic_arg).collect(),
			args: args.into_iter().map(call_arg).collect(),
		},
		ExprKind::MemberAccess {
			parent,
			member,
			optional,
		} => ExprKind::MemberAccess {
			parent: box_expr(parent),
			member: ident(member),
			optional,
		},
		ExprKind::IndexAccess {
			parent,
			index,
			optional,
		} => ExprKind::IndexAccess {
			parent: box_expr(parent),
			index: box_expr(index),
			optional,
		},
		ExprKind::Closure {
			params,
			generics,
			return_type,
			body,
		} => ExprKind::Closure {
			params: params.into_iter().map(closure_param).collect(),
			generics: generics.into_iter().map(generic_param).collect(),
			return_type: return_type.map(spanned_type),
			body: box_expr(body),
		},
		ExprKind::PrefixOp { op, value } => ExprKind::PrefixOp {
			op,
			value: box_expr(value),
		},
		ExprKind::PostfixOp { op, value } => ExprKind::PostfixOp {
			op,
			value: box_expr(value),
		},
		ExprKind::BinaryOp { lhs, op, rhs } => ExprKind::BinaryOp {
			lhs: box_expr(lhs),
			op,
			rhs: box_expr(rhs),
		},
		ExprKind::TypeOp { lhs, op, rhs } => ExprKind::TypeOp {
			lhs: box_expr(lhs),
			op,
			rhs: spanned_type(rhs),
		},
		ExprKind::PatternOp { lhs, op, rhs } => ExprKind::PatternOp {
			lhs: box_expr(lhs),
			op,
			rhs: spanned_pattern(rhs),
		},
		ExprKind::AssignOp { lhs, op, rhs } => ExprKind::AssignOp {
			lhs: box_expr(lhs),
			op,
			rhs: box_expr(rhs),
		},
		ExprKind::Return { value, label } => ExprKind::Return {
			value: opt_box_expr(value),
			label: opt_ident(label),
		},
		ExprKind::Break { value, label } => ExprKind::Break {
			value: opt_box_expr(value),
			label: opt_ident(label),
		},
		ExprKind::Continue { label } => ExprKind::Continue {
			label: opt_ident(label),
		},
		ExprKind::While {
			condition,
			body,
			label,
		} => ExprKind::While {
			condition: box_expr(condition),
			body: box_expr(body),
			label: opt_ident(label),
		},
		ExprKind::For {
			variable,
			iterable,
			body,
			label,
		} => ExprKind::For {
			variable: spanned_pattern(variable),
			iterable: box_expr(iterable),
			body: box_expr(body),
			label: opt_ident(label),
		},
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => ExprKind::If {
			condition: box_expr(condition),
			then: box_expr(then),
			otherwise: opt_box_expr(otherwise),
		},
		ExprKind::Match { value, arms } => ExprKind::Match {
			value: box_expr(value),
			arms: arms.into_iter().map(match_arm).collect(),
		},
		ExprKind::This => ExprKind::This,
		ExprKind::Block { body, label } => ExprKind::Block {
			body: body.into_iter().map(statement).collect(),
			label: opt_ident(label),
		},
		ExprKind::Grouped(inner) => ExprKind::Grouped(box_expr(inner)),
	}
}

fn spanned_string_part(s: Spanned<StringPart>) -> Spanned<StringPart> {
	spanned(s, |s| match s {
		StringPart::Text(t) => StringPart::Text(t),
		StringPart::EscapeSequence(e) => StringPart::EscapeSequence(e),
		StringPart::InterpolatedExpr(e) => StringPart::InterpolatedExpr(expr(e)),
	})
}

fn list_item(i: Spanned<ListItem>) -> Spanned<ListItem> {
	spanned(i, |i| match i {
		ListItem::Expr(e) => ListItem::Expr(expr(e)),
		ListItem::Spread(e) => ListItem::Spread(expr(e)),
	})
}

fn map_entry(m: Spanned<MapEntry>) -> Spanned<MapEntry> {
	spanned(m, |m| match m {
		MapEntry::Entry(k, v) => MapEntry::Entry(expr(k), expr(v)),
		MapEntry::Spread(e) => MapEntry::Spread(expr(e)),
	})
}

fn range_kind(r: RangeKind) -> RangeKind {
	match r {
		RangeKind::From(e) => RangeKind::From(box_expr(e)),
		RangeKind::To(e) => RangeKind::To(box_expr(e)),
		RangeKind::Exclusive { min, max } => RangeKind::Exclusive {
			min: box_expr(min),
			max: box_expr(max),
		},
		RangeKind::ToInclusive(e) => RangeKind::ToInclusive(box_expr(e)),
		RangeKind::Inclusive { min, max } => RangeKind::Inclusive {
			min: box_expr(min),
			max: box_expr(max),
		},
	}
}

fn closure_param(c: Spanned<ClosureParam>) -> Spanned<ClosureParam> {
	spanned(c, |c| ClosureParam {
		name: spanned_pattern(c.name),
		type_: c.type_.map(spanned_type),
		mutable: c.mutable,
		spread: c.spread,
	})
}

fn call_arg(c: Spanned<CallArg>) -> Spanned<CallArg> {
	spanned(c, |c| CallArg {
		value: expr(c.value),
		name: opt_ident(c.name),
		spread: c.spread,
	})
}

fn match_arm(m: MatchArm) -> MatchArm {
	MatchArm {
		pattern: spanned_pattern(m.pattern),
		guard: m.guard.map(expr),
		body: expr(m.body),
	}
}

fn statement(s: Spanned<Statement>) -> Spanned<Statement> {
	spanned(s, |s| match s {
		Statement::Expr(e) => Statement::Expr(expr(e)),
		Statement::Let { meta, value } => Statement::Let {
			meta: let_declaration(meta),
			value: expr(value),
		},
	})
}

// ── Declarations (decl.rs) ───────────────────────────────────────────────────

fn let_declaration(l: LetDeclaration) -> LetDeclaration {
	LetDeclaration {
		kind: l.kind,
		name: spanned_pattern(l.name),
		type_: l.type_.map(spanned_type),
	}
}

fn func_declaration(f: FuncDeclaration) -> FuncDeclaration {
	FuncDeclaration {
		name: ident(f.name),
		kind: f.kind,
		generics: f.generics.into_iter().map(generic_param).collect(),
		params: f.params.into_iter().map(func_param).collect(),
		return_type: f.return_type.map(spanned_type),
	}
}

fn func_param(p: Spanned<FuncParam>) -> Spanned<FuncParam> {
	spanned(p, |p| FuncParam {
		name: spanned_pattern(p.name),
		type_: spanned_type(p.type_),
		mutable: p.mutable,
		spread: p.spread,
	})
}

fn struct_field(f: Spanned<StructField>) -> Spanned<StructField> {
	spanned(f, |f| StructField {
		visibility: f.visibility,
		name: ident(f.name),
		type_: spanned_type(f.type_),
		default: f.default.map(expr),
	})
}

fn enum_variant(e: Spanned<EnumVariant>) -> Spanned<EnumVariant> {
	spanned(e, |e| EnumVariant {
		name: ident(e.name),
		fields: e.fields.into_iter().map(struct_field).collect(),
	})
}

fn impl_member(m: Spanned<ImplMember>) -> Spanned<ImplMember> {
	spanned(m, |m| match m {
		ImplMember::Let {
			visibility,
			meta,
			value,
		} => ImplMember::Let {
			visibility,
			meta: let_declaration(meta),
			value: expr(value),
		},
		ImplMember::ExternalLet(v, s, meta) => ImplMember::ExternalLet(v, s, let_declaration(meta)),
		ImplMember::Func {
			visibility,
			meta,
			body,
		} => ImplMember::Func {
			visibility,
			meta: func_declaration(meta),
			body: expr(body),
		},
		ImplMember::ExternalFunc(v, s, meta) => ImplMember::ExternalFunc(v, s, func_declaration(meta)),
	})
}

fn interface_element(e: Spanned<InterfaceElement>) -> Spanned<InterfaceElement> {
	spanned(e, |e| match e {
		InterfaceElement::Let { meta, value } => InterfaceElement::Let {
			meta: let_declaration(meta),
			value: value.map(expr),
		},
		InterfaceElement::Func { meta, body } => InterfaceElement::Func {
			meta: func_declaration(meta),
			body: body.map(expr),
		},
	})
}

fn interface_member(m: Spanned<InterfaceMember>) -> Spanned<InterfaceMember> {
	spanned(m, |m| match m {
		InterfaceMember::Element(e) => InterfaceMember::Element(Box::new(interface_element(*e))),
		InterfaceMember::Impl {
			interface,
			generics,
			members,
		} => InterfaceMember::Impl {
			interface: (
				ident(interface.0),
				interface.1.into_iter().map(generic_arg).collect(),
			),
			generics: generics.into_iter().map(generic_param).collect(),
			members: members.into_iter().map(impl_member).collect(),
		},
	})
}

fn struct_impl(m: Spanned<StructImpl>) -> Spanned<StructImpl> {
	spanned(m, |m| StructImpl {
		interface: (
			ident(m.interface.0),
			m.interface.1.into_iter().map(generic_arg).collect(),
		),
		generics: m.generics.into_iter().map(generic_param).collect(),
		members: m.members.into_iter().map(impl_member).collect(),
	})
}

fn import_root(r: ImportRoot) -> ImportRoot {
	match r {
		ImportRoot::Package(i) => ImportRoot::Package(ident(i)),
		ImportRoot::Project => ImportRoot::Project,
		ImportRoot::Current => ImportRoot::Current,
		ImportRoot::Parent => ImportRoot::Parent,
	}
}

fn declaration(d: Declaration) -> Declaration {
	match d {
		Declaration::Import {
			root,
			path,
			alias,
			idents,
		} => Declaration::Import {
			root: import_root(root),
			path: path.into_iter().map(ident).collect(),
			alias: opt_ident(alias),
			idents: idents.map(|v| {
				v.into_iter()
					.map(|(a, b)| (ident(a), opt_ident(b)))
					.collect()
			}),
		},
		Declaration::Let {
			visibility,
			meta,
			value,
		} => Declaration::Let {
			visibility,
			meta: let_declaration(meta),
			value: expr(value),
		},
		Declaration::ExternalLet(v, s, meta) => Declaration::ExternalLet(v, s, let_declaration(meta)),
		Declaration::Func {
			visibility,
			meta,
			body,
		} => Declaration::Func {
			visibility,
			meta: func_declaration(meta),
			body: expr(body),
		},
		Declaration::ExternalFunc(v, s, meta) => {
			Declaration::ExternalFunc(v, s, func_declaration(meta))
		}
		Declaration::TypeAlias {
			visibility,
			meta,
			value,
		} => Declaration::TypeAlias {
			visibility,
			meta: TypeAliasDeclaration {
				name: ident(meta.name),
				generics: meta.generics.into_iter().map(generic_param).collect(),
			},
			value: spanned_type(value),
		},
		Declaration::Struct {
			visibility,
			name,
			generics,
			fields,
			members,
			impls,
		} => Declaration::Struct {
			visibility,
			name: ident(name),
			generics: generics.into_iter().map(generic_param).collect(),
			fields: fields.into_iter().map(struct_field).collect(),
			members: members.into_iter().map(impl_member).collect(),
			impls: impls.into_iter().map(struct_impl).collect(),
		},
		Declaration::Enum {
			visibility,
			name,
			generics,
			variants,
			members,
			impls,
		} => Declaration::Enum {
			visibility,
			name: ident(name),
			generics: generics.into_iter().map(generic_param).collect(),
			variants: variants.into_iter().map(enum_variant).collect(),
			members: members.into_iter().map(impl_member).collect(),
			impls: impls.into_iter().map(struct_impl).collect(),
		},
		Declaration::Namespace {
			visibility,
			name,
			members,
		} => Declaration::Namespace {
			visibility,
			name: ident(name),
			members: members.into_iter().map(impl_member).collect(),
		},
		Declaration::Interface {
			visibility,
			name,
			generics,
			super_interfaces,
			members,
		} => Declaration::Interface {
			visibility,
			name: ident(name),
			generics: generics.into_iter().map(generic_param).collect(),
			super_interfaces: super_interfaces
				.into_iter()
				.map(|si| {
					spanned(si, |(n, gs)| {
						(ident(n), gs.into_iter().map(generic_arg).collect())
					})
				})
				.collect(),
			members: members.into_iter().map(interface_member).collect(),
		},
		Declaration::Impl {
			visibility,
			generics,
			mutable,
			type_,
			members,
		} => Declaration::Impl {
			visibility,
			generics: generics.into_iter().map(generic_param).collect(),
			mutable,
			type_: spanned_type(type_),
			members: members.into_iter().map(impl_member).collect(),
		},
		Declaration::ImplFor {
			visibility,
			generics,
			mutable,
			type_,
			for_interface,
			members,
		} => Declaration::ImplFor {
			visibility,
			generics: generics.into_iter().map(generic_param).collect(),
			mutable,
			type_: spanned_type(type_),
			for_interface: (
				ident(for_interface.0),
				for_interface.1.into_iter().map(generic_arg).collect(),
			),
			members: members.into_iter().map(impl_member).collect(),
		},
	}
}

/// Clone `module`, shifting every `NodeId`/`Span` throughout the whole tree by an
/// offset unique to `index` — `module`'s position in
/// [`check_module_with_prelude`]'s `prelude` slice. See this file's doc comment
/// for why offsetting exists at all; see [`node_base_for`]/[`span_base_for`] for
/// why per-index offsets are necessary once more than one prelude module is
/// flattened into the same combined program (a fixed, index-independent offset
/// would let two prelude modules collide with *each other*, not just avoid
/// colliding with the user module).
pub fn offset_module(module: &Module, index: usize) -> Module {
	CUR_NODE_BASE.with(|c| c.set(node_base_for(index)));
	CUR_SPAN_BASE.with(|c| c.set(span_base_for(index)));
	Module {
		members: module.members.iter().cloned().map(declaration).collect(),
		path: module.path.clone(),
	}
}

// ── The facade itself ────────────────────────────────────────────────────────

/// Replace any label pointing into a prelude clone (`span.start >= SPAN_BASE`)
/// with a note instead of dropping the diagnostic outright — used for a
/// user-anchored diagnostic (e.g. `Redefinition`) whose secondary label would
/// otherwise leak a stdlib-internal span.
fn scrub_prelude_labels(mut diag: Diagnostic) -> Diagnostic {
	let mut kept = Vec::with_capacity(diag.labels.len());
	for label in diag.labels {
		if label.span.start >= SPAN_BASE {
			diag
				.notes
				.push("previously defined in the std prelude".into());
		} else {
			kept.push(label);
		}
	}
	diag.labels = kept;
	diag
}

/// Check `module` with `prelude`'s declarations flattened ahead of its own —
/// additive: [`crate::check_module`]/[`crate::check_module_entry`] are completely
/// unaffected by this function's existence.
///
/// `prelude` modules are cloned through [`offset_module`] before flattening, so
/// their `NodeId`s and `Span`s can never collide with `module`'s own (see this
/// module's doc comment). Diagnostics anchored inside a prelude clone (a stdlib
/// source bug, or fallout from a user shadowing a prelude name) are dropped rather
/// than shown to the user — a user program must never see a stdlib span; the
/// `Redefinition` diagnostic anchored at the user's own shadowing declaration
/// (with its "first defined here" label rewritten to a note) is what carries the
/// signal instead.
///
/// Library-mode counterpart — mirrors [`crate::check_module`]. See
/// [`check_module_entry_with_prelude`] for the entry-mode counterpart, which
/// additionally requires the combined module to declare a valid top-level
/// `main` (mirrors [`crate::check_module_entry`]).
pub fn check_module_with_prelude(module: &Module, prelude: &[Module]) -> Checked {
	check_module_with_prelude_impl(module, prelude, EntryMode::Library)
}

/// Entry-mode counterpart of [`check_module_with_prelude`] — mirrors
/// [`crate::check_module_entry`]: identical, except the combined module
/// (prelude + `module`) is additionally required to declare a valid top-level
/// `main`. The prelude itself declares no `main`, so this is satisfied
/// precisely when `module` declares one, exactly as in the prelude-less case.
pub fn check_module_entry_with_prelude(module: &Module, prelude: &[Module]) -> Checked {
	check_module_with_prelude_impl(module, prelude, EntryMode::Entry)
}

fn check_module_with_prelude_impl(
	module: &Module,
	prelude: &[Module],
	entry: EntryMode,
) -> Checked {
	let mut members = Vec::new();

	for (index, p) in prelude.iter().enumerate() {
		let offset = offset_module(p, index);
		for decl in offset.members {
			if matches!(decl, Declaration::Import { .. }) {
				continue;
			}
			members.push(decl);
		}
	}
	for decl in &module.members {
		if matches!(decl, Declaration::Import { .. }) {
			continue;
		}
		members.push(decl.clone());
	}

	let combined = Module {
		members,
		path: module.path.clone(),
	};
	let mut checked = check_module_impl(&combined, entry);

	// Diagnostics anchored inside a prelude clone (`span.start >= SPAN_BASE`)
	// are always dropped, never shown to the user — a user program must never
	// see a prelude span. Originally this was believed to only ever fire in
	// one of two cases: the user shadows a prelude name (fallout, the
	// `Redefinition` diagnostic anchored at the user's own declaration is what
	// carries the signal instead — see `scrub_prelude_labels`), or a genuine
	// bug in the prelude source itself (worth flagging loudly during
	// development, hence a `debug_assert` used to guard this). That second
	// case is no longer a safe assumption once `prelude` can contain more than
	// the curated, trusted `std.ops` slice: `nymph-compiler`'s project driver
	// flattens every transitively-imported (arbitrary, user-authored) module
	// in as a "prelude" entry too (see `nymph-compiler/src/project/mod.rs`'s
	// `prelude_slice`), so a genuine type error in an imported dependency
	// module reappears here with a prelude-offset span and no user
	// shadowing — expected, not a checker bug: that dependency was (or will
	// be) checked and reported on its own turn by that driver, so silently
	// dropping the duplicate, re-offset copy here is correct, not a bug to
	// assert against.
	checked.diags = checked
		.diags
		.into_iter()
		.filter(|d| d.span.start < SPAN_BASE)
		.map(scrub_prelude_labels)
		.collect();
	checked
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every `Expr` node's `(NodeId, Span)` reachable from a function's body,
	/// collected depth-first. Enough coverage for this file's own offsetting
	/// tests below (small, synthetic, single-function modules) without needing a
	/// general-purpose AST visitor — mirrors `tests/prelude.rs`'s
	/// `collect_binary_ops` helper, generalized to every expression shape instead
	/// of just `BinaryOp`.
	fn collect_expr_ids(e: &Expr, out: &mut Vec<(NodeId, Span)>) {
		out.push((e.id, e.span));
		match &e.kind {
			ExprKind::BinaryOp { lhs, rhs, .. } => {
				collect_expr_ids(lhs, out);
				collect_expr_ids(rhs, out);
			}
			ExprKind::Grouped(inner) => collect_expr_ids(inner, out),
			ExprKind::Block { body, .. } => {
				for stmt in body {
					match &stmt.0 {
						Statement::Expr(inner) => collect_expr_ids(inner, out),
						Statement::Let { value, .. } => collect_expr_ids(value, out),
					}
				}
			}
			_ => {}
		}
	}

	/// Every `(NodeId, Span)` pair reachable from any top-level `func`'s body in
	/// `module`.
	fn collect_module_expr_ids(module: &Module) -> Vec<(NodeId, Span)> {
		let mut out = Vec::new();
		for decl in &module.members {
			if let Declaration::Func { body, .. } = decl {
				collect_expr_ids(body, &mut out);
			}
		}
		out
	}

	fn parse(source: &str) -> Module {
		let parsed = nymph_syntax::parse_module(source, "<test>");
		assert!(
			parsed.diagnostics.iter().all(|d| !d.is_error()),
			"test source failed to parse: {:?}",
			parsed.diagnostics
		);
		parsed.tree
	}

	#[test]
	fn distinct_prelude_modules_get_disjoint_node_and_span_offsets() {
		// Two prelude modules parsed *independently* from the exact same source
		// text: each parse's `NodeId`/`Span` counters restart at 0 (see
		// `nymph_syntax::parser::Parser::new`), so `a` and `b` carry identical
		// pre-offset ids/spans by construction — the worst case for the bug this
		// pins (Finding 1: `check_module_with_prelude` reused the exact same
		// `NODE_BASE`/`SPAN_BASE` for every module in its `prelude` slice, so two
		// prelude modules flattened together in one call collided with EACH
		// OTHER, not just avoided colliding with the user module).
		let source = "func f(): int = 1 + 2";
		let a = parse(source);
		let b = parse(source);

		let offset_a = offset_module(&a, 0);
		let offset_b = offset_module(&b, 1);

		let ids_a = collect_module_expr_ids(&offset_a);
		let ids_b = collect_module_expr_ids(&offset_b);
		assert_eq!(ids_a.len(), 3, "sanity: `1 + 2` is 3 expr nodes");
		assert_eq!(ids_b.len(), 3);

		for (id_a, span_a) in &ids_a {
			for (id_b, span_b) in &ids_b {
				assert_ne!(
					id_a, id_b,
					"prelude module 0 and module 1 got a colliding NodeId {id_a:?} — \
					 the same NODE_BASE was reused for every module in the slice"
				);
				assert_ne!(
					span_a, span_b,
					"prelude module 0 and module 1 got a colliding Span {span_a:?} — \
					 the same SPAN_BASE was reused for every module in the slice"
				);
			}
		}

		// Both still land at or above the single-module `SPAN_BASE` — the
		// diagnostic-partitioning threshold in `check_module_with_prelude` must
		// keep classifying every prelude module's own diagnostics as
		// prelude-internal, not just the first one's.
		for (_, span) in ids_a.iter().chain(&ids_b) {
			assert!(span.start >= SPAN_BASE);
		}
	}

	#[test]
	fn a_single_prelude_module_keeps_the_original_base_offset() {
		// Backward compatibility: `index == 0` (the only case any current call
		// site — `check_module_with_prelude` with today's single-element prelude
		// slice — ever exercises) must offset exactly as it always did, so this
		// fix changes nothing observable for the one prelude module in
		// production use today.
		let source = "func f(): int = 1";
		let module = parse(source);
		let (unoffset_id, unoffset_span) = collect_module_expr_ids(&module)[0];

		let offset = offset_module(&module, 0);
		let (id, span) = collect_module_expr_ids(&offset)[0];

		assert_eq!(id, NodeId(unoffset_id.0 + NODE_BASE));
		assert_eq!(
			span,
			Span::new(
				unoffset_span.start + SPAN_BASE,
				unoffset_span.end + SPAN_BASE
			)
		);
	}

	#[test]
	fn eight_prelude_modules_all_get_pairwise_disjoint_offsets() {
		// Generalizes `distinct_prelude_modules_get_disjoint_node_and_span_offsets`
		// from 2 to 8 — the exact number of modules `nymph-compiler`'s
		// `core_prelude()` flattens as the ambient `core` prelude (core/std
		// split, Slice A: `ops`, `default`, `option`, `result`, `convert`,
		// `iter`, `iter/iterable`, `range`). All 8 are parsed from the *same*
		// source text (so every pre-offset `NodeId`/`Span` starts identical
		// across all 8 — the worst case), offset by their slice index 0..7, and
		// checked pairwise: no two of the 8 may share a `NodeId` or `Span`,
		// proving the offset machinery scales cleanly to the real core-module
		// count and not just the 2-module case above.
		let source = "func f(): int = 1 + 2";
		let modules: Vec<Module> = (0..8).map(|_| parse(source)).collect();
		let offset: Vec<Module> = modules
			.iter()
			.enumerate()
			.map(|(i, m)| offset_module(m, i))
			.collect();
		let ids: Vec<Vec<(NodeId, Span)>> = offset.iter().map(collect_module_expr_ids).collect();
		for group in &ids {
			assert_eq!(group.len(), 3, "sanity: `1 + 2` is 3 expr nodes");
		}

		for i in 0..ids.len() {
			for j in (i + 1)..ids.len() {
				for (id_i, span_i) in &ids[i] {
					for (id_j, span_j) in &ids[j] {
						assert_ne!(
							id_i, id_j,
							"prelude module {i} and module {j} got a colliding NodeId {id_i:?}"
						);
						assert_ne!(
							span_i, span_j,
							"prelude module {i} and module {j} got a colliding Span {span_i:?}"
						);
					}
				}
			}
		}

		// Every one of the 8 still lands at or above `SPAN_BASE` — the
		// diagnostic-partitioning threshold in `check_module_with_prelude_impl`
		// must keep classifying every one of the 8 core modules' own
		// diagnostics as prelude-internal, not just the first couple.
		for group in &ids {
			for (_, span) in group {
				assert!(span.start >= SPAN_BASE);
			}
		}
	}
}
