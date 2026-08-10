//! A minimal position -> type query over a checked module, for the language
//! server's `textDocument/hover` (`nymph-lsp`). Purely additive: it reads
//! only the already-public [`Checked`] result, including the exact [`DefMap`]
//! that minted its semantic types. That pairing matters for project checks:
//! imported and ambient definitions occupy the same `DefId` arena as local
//! definitions, so rebuilding a local-only map would misname them or index a
//! different arena. Nothing here touches checker or lowering internals.
//!
//! # Contract
//!
//! [`type_at`]'s arguments MUST have the exact same declaration layout used
//! for checking. Pointer identity is unnecessary and cloning the module is
//! safe, but declaration filtering, order, and nesting must be identical:
//! local `DefId` and `DefOrigin::Local::member` are assigned ordinally. For prelude-aware
//! checks, use the [`crate::CheckedModule::module`] returned alongside its
//! [`crate::CheckedModule::checked`].
//!
//! Only [`nymph_ast::expr::Expr`] nodes carry a [`nymph_ast::NodeId`] and
//! get annotated — patterns (including a `let` binder's own name), types,
//! and other non-expression nodes never appear in [`crate::Annotations`].
//! So hovering a `let` binder returns `None`; hovering its initializer
//! expression, or a later `Identifier` use of the bound name, resolves.

use std::{collections::HashSet, sync::Arc};

use ecow::EcoString;
use nymph_ast::{
	Ident, NodeId, Span, Spanned,
	decl::{
		Declaration, EnumVariant, FuncDeclaration, FuncKind, FuncParam, ImplMember, InterfaceElement,
		InterfaceMember, LetKind, Module, StructField, StructImpl, Visibility,
	},
	expr::{
		Expr, ExprKind, ListItem, ListPatternEntry, MapEntry, MapPatternEntry, Pattern, RangeKind,
		Statement, StringPart, StructPatternField,
	},
	token::Token,
	ty::{GenericParam, Type},
};
use rustc_hash::FxHashMap;

use crate::{
	Checked, DeclarationCategory, DeclarationKey, DefId, GenericArgs, Interner, ModuleEnvironment,
	ResolvedImportBinding, Ty, TyKind,
	annotate::VariantResolution,
	def::{self, DefMap},
};

/// Checker-owned member candidates for the member access being edited at
/// `offset`. The accepted interval is half-open and starts immediately after
/// the receiver; malformed/unresolved accesses safely return an empty slice.
#[must_use]
pub fn member_completions_at(
	analysis: &crate::SemanticAnalysis,
	offset: usize,
) -> Vec<crate::MemberCompletion> {
	let mut expressions = Vec::new();
	for declaration in &analysis.module.members {
		collect_decl_exprs(declaration, &mut expressions);
	}
	expressions
		.into_iter()
		.filter_map(|expression| {
			let ExprKind::MemberAccess { parent, member, .. } = &expression.kind else {
				return None;
			};
			// A recovered bare dot has a zero-width missing member and may also
			// leave the access expression ending at the receiver boundary. The dot
			// byte is nevertheless the one valid query position; preserve the
			// strict half-open contract by synthesizing exactly that one byte.
			let end = member
				.1
				.end
				.max(expression.span.end)
				.max(parent.span.end.saturating_add(1));
			(parent.span.end <= offset && offset < end)
				.then_some((expression.span.end - expression.span.start, parent.id))
		})
		.min_by_key(|(width, _)| *width)
		.map(|(_, receiver)| {
			analysis
				.checked
				.annotations
				.member_completions(receiver)
				.to_vec()
		})
		.unwrap_or_default()
}

/// Editor-facing category of a name made lexically available by an import.
///
/// This deliberately carries only stable semantic meaning needed by tooling;
/// checker-local IDs and resolver internals remain private to sema/compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportedNameKind {
	Function,
	Value,
	Variable,
	TypeAlias,
	Struct,
	Enum,
	Interface,
	Namespace,
	Variant,
}

/// One resolved spelling visible in the importing module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedName {
	pub name: String,
	pub kind: ImportedNameKind,
}

/// Project resolved import bindings into immutable, editor-facing facts.
///
/// Aliases are retained as their local spelling, namespace imports are
/// represented directly, imported enums contribute their known bare variants,
/// and unresolved/private imports (`Poison`) are omitted. The compiler remains
/// the sole owner of import resolution and visibility.
#[must_use]
pub fn imported_names(
	bindings: &FxHashMap<EcoString, ResolvedImportBinding>,
	modules: &[Arc<ModuleEnvironment>],
	module: &Module,
) -> Vec<ImportedName> {
	let mut names = bindings
		.iter()
		.filter_map(|(name, binding)| {
			let kind = match binding {
				ResolvedImportBinding::Namespace(_) => ImportedNameKind::Namespace,
				ResolvedImportBinding::Poison => return None,
				ResolvedImportBinding::Definition(definition) => match &definition.key {
					DeclarationKey::TopLevel { category, .. } | DeclarationKey::Member { category, .. } => {
						match category {
							DeclarationCategory::Function | DeclarationCategory::Method => {
								ImportedNameKind::Function
							}
							DeclarationCategory::Let | DeclarationCategory::Field => {
								if imported_definition_is_mutable(definition, modules) {
									ImportedNameKind::Variable
								} else {
									ImportedNameKind::Value
								}
							}
							DeclarationCategory::TypeAlias => ImportedNameKind::TypeAlias,
							DeclarationCategory::Struct => ImportedNameKind::Struct,
							DeclarationCategory::Enum => ImportedNameKind::Enum,
							DeclarationCategory::Interface => ImportedNameKind::Interface,
							DeclarationCategory::Namespace => ImportedNameKind::Namespace,
							DeclarationCategory::Variant => ImportedNameKind::Variant,
							DeclarationCategory::Static
							| DeclarationCategory::Implementation
							| DeclarationCategory::MethodBody => return None,
						}
					}
					DeclarationKey::Implementation { .. }
					| DeclarationKey::RecoveredImplementation { .. }
					| DeclarationKey::MethodBody { .. }
					| DeclarationKey::MaterializedInterfaceMember { .. } => return None,
				},
			};
			Some(ImportedName {
				name: name.to_string(),
				kind,
			})
		})
		.collect::<Vec<_>>();
	names.sort_by(|left, right| left.name.cmp(&right.name));
	let mut seen = names
		.iter()
		.map(|imported| imported.name.clone())
		.collect::<HashSet<_>>();
	let local_definitions = def::build_def_map(module, &mut Vec::new());
	seen.extend(local_definitions.by_name.keys().map(ToString::to_string));
	let imported_enums = bindings
		.values()
		.filter_map(|binding| match binding {
			ResolvedImportBinding::Definition(definition)
				if matches!(
					definition.key,
					DeclarationKey::TopLevel {
						category: DeclarationCategory::Enum,
						..
					}
				) =>
			{
				Some(definition)
			}
			_ => None,
		})
		.collect::<HashSet<_>>();
	let mut variants: FxHashMap<String, HashSet<crate::DefinitionId>> = FxHashMap::default();
	for module in modules {
		match module.as_ref() {
			ModuleEnvironment::Complete(interface) => {
				for definition in &interface.exports {
					if imported_enums.contains(&definition.id) {
						for variant in &definition.variants {
							variants
								.entry(variant.name.to_string())
								.or_default()
								.insert(variant.id.clone());
						}
					}
				}
			}
			ModuleEnvironment::Recovered(interface) => {
				for definition in &interface.exports {
					if imported_enums.contains(&definition.id) {
						for variant in &definition.variants {
							variants
								.entry(variant.name.to_string())
								.or_default()
								.insert(variant.id.clone());
						}
					}
				}
			}
		}
	}
	for (variant, candidates) in variants {
		if candidates.len() == 1
			&& !local_definitions.variants.contains_key(variant.as_str())
			&& seen.insert(variant.clone())
		{
			names.push(ImportedName {
				name: variant,
				kind: ImportedNameKind::Variant,
			});
		}
	}
	names.sort_by(|left, right| left.name.cmp(&right.name));
	names
}

fn imported_definition_is_mutable(
	definition: &crate::DefinitionId,
	modules: &[Arc<ModuleEnvironment>],
) -> bool {
	modules.iter().any(|module| match module.as_ref() {
		ModuleEnvironment::Complete(interface) => interface.exports.iter().any(|candidate| {
			candidate.id == *definition
				&& candidate.declaration_kind == Some(crate::MemberKind::MutableValue)
		}),
		ModuleEnvironment::Recovered(interface) => interface.exports.iter().any(|candidate| {
			candidate.id == *definition
				&& candidate.declaration_kind == Some(crate::MemberKind::MutableValue)
		}),
	})
}

/// Find the type of the smallest checked expression covering byte `offset`
/// in `module`, rendered as a display string (`"int"`, `"#[Option<T0>]"`,
/// `MyStruct<int>`, …). Returns `None` when no annotated expression covers
/// `offset` (whitespace, a comment, a pattern/binder position, or an
/// expression the checker never annotated, e.g. inside a still-erroring
/// subtree), OR when the smallest covering expression is a container/
/// control-flow kind (`Block`, `If`, `While`, `For`, `Match`, `Closure`,
/// `Return`, `Break`, `Continue`) rather than a leaf/primary value
/// genuinely under the cursor — hovering the `let`/`func`/`while`/`for`
/// keyword, a binder name, a type annotation, or whitespace inside a block
/// all land on one of these containers (the only expr kinds actually
/// annotated among them are `While`/`For` as `void` and a value-position
/// `Block` as its trailing expression's type) and would otherwise leak the
/// *enclosing* type rather than reporting nothing. A leaf under the cursor
/// always has a strictly smaller span than its enclosing container, so it
/// is always preferred by the `min_by_key` above this guard — this check
/// never suppresses a genuine leaf hover. See the module doc comment for
/// the `checked`/`module` pairing contract this relies on.
#[must_use]
pub fn type_at(module: &Module, checked: &Checked, offset: usize) -> Option<String> {
	// A variant construction (`Ok(value = 1)`) or bare/qualified nullary
	// reference (`None`, `Result.Ok`) must win over the primary path below:
	// the smallest expr covering the cursor there is the CALLEE identifier
	// (for a construction) or the identifier/member itself (for a
	// reference), whose own *type* is the plain enum (`Result`/`Opt<_>`) —
	// the variant resolution lives on a different node (the `Call`, for a
	// construction) than the one the primary path's `min_by_key` would pick.
	// See [`variant_hover_at`].
	if let Some(rendered) = variant_hover_at(module, checked, offset) {
		return Some(rendered);
	}

	let mut exprs = Vec::new();
	for decl in &module.members {
		collect_decl_exprs(decl, &mut exprs);
	}

	let smallest = exprs
		.into_iter()
		.filter(|e: &&Expr| covers(e.span, offset))
		.min_by_key(|e| e.span.end - e.span.start);

	if let Some(smallest) = smallest
		&& !suppresses_hover(&smallest.kind)
		&& let Some(info) = checked.annotations.get(smallest.id)
		&& !matches!(
			checked.interner.kind(info.ty),
			TyKind::Error | TyKind::Infer(_)
		) {
		let defs = &checked.semantic.definitions;
		let params = generic_scope_at(module, offset);

		// A call-site callee (`helper` in `helper()`): if it resolves to a
		// top-level `func`/`external func` declaration and isn't shadowed by
		// a local/param binder of the same name, show its full named
		// signature instead of the unnamed `Fn` type — see
		// [`render_named_signature`].
		if let ExprKind::Identifier(name) = &smallest.kind
			&& matches!(checked.interner.kind(info.ty), TyKind::Fn { .. })
			&& let Some(id) = defs.get(name.0.as_str())
			&& matches!(defs.data(id).kind, def::DefKind::Func)
			&& let Some(member) = defs.local_member(id)
			&& !is_shadowed_by_local(module, smallest.id)
			&& let Some(meta) = func_decl_meta(&module.members[member])
		{
			let inferred_ret = func_decl_body(&module.members[member])
				.and_then(|body| inferred_return(body, checked, module, &defs));
			return Some(render_named_signature(meta, inferred_ret));
		}

		return Some(render(&checked.interner, &defs, &params, info.ty));
	}

	fallback_type_at(module, checked, offset)
}

/// BUG 1 fix: variant-construction/reference hover. Walks every `Expr` in
/// `module` (mirroring [`type_at`]'s own walk) collecting a candidate at the
/// TIGHTEST name span the checker attached a [`VariantResolution`] to:
///
///   - `ExprKind::Call { func, .. }` whose CALL node (`e.id`, not `func`'s —
///     see `infer_variant_ctor`'s `self.annotations.record_variant(id, res)`,
///     called with the call expr's own id) resolved to a variant: the name
///     span is the callee's own name (`func.span` for a bare `Ok(..)`
///     callee, the member span for a qualified `Result.Ok(..)` callee).
///   - `ExprKind::Identifier`/`ExprKind::MemberAccess` whose OWN node
///     resolved to a variant (a bare/qualified nullary reference, e.g.
///     `None`/`Result.Ok`): the name span is the identifier's/member's own
///     span.
///
/// Keeping the span narrow to just the name (never the whole call, which
/// also covers the arguments) means hovering an argument still falls
/// through to this returning `None`, landing back in [`type_at`]'s normal
/// per-argument render.
fn variant_hover_at(module: &Module, checked: &Checked, offset: usize) -> Option<String> {
	let mut exprs = Vec::new();
	for decl in &module.members {
		collect_decl_exprs(decl, &mut exprs);
	}

	let mut candidates: Vec<(Span, &VariantResolution)> = Vec::new();
	for e in &exprs {
		match &e.kind {
			ExprKind::Call { func, .. } => {
				if let Some(res) = checked.annotations.variant_of(e.id) {
					let name_span = match &func.kind {
						ExprKind::Identifier(_) => func.span,
						ExprKind::MemberAccess { member, .. } => member.1,
						_ => continue,
					};
					candidates.push((name_span, res));
				}
			}
			ExprKind::Identifier(_) => {
				if let Some(res) = checked.annotations.variant_of(e.id) {
					candidates.push((e.span, res));
				}
			}
			ExprKind::MemberAccess { member, .. } => {
				if let Some(res) = checked.annotations.variant_of(e.id) {
					candidates.push((member.1, res));
				}
			}
			_ => {}
		}
	}

	let (_, res) = candidates
		.into_iter()
		.filter(|(span, _)| covers(*span, offset))
		.min_by_key(|(span, _)| span.end - span.start)?;

	render_variant_from_resolution(module, checked, res)
}

/// Render a resolved `(enum, variant)` name pair as `EnumName.Variant(f: T,
/// ...)`, identical in shape to [`push_variant_decl_candidate`] (the
/// pattern-hover sibling of this function). `None` — never a guess — when
/// the resolution's names don't line up with a live declaration.
fn render_variant_from_resolution(
	module: &Module,
	checked: &Checked,
	res: &VariantResolution,
) -> Option<String> {
	let defs = &checked.semantic.definitions;
	let enum_id = variant_enum_definition(defs, res)?;
	if let Some(member) = defs.local_member(enum_id) {
		let Declaration::Enum { variants, .. } = &module.members[member] else {
			return None;
		};
		let variant = variants.iter().find(|v| v.0.name.0 == res.variant)?;
		return Some(format!(
			"{}.{}",
			res.enum_name,
			render_enum_variant(&variant.0)
		));
	}

	let signature = checked.semantic.signatures.enums.get(&enum_id)?;
	let variant = semantic_variant(checked, enum_id, res)?;
	Some(format!(
		"{}.{}",
		defs.data(enum_id).name,
		render_semantic_variant(variant, &checked.interner, defs, &signature.generics)
	))
}

fn variant_enum_definition(defs: &DefMap, res: &VariantResolution) -> Option<DefId> {
	let enum_id = res
		.enum_target
		.as_ref()
		.and_then(|target| defs.by_stable(target))
		.or_else(|| defs.get(res.enum_name.as_str()))?;
	let def::DefKind::Enum = defs.data(enum_id).kind else {
		return None;
	};
	Some(enum_id)
}

fn semantic_variant<'a>(
	checked: &'a Checked,
	enum_id: DefId,
	res: &VariantResolution,
) -> Option<&'a def::VariantSig> {
	let variants = &checked.semantic.signatures.enums.get(&enum_id)?.variants;
	match &res.variant_target {
		Some(target) => variants
			.iter()
			.find(|variant| variant.target.as_ref() == Some(target)),
		None => variants.iter().find(|variant| variant.name == res.variant),
	}
}

/// Container/control-flow expression kinds that enclose the cursor rather
/// than sit under it — see [`type_at`]'s doc comment for why hovering one
/// of these must report `None` instead of the container's own type.
fn suppresses_hover(kind: &ExprKind) -> bool {
	matches!(
		kind,
		ExprKind::Block { .. }
			| ExprKind::If { .. }
			| ExprKind::While { .. }
			| ExprKind::For { .. }
			| ExprKind::Match { .. }
			| ExprKind::Closure { .. }
			| ExprKind::Return { .. }
			| ExprKind::Break { .. }
			| ExprKind::Continue { .. }
			| ExprKind::Call { .. }
			| ExprKind::BinaryOp { .. }
			| ExprKind::AssignOp { .. }
			| ExprKind::PrefixOp { .. }
			| ExprKind::PostfixOp { .. }
			| ExprKind::TypeOp { .. }
			| ExprKind::PatternOp { .. }
			| ExprKind::List(_)
			| ExprKind::Tuple(_)
			| ExprKind::Map(_)
			| ExprKind::Range(_)
			| ExprKind::Grouped(_)
	)
}

/// Whether `offset` lies in `span` under the semantic-query contract.
///
/// Source spans are strict half-open ranges: their start is included and
/// their end is excluded. Empty or reversed spans therefore contain no
/// offsets. Cursor conveniences such as whitespace left bias belong in the
/// LSP layer and must not weaken this core rule.
fn covers(span: Span, offset: usize) -> bool {
	span.start <= offset && offset < span.end
}

/// Render a (already deeply-resolved — see `Checker::record`) type,
/// mirroring `check::Checker::display_resolved`. `params` names the
/// generic-parameter scope enclosing the hovered expression (see
/// [`generic_scope_at`]); a `Param(idx)` renders as `params[idx]` when in
/// range, falling back to the internal `T{idx}` otherwise (out-of-scope
/// index, or a synthetic `impl Interface` param).
fn render(interner: &Interner, defs: &DefMap, params: &[EcoString], ty: Ty) -> String {
	match interner.kind(ty) {
		TyKind::Int => "int".to_string(),
		TyKind::UInt => "uint".to_string(),
		TyKind::Float => "float".to_string(),
		TyKind::Char => "char".to_string(),
		TyKind::String => "string".to_string(),
		TyKind::Boolean => "boolean".to_string(),
		TyKind::Void => "void".to_string(),
		TyKind::Never => "never".to_string(),
		TyKind::SelfTy => "self".to_string(),
		TyKind::Error => "<error>".to_string(),
		TyKind::Infer(_) => "_".to_string(),
		TyKind::Param(p) => params
			.get(p.0 as usize)
			.map(ToString::to_string)
			.unwrap_or_else(|| format!("T{}", p.0)),
		TyKind::List(elem) => format!("#[{}]", render(interner, defs, params, *elem)),
		TyKind::Tuple(elems) => {
			let inner: Vec<_> = elems
				.iter()
				.map(|&e| render(interner, defs, params, e))
				.collect();
			format!("#({})", inner.join(", "))
		}
		TyKind::Map(key, value) => format!(
			"#{{{}: {}}}",
			render(interner, defs, params, *key),
			render(interner, defs, params, *value)
		),
		TyKind::Fn {
			params: fn_params,
			ret,
		} => {
			let inner: Vec<_> = fn_params
				.iter()
				.map(|&p| render(interner, defs, params, p))
				.collect();
			format!(
				"({}) -> {}",
				inner.join(", "),
				render(interner, defs, params, *ret)
			)
		}
		TyKind::Adt(def_id, args) => render_adt(interner, defs, params, *def_id, args),
		TyKind::Intersection(parts) => {
			let inner: Vec<_> = parts
				.iter()
				.map(|&p| render(interner, defs, params, p))
				.collect();
			inner.join(" + ")
		}
		TyKind::Mut(inner) => format!("mut {}", render(interner, defs, params, *inner)),
	}
}

fn render_adt(
	interner: &Interner,
	defs: &DefMap,
	params: &[EcoString],
	def_id: nymph_hir::ids::DefId,
	args: &GenericArgs,
) -> String {
	let name = defs.data(def_id).name.clone();
	if args.is_empty() {
		return name.to_string();
	}
	let mut inner: Vec<String> = args
		.positional
		.iter()
		.map(|&t| render(interner, defs, params, t))
		.collect();
	inner.extend(
		args
			.named
			.iter()
			.map(|(n, t)| format!("{n} = {}", render(interner, defs, params, *t))),
	);
	format!("{name}<{}>", inner.join(", "))
}

fn render_semantic_variant(
	variant: &def::VariantSig,
	interner: &Interner,
	defs: &DefMap,
	params: &[EcoString],
) -> String {
	if variant.fields.is_empty() {
		return variant.name.to_string();
	}
	let fields: Vec<_> = variant
		.fields
		.iter()
		.map(|(name, ty)| format!("{name}: {}", render(interner, defs, params, *ty)))
		.collect();
	format!("{}({})", variant.name, fields.join(", "))
}

// ── Keyword documentation (for hover) ───────────────────────────────────────
//
// [`type_at`] only ever resolves an `Expr`, a declaration name, a
// type-position name, or a param/binder name — a bare keyword token (`for`,
// `let`, `mut`, `int`, …) never covers a smaller node than one of
// [`suppresses_hover`]'s containers (or, for a bare type keyword used as a
// value's type annotation, isn't an `Expr`/fallback candidate at all) and so
// correctly returns `None` for it. [`keyword_doc_at`] is the *separate*
// prose answer for that same position: `hover.rs` calls it only after
// `type_at` has already returned `None`, so code always wins and a keyword
// covered by some other hoverable node is never shadowed by its own doc.

/// Short one-line PROSE documentation for a Nymph keyword, keyed by exact
/// [`Token`] variant so tests can assert it verbatim.
fn keyword_doc(token: &Token) -> Option<&'static str> {
	match token {
		Token::Func => Some(
			"`func` declares a function: `func name(params): ReturnType = body`. \
			 A `mut func` inside a struct/enum may mutate `this`.",
		),
		Token::Let => Some(
			"`let` introduces a binding: `let name = value`, optionally with a \
			 type annotation (`let name: Type = value`).",
		),
		Token::Mut => Some(
			"`mut` marks something mutable — a reassignable `let mut` binding, a \
			 mutable function parameter, a `mut func` method, or a `mut T` view type.",
		),
		Token::If => Some(
			"`if` branches on a boolean condition: `if cond { ... } else { ... }`. \
			 Like a block, an `if`/`else` is itself an expression.",
		),
		Token::Else => Some("`else` supplies the alternative branch of an `if` expression."),
		Token::Match => Some(
			"`match` pattern-matches a value against a series of arms: \
			 `match value { pattern -> body, ... }`.",
		),
		Token::For => Some("`for` iterates over an iterable: `for item in iterable { ... }`."),
		Token::While => Some("`while` loops while a boolean condition holds: `while cond { ... }`."),
		Token::Struct => Some(
			"`struct` declares a product type with named fields: \
			 `struct Name(field: Type, ...)`.",
		),
		Token::Enum => Some(
			"`enum` declares a sum type as a set of variants, each with its own \
			 (possibly empty) fields: `enum Name { Variant(field: Type), ... }`.",
		),
		Token::Interface => Some(
			"`interface` declares a set of member signatures (`func`s and `let`s) \
			 a type can implement, optionally with default bodies.",
		),
		Token::Impl => Some(
			"`impl` implements an interface for a type (`impl Iface for Type { ... }`) \
			 or adds inherent members to it (`impl Type { ... }`).",
		),
		Token::Namespace => {
			Some("`namespace` groups type-level (static) members that aren't tied to an instance.")
		}
		Token::Type => Some("`type` declares a type alias: `type Alias<G..> = Type`."),
		Token::Return => Some("`return` exits the enclosing function early with a value."),
		Token::Break => Some("`break` exits the enclosing loop early, optionally with a value."),
		Token::Continue => Some("`continue` skips to the next iteration of the enclosing loop."),
		Token::Import => Some(
			"`import` brings a module into scope: `import @/math`, `import @/math as m`, \
			 or `import @/math with (sin, cos)`.",
		),
		Token::With => {
			Some("`with` selects specific names to import: `import @/math with (sin, cos)`.")
		}
		Token::As => Some("`as` casts a value to another type: `value as Type`."),
		Token::Is => Some(
			"`is` tests whether a value matches a pattern, yielding a `boolean`: `value is Pattern`.",
		),
		Token::In => Some(
			"`in` names the iterable in a `for` loop (`for x in xs`), or tests \
			 membership as a binary operator (`x in xs`).",
		),
		Token::Public => Some("`public` makes a declaration visible outside its module."),
		Token::Internal => Some("`internal` makes a declaration visible within its own package only."),
		Token::Private => Some("`private` makes a declaration visible within its own module only."),
		Token::External => Some(
			"`external` declares a binding backed by a foreign (JS) implementation: \
			 `external(js_name) func name(...): Ret`.",
		),
		Token::Async => Some("`async` is reserved for a future async model — not yet usable."),
		Token::Await => Some("`await` is reserved for a future async model — not yet usable."),
		Token::This => Some("`this` refers to the current instance inside a method body."),
		Token::True => Some("`true` — the `boolean` literal for truth."),
		Token::False => Some("`false` — the `boolean` literal for falsity."),
		Token::IntType => Some("`int` — a signed integer type."),
		Token::UIntType => Some("`uint` — an unsigned integer type."),
		Token::FloatType => Some("`float` — a floating-point number type."),
		Token::BooleanType => Some("`boolean` — the `true`/`false` type."),
		Token::CharType => Some("`char` — a single Unicode scalar value."),
		Token::StringType => Some("`string` — a UTF-8 text type."),
		Token::VoidType => Some("`void` — the type of a value carrying no information (`#()`)."),
		Token::NeverType => {
			Some("`never` — the type of an expression that never produces a value (diverges).")
		}
		Token::SelfType => Some("`self` — the type currently being declared or implemented."),
		_ => None,
	}
}

/// Short PROSE documentation for the keyword token covering byte `offset` in
/// `text`, or `None` when `offset` isn't over a keyword token at all
/// (whitespace, an operator/delimiter, an identifier, a literal, or a
/// keyword-looking word that's actually inside a comment or a string — since
/// comments aren't tokenized and a string's contents are one `Token::Str`,
/// neither ever lexes as a keyword token). `hover.rs` calls this only after
/// [`type_at`] has already returned `None` for the same `offset`, so code
/// hovers always win over a keyword doc.
#[must_use]
pub fn keyword_doc_at(text: &str, offset: usize) -> Option<&'static str> {
	let tokens = nymph_syntax::lex(text).tokens;
	let smallest = tokens
		.iter()
		.filter(|t| covers(t.1, offset))
		.min_by_key(|t| t.1.end - t.1.start)?;
	keyword_doc(&smallest.0)
}

// ── Generic-parameter name recovery (for hover) ─────────────────────────────
//
// The checker assigns `Param(idx)` indices per declaration, in an order that
// mirrors each body-checking scope built in `members.rs`/`infer_expr.rs`:
// owner (struct/enum/interface/impl) generics first, then — only for a
// struct/enum's own direct members and a top-level inherent `impl` — that
// member's own generics appended after. A nested `impl Iface { .. }` (either
// inside a struct/enum body or as a top-level `impl .. for ..`) does NOT
// extend the scope with the method's own generics (see `members.rs`'s
// `check_inner_impl_bodies`/`check_impl_for_bodies`, which never thread
// `meta.generics` into the pushed param scope there) — so this recovery
// mirrors that omission rather than guessing a name for indices the checker
// itself never binds to a source name.

/// The ordered generic-parameter names in scope at byte `offset` in
/// `module` — outermost (owner) first, then the enclosing member's own, if
/// any — used to turn a `TyKind::Param(idx)` back into its source name for
/// hover. Returns an empty list outside any generic-bearing declaration (a
/// bare top-level `let`, or a scope this recovery doesn't model), which
/// simply leaves every `Param` rendering as the internal `T{idx}` fallback.
fn generic_scope_at(module: &Module, offset: usize) -> Vec<EcoString> {
	for decl in &module.members {
		if let Some(scope) = decl_generic_scope(decl, offset) {
			return scope;
		}
	}
	Vec::new()
}

fn names_of(generics: &[Spanned<GenericParam>]) -> Vec<EcoString> {
	generics.iter().map(|g| g.0.name.0.clone()).collect()
}

fn extended(owner: &[EcoString], extra: &[Spanned<GenericParam>]) -> Vec<EcoString> {
	owner.iter().cloned().chain(names_of(extra)).collect()
}

fn decl_generic_scope(decl: &Declaration, offset: usize) -> Option<Vec<EcoString>> {
	match decl {
		Declaration::Import { .. }
		| Declaration::ExternalLet(..)
		| Declaration::ExternalFunc(..)
		| Declaration::TypeAlias { .. } => None,
		Declaration::Let { value, .. } => covers(value.span, offset).then(Vec::new),
		Declaration::Func { meta, body, .. } => {
			covers(body.span, offset).then(|| names_of(&meta.generics))
		}
		Declaration::Struct {
			generics,
			fields,
			members,
			impls,
			..
		} => {
			let owner = names_of(generics);
			for f in fields {
				if let Some(default) = &f.0.default
					&& covers(default.span, offset)
				{
					return Some(owner);
				}
			}
			for m in members {
				if let Some(v) = impl_member_scope(&m.0, &owner, true, offset) {
					return Some(v);
				}
			}
			for si in impls {
				if let Some(v) = struct_impl_scope(&si.0, &owner, offset) {
					return Some(v);
				}
			}
			None
		}
		Declaration::Enum {
			generics,
			members,
			impls,
			..
		} => {
			let owner = names_of(generics);
			for m in members {
				if let Some(v) = impl_member_scope(&m.0, &owner, true, offset) {
					return Some(v);
				}
			}
			for si in impls {
				if let Some(v) = struct_impl_scope(&si.0, &owner, offset) {
					return Some(v);
				}
			}
			None
		}
		Declaration::Namespace { members, .. } => {
			for m in members {
				if let Some(v) = impl_member_scope(&m.0, &[], true, offset) {
					return Some(v);
				}
			}
			None
		}
		Declaration::Interface {
			generics, members, ..
		} => {
			let owner = names_of(generics);
			for m in members {
				if let Some(v) = interface_member_scope(&m.0, &owner, offset) {
					return Some(v);
				}
			}
			None
		}
		Declaration::Impl {
			generics, members, ..
		} => {
			let owner = names_of(generics);
			for m in members {
				if let Some(v) = impl_member_scope(&m.0, &owner, true, offset) {
					return Some(v);
				}
			}
			None
		}
		Declaration::ImplFor {
			generics, members, ..
		} => {
			let owner = names_of(generics);
			for m in members {
				if let Some(v) = impl_member_scope(&m.0, &owner, false, offset) {
					return Some(v);
				}
			}
			None
		}
	}
}

/// A direct `struct`/`enum`/`impl` member's generic scope. `include_own`
/// selects whether the member's own generics extend `owner` (true for a
/// struct/enum's direct members and a top-level inherent `impl` — see
/// `members.rs`'s `check_method_body`; false for a top-level `impl .. for
/// ..`'s members — see `check_interface_impl_members`, which never threads
/// `meta.generics` into its pushed scope).
fn impl_member_scope(
	member: &ImplMember,
	owner: &[EcoString],
	include_own: bool,
	offset: usize,
) -> Option<Vec<EcoString>> {
	match member {
		ImplMember::ExternalLet(..) | ImplMember::ExternalFunc(..) => None,
		ImplMember::Func { meta, body, .. } => {
			if !covers(body.span, offset) {
				return None;
			}
			Some(if include_own {
				extended(owner, &meta.generics)
			} else {
				owner.to_vec()
			})
		}
		ImplMember::Let { value, .. } => covers(value.span, offset).then(|| owner.to_vec()),
	}
}

/// A nested `impl Iface { .. }` block inside a struct/enum body: the scope
/// is the owner's generics plus the impl block's own (never the member's
/// own generics — see `members.rs`'s `check_inner_impl_bodies`).
fn struct_impl_scope(
	struct_impl: &StructImpl,
	owner: &[EcoString],
	offset: usize,
) -> Option<Vec<EcoString>> {
	let combined = extended(owner, &struct_impl.generics);
	for m in &struct_impl.members {
		if let Some(v) = impl_member_scope(&m.0, &combined, false, offset) {
			return Some(v);
		}
	}
	None
}

/// An interface member's generic scope: a default-bodied method extends
/// `owner` (the interface's own generics) with its own generics (see
/// `members.rs`'s `check_interface_default_body`); a nested `impl .. { .. }`
/// (a super-interface default impl) extends `owner` with the impl block's
/// own generics only, mirroring [`struct_impl_scope`].
fn interface_member_scope(
	member: &InterfaceMember,
	owner: &[EcoString],
	offset: usize,
) -> Option<Vec<EcoString>> {
	match member {
		InterfaceMember::Element(elem) => match &elem.0 {
			InterfaceElement::Func {
				meta,
				body: Some(body),
			} => covers(body.span, offset).then(|| extended(owner, &meta.generics)),
			InterfaceElement::Let {
				value: Some(value), ..
			} => covers(value.span, offset).then(|| owner.to_vec()),
			_ => None,
		},
		InterfaceMember::Impl {
			generics, members, ..
		} => {
			let combined = extended(owner, generics);
			for m in members {
				if let Some(v) = impl_member_scope(&m.0, &combined, false, offset) {
					return Some(v);
				}
			}
			None
		}
	}
}

// ── AST walk: every `Expr` reachable from a module, in declaration order ───

fn collect_decl_exprs<'a>(decl: &'a Declaration, out: &mut Vec<&'a Expr>) {
	match decl {
		Declaration::Import { .. }
		| Declaration::ExternalLet(..)
		| Declaration::ExternalFunc(..)
		| Declaration::TypeAlias { .. } => {}
		Declaration::Let { value, .. } | Declaration::Func { body: value, .. } => {
			collect_expr(value, out);
		}
		Declaration::Struct {
			fields,
			members,
			impls,
			..
		} => {
			for f in fields {
				if let Some(default) = &f.0.default {
					collect_expr(default, out);
				}
			}
			for m in members {
				collect_impl_member(&m.0, out);
			}
			for si in impls {
				collect_struct_impl(&si.0, out);
			}
		}
		Declaration::Enum { members, impls, .. } => {
			for m in members {
				collect_impl_member(&m.0, out);
			}
			for si in impls {
				collect_struct_impl(&si.0, out);
			}
		}
		Declaration::Namespace { members, .. } => {
			for m in members {
				collect_impl_member(&m.0, out);
			}
		}
		Declaration::Interface { members, .. } => {
			for m in members {
				collect_interface_member(&m.0, out);
			}
		}
		Declaration::Impl { members, .. } | Declaration::ImplFor { members, .. } => {
			for m in members {
				collect_impl_member(&m.0, out);
			}
		}
	}
}

fn collect_impl_member<'a>(member: &'a ImplMember, out: &mut Vec<&'a Expr>) {
	match member {
		ImplMember::ExternalLet(..) | ImplMember::ExternalFunc(..) => {}
		ImplMember::Let { value, .. } | ImplMember::Func { body: value, .. } => {
			collect_expr(value, out);
		}
	}
}

fn collect_struct_impl<'a>(struct_impl: &'a StructImpl, out: &mut Vec<&'a Expr>) {
	for m in &struct_impl.members {
		collect_impl_member(&m.0, out);
	}
}

fn collect_interface_member<'a>(member: &'a InterfaceMember, out: &mut Vec<&'a Expr>) {
	match member {
		InterfaceMember::Element(elem) => match &elem.0 {
			InterfaceElement::Let { value, .. } => {
				if let Some(v) = value {
					collect_expr(v, out);
				}
			}
			InterfaceElement::Func { body, .. } => {
				if let Some(b) = body {
					collect_expr(b, out);
				}
			}
		},
		InterfaceMember::Impl { members, .. } => {
			for m in members {
				collect_impl_member(&m.0, out);
			}
		}
	}
}

fn collect_expr<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
	out.push(expr);
	match &expr.kind {
		ExprKind::Int(_)
		| ExprKind::UInt(_)
		| ExprKind::Float(_)
		| ExprKind::Char(_)
		| ExprKind::Boolean(_)
		| ExprKind::Identifier(_)
		| ExprKind::AnonymousParam(_)
		| ExprKind::This
		| ExprKind::Continue { .. } => {}
		ExprKind::String(parts) => {
			for p in parts {
				if let StringPart::InterpolatedExpr(e) = &p.0 {
					collect_expr(e, out);
				}
			}
		}
		ExprKind::List(items) | ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(e) | ListItem::Spread(e) => collect_expr(e, out),
				}
			}
		}
		ExprKind::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapEntry::Entry(k, v) => {
						collect_expr(k, out);
						collect_expr(v, out);
					}
					MapEntry::Spread(e) => collect_expr(e, out),
				}
			}
		}
		ExprKind::Range(kind) => match kind {
			RangeKind::From(e) | RangeKind::To(e) | RangeKind::ToInclusive(e) => {
				collect_expr(e, out);
			}
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				collect_expr(min, out);
				collect_expr(max, out);
			}
		},
		ExprKind::Call { func, args, .. } => {
			collect_expr(func, out);
			for arg in args {
				collect_expr(&arg.0.value, out);
			}
		}
		ExprKind::MemberAccess { parent, .. } => collect_expr(parent, out),
		ExprKind::IndexAccess { parent, index, .. } => {
			collect_expr(parent, out);
			collect_expr(index, out);
		}
		ExprKind::Closure { body, .. } => collect_expr(body, out),
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
			collect_expr(value, out);
		}
		ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
			collect_expr(lhs, out);
			collect_expr(rhs, out);
		}
		ExprKind::TypeOp { lhs, .. } | ExprKind::PatternOp { lhs, .. } => collect_expr(lhs, out),
		ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
			if let Some(v) = value {
				collect_expr(v, out);
			}
		}
		ExprKind::While {
			condition, body, ..
		} => {
			collect_expr(condition, out);
			collect_expr(body, out);
		}
		ExprKind::For { iterable, body, .. } => {
			collect_expr(iterable, out);
			collect_expr(body, out);
		}
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			collect_expr(condition, out);
			collect_expr(then, out);
			if let Some(o) = otherwise {
				collect_expr(o, out);
			}
		}
		ExprKind::Match { value, arms } => {
			collect_expr(value, out);
			for arm in arms {
				if let Some(g) = &arm.guard {
					collect_expr(g, out);
				}
				collect_expr(&arm.body, out);
			}
		}
		ExprKind::Block { body, .. } => {
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) | Statement::Let { value: e, .. } => collect_expr(e, out),
				}
			}
		}
		ExprKind::Grouped(inner) => collect_expr(inner, out),
	}
}

// ── Go-to-definition ───────────────────────────────────────────────────────
//
// `Resolution` (see `crate::annotate`) is operator-dispatch metadata (a
// resolved method name + an impl-header span), never an identifier -> binder
// mapping — the checker records no such mapping. So go-to-definition is built
// straight from the AST plus a freshly rebuilt [`DefMap`], independent of any
// `Checked` result.

/// Resolve the stable declaration denoted at an exact byte offset in checked
/// source. This deliberately excludes ordinary fields/members. Namespace
/// members are considered only over the member identifier's strict half-open span.
#[must_use]
pub fn stable_definition_at(
	analysis: &crate::SemanticAnalysis,
	offset: usize,
) -> Option<crate::DefinitionId> {
	let mut exprs = Vec::new();
	for decl in &analysis.module.members {
		collect_decl_exprs(decl, &mut exprs);
	}
	let candidate = exprs
		.into_iter()
		.filter_map(|expr| {
			let span = match &expr.kind {
				ExprKind::Identifier(_) => expr.span,
				ExprKind::MemberAccess { member, .. } => member.1,
				_ => return None,
			};
			covers(span, offset).then_some((span, expr.id))
		})
		.min_by_key(|(span, _)| span.end - span.start);
	if let Some((_, id)) = candidate
		&& let Some(target) = analysis.annotations.definition_target_of(id)
	{
		return Some(target.clone());
	}

	analysis
		.annotations
		.type_definition_targets()
		.filter(|(span, _)| covers(*span, offset))
		.min_by_key(|(span, _)| span.end - span.start)
		.map(|(_, target)| target.clone())
}

/// Semantic identity of a source symbol.
///
/// Project declarations use their globally stable checker identity. Lexical
/// binders use their exact declaration-name span, which is stable for an
/// immutable [`crate::SemanticAnalysis`] and distinguishes shadowed names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SymbolIdentity {
	Definition(crate::DefinitionId),
	Module(crate::ModuleIdentity),
	Local(Span),
}

fn reference_definition_identity(target: &crate::DefinitionId) -> crate::DefinitionId {
	match &target.key {
		DeclarationKey::MaterializedInterfaceMember {
			interface_member, ..
		} => (**interface_member).clone(),
		_ => target.clone(),
	}
}

/// One exact source occurrence of a semantic symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceOccurrence {
	pub span: Span,
	pub is_declaration: bool,
}

/// Return the semantic symbol whose exact, half-open name span contains
/// `offset`. Only checker-recorded stable targets and lexically resolved local
/// binders are considered; unresolved recovery syntax is never matched by
/// spelling.
#[must_use]
pub fn symbol_at(analysis: &crate::SemanticAnalysis, offset: usize) -> Option<SymbolIdentity> {
	all_symbol_occurrences(analysis)
		.into_iter()
		.filter(|(_, occurrence)| covers(occurrence.span, offset))
		.min_by_key(|(_, occurrence)| occurrence.span.end - occurrence.span.start)
		.map(|(identity, _)| identity)
}

/// Return all declaration and reference occurrences of `symbol`, deduplicated
/// and ordered by `(span.start, span.end)`. Occurrence spans contain only the
/// target token (including the member token of a qualified access).
#[must_use]
pub fn references_to(
	analysis: &crate::SemanticAnalysis,
	symbol: &SymbolIdentity,
) -> Vec<ReferenceOccurrence> {
	let mut occurrences = all_symbol_occurrences(analysis)
		.into_iter()
		.filter_map(|(identity, occurrence)| (identity == *symbol).then_some(occurrence))
		.collect::<Vec<_>>();
	occurrences.sort_unstable_by_key(|occurrence| {
		(
			occurrence.span.start,
			occurrence.span.end,
			!occurrence.is_declaration,
		)
	});
	occurrences.dedup_by_key(|occurrence| (occurrence.span.start, occurrence.span.end));
	occurrences
}

fn all_symbol_occurrences(
	analysis: &crate::SemanticAnalysis,
) -> Vec<(SymbolIdentity, ReferenceOccurrence)> {
	let mut result = analysis
		.declarations
		.iter()
		.map(|(identity, provenance)| {
			(
				SymbolIdentity::Definition(reference_definition_identity(identity)),
				ReferenceOccurrence {
					span: provenance.name_span,
					is_declaration: true,
				},
			)
		})
		.collect::<Vec<_>>();
	result.extend(analysis.import_references.iter().map(|(span, target)| {
		let identity = match target {
			crate::ImportReferenceTarget::Definition(target) => {
				SymbolIdentity::Definition(reference_definition_identity(target))
			}
			crate::ImportReferenceTarget::Module(target) => SymbolIdentity::Module(target.clone()),
		};
		(
			identity,
			ReferenceOccurrence {
				span: *span,
				is_declaration: false,
			},
		)
	}));

	let mut exprs = Vec::new();
	for declaration in &analysis.module.members {
		collect_decl_exprs(declaration, &mut exprs);
	}
	for expr in &exprs {
		let target_span = match &expr.kind {
			ExprKind::Identifier(_) => Some(expr.span),
			ExprKind::MemberAccess { member, .. } => Some(member.1),
			_ => None,
		};
		if let Some(span) = target_span
			&& let Some(target) = analysis.annotations.definition_target_of(expr.id)
		{
			result.push((
				SymbolIdentity::Definition(reference_definition_identity(target)),
				ReferenceOccurrence {
					span,
					is_declaration: false,
				},
			));
		}
		if let Some(resolution) = analysis.annotations.variant_of(expr.id)
			&& let Some(target) = &resolution.variant_target
			&& let Some(span) = variant_expression_name_span(expr)
		{
			result.push((
				SymbolIdentity::Definition(reference_definition_identity(target)),
				ReferenceOccurrence {
					span,
					is_declaration: false,
				},
			));
		}
	}
	for (span, target) in analysis.annotations.type_definition_targets() {
		result.push((
			SymbolIdentity::Definition(reference_definition_identity(target)),
			ReferenceOccurrence {
				span,
				is_declaration: false,
			},
		));
	}
	for (span, target) in analysis.annotations.source_definition_targets() {
		result.push((
			SymbolIdentity::Definition(reference_definition_identity(target)),
			ReferenceOccurrence {
				span,
				is_declaration: false,
			},
		));
	}
	for (id, target) in analysis.annotations.module_targets() {
		if let Some(expr) = exprs.iter().find(|expr| expr.id == id)
			&& matches!(expr.kind, ExprKind::Identifier(_))
		{
			result.push((
				SymbolIdentity::Module(target.clone()),
				ReferenceOccurrence {
					span: expr.span,
					is_declaration: false,
				},
			));
		}
	}

	for expr in exprs {
		if let Some(declaration) = analysis.annotations.local_definition_target_of(expr.id) {
			result.push((
				SymbolIdentity::Local(declaration),
				ReferenceOccurrence {
					span: expr.span,
					is_declaration: false,
				},
			));
		}
	}
	for (span, identity) in analysis.annotations.local_declarations() {
		result.push((
			SymbolIdentity::Local(identity),
			ReferenceOccurrence {
				span,
				is_declaration: true,
			},
		));
	}
	result
}

fn variant_expression_name_span(expr: &Expr) -> Option<Span> {
	match &expr.kind {
		ExprKind::Identifier(_) => Some(expr.span),
		ExprKind::MemberAccess { member, .. } => Some(member.1),
		ExprKind::Call { func, .. } => match &func.kind {
			ExprKind::Identifier(_) => Some(func.span),
			ExprKind::MemberAccess { member, .. } => Some(member.1),
			_ => None,
		},
		_ => None,
	}
}

/// Return the semantic category of a stable declaration in this analysis's
/// exact imported, ambient, and local definition arena.
///
/// Tooling must use this instead of rebuilding a source-local [`DefMap`]: a
/// stable target recorded in [`crate::Annotations`] may belong to an imported
/// module or the ambient prelude, and its checker-local [`crate::DefId`] is
/// meaningful only inside the arena that produced the analysis.
#[must_use]
pub fn stable_definition_kind(
	analysis: &crate::SemanticAnalysis,
	target: &crate::DefinitionId,
) -> Option<crate::DefKind> {
	let semantic = &analysis.checked.semantic;
	for namespace in semantic.signatures.namespaces.values() {
		for member in namespace.members.values() {
			match member {
				crate::NamespaceMemberSig::Func {
					target: Some(member_target),
					..
				} if member_target == target => return Some(crate::DefKind::Func),
				crate::NamespaceMemberSig::Value {
					target: Some(member_target),
					..
				} if member_target == target => return Some(crate::DefKind::Let),
				_ => {}
			}
		}
	}
	if semantic.inherent.iter().any(|implementation| {
		implementation
			.methods
			.values()
			.any(|method| method.definition.as_ref() == Some(target))
	}) {
		return Some(crate::DefKind::Func);
	}
	let definition = semantic.definitions.by_stable(target)?;
	if semantic.signatures.funcs.contains_key(&definition) {
		return Some(crate::DefKind::Func);
	}
	if semantic.signatures.lets.contains_key(&definition) {
		return Some(crate::DefKind::Let);
	}
	Some(semantic.definitions.data(definition).kind)
}

/// Return the semantic category bound to a source-visible name in this
/// analysis's exact local/imported/prelude definition arena.
///
/// This complements [`stable_definition_kind`] for declaration syntax that
/// introduces a local spelling without an expression node, notably import
/// aliases. Whole-module import namespaces intentionally have no stable
/// declaration ID, but they are still present in the checked definition map.
#[must_use]
pub fn definition_kind_by_name(
	analysis: &crate::SemanticAnalysis,
	name: &str,
) -> Option<crate::DefKind> {
	let semantic = &analysis.checked.semantic;
	let definition = semantic.definitions.get(name)?;
	Some(semantic.definitions.data(definition).kind)
}

/// Return the semantic category of an imported lexical binding in this
/// analysis's exact definition arena.
///
/// Unlike a stable-target lookup, this also covers imported module namespace
/// bindings: those are checker-owned synthetic definitions and deliberately do
/// not pretend to have a source declaration identity.
#[must_use]
pub fn imported_definition_kind_by_name(
	analysis: &crate::SemanticAnalysis,
	name: &str,
) -> Option<crate::DefKind> {
	let semantic = &analysis.checked.semantic;
	let definition = semantic.definitions.get(name)?;
	let data = semantic.definitions.data(definition);
	matches!(data.origin, crate::DefOrigin::Imported { .. }).then_some(data.kind)
}

/// Return the semantic category of the direct type/namespace parent selected
/// by the checker for a qualified member expression.
///
/// The direct-member marker proves that `parent_name` was not a shadowing
/// local. Looking it up in the producing analysis's arena therefore preserves
/// the checker decision while also covering synthetic imported namespaces,
/// which have no stable [`crate::DefinitionId`].
#[must_use]
pub fn direct_member_parent_kind(
	analysis: &crate::SemanticAnalysis,
	member: nymph_ast::NodeId,
	parent_name: &str,
) -> Option<crate::DefKind> {
	if !analysis
		.annotations
		.direct_namespace_members()
		.any(|candidate| candidate == member)
	{
		return None;
	}
	let semantic = &analysis.checked.semantic;
	let definition = semantic.definitions.get(parent_name)?;
	Some(semantic.definitions.data(definition).kind)
}

/// Find the definition site of the identifier (or bare enum variant, or
/// type-position type name) at byte `offset` in `module`. Resolution order,
/// mirroring `infer_identifier`'s shadowing: the nearest enclosing
/// local/parameter binder, then a top-level definition, then a bare variant
/// name, then (only if no `Expr` identifier covers `offset`) a `Type::Reference`
/// in a declaration signature. Returns `None` — never a wrong jump — when
/// nothing resolves: member/field access after `.` (needs the checker), an
/// unresolvable name, or an ambiguous bare variant.
#[must_use]
pub fn definition_at(module: &Module, offset: usize) -> Option<Span> {
	let mut idents = Vec::new();
	for decl in &module.members {
		collect_decl_exprs(decl, &mut idents);
	}
	let target = idents
		.into_iter()
		.filter(|e| covers(e.span, offset) && matches!(e.kind, ExprKind::Identifier(_)))
		.min_by_key(|e| e.span.end - e.span.start);

	if let Some(target) = target {
		let ExprKind::Identifier(ident) = &target.kind else {
			unreachable!("filtered to Identifier above")
		};
		let name = ident.0.as_str();

		// `Some(Some(span))` — a local/param binder matched, jump there.
		// `Some(None)` — the identifier node was found but no enclosing
		// local/param binder matched it (e.g. it names a top-level func); fall
		// through to the `DefMap` lookup below rather than returning `None`.
		// `None` — the node isn't in this declaration; keep searching.
		for decl in &module.members {
			if let Some(Some(span)) = walk_decl_for_binder(decl, target.id) {
				return Some(span);
			}
		}

		let defs = def::build_def_map(module, &mut Vec::new());
		if let Some(id) = defs.get(name) {
			return Some(defs.data(id).span);
		}
		if let Some(Ok((enum_def, _variant))) = defs.resolve_variant(name) {
			return Some(defs.data(enum_def).span);
		}
		return None;
	}

	// No `Expr::Identifier` covers `offset` — a `this.method` call's method
	// name is the `member` of a `MemberAccess`, never its own `Identifier`
	// expr, so it falls through to here. Resolved purely syntactically (the
	// enclosing decl's own Self type, from its AST header) — see
	// [`this_method_definition_at`].
	if let Some(span) = this_method_definition_at(module, offset) {
		return Some(span);
	}

	// No `Expr` identifier covers `offset` — try a type-position reference
	// (`let x: MyStruct`, a param/return annotation, a struct field's type, …).
	let mut type_refs = Vec::new();
	for decl in &module.members {
		collect_decl_type_refs(decl, &mut type_refs);
	}
	let (name, _) = type_refs
		.into_iter()
		.filter(|(_, span)| covers(*span, offset))
		.min_by_key(|(_, span)| span.end - span.start)?;

	let defs = def::build_def_map(module, &mut Vec::new());
	defs.get(name.0.as_str()).map(|id| defs.data(id).span)
}

/// Every local/parameter binder whose semantic scope encloses byte `offset` —
/// NOT
/// top-level declarations, which callers can already read straight off the
/// public `nymph_ast::decl::Module` (with a real `SymbolKind`/
/// `CompletionItemKind`, which a bare `&str` can't carry). Used by
/// `textDocument/completion`'s identifier suggestions — member completion
/// after a `.` needs the checker and is not covered here. Returns an empty
/// vector when no expression scope applies at the exact offset.
#[must_use]
pub fn scope_names_at(module: &Module, offset: usize) -> Vec<String> {
	collect_scope_names_at(module, offset).names
}

/// The same exact-offset scope query as [`scope_names_at`], preserving whether
/// an expression scope applies. `None` means there is no scope at `offset`;
/// `Some(Vec::new())` means a scope applies but has no local or parameter names.
///
/// This distinction lets cursor-oriented clients decide when a fallback query
/// is appropriate without changing [`scope_names_at`]'s established API.
#[must_use]
pub fn scope_names_at_exact(module: &Module, offset: usize) -> Option<Vec<String>> {
	let result = collect_scope_names_at(module, offset);
	result.applicable.then_some(result.names)
}

fn collect_scope_names_at(module: &Module, offset: usize) -> ScopeNames {
	let mut result = ScopeNames::default();

	for decl in &module.members {
		collect_decl_scope_names(decl, offset, &mut result);
	}

	result
}

#[derive(Default)]
struct ScopeNames {
	applicable: bool,
	names: Vec<String>,
}

/// The bound identifiers a pattern introduces, in left-to-right occurrence
/// order, including every nested binder (destructuring, `...rest`, `A | B`).
fn pattern_bindings<'a>(pattern: &'a Pattern, out: &mut Vec<(&'a str, Span)>) {
	match pattern {
		Pattern::Binding { name, inner } => {
			out.push((name.0.as_str(), name.1));
			pattern_bindings(&inner.0, out);
		}
		Pattern::List(items) | Pattern::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListPatternEntry::Item(p) => pattern_bindings(&p.0, out),
					ListPatternEntry::Rest(Some(name)) => out.push((name.0.as_str(), name.1)),
					ListPatternEntry::Rest(None) => {}
				}
			}
		}
		Pattern::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapPatternEntry::Entry(k, v) => {
						pattern_bindings(&k.0, out);
						pattern_bindings(&v.0, out);
					}
					MapPatternEntry::Rest(Some(name)) => out.push((name.0.as_str(), name.1)),
					MapPatternEntry::Rest(None) => {}
				}
			}
		}
		Pattern::Struct { fields, .. } => {
			for field in fields {
				match &field.0 {
					StructPatternField::Value { value, .. } => pattern_bindings(&value.0, out),
					StructPatternField::Positional(value) => pattern_bindings(&value.0, out),
					StructPatternField::Named(name) => out.push((name.0.as_str(), name.1)),
					StructPatternField::Rest => {}
				}
			}
		}
		Pattern::Union(a, b) => {
			pattern_bindings(&a.0, out);
			pattern_bindings(&b.0, out);
		}
		Pattern::Grouped(inner) => pattern_bindings(&inner.0, out),
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Range(_)
		| Pattern::Placeholder => {}
	}
}

/// Whether the identifier `Expr` node `target_id` resolves to a local/
/// parameter binder rather than falling through to a top-level definition —
/// see [`walk_decl_for_binder`]'s `Option<Option<Span>>` convention. Used by
/// [`type_at`]'s call-site signature upgrade to avoid misreporting a
/// function-typed local that happens to shadow a top-level `func` of the
/// same name.
fn is_shadowed_by_local(module: &Module, target_id: NodeId) -> bool {
	module
		.members
		.iter()
		.any(|decl| matches!(walk_decl_for_binder(decl, target_id), Some(Some(_))))
}

/// The [`FuncDeclaration`] meta of a top-level `func`/`external func`
/// declaration, for [`render_named_signature`]. `None` for any other
/// declaration kind — a [`crate::def::DefKind::Func`] always indexes one of
/// these two arms, so this only fails to find one if `member` is stale.
fn func_decl_meta(decl: &Declaration) -> Option<&FuncDeclaration> {
	match decl {
		Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => Some(meta),
		_ => None,
	}
}

/// Walk `decl` looking for the `Expr` node with id `target_id`. `None` means
/// it wasn't found in this declaration; `Some(resolution)` means it was found
/// (a local/param binder's span, or `None` if no local/param binder covers
/// it — the caller then falls back to the top-level `DefMap`).
fn walk_decl_for_binder(decl: &Declaration, target_id: NodeId) -> Option<Option<Span>> {
	match decl {
		Declaration::Import { .. }
		| Declaration::ExternalLet(..)
		| Declaration::ExternalFunc(..)
		| Declaration::TypeAlias { .. } => None,
		Declaration::Func { meta, body, .. } => {
			let mut scopes = Vec::new();
			for p in &meta.params {
				pattern_bindings(&p.0.name.0, &mut scopes);
			}
			walk_expr(body, target_id, &mut scopes)
		}
		Declaration::Let { value, .. } => walk_expr(value, target_id, &mut Vec::new()),
		Declaration::Struct {
			fields,
			members,
			impls,
			..
		} => {
			for f in fields {
				if let Some(default) = &f.0.default
					&& let Some(r) = walk_expr(default, target_id, &mut Vec::new())
				{
					return Some(r);
				}
			}
			for m in members {
				if let Some(r) = walk_impl_member_for_binder(&m.0, target_id) {
					return Some(r);
				}
			}
			for si in impls {
				for m in &si.0.members {
					if let Some(r) = walk_impl_member_for_binder(&m.0, target_id) {
						return Some(r);
					}
				}
			}
			None
		}
		Declaration::Enum { members, impls, .. } => {
			for m in members {
				if let Some(r) = walk_impl_member_for_binder(&m.0, target_id) {
					return Some(r);
				}
			}
			for si in impls {
				for m in &si.0.members {
					if let Some(r) = walk_impl_member_for_binder(&m.0, target_id) {
						return Some(r);
					}
				}
			}
			None
		}
		Declaration::Namespace { members, .. } => {
			for m in members {
				if let Some(r) = walk_impl_member_for_binder(&m.0, target_id) {
					return Some(r);
				}
			}
			None
		}
		Declaration::Interface { members, .. } => {
			for m in members {
				if let Some(r) = walk_interface_member_for_binder(&m.0, target_id) {
					return Some(r);
				}
			}
			None
		}
		Declaration::Impl { members, .. } | Declaration::ImplFor { members, .. } => {
			for m in members {
				if let Some(r) = walk_impl_member_for_binder(&m.0, target_id) {
					return Some(r);
				}
			}
			None
		}
	}
}

fn walk_impl_member_for_binder(member: &ImplMember, target_id: NodeId) -> Option<Option<Span>> {
	match member {
		ImplMember::ExternalLet(..) | ImplMember::ExternalFunc(..) => None,
		ImplMember::Func { meta, body, .. } => {
			let mut scopes = Vec::new();
			for p in &meta.params {
				pattern_bindings(&p.0.name.0, &mut scopes);
			}
			walk_expr(body, target_id, &mut scopes)
		}
		ImplMember::Let { value, .. } => walk_expr(value, target_id, &mut Vec::new()),
	}
}

fn walk_interface_member_for_binder(
	member: &InterfaceMember,
	target_id: NodeId,
) -> Option<Option<Span>> {
	match member {
		InterfaceMember::Element(elem) => match &elem.0 {
			InterfaceElement::Let { value, .. } => value
				.as_ref()
				.and_then(|v| walk_expr(v, target_id, &mut Vec::new())),
			InterfaceElement::Func { meta, body } => {
				let body = body.as_ref()?;
				let mut scopes = Vec::new();
				for p in &meta.params {
					pattern_bindings(&p.0.name.0, &mut scopes);
				}
				walk_expr(body, target_id, &mut scopes)
			}
		},
		InterfaceMember::Impl { members, .. } => {
			for m in members {
				if let Some(r) = walk_impl_member_for_binder(&m.0, target_id) {
					return Some(r);
				}
			}
			None
		}
	}
}

/// Walk `expr` for the node with id `target_id`, threading a shadowing-order
/// scope stack of `(name, binder_span)` pairs. See [`walk_decl_for_binder`]
/// for the `Option<Option<_>>` "found?/resolved?" convention.
fn walk_expr<'a>(
	expr: &'a Expr,
	target_id: NodeId,
	scopes: &mut Vec<(&'a str, Span)>,
) -> Option<Option<Span>> {
	if expr.id == target_id {
		let ExprKind::Identifier(ident) = &expr.kind else {
			// Some other node happens to share the id space at this point —
			// go-to-definition only ever targets `Identifier` nodes, so
			// anything else at this id is not our target.
			return None;
		};
		let name = ident.0.as_str();
		return Some(
			scopes
				.iter()
				.rev()
				.find(|(n, _)| *n == name)
				.map(|(_, span)| *span),
		);
	}

	match &expr.kind {
		ExprKind::Int(_)
		| ExprKind::UInt(_)
		| ExprKind::Float(_)
		| ExprKind::Char(_)
		| ExprKind::Boolean(_)
		| ExprKind::Identifier(_)
		| ExprKind::AnonymousParam(_)
		| ExprKind::This
		| ExprKind::Continue { .. } => None,
		ExprKind::String(parts) => parts.iter().find_map(|p| match &p.0 {
			StringPart::InterpolatedExpr(e) => walk_expr(e, target_id, scopes),
			_ => None,
		}),
		ExprKind::List(items) | ExprKind::Tuple(items) => items.iter().find_map(|item| match &item.0 {
			ListItem::Expr(e) | ListItem::Spread(e) => walk_expr(e, target_id, scopes),
		}),
		ExprKind::Map(entries) => entries.iter().find_map(|entry| match &entry.0 {
			MapEntry::Entry(k, v) => {
				walk_expr(k, target_id, scopes).or_else(|| walk_expr(v, target_id, scopes))
			}
			MapEntry::Spread(e) => walk_expr(e, target_id, scopes),
		}),
		ExprKind::Range(kind) => match kind {
			RangeKind::From(e) | RangeKind::To(e) | RangeKind::ToInclusive(e) => {
				walk_expr(e, target_id, scopes)
			}
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				walk_expr(min, target_id, scopes).or_else(|| walk_expr(max, target_id, scopes))
			}
		},
		ExprKind::Call { func, args, .. } => walk_expr(func, target_id, scopes).or_else(|| {
			args
				.iter()
				.find_map(|arg| walk_expr(&arg.0.value, target_id, scopes))
		}),
		ExprKind::MemberAccess { parent, .. } => walk_expr(parent, target_id, scopes),
		ExprKind::IndexAccess { parent, index, .. } => {
			walk_expr(parent, target_id, scopes).or_else(|| walk_expr(index, target_id, scopes))
		}
		ExprKind::Closure { params, body, .. } => {
			let base = scopes.len();
			for p in params {
				pattern_bindings(&p.0.name.0, scopes);
			}
			let result = walk_expr(body, target_id, scopes);
			scopes.truncate(base);
			result
		}
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
			walk_expr(value, target_id, scopes)
		}
		ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
			walk_expr(lhs, target_id, scopes).or_else(|| walk_expr(rhs, target_id, scopes))
		}
		ExprKind::TypeOp { lhs, .. } | ExprKind::PatternOp { lhs, .. } => {
			walk_expr(lhs, target_id, scopes)
		}
		ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
			value.as_ref().and_then(|v| walk_expr(v, target_id, scopes))
		}
		ExprKind::While {
			condition, body, ..
		} => walk_expr(condition, target_id, scopes).or_else(|| walk_expr(body, target_id, scopes)),
		ExprKind::For {
			variable,
			iterable,
			body,
			..
		} => {
			if let Some(r) = walk_expr(iterable, target_id, scopes) {
				return Some(r);
			}
			let base = scopes.len();
			pattern_bindings(&variable.0, scopes);
			let result = walk_expr(body, target_id, scopes);
			scopes.truncate(base);
			result
		}
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => walk_expr(condition, target_id, scopes)
			.or_else(|| walk_expr(then, target_id, scopes))
			.or_else(|| {
				otherwise
					.as_ref()
					.and_then(|o| walk_expr(o, target_id, scopes))
			}),
		ExprKind::Match { value, arms } => {
			if let Some(r) = walk_expr(value, target_id, scopes) {
				return Some(r);
			}
			for arm in arms {
				let base = scopes.len();
				pattern_bindings(&arm.pattern.0, scopes);
				let result = arm
					.guard
					.as_ref()
					.and_then(|g| walk_expr(g, target_id, scopes))
					.or_else(|| walk_expr(&arm.body, target_id, scopes));
				scopes.truncate(base);
				if result.is_some() {
					return result;
				}
			}
			None
		}
		ExprKind::Block { body, .. } => {
			let base = scopes.len();
			let mut result = None;
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) => {
						result = walk_expr(e, target_id, scopes);
					}
					Statement::Let { meta, value } => {
						result = walk_expr(value, target_id, scopes);
						if result.is_none() {
							pattern_bindings(&meta.name.0, scopes);
						}
					}
				}
				if result.is_some() {
					break;
				}
			}
			scopes.truncate(base);
			result
		}
		ExprKind::Grouped(inner) => walk_expr(inner, target_id, scopes),
	}
}

// ── `this.method()` go-to-definition (BUG 2a) ──────────────────────────────
//
// `definition_at`'s identifier walk never targets a `MemberAccess`'s own
// `member` — go-to-def on `this.method()` therefore falls through to here.
// Resolved PURELY SYNTACTICALLY (no `Checked`, no solver): each top-level
// declaration syntactically pins what `this` means inside it (a struct/enum
// IS its own Self type; a top-level `impl`/`impl .. for ..`'s `this` is its
// own `type_` header), so the method name under the cursor plus that Self
// type name is enough to search the whole module for a uniquely-named
// method and jump there. `Interface`/`Namespace`/anything else has no fixed
// Self type from its own header alone and is skipped — conservative, never
// a wrong jump.

/// The Self-type name `this` refers to inside `decl`'s own body/members, if
/// syntactically fixed by `decl`'s own header. `None` for a kind whose
/// `this` isn't determined by the header alone (`Interface` — a default
/// body's `this` is an unknown implementor; `Namespace` — no `this` at all).
fn self_type_name(decl: &Declaration) -> Option<EcoString> {
	match decl {
		Declaration::Struct { name, .. } | Declaration::Enum { name, .. } => Some(name.0.clone()),
		Declaration::Impl { type_, .. } | Declaration::ImplFor { type_, .. } => type_ref_name(&type_.0),
		_ => None,
	}
}

/// Peel `Type::Mut`/`Type::Grouped` down to a `Type::Reference`'s own name —
/// the surface spellings `impl Point`, `impl mut Point`, `impl (Point)` all
/// name the same Self type.
fn type_ref_name(ty: &Type) -> Option<EcoString> {
	match ty {
		Type::Reference { name, .. } => Some(name.0.clone()),
		Type::Mut(inner) | Type::Grouped(inner) => type_ref_name(&inner.0),
		_ => None,
	}
}

/// Find the `func` member named `method_name` declared on Self type
/// `self_name`, searching the WHOLE module (not just one declaration) —
/// `self_name` may be implemented via a top-level `impl`/`impl .. for ..`
/// elsewhere in the module, not only the struct/enum's own inline members.
/// Returns `None` — never a wrong jump — unless exactly one such method
/// exists; a field of the same name, or an ambiguous multiply-declared
/// method, both yield `None`.
fn unique_method_span(module: &Module, self_name: &str, method_name: &str) -> Option<Span> {
	let mut spans = Vec::new();
	for decl in &module.members {
		match decl {
			Declaration::Struct {
				name,
				members,
				impls,
				..
			}
			| Declaration::Enum {
				name,
				members,
				impls,
				..
			} if name.0.as_str() == self_name => {
				collect_method_spans(members, method_name, &mut spans);
				for si in impls {
					collect_method_spans(&si.0.members, method_name, &mut spans);
				}
			}
			Declaration::Impl { type_, members, .. } | Declaration::ImplFor { type_, members, .. }
				if type_ref_name(&type_.0).as_deref() == Some(self_name) =>
			{
				collect_method_spans(members, method_name, &mut spans);
			}
			_ => {}
		}
	}
	match spans.as_slice() {
		[span] => Some(*span),
		_ => None,
	}
}

fn collect_method_spans(members: &[Spanned<ImplMember>], method_name: &str, out: &mut Vec<Span>) {
	for m in members {
		match &m.0 {
			ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta)
				if meta.name.0.as_str() == method_name =>
			{
				out.push(meta.name.1);
			}
			_ => {}
		}
	}
}

/// Find the declaration span of the method named by a `this.method` access
/// covering byte `offset` in `module`. `None` when `offset` isn't over such
/// an access, its enclosing declaration's Self type isn't syntactically
/// fixed (see [`self_type_name`]), or the method name doesn't resolve
/// uniquely (see [`unique_method_span`]) — e.g. `this.field` (no `Func`
/// member of that name) always yields `None`.
fn this_method_definition_at(module: &Module, offset: usize) -> Option<Span> {
	for decl in &module.members {
		let Some(self_name) = self_type_name(decl) else {
			continue;
		};

		let mut exprs = Vec::new();
		collect_decl_exprs(decl, &mut exprs);
		let target = exprs
			.into_iter()
			.filter(|e| {
				matches!(
					&e.kind,
					ExprKind::MemberAccess { parent, member, .. }
						if matches!(parent.kind, ExprKind::This) && covers(member.1, offset)
				)
			})
			.min_by_key(|e| e.span.end - e.span.start);

		if let Some(target) = target {
			let ExprKind::MemberAccess { member, .. } = &target.kind else {
				unreachable!("filtered to MemberAccess above")
			};
			return unique_method_span(module, self_name.as_str(), member.0.as_str());
		}
	}
	None
}

// ── Type-position references (for go-to-definition on a type name) ────────

/// `Type::Reference` names reachable from `decl`'s own signature (param/
/// return/field/let annotations, a type alias's value) — NOT from expressions
/// nested inside bodies (closures' own annotations, `as`/`is` operand types),
/// which would need threading this walk through [`walk_expr`] too. Good
/// enough for the common "jump from an annotation to its struct/enum/
/// interface" case; anything missed here simply returns `None`.
fn collect_decl_type_refs<'a>(decl: &'a Declaration, out: &mut Vec<(&'a Ident, Span)>) {
	match decl {
		Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
			for p in &meta.params {
				collect_type_refs(&p.0.type_, out);
			}
			if let Some(rt) = &meta.return_type {
				collect_type_refs(rt, out);
			}
		}
		Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
			if let Some(t) = &meta.type_ {
				collect_type_refs(t, out);
			}
		}
		Declaration::TypeAlias { value, .. } => collect_type_refs(value, out),
		Declaration::Struct {
			fields, members, ..
		} => {
			for f in fields {
				collect_type_refs(&f.0.type_, out);
			}
			for m in members {
				collect_impl_member_type_refs(&m.0, out);
			}
		}
		Declaration::Enum {
			variants, members, ..
		} => {
			for v in variants {
				for f in &v.0.fields {
					collect_type_refs(&f.0.type_, out);
				}
			}
			for m in members {
				collect_impl_member_type_refs(&m.0, out);
			}
		}
		Declaration::Namespace { members, .. } => {
			for m in members {
				collect_impl_member_type_refs(&m.0, out);
			}
		}
		Declaration::Interface { members, .. } => {
			for m in members {
				if let InterfaceMember::Element(elem) = &m.0
					&& let InterfaceElement::Func { meta, .. } = &elem.0
				{
					for p in &meta.params {
						collect_type_refs(&p.0.type_, out);
					}
					if let Some(rt) = &meta.return_type {
						collect_type_refs(rt, out);
					}
				}
			}
		}
		Declaration::Impl { members, .. } | Declaration::ImplFor { members, .. } => {
			for m in members {
				collect_impl_member_type_refs(&m.0, out);
			}
		}
		Declaration::Import { .. } => {}
	}
}

fn collect_impl_member_type_refs<'a>(member: &'a ImplMember, out: &mut Vec<(&'a Ident, Span)>) {
	match member {
		ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => {
			for p in &meta.params {
				collect_type_refs(&p.0.type_, out);
			}
			if let Some(rt) = &meta.return_type {
				collect_type_refs(rt, out);
			}
		}
		ImplMember::Let { meta, .. } | ImplMember::ExternalLet(_, _, meta) => {
			if let Some(t) = &meta.type_ {
				collect_type_refs(t, out);
			}
		}
	}
}

fn collect_type_refs<'a>(ty: &'a Spanned<Type>, out: &mut Vec<(&'a Ident, Span)>) {
	match &ty.0 {
		Type::Reference { name, generics } => {
			out.push((name, ty.1));
			for g in generics {
				collect_type_refs(&g.0.value, out);
			}
		}
		Type::List(inner) | Type::Grouped(inner) | Type::Mut(inner) => collect_type_refs(inner, out),
		Type::Tuple(elems) => {
			for e in elems {
				collect_type_refs(e, out);
			}
		}
		Type::Map(k, v) => {
			collect_type_refs(k, out);
			collect_type_refs(v, out);
		}
		Type::Function {
			params,
			return_type,
		} => {
			for (_, t) in params {
				collect_type_refs(t, out);
			}
			collect_type_refs(return_type, out);
		}
		Type::Intersection(a, b) => {
			collect_type_refs(a, out);
			collect_type_refs(b, out);
		}
		Type::Int
		| Type::UInt
		| Type::Float
		| Type::Char
		| Type::String
		| Type::Boolean
		| Type::Void
		| Type::Never
		| Type::SelfType
		| Type::Infer => {}
	}
}

// ── Tier-2 hover fallback ────────────────────────────────────────────────────
//
// [`type_at`]'s primary path only ever resolves an annotated `Expr` — which,
// per the module doc comment, excludes patterns/binders, types, and
// declaration names entirely. This fallback fires only when that primary
// path yields nothing (no covering expr, a suppressed container, or a
// missing/`Error`/`Infer` annotation) and mirrors the existing
// `collect_decl_type_refs`/`pattern_bindings` walks used by
// go-to-definition/completion, rendering a TYPE at each tight, non-container
// position instead of a span. Every candidate here keys off a narrow span —
// a binder name, a param name, a decl name, a struct-field name, or an
// individual `Spanned<Type>` node — never a whole declaration/body span, so
// keywords/operators/brackets/commas (covered only by a suppressed
// container) never pick up a candidate and correctly stay `None`.

/// Tier-2 fallback for [`type_at`]: collect every fallback position reachable
/// from `module`, keep those whose span covers `offset`, and render the
/// smallest. Returns `None` when nothing at `offset` is a recognized
/// fallback position (e.g. a keyword, an operator, or whitespace inside a
/// suppressed container).
fn fallback_type_at(module: &Module, checked: &Checked, offset: usize) -> Option<String> {
	let defs = &checked.semantic.definitions;
	let params = generic_scope_at(module, offset);

	let mut candidates: Vec<(Span, String)> = Vec::new();
	for decl in &module.members {
		collect_fallback_decl(decl, checked, module, &defs, &params, &mut candidates);
	}

	candidates
		.into_iter()
		.filter(|(span, _)| covers(*span, offset))
		.min_by_key(|(span, _)| span.end - span.start)
		.map(|(_, rendered)| rendered)
}

/// A faithful *syntactic* renderer for a surface [`Type`] node (as opposed to
/// [`render`], which renders an already-resolved semantic `Ty`). A written
/// type's generic-parameter names are already its source names (a param
/// typed `V` is literally a `Type::Reference` named `"V"`), so — unlike
/// [`render`] — this needs no generic-scope threading. Returns `None` only
/// for `Type::Infer` (`_`), which carries no renderable information.
fn render_type_node(ty: &Type) -> Option<String> {
	match ty {
		Type::Int => Some("int".to_string()),
		Type::UInt => Some("uint".to_string()),
		Type::Float => Some("float".to_string()),
		Type::Char => Some("char".to_string()),
		Type::String => Some("string".to_string()),
		Type::Boolean => Some("boolean".to_string()),
		Type::Void => Some("void".to_string()),
		Type::Never => Some("never".to_string()),
		Type::SelfType => Some("self".to_string()),
		Type::Infer => None,
		Type::Intersection(a, b) => Some(format!(
			"{} + {}",
			render_type_node(&a.0)?,
			render_type_node(&b.0)?
		)),
		Type::List(inner) => Some(format!("#[{}]", render_type_node(&inner.0)?)),
		Type::Tuple(elems) => {
			let inner: Option<Vec<String>> = elems.iter().map(|e| render_type_node(&e.0)).collect();
			Some(format!("#({})", inner?.join(", ")))
		}
		Type::Map(key, value) => Some(format!(
			"#{{{}: {}}}",
			render_type_node(&key.0)?,
			render_type_node(&value.0)?
		)),
		Type::Function {
			params,
			return_type,
		} => {
			let inner: Option<Vec<String>> = params.iter().map(|(_, t)| render_type_node(&t.0)).collect();
			Some(format!(
				"({}) -> {}",
				inner?.join(", "),
				render_type_node(&return_type.0)?
			))
		}
		Type::Reference { name, generics } => {
			if generics.is_empty() {
				return Some(name.0.to_string());
			}
			let inner: Option<Vec<String>> = generics
				.iter()
				.map(|g| {
					let rendered = render_type_node(&g.0.value.0)?;
					Some(match &g.0.name {
						Some(label) => format!("{} = {rendered}", label.0),
						None => rendered,
					})
				})
				.collect();
			Some(format!("{}<{}>", name.0, inner?.join(", ")))
		}
		Type::Grouped(inner) => render_type_node(&inner.0),
		Type::Mut(inner) => Some(format!("mut {}", render_type_node(&inner.0)?)),
	}
}

/// Like [`render_type_node`], but a bare `Type::Reference` (no generics of its
/// own) whose name is a key in `subst` renders as the substituted string
/// instead of its own source name — used to show `Some(v: int)`/`v -> int`
/// for a generic enum/struct matched against a concrete scrutinee, rather
/// than the still-generic `Some(v: T)`/`v -> T`. `subst` is empty for every
/// caller that has no semantic scrutinee type to substitute from (a plain
/// `let`/param destructure), which makes this behave exactly like
/// [`render_type_node`] in that case — zero behavior change there.
fn render_type_node_subst(ty: &Type, subst: &FxHashMap<EcoString, String>) -> Option<String> {
	match ty {
		Type::Int => Some("int".to_string()),
		Type::UInt => Some("uint".to_string()),
		Type::Float => Some("float".to_string()),
		Type::Char => Some("char".to_string()),
		Type::String => Some("string".to_string()),
		Type::Boolean => Some("boolean".to_string()),
		Type::Void => Some("void".to_string()),
		Type::Never => Some("never".to_string()),
		Type::SelfType => Some("self".to_string()),
		Type::Infer => None,
		Type::Intersection(a, b) => Some(format!(
			"{} + {}",
			render_type_node_subst(&a.0, subst)?,
			render_type_node_subst(&b.0, subst)?
		)),
		Type::List(inner) => Some(format!("#[{}]", render_type_node_subst(&inner.0, subst)?)),
		Type::Tuple(elems) => {
			let inner: Option<Vec<String>> = elems
				.iter()
				.map(|e| render_type_node_subst(&e.0, subst))
				.collect();
			Some(format!("#({})", inner?.join(", ")))
		}
		Type::Map(key, value) => Some(format!(
			"#{{{}: {}}}",
			render_type_node_subst(&key.0, subst)?,
			render_type_node_subst(&value.0, subst)?
		)),
		Type::Function {
			params,
			return_type,
		} => {
			let inner: Option<Vec<String>> = params
				.iter()
				.map(|(_, t)| render_type_node_subst(&t.0, subst))
				.collect();
			Some(format!(
				"({}) -> {}",
				inner?.join(", "),
				render_type_node_subst(&return_type.0, subst)?
			))
		}
		Type::Reference { name, generics } => {
			if generics.is_empty() {
				if let Some(rendered) = subst.get(name.0.as_str()) {
					return Some(rendered.clone());
				}
				return Some(name.0.to_string());
			}
			let inner: Option<Vec<String>> = generics
				.iter()
				.map(|g| {
					let rendered = render_type_node_subst(&g.0.value.0, subst)?;
					Some(match &g.0.name {
						Some(label) => format!("{} = {rendered}", label.0),
						None => rendered,
					})
				})
				.collect();
			Some(format!("{}<{}>", name.0, inner?.join(", ")))
		}
		Type::Grouped(inner) => render_type_node_subst(&inner.0, subst),
		Type::Mut(inner) => Some(format!("mut {}", render_type_node_subst(&inner.0, subst)?)),
	}
}

/// Build a `generic name -> rendered concrete type` substitution map from an
/// already-resolved `Adt(def_id, args)` — e.g. a `match` scrutinee's own
/// annotated type — for rendering a generic struct/enum's *declared* field
/// types as their concrete instantiation (see [`render_type_node_subst`]).
/// `def_id` must name a struct or enum declaration (any other `DefKind`
/// yields an empty map, so a caller that can't be sure just gets no
/// substitutions rather than a wrong one); the zip against `args.positional`
/// stops at whichever of the two is shorter, so a generic-arity mismatch
/// (only possible in an already-erroring program) is safe.
fn generic_subst_from_adt(
	interner: &Interner,
	defs: &DefMap,
	module: &Module,
	params: &[EcoString],
	def_id: DefId,
	args: &GenericArgs,
) -> FxHashMap<EcoString, String> {
	let mut subst = FxHashMap::default();
	let Some(member) = defs.local_member(def_id) else {
		return subst;
	};
	let generics: &[Spanned<GenericParam>] = match defs.data(def_id).kind {
		def::DefKind::Enum => match &module.members[member] {
			Declaration::Enum { generics, .. } => generics,
			_ => return subst,
		},
		def::DefKind::Struct => match &module.members[member] {
			Declaration::Struct { generics, .. } => generics,
			_ => return subst,
		},
		_ => return subst,
	};
	for (g, &t) in generics.iter().zip(args.positional.iter()) {
		subst.insert(g.0.name.0.clone(), render(interner, defs, params, t));
	}
	for (label, t) in &args.named {
		if generics.iter().any(|g| &g.0.name.0 == label) {
			subst.insert(label.clone(), render(interner, defs, params, *t));
		}
	}
	subst
}

/// [`generic_subst_from_adt`], but peeling `ty` down to an `Adt` first (through
/// a `mut` view) — the substitution map for a `Pattern::Struct` matched
/// against `ty` inside [`bind_pattern_semantic`]. Empty when `ty` isn't (a
/// view of) an `Adt` at all, which simply leaves every generic rendering as
/// its own still-generic source name — never a wrong substitution.
fn adt_generic_subst(
	checked: &Checked,
	defs: &DefMap,
	module: &Module,
	params: &[EcoString],
	ty: Ty,
) -> FxHashMap<EcoString, String> {
	let mut ty = ty;
	if let TyKind::Mut(inner) = checked.interner.kind(ty) {
		ty = *inner;
	}
	let TyKind::Adt(def_id, args) = checked.interner.kind(ty) else {
		return FxHashMap::default();
	};
	generic_subst_from_adt(&checked.interner, defs, module, params, *def_id, args)
}

/// The surface keyword prefix for a declaration's [`Visibility`] (`"public
/// "`, `"internal "`, `"private "`), or the empty string when unspecified —
/// mirrors `parse_visibility`'s surface grammar.
fn render_visibility_prefix(visibility: Option<Visibility>) -> &'static str {
	match visibility {
		Some(Visibility::Public) => "public ",
		Some(Visibility::Internal) => "internal ",
		Some(Visibility::Private) => "private ",
		None => "",
	}
}

/// A single declared generic parameter, with its bound if any: `T` or `T:
/// Area + Into<string>`. Used both inside a `<...>` header
/// ([`render_generics`]) and as its own fallback candidate at the param's
/// declaration span (a generic-param hover) — see the module-level plan for
/// why bounds live here rather than in [`generic_scope_at`]'s semantic
/// `Param(idx)` recovery.
fn render_generic_param(param: &GenericParam) -> String {
	match &param.constraint {
		Some(constraint) => format!(
			"{}: {}",
			param.name.0,
			render_type_node(&constraint.0).unwrap_or_else(|| param.name.0.to_string())
		),
		None => param.name.0.to_string(),
	}
}

/// A declaration's `<...>` generic-parameter header, or the empty string
/// when it declares none.
fn render_generics(generics: &[Spanned<GenericParam>]) -> String {
	if generics.is_empty() {
		return String::new();
	}
	let inner: Vec<String> = generics
		.iter()
		.map(|g| render_generic_param(&g.0))
		.collect();
	format!("<{}>", inner.join(", "))
}

/// Record a fallback candidate at every generic parameter's own declaration
/// span (a generic-param hover shows its name + bound) — see
/// [`render_generic_param`].
fn push_generic_param_candidates(
	generics: &[Spanned<GenericParam>],
	out: &mut Vec<(Span, String)>,
) {
	for g in generics {
		out.push((g.0.name.1, render_generic_param(&g.0)));
	}
}

/// A function parameter's bound *name*, rendered from its (syntactic)
/// pattern — used by [`render_named_signature`] to show `a: int` rather than
/// a bare `int`. `Binding`/`Placeholder` render directly; a destructuring
/// `Tuple`/`List` pattern renders best-effort as `#(a, b)`/`#[a, b]`. Any
/// other (exotic — struct/union/literal) pattern has no single sensible
/// name, so this returns `None` and the caller falls back to showing the
/// declared type alone, with no `name:` prefix.
fn render_param_pattern(pattern: &Pattern) -> Option<String> {
	fn render_entries(items: &[Spanned<ListPatternEntry>]) -> Vec<String> {
		items
			.iter()
			.filter_map(|item| match &item.0 {
				ListPatternEntry::Item(p) => render_param_pattern(&p.0),
				ListPatternEntry::Rest(Some(name)) => Some(format!("...{}", name.0)),
				ListPatternEntry::Rest(None) => Some("...".to_string()),
			})
			.collect()
	}

	match pattern {
		Pattern::Binding { name, .. } => Some(name.0.to_string()),
		Pattern::Placeholder => Some("_".to_string()),
		Pattern::Tuple(items) => Some(format!("#({})", render_entries(items).join(", "))),
		Pattern::List(items) => Some(format!("#[{}]", render_entries(items).join(", "))),
		_ => None,
	}
}

/// A `func`'s full named signature, rendered from its declared (unchecked)
/// `FuncDeclaration` meta: `[mut ]func name<G..>(p1: T1, p2: T2): Ret` — used
/// for both a declaration-site func name hover and a call-site callee hover
/// (F6). An untyped/inferred parameter or a missing return type falls back
/// to `_`/`void` respectively rather than failing the whole signature.
/// The inferred return type of an unannotated func, for
/// [`render_named_signature`]'s `inferred_ret` argument. `body.id` is
/// annotated with its return type only for an unannotated func — see
/// `members.rs`'s `infer_omitted_returns` trial pass (`infer_inherent_return`),
/// which runs the body in infer-mode (recording every node) and rolls back
/// the unify table but not the `Annotations` side-table, so the recorded
/// type survives. A plain top-level `Declaration::Func` gets no such trial
/// pass, so when `body` is itself a `Block`/`If`/`Match`/`Grouped` — the
/// container kinds `check_dispatch` (`infer_expr.rs`) recurses through via
/// `check(child, expected)` without ever recording the container's OWN node
/// id, only its children's — `checked.annotations.get(body.id)` misses
/// entirely; [`representative_return_exprs`] finds a descendant that DOES
/// carry a recording to fall back on instead of silently reporting `void`.
/// `None` if no such annotation exists (an annotated func's body is checked
/// in check-mode too, but never consults this helper's result) or the
/// found type is `Error`, or still has an unresolved inference variable
/// anywhere inside it — not just at its own top level, since e.g. an
/// unpinned empty-list return (`#[]`) records as `List(Infer(_))`, whose
/// OWN `TyKind` is `List` — a nested `_` must never leak into the rendered
/// signature (the caller falls back to `void` rather than surfacing
/// `<error>`/`#[_]`). The generic scope is re-derived at `body.span.start`
/// rather than reusing a scope threaded from the hover offset: a func-NAME
/// hover offset falls outside `body.span`, so every scope helper gating on
/// `covers` would return an empty scope there.
fn inferred_return(
	body: &Expr,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
) -> Option<String> {
	let info = checked.annotations.get(body.id).or_else(|| {
		let mut candidates = Vec::new();
		representative_return_exprs(body, &mut candidates);
		candidates
			.into_iter()
			.find_map(|c| checked.annotations.get(c.id))
	})?;
	if matches!(checked.interner.kind(info.ty), TyKind::Error)
		|| has_unresolved_infer(&checked.interner, info.ty)
	{
		return None;
	}
	let scope = generic_scope_at(module, body.span.start);
	Some(render(&checked.interner, defs, &scope, info.ty))
}

/// Depth-first collects every descendant of `expr` that `check_dispatch`
/// (`infer_expr.rs`) WOULD record on its own — i.e. peels straight through
/// `Grouped`/`Block`/`If`/`Match`, the four container kinds whose own node id
/// is never recorded (only their child(ren) are checked and, if a
/// non-container kind, recorded). A `Block`'s value is defined solely by its
/// trailing expression statement (or nothing, if empty/all-`let`, in which
/// case the block itself is `void` and no candidate is pushed for it); an
/// `If`'s by either branch (both are checked against the same `expected`);
/// a `Match`'s by every arm's body (multiple candidates guard against a
/// specific arm's own leaf not being the one that got recorded — e.g. an
/// integer literal that widens via `int_literal_coerces_to` is deliberately
/// never recorded by that `check_dispatch` arm, but a sibling arm usually
/// is). Every pushed candidate is a leaf kind `check_dispatch` DOES record,
/// so [`inferred_return`] only needs a single flat lookup per candidate.
fn representative_return_exprs<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
	match &expr.kind {
		ExprKind::Grouped(inner) => representative_return_exprs(inner, out),
		ExprKind::Block { body, .. } => {
			if let Some(Spanned(Statement::Expr(tail), _)) = body.last() {
				representative_return_exprs(tail, out);
			}
		}
		ExprKind::If {
			then, otherwise, ..
		} => {
			representative_return_exprs(then, out);
			if let Some(otherwise) = otherwise {
				representative_return_exprs(otherwise, out);
			}
		}
		ExprKind::Match { arms, .. } => {
			for arm in arms {
				representative_return_exprs(&arm.body, out);
			}
		}
		_ => out.push(expr),
	}
}

/// Whether `ty` (already deeply resolved by `Checker::record` at the moment
/// it was recorded — see `check.rs`'s doc comment on `record`) still
/// contains an unresolved inference variable anywhere within it, not just at
/// its own top level. Mirrors `Checker::has_infer` (`check.rs`) structurally,
/// but works directly off the already-resolved `Ty`: the unify table is
/// dropped once checking finishes, so there is nothing left to peel here — a
/// nested `Infer` found by this walk is a genuinely-unresolved variable
/// (e.g. an unpinned empty list literal's element type), not merely an
/// unzonked one.
fn has_unresolved_infer(interner: &Interner, ty: Ty) -> bool {
	match interner.kind(ty) {
		TyKind::Infer(_) => true,
		TyKind::List(elem) => has_unresolved_infer(interner, *elem),
		TyKind::Tuple(elems) => elems.iter().any(|&e| has_unresolved_infer(interner, e)),
		TyKind::Map(key, value) => {
			has_unresolved_infer(interner, *key) || has_unresolved_infer(interner, *value)
		}
		TyKind::Fn { params, ret } => {
			params.iter().any(|&p| has_unresolved_infer(interner, p))
				|| has_unresolved_infer(interner, *ret)
		}
		TyKind::Adt(_, args) => {
			args
				.positional
				.iter()
				.any(|&t| has_unresolved_infer(interner, t))
				|| args
					.named
					.iter()
					.any(|(_, t)| has_unresolved_infer(interner, *t))
		}
		TyKind::Intersection(parts) => parts.iter().any(|&p| has_unresolved_infer(interner, p)),
		TyKind::Mut(inner) => has_unresolved_infer(interner, *inner),
		_ => false,
	}
}

/// The `Expr` body of a top-level `func` declaration, for
/// [`inferred_return`]. `None` for an `external func` (no body) or any other
/// declaration kind.
fn func_decl_body(decl: &Declaration) -> Option<&Expr> {
	match decl {
		Declaration::Func { body, .. } => Some(body),
		_ => None,
	}
}

fn render_named_signature(meta: &FuncDeclaration, inferred_ret: Option<String>) -> String {
	let kind_kw = match meta.kind {
		FuncKind::Mut => "mut ",
		FuncKind::Namespace => "namespace ",
		FuncKind::Instance => "",
	};
	let generics = render_generics(&meta.generics);
	let params: Vec<String> = meta
		.params
		.iter()
		.map(|p| {
			let ty = render_type_node(&p.0.type_.0).unwrap_or_else(|| "_".to_string());
			let mut prefix = String::new();
			if p.0.spread {
				prefix.push_str("...");
			}
			if p.0.mutable {
				prefix.push_str("mut ");
			}
			match render_param_pattern(&p.0.name.0) {
				Some(name) => format!("{prefix}{name}: {ty}"),
				None => format!("{prefix}{ty}"),
			}
		})
		.collect();
	let ret = match &meta.return_type {
		Some(rt) => render_type_node(&rt.0).unwrap_or_else(|| "void".to_string()),
		None => inferred_ret.unwrap_or_else(|| "void".to_string()),
	};
	format!(
		"{kind_kw}func {}{generics}({}): {ret}",
		meta.name.0,
		params.join(", ")
	)
}

/// A struct field's own `name: Type` rendering (shared by the struct-decl
/// header and a field-decl-name hover) — an unrenderable (`_`) field type
/// still shows a name rather than dropping the field.
fn render_struct_field(field: &StructField) -> String {
	let ty = render_type_node(&field.type_.0).unwrap_or_else(|| "_".to_string());
	format!("{}: {}", field.name.0, ty)
}

/// A `struct` declaration's full structure: `[public ]struct Name<G..>(f1:
/// T1, f2: T2, ...)`. A struct with no fields omits the parens entirely
/// (`struct Marker`), matching the surface grammar (`parse_struct` only
/// parses a field list when a `(` follows the generics). Field-level
/// visibility and defaults are intentionally omitted — see the module's
/// design notes on faithfulness vs. scope.
fn render_struct_decl(
	visibility: Option<Visibility>,
	name: &Ident,
	generics: &[Spanned<GenericParam>],
	fields: &[Spanned<StructField>],
) -> String {
	let vis = render_visibility_prefix(visibility);
	let generics_str = render_generics(generics);
	if fields.is_empty() {
		return format!("{vis}struct {}{generics_str}", name.0);
	}
	let field_strs: Vec<String> = fields.iter().map(|f| render_struct_field(&f.0)).collect();
	format!(
		"{vis}struct {}{generics_str}({})",
		name.0,
		field_strs.join(", ")
	)
}

/// A single enum variant's own shape: a fieldless variant renders bare
/// (`V2`), a field-bearing one as `V1(f1: T1, ...)`.
fn render_enum_variant(variant: &EnumVariant) -> String {
	if variant.fields.is_empty() {
		return variant.name.0.to_string();
	}
	let field_strs: Vec<String> = variant
		.fields
		.iter()
		.map(|f| render_struct_field(&f.0))
		.collect();
	format!("{}({})", variant.name.0, field_strs.join(", "))
}

/// An enum's full structure: `enum Name<G..> { V1(f: T), V2, ... }`.
fn render_enum_decl(
	visibility: Option<Visibility>,
	name: &Ident,
	generics: &[Spanned<GenericParam>],
	variants: &[Spanned<EnumVariant>],
) -> String {
	let vis = render_visibility_prefix(visibility);
	let generics_str = render_generics(generics);
	let variant_strs: Vec<String> = variants.iter().map(|v| render_enum_variant(&v.0)).collect();
	if variant_strs.is_empty() {
		return format!("{vis}enum {}{generics_str} {{}}", name.0);
	}
	format!(
		"{vis}enum {}{generics_str} {{ {} }}",
		name.0,
		variant_strs.join(", ")
	)
}

/// An enum-variant *declaration name*'s own hover: `EnumName.Variant(f:
/// T, ...)` — its own shape, qualified by the enum it belongs to (upgraded
/// from just the owning enum's bare name).
fn render_variant_decl(enum_name: &Ident, variant: &EnumVariant) -> String {
	format!("{}.{}", enum_name.0, render_enum_variant(variant))
}

/// A single interface member's signature line: a `func` element renders its
/// full named signature (body omitted); a `let` element renders `let[ mut]
/// x: T`. A nested `impl` block (a super-interface default impl) is
/// summarized as `None` and dropped from the member list — its own members
/// still hover individually via [`collect_fallback_interface_member`].
fn render_interface_member(member: &InterfaceMember) -> Option<String> {
	match member {
		InterfaceMember::Element(elem) => match &elem.0 {
			InterfaceElement::Func { meta, .. } => Some(render_named_signature(meta, None)),
			InterfaceElement::Let { meta, .. } => {
				let modifier = match meta.kind {
					LetKind::Mut => "mut ",
					LetKind::Namespace => "namespace ",
					LetKind::Instance => "",
				};
				let name = render_param_pattern(&meta.name.0).unwrap_or_else(|| "_".to_string());
				let ty = meta
					.type_
					.as_ref()
					.and_then(|t| render_type_node(&t.0))
					.unwrap_or_else(|| "_".to_string());
				Some(format!("let {modifier}{name}: {ty}"))
			}
		},
		InterfaceMember::Impl { .. } => None,
	}
}

/// An interface's full structure: `interface Name<G..> { <members> }`.
fn render_interface_decl(
	visibility: Option<Visibility>,
	name: &Ident,
	generics: &[Spanned<GenericParam>],
	members: &[Spanned<InterfaceMember>],
) -> String {
	let vis = render_visibility_prefix(visibility);
	let generics_str = render_generics(generics);
	let member_strs: Vec<String> = members
		.iter()
		.filter_map(|m| render_interface_member(&m.0))
		.collect();
	if member_strs.is_empty() {
		return format!("{vis}interface {}{generics_str} {{}}", name.0);
	}
	format!(
		"{vis}interface {}{generics_str} {{ {} }}",
		name.0,
		member_strs.join(", ")
	)
}

/// Record a fallback candidate at every `Spanned<Type>` node reachable from
/// `ty` (F7 type-position names) — not just `Type::Reference`s like
/// [`collect_type_refs`], since a primitive (`int`) or structural
/// (`#[int]`/`(int) -> int`) type position must hover too. Recording every
/// nested node (not just the outermost) lets `fallback_type_at`'s
/// `min_by_key` pick the innermost covering node, exactly mirroring the
/// primary path's leaf-preference.
fn collect_type_node_candidates(ty: &Spanned<Type>, out: &mut Vec<(Span, String)>) {
	if let Some(rendered) = render_type_node(&ty.0) {
		out.push((ty.1, rendered));
	}
	match &ty.0 {
		Type::Reference { generics, .. } => {
			for g in generics {
				collect_type_node_candidates(&g.0.value, out);
			}
		}
		Type::List(inner) | Type::Grouped(inner) | Type::Mut(inner) => {
			collect_type_node_candidates(inner, out);
		}
		Type::Tuple(elems) => {
			for e in elems {
				collect_type_node_candidates(e, out);
			}
		}
		Type::Map(key, value) => {
			collect_type_node_candidates(key, out);
			collect_type_node_candidates(value, out);
		}
		Type::Function {
			params,
			return_type,
		} => {
			for (_, t) in params {
				collect_type_node_candidates(t, out);
			}
			collect_type_node_candidates(return_type, out);
		}
		Type::Intersection(a, b) => {
			collect_type_node_candidates(a, out);
			collect_type_node_candidates(b, out);
		}
		Type::Int
		| Type::UInt
		| Type::Float
		| Type::Char
		| Type::String
		| Type::Boolean
		| Type::Void
		| Type::Never
		| Type::SelfType
		| Type::Infer => {}
	}
}

/// Look up a struct's or an enum variant's declared fields by name, for
/// [`bind_pattern_semantic`]/[`bind_pattern_syntactic`]'s `Pattern::Struct`
/// case. Two shapes:
///
///   - An enum-variant pattern (`Circle(radius = r)`): the checker already
///     resolved which `(enum, variant)` this pattern matches and recorded it
///     keyed by the pattern's own span (patterns carry no `NodeId`) — see
///     [`crate::annotate::Annotations::pattern_variant_of`]. Look the variant
///     up by that name instead of re-deriving it from `pattern`'s own written
///     path, which — for a bare variant name — doesn't disambiguate which
///     enum on its own.
///   - A plain struct pattern (`Point(x = 0)`): the path's own last segment
///     names the struct directly.
///
/// Returns `None` when the def can't be found or isn't the expected kind —
/// callers treat that as "bind nothing here" rather than guessing.
enum ConstructorFieldTypes<'a> {
	Syntactic(Vec<(&'a str, &'a Spanned<Type>)>),
	Semantic {
		owner: DefId,
		generics: &'a [EcoString],
		fields: &'a [(EcoString, Ty)],
	},
}

fn struct_field_types<'m>(
	module: &'m Module,
	checked: &'m Checked,
	pattern: &Spanned<Pattern>,
) -> Option<ConstructorFieldTypes<'m>> {
	let defs = &checked.semantic.definitions;
	if let Some(res) = checked.annotations.pattern_variant_of(pattern.1) {
		let enum_id = variant_enum_definition(defs, res)?;
		if let Some(member) = defs.local_member(enum_id) {
			let Declaration::Enum { variants, .. } = &module.members[member] else {
				return None;
			};
			let variant = variants.iter().find(|v| v.0.name.0 == res.variant)?;
			return Some(ConstructorFieldTypes::Syntactic(
				variant
					.0
					.fields
					.iter()
					.map(|f| (f.0.name.0.as_str(), &f.0.type_))
					.collect(),
			));
		}
		let signature = checked.semantic.signatures.enums.get(&enum_id)?;
		return Some(ConstructorFieldTypes::Semantic {
			owner: enum_id,
			generics: &signature.generics,
			fields: &semantic_variant(checked, enum_id, res)?.fields,
		});
	}
	let Pattern::Struct { path, .. } = &pattern.0 else {
		return None;
	};
	let name = path.last()?;
	let id = defs.get(name.0.as_str())?;
	let def::DefKind::Struct = defs.data(id).kind else {
		return None;
	};
	if let Some(member) = defs.local_member(id) {
		let Declaration::Struct { fields, .. } = &module.members[member] else {
			return None;
		};
		return Some(ConstructorFieldTypes::Syntactic(
			fields
				.iter()
				.map(|f| (f.0.name.0.as_str(), &f.0.type_))
				.collect(),
		));
	}
	let signature = checked.semantic.signatures.structs.get(&id)?;
	Some(ConstructorFieldTypes::Semantic {
		owner: id,
		generics: &signature.generics,
		fields: &signature.fields,
	})
}

/// Shared `Pattern::Struct` leaf logic for both
/// [`bind_pattern_syntactic`] and [`bind_pattern_semantic`]: once a caller
/// has resolved `pattern`'s field list (struct or enum-variant, per
/// [`struct_field_types`]), bind each field pattern's own name(s) to that
/// field's declared `Type` node, recursing through
/// [`bind_pattern_syntactic`] for a field that destructures further.
fn bind_struct_pattern_fields(
	fields: &[Spanned<StructPatternField>],
	field_list: &[(&str, &Spanned<Type>)],
	subst: &FxHashMap<EcoString, String>,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	out: &mut Vec<(Span, String)>,
) {
	for field in fields {
		match &field.0 {
			StructPatternField::Value { name, value } => {
				if let Some((_, ft)) = field_list.iter().find(|(n, _)| *n == name.0.as_str()) {
					bind_pattern_syntactic(value, &ft.0, subst, checked, module, defs, out);
				}
			}
			StructPatternField::Named(name) => {
				if let Some((_, ft)) = field_list.iter().find(|(n, _)| *n == name.0.as_str())
					&& let Some(rendered) = render_type_node_subst(&ft.0, subst)
				{
					out.push((name.1, rendered));
				}
			}
			// A positional sub-pattern destructures the constructor's sole field; recurse
			// against that one field's declared type when there is exactly one.
			StructPatternField::Positional(value) => {
				if let [(_, ft)] = field_list {
					bind_pattern_syntactic(value, &ft.0, subst, checked, module, defs, out);
				}
			}
			StructPatternField::Rest => {}
		}
	}
}

fn bind_semantic_struct_pattern_fields(
	fields: &[Spanned<StructPatternField>],
	field_list: &[(EcoString, Ty)],
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	params: &[EcoString],
	out: &mut Vec<(Span, String)>,
) {
	for field in fields {
		match &field.0 {
			StructPatternField::Value { name, value } => {
				if let Some((_, ty)) = field_list.iter().find(|(field, _)| field == &name.0) {
					bind_pattern_semantic(value, *ty, checked, module, defs, params, out);
				}
			}
			StructPatternField::Named(name) => {
				if let Some((_, ty)) = field_list.iter().find(|(field, _)| field == &name.0)
					&& !matches!(checked.interner.kind(*ty), TyKind::Error | TyKind::Infer(_))
				{
					out.push((name.1, render(&checked.interner, defs, params, *ty)));
				}
			}
			StructPatternField::Positional(value) => {
				if let [(_, ty)] = field_list {
					bind_pattern_semantic(value, *ty, checked, module, defs, params, out);
				}
			}
			StructPatternField::Rest => {}
		}
	}
}

fn syntactic_generic_renderings(generics: &[EcoString], ty: &Type) -> Vec<EcoString> {
	let mut rendered = generics.to_vec();
	let Type::Reference { generics: args, .. } = ty else {
		return rendered;
	};
	let mut positional = 0;
	for arg in args {
		let Some(value) = render_type_node(&arg.0.value.0) else {
			continue;
		};
		if let Some(name) = &arg.0.name {
			if let Some(index) = generics.iter().position(|generic| generic == &name.0) {
				rendered[index] = value.into();
			}
		} else {
			if let Some(slot) = rendered.get_mut(positional) {
				*slot = value.into();
			}
			positional += 1;
		}
	}
	rendered
}

fn semantic_generic_renderings(
	checked: &Checked,
	defs: &DefMap,
	params: &[EcoString],
	ty: Ty,
	owner: DefId,
	generics: &[EcoString],
) -> Vec<EcoString> {
	let mut rendered = generics.to_vec();
	let mut ty = ty;
	if let TyKind::Mut(inner) = checked.interner.kind(ty) {
		ty = *inner;
	}
	let TyKind::Adt(definition, args) = checked.interner.kind(ty) else {
		return rendered;
	};
	if *definition != owner {
		return rendered;
	}
	for (slot, argument) in rendered.iter_mut().zip(&args.positional) {
		*slot = render(&checked.interner, defs, params, *argument).into();
	}
	for (name, argument) in &args.named {
		if let Some(index) = generics.iter().position(|generic| generic == name) {
			rendered[index] = render(&checked.interner, defs, params, *argument).into();
		}
	}
	rendered
}

/// Bind every name a *syntactic* `Type` node's matching pattern introduces to
/// its own precise type (F5/F7's destructuring case), walking `pattern` and
/// `ty` in lock-step. A structural mismatch — arity, a `Rest` entry against a
/// fixed tuple, a type that isn't compound the way the pattern is, or an
/// unresolvable struct/variant name — binds nothing for that subtree rather
/// than falling back to the whole enclosing type: an omitted candidate
/// leaves hover at `None`, which is always safe; a mismatched type is not.
fn bind_pattern_syntactic(
	pattern: &Spanned<Pattern>,
	ty: &Type,
	subst: &FxHashMap<EcoString, String>,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	out: &mut Vec<(Span, String)>,
) {
	match &pattern.0 {
		Pattern::Binding { name, inner } => {
			// A nullary-variant CONSTRUCTOR pattern (`Square`, `None`) parses
			// as a `Binding` with a `Placeholder` inner — see the module's
			// pattern-hover notes. It must not also render as a binder of
			// `ty` here (that would wrongly hover `Square` as its enclosing
			// enum type); its own decl hover is emitted separately by
			// [`push_pattern_variant_candidates`].
			let is_nullary_variant_ctor = matches!(inner.0, Pattern::Placeholder)
				&& checked.annotations.pattern_variant_of(pattern.1).is_some();
			if !is_nullary_variant_ctor && let Some(rendered) = render_type_node_subst(ty, subst) {
				out.push((name.1, rendered));
			}
			bind_pattern_syntactic(inner, ty, subst, checked, module, defs, out);
		}
		Pattern::Tuple(items) => {
			if let Type::Tuple(elems) = ty
				&& elems.len() == items.len()
				&& items
					.iter()
					.all(|i| matches!(i.0, ListPatternEntry::Item(_)))
			{
				for (item, elem_ty) in items.iter().zip(elems) {
					if let ListPatternEntry::Item(p) = &item.0 {
						bind_pattern_syntactic(p, &elem_ty.0, subst, checked, module, defs, out);
					}
				}
			}
		}
		Pattern::List(items) => {
			if let Type::List(elem) = ty {
				for item in items {
					match &item.0 {
						ListPatternEntry::Item(p) => {
							bind_pattern_syntactic(p, &elem.0, subst, checked, module, defs, out);
						}
						ListPatternEntry::Rest(Some(name)) => {
							if let Some(rendered) = render_type_node_subst(ty, subst) {
								out.push((name.1, rendered));
							}
						}
						ListPatternEntry::Rest(None) => {}
					}
				}
			}
		}
		Pattern::Map(entries) => {
			if let Type::Map(key, value) = ty {
				for entry in entries {
					match &entry.0 {
						MapPatternEntry::Entry(k, v) => {
							bind_pattern_syntactic(k, &key.0, subst, checked, module, defs, out);
							bind_pattern_syntactic(v, &value.0, subst, checked, module, defs, out);
						}
						MapPatternEntry::Rest(Some(name)) => {
							if let Some(rendered) = render_type_node_subst(ty, subst) {
								out.push((name.1, rendered));
							}
						}
						MapPatternEntry::Rest(None) => {}
					}
				}
			}
		}
		Pattern::Struct { fields, .. } => match struct_field_types(module, checked, pattern) {
			Some(ConstructorFieldTypes::Syntactic(field_list)) => {
				bind_struct_pattern_fields(
					fields,
					&field_list,
					&FxHashMap::default(),
					checked,
					module,
					defs,
					out,
				);
			}
			Some(ConstructorFieldTypes::Semantic {
				generics,
				fields: field_list,
				..
			}) => {
				let params = syntactic_generic_renderings(generics, ty);
				bind_semantic_struct_pattern_fields(
					fields, field_list, checked, module, defs, &params, out,
				);
			}
			None => {}
		},
		Pattern::Grouped(inner) => bind_pattern_syntactic(inner, ty, subst, checked, module, defs, out),
		Pattern::Union(a, b) => {
			bind_pattern_syntactic(a, ty, subst, checked, module, defs, out);
			bind_pattern_syntactic(b, ty, subst, checked, module, defs, out);
		}
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Range(_)
		| Pattern::Placeholder => {}
	}
}

/// Bind every name a pattern introduces to its own precise type (F4's
/// destructuring case), walking `pattern` and a *semantic* `Ty` in
/// lock-step — the same [`render`] the primary path uses for a plain
/// binding, applied at each structural position instead of once for the
/// whole pattern. Local constructors retain their source [`Type`] nodes;
/// imported constructors use the checked semantic field signatures and
/// substitute the matched ADT's arguments before rendering binders.
fn bind_pattern_semantic(
	pattern: &Spanned<Pattern>,
	ty: Ty,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	params: &[EcoString],
	out: &mut Vec<(Span, String)>,
) {
	match &pattern.0 {
		Pattern::Binding { name, inner } => {
			// See the matching guard in `bind_pattern_syntactic`: a
			// resolved nullary-variant constructor pattern must not also
			// render as a whole-value binder here.
			let is_nullary_variant_ctor = matches!(inner.0, Pattern::Placeholder)
				&& checked.annotations.pattern_variant_of(pattern.1).is_some();
			if !is_nullary_variant_ctor
				&& !matches!(checked.interner.kind(ty), TyKind::Error | TyKind::Infer(_))
			{
				out.push((name.1, render(&checked.interner, defs, params, ty)));
			}
			bind_pattern_semantic(inner, ty, checked, module, defs, params, out);
		}
		Pattern::Tuple(items) => {
			if let TyKind::Tuple(elems) = checked.interner.kind(ty)
				&& elems.len() == items.len()
				&& items
					.iter()
					.all(|i| matches!(i.0, ListPatternEntry::Item(_)))
			{
				let elems = elems.clone();
				for (item, elem_ty) in items.iter().zip(elems) {
					if let ListPatternEntry::Item(p) = &item.0 {
						bind_pattern_semantic(p, elem_ty, checked, module, defs, params, out);
					}
				}
			}
		}
		Pattern::List(items) => {
			if let TyKind::List(elem) = checked.interner.kind(ty) {
				let elem = *elem;
				for item in items {
					match &item.0 {
						ListPatternEntry::Item(p) => {
							bind_pattern_semantic(p, elem, checked, module, defs, params, out);
						}
						ListPatternEntry::Rest(Some(name)) => {
							if !matches!(checked.interner.kind(ty), TyKind::Error | TyKind::Infer(_)) {
								out.push((name.1, render(&checked.interner, defs, params, ty)));
							}
						}
						ListPatternEntry::Rest(None) => {}
					}
				}
			}
		}
		Pattern::Map(entries) => {
			if let TyKind::Map(key_ty, value_ty) = checked.interner.kind(ty) {
				let (key_ty, value_ty) = (*key_ty, *value_ty);
				for entry in entries {
					match &entry.0 {
						MapPatternEntry::Entry(k, v) => {
							bind_pattern_semantic(k, key_ty, checked, module, defs, params, out);
							bind_pattern_semantic(v, value_ty, checked, module, defs, params, out);
						}
						MapPatternEntry::Rest(Some(name)) => {
							if !matches!(checked.interner.kind(ty), TyKind::Error | TyKind::Infer(_)) {
								out.push((name.1, render(&checked.interner, defs, params, ty)));
							}
						}
						MapPatternEntry::Rest(None) => {}
					}
				}
			}
		}
		Pattern::Struct { fields, .. } => match struct_field_types(module, checked, pattern) {
			Some(ConstructorFieldTypes::Syntactic(field_list)) => {
				let subst = adt_generic_subst(checked, defs, module, params, ty);
				bind_struct_pattern_fields(fields, &field_list, &subst, checked, module, defs, out);
			}
			Some(ConstructorFieldTypes::Semantic {
				owner,
				generics,
				fields: field_list,
			}) => {
				let params = semantic_generic_renderings(checked, defs, params, ty, owner, generics);
				bind_semantic_struct_pattern_fields(
					fields, field_list, checked, module, defs, &params, out,
				);
			}
			None => {}
		},
		Pattern::Grouped(inner) => bind_pattern_semantic(inner, ty, checked, module, defs, params, out),
		Pattern::Union(a, b) => {
			bind_pattern_semantic(a, ty, checked, module, defs, params, out);
			bind_pattern_semantic(b, ty, checked, module, defs, params, out);
		}
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Range(_)
		| Pattern::Placeholder => {}
	}
}

// ── Match/for pattern hovers: variant names and binder types ───────────────
//
// A `Pattern` carries no `NodeId` (see the module doc comment), so hovering
// an enum-variant name in a match arm (`Circle` in `Circle(radius) -> ...`)
// or a field/element binder inside that pattern (`radius`) is invisible to
// both the primary `type_at` path (which only walks annotated `Expr`s) and
// [`fallback_type_at`]'s existing walk (which — until now — never descended
// into `arm.pattern`/a `for` loop's own `variable` pattern at all). These two
// entry points close that gap from [`collect_fallback_exprs`]'s `Match`/`For`
// arms, reusing [`bind_pattern_semantic`] for binder types and a small
// parallel walk ([`push_pattern_variant_candidates`]) for variant-name decls
// — both read-only against already-recorded checker data (the scrutinee/
// iterable's own annotated `Expr` type, plus
// [`crate::annotate::Annotations::pattern_variant_of`]).

/// A single pattern's resolved variant-name candidate: `EnumName.Variant(f:
/// T, ...)`, keyed at the tight span of just the variant's own name (never
/// the whole pattern, which would also cover its field binders) — reuses the
/// same [`render_enum_variant`] the rich-hover enum-decl renderer uses,
/// qualified by the enum name exactly like [`render_variant_decl`] (built
/// directly here since `res.enum_name` is a bare `EcoString`, not the
/// `&Ident` that helper expects). `None` (nothing pushed) when the
/// resolution's `(enum, variant)` names don't line up with a live
/// declaration — never guessed.
fn push_variant_decl_candidate(
	pattern: &Spanned<Pattern>,
	res: &VariantResolution,
	module: &Module,
	checked: &Checked,
	out: &mut Vec<(Span, String)>,
) {
	let Some(rendered) = render_variant_from_resolution(module, checked, res) else {
		return;
	};
	let span = match &pattern.0 {
		Pattern::Struct { path, .. } => match (path.first(), path.last()) {
			(Some(first), Some(last)) => first.1.to(last.1),
			_ => pattern.1,
		},
		Pattern::Binding { name, .. } => name.1,
		_ => pattern.1,
	};
	out.push((span, rendered));
}

/// Walk every sub-pattern reachable from `pattern` (mirroring
/// [`pattern_bindings`]'s recursion shape) and record a
/// [`push_variant_decl_candidate`] wherever
/// [`crate::annotate::Annotations::pattern_variant_of`] resolved that exact
/// sub-pattern's span to a variant — covers both a top-level match-arm
/// pattern and a variant pattern nested inside a tuple/list/map/struct/union.
fn push_pattern_variant_candidates(
	pattern: &Spanned<Pattern>,
	checked: &Checked,
	module: &Module,
	out: &mut Vec<(Span, String)>,
) {
	if let Some(res) = checked.annotations.pattern_variant_of(pattern.1) {
		push_variant_decl_candidate(pattern, res, module, checked, out);
	}
	match &pattern.0 {
		Pattern::Binding { inner, .. } => {
			push_pattern_variant_candidates(inner, checked, module, out);
		}
		Pattern::List(items) | Pattern::Tuple(items) => {
			for item in items {
				if let ListPatternEntry::Item(p) = &item.0 {
					push_pattern_variant_candidates(p, checked, module, out);
				}
			}
		}
		Pattern::Map(entries) => {
			for entry in entries {
				if let MapPatternEntry::Entry(k, v) = &entry.0 {
					push_pattern_variant_candidates(k, checked, module, out);
					push_pattern_variant_candidates(v, checked, module, out);
				}
			}
		}
		Pattern::Struct { fields, .. } => {
			for field in fields {
				match &field.0 {
					StructPatternField::Value { value, .. } | StructPatternField::Positional(value) => {
						push_pattern_variant_candidates(value, checked, module, out);
					}
					StructPatternField::Named(_) | StructPatternField::Rest => {}
				}
			}
		}
		Pattern::Union(a, b) => {
			push_pattern_variant_candidates(a, checked, module, out);
			push_pattern_variant_candidates(b, checked, module, out);
		}
		Pattern::Grouped(inner) => push_pattern_variant_candidates(inner, checked, module, out),
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Range(_)
		| Pattern::Placeholder => {}
	}
}

/// A `for` loop's own `variable` pattern binder(s) (F-for): derive the
/// element type from the iterable `Expr`'s own annotated type — a `List`
/// peels to its element directly; a `Map` is best-effort, recoverable only
/// when `variable` destructures as a 2-tuple `#(key, value)` (a whole-entry
/// binder would need this module to mint a fresh tuple `Ty`, which needs a
/// `&mut Interner` this read-only query never has). Anything else (a
/// `Range`, a user `Iterable`/`Iterator`) has no read-only-recoverable
/// element type here and contributes nothing — never a wrong type.
fn push_for_binder_candidates(
	variable: &Spanned<Pattern>,
	iterable: &Expr,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	params: &[EcoString],
	out: &mut Vec<(Span, String)>,
) {
	let Some(info) = checked.annotations.get(iterable.id) else {
		return;
	};
	if matches!(
		checked.interner.kind(info.ty),
		TyKind::Error | TyKind::Infer(_)
	) {
		return;
	}
	let mut ty = info.ty;
	if let TyKind::Mut(inner) = checked.interner.kind(ty) {
		ty = *inner;
	}
	match checked.interner.kind(ty) {
		TyKind::List(elem) => {
			bind_pattern_semantic(variable, *elem, checked, module, defs, params, out);
		}
		TyKind::Map(key, value) => {
			if let Pattern::Tuple(items) = &variable.0
				&& items.len() == 2
				&& let ListPatternEntry::Item(kp) = &items[0].0
				&& let ListPatternEntry::Item(vp) = &items[1].0
			{
				let (key, value) = (*key, *value);
				bind_pattern_semantic(kp, key, checked, module, defs, params, out);
				bind_pattern_semantic(vp, value, checked, module, defs, params, out);
			}
		}
		_ => {}
	}
}

/// A `let` binder's fallback candidate (F4): every name the pattern
/// introduces renders as its own structural slice of the initializer's
/// annotated (semantic) type — see [`bind_pattern_semantic`] — so it stays
/// consistent with how a later use of the same name would hover. Silently
/// records nothing when the initializer itself has no usable annotation
/// (missing, `Error`, or `Infer`), so a binder for an ill-typed initializer
/// stays `None` rather than showing `"<error>"`/`"_"`.
fn push_let_binder(
	name: &Spanned<Pattern>,
	value: &Expr,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	params: &[EcoString],
	out: &mut Vec<(Span, String)>,
) {
	let Some(info) = checked.annotations.get(value.id) else {
		return;
	};
	if matches!(
		checked.interner.kind(info.ty),
		TyKind::Error | TyKind::Infer(_)
	) {
		return;
	}
	bind_pattern_semantic(name, info.ty, checked, module, defs, params, out);
}

/// A parameter list's fallback candidates (F5 + F7): each param's own
/// `Type` node (F7), plus every name its pattern introduces rendered as its
/// own structural slice of that declared type (F5) — see
/// [`bind_pattern_syntactic`]. An untyped param (a closure param with no
/// annotation) simply contributes nothing here — handled by the caller.
fn push_func_param_types(
	func_params: &[Spanned<FuncParam>],
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	out: &mut Vec<(Span, String)>,
) {
	for p in func_params {
		collect_type_node_candidates(&p.0.type_, out);
		bind_pattern_syntactic(
			&p.0.name,
			&p.0.type_.0,
			&FxHashMap::default(),
			checked,
			module,
			defs,
			out,
		);
	}
}

fn collect_fallback_decl(
	decl: &Declaration,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	params: &[EcoString],
	out: &mut Vec<(Span, String)>,
) {
	match decl {
		Declaration::Import { .. } | Declaration::TypeAlias { .. } => {}
		Declaration::ExternalLet(_, _, meta) => {
			if let Some(t) = &meta.type_ {
				collect_type_node_candidates(t, out);
			}
		}
		Declaration::ExternalFunc(_, _, meta) => {
			out.push((meta.name.1, render_named_signature(meta, None)));
			push_generic_param_candidates(&meta.generics, out);
			push_func_param_types(&meta.params, checked, module, defs, out);
			if let Some(rt) = &meta.return_type {
				collect_type_node_candidates(rt, out);
			}
		}
		Declaration::Let { meta, value, .. } => {
			push_let_binder(&meta.name, value, checked, module, defs, params, out);
			if let Some(t) = &meta.type_ {
				collect_type_node_candidates(t, out);
			}
			collect_fallback_exprs(value, checked, module, defs, params, out);
		}
		Declaration::Func { meta, body, .. } => {
			let inferred_ret = inferred_return(body, checked, module, defs);
			out.push((meta.name.1, render_named_signature(meta, inferred_ret)));
			push_generic_param_candidates(&meta.generics, out);
			push_func_param_types(&meta.params, checked, module, defs, out);
			if let Some(rt) = &meta.return_type {
				collect_type_node_candidates(rt, out);
			}
			collect_fallback_exprs(body, checked, module, defs, params, out);
		}
		Declaration::Struct {
			visibility,
			name,
			generics,
			fields,
			members,
			impls,
		} => {
			out.push((
				name.1,
				render_struct_decl(*visibility, name, generics, fields),
			));
			push_generic_param_candidates(generics, out);
			for f in fields {
				out.push((f.0.name.1, render_struct_field(&f.0)));
				collect_type_node_candidates(&f.0.type_, out);
				if let Some(default) = &f.0.default {
					collect_fallback_exprs(default, checked, module, defs, params, out);
				}
			}
			for m in members {
				collect_fallback_impl_member(&m.0, checked, module, defs, params, out);
			}
			for si in impls {
				push_generic_param_candidates(&si.0.generics, out);
				for m in &si.0.members {
					collect_fallback_impl_member(&m.0, checked, module, defs, params, out);
				}
			}
		}
		Declaration::Enum {
			visibility,
			name,
			generics,
			variants,
			members,
			impls,
		} => {
			out.push((
				name.1,
				render_enum_decl(*visibility, name, generics, variants),
			));
			push_generic_param_candidates(generics, out);
			for v in variants {
				out.push((v.0.name.1, render_variant_decl(name, &v.0)));
				for f in &v.0.fields {
					collect_type_node_candidates(&f.0.type_, out);
				}
			}
			for m in members {
				collect_fallback_impl_member(&m.0, checked, module, defs, params, out);
			}
			for si in impls {
				push_generic_param_candidates(&si.0.generics, out);
				for m in &si.0.members {
					collect_fallback_impl_member(&m.0, checked, module, defs, params, out);
				}
			}
		}
		Declaration::Namespace { name, members, .. } => {
			out.push((name.1, name.0.to_string()));
			for m in members {
				collect_fallback_impl_member(&m.0, checked, module, defs, params, out);
			}
		}
		Declaration::Interface {
			visibility,
			name,
			generics,
			members,
			..
		} => {
			out.push((
				name.1,
				render_interface_decl(*visibility, name, generics, members),
			));
			push_generic_param_candidates(generics, out);
			for m in members {
				collect_fallback_interface_member(&m.0, checked, module, defs, params, out);
			}
		}
		Declaration::Impl {
			generics,
			type_,
			members,
			..
		} => {
			push_generic_param_candidates(generics, out);
			collect_type_node_candidates(type_, out);
			for m in members {
				collect_fallback_impl_member(&m.0, checked, module, defs, params, out);
			}
		}
		Declaration::ImplFor {
			generics,
			type_,
			members,
			..
		} => {
			push_generic_param_candidates(generics, out);
			collect_type_node_candidates(type_, out);
			for m in members {
				collect_fallback_impl_member(&m.0, checked, module, defs, params, out);
			}
		}
	}
}

fn collect_fallback_impl_member(
	member: &ImplMember,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	params: &[EcoString],
	out: &mut Vec<(Span, String)>,
) {
	match member {
		ImplMember::ExternalLet(_, _, meta) => {
			if let Some(t) = &meta.type_ {
				collect_type_node_candidates(t, out);
			}
		}
		ImplMember::ExternalFunc(_, _, meta) => {
			out.push((meta.name.1, render_named_signature(meta, None)));
			push_generic_param_candidates(&meta.generics, out);
			push_func_param_types(&meta.params, checked, module, defs, out);
			if let Some(rt) = &meta.return_type {
				collect_type_node_candidates(rt, out);
			}
		}
		ImplMember::Func { meta, body, .. } => {
			let inferred_ret = inferred_return(body, checked, module, defs);
			out.push((meta.name.1, render_named_signature(meta, inferred_ret)));
			push_generic_param_candidates(&meta.generics, out);
			push_func_param_types(&meta.params, checked, module, defs, out);
			if let Some(rt) = &meta.return_type {
				collect_type_node_candidates(rt, out);
			}
			collect_fallback_exprs(body, checked, module, defs, params, out);
		}
		ImplMember::Let { meta, value, .. } => {
			push_let_binder(&meta.name, value, checked, module, defs, params, out);
			if let Some(t) = &meta.type_ {
				collect_type_node_candidates(t, out);
			}
			collect_fallback_exprs(value, checked, module, defs, params, out);
		}
	}
}

fn collect_fallback_interface_member(
	member: &InterfaceMember,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	params: &[EcoString],
	out: &mut Vec<(Span, String)>,
) {
	match member {
		InterfaceMember::Element(elem) => match &elem.0 {
			InterfaceElement::Let { meta, value } => {
				if let Some(t) = &meta.type_ {
					collect_type_node_candidates(t, out);
				}
				if let Some(v) = value {
					push_let_binder(&meta.name, v, checked, module, defs, params, out);
					collect_fallback_exprs(v, checked, module, defs, params, out);
				}
			}
			InterfaceElement::Func { meta, body } => {
				let inferred_ret = body
					.as_ref()
					.and_then(|b| inferred_return(b, checked, module, defs));
				out.push((meta.name.1, render_named_signature(meta, inferred_ret)));
				push_generic_param_candidates(&meta.generics, out);
				push_func_param_types(&meta.params, checked, module, defs, out);
				if let Some(rt) = &meta.return_type {
					collect_type_node_candidates(rt, out);
				}
				if let Some(b) = body {
					collect_fallback_exprs(b, checked, module, defs, params, out);
				}
			}
		},
		InterfaceMember::Impl {
			generics, members, ..
		} => {
			push_generic_param_candidates(generics, out);
			for m in members {
				collect_fallback_impl_member(&m.0, checked, module, defs, params, out);
			}
		}
	}
}

/// Mirrors [`collect_expr`]'s recursion (every `Expr` reachable, in the same
/// shape), but additionally records three fallback-only positions along the
/// way: a `Block`'s own nested `let` binders (F4), a `Closure` param's
/// declared type (F5/F7), and a construction callee — a bare `Identifier` in
/// call position that the checker left unannotated (e.g. `Point` in
/// `Point(x = 0)`, as opposed to a real function call's callee, which the
/// checker always annotates with its `Fn` type and so already resolves via
/// the primary path) — resolved by name through `defs` (F7).
fn collect_fallback_exprs(
	expr: &Expr,
	checked: &Checked,
	module: &Module,
	defs: &DefMap,
	params: &[EcoString],
	out: &mut Vec<(Span, String)>,
) {
	match &expr.kind {
		ExprKind::Int(_)
		| ExprKind::UInt(_)
		| ExprKind::Float(_)
		| ExprKind::Char(_)
		| ExprKind::Boolean(_)
		| ExprKind::Identifier(_)
		| ExprKind::AnonymousParam(_)
		| ExprKind::This
		| ExprKind::Continue { .. } => {}
		ExprKind::String(parts) => {
			for p in parts {
				if let StringPart::InterpolatedExpr(e) = &p.0 {
					collect_fallback_exprs(e, checked, module, defs, params, out);
				}
			}
		}
		ExprKind::List(items) | ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(e) | ListItem::Spread(e) => {
						collect_fallback_exprs(e, checked, module, defs, params, out);
					}
				}
			}
		}
		ExprKind::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapEntry::Entry(k, v) => {
						collect_fallback_exprs(k, checked, module, defs, params, out);
						collect_fallback_exprs(v, checked, module, defs, params, out);
					}
					MapEntry::Spread(e) => collect_fallback_exprs(e, checked, module, defs, params, out),
				}
			}
		}
		ExprKind::Range(kind) => match kind {
			RangeKind::From(e) | RangeKind::To(e) | RangeKind::ToInclusive(e) => {
				collect_fallback_exprs(e, checked, module, defs, params, out);
			}
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				collect_fallback_exprs(min, checked, module, defs, params, out);
				collect_fallback_exprs(max, checked, module, defs, params, out);
			}
		},
		ExprKind::Call { func, args, .. } => {
			if let ExprKind::Identifier(name) = &func.kind
				&& checked.annotations.get(func.id).is_none()
			{
				// A construction callee the checker left unannotated: a bare
				// top-level name (a struct) resolves directly through `defs`;
				// a bare enum-variant name (e.g. `Circle` in `Circle(radius =
				// 1)`) isn't in `defs.by_name` at all (see `DefMap`'s own
				// doc comment) and needs the same `resolve_variant` fallback
				// `definition_at` already uses for go-to-definition (F7).
				if let Some(id) = defs.get(name.0.as_str()) {
					out.push((func.span, defs.data(id).name.to_string()));
				} else if let Some(Ok((enum_def, _variant))) = defs.resolve_variant(name.0.as_str()) {
					out.push((func.span, defs.data(enum_def).name.to_string()));
				}
			}
			collect_fallback_exprs(func, checked, module, defs, params, out);
			for arg in args {
				collect_fallback_exprs(&arg.0.value, checked, module, defs, params, out);
			}
		}
		ExprKind::MemberAccess { parent, .. } => {
			collect_fallback_exprs(parent, checked, module, defs, params, out);
		}
		ExprKind::IndexAccess { parent, index, .. } => {
			collect_fallback_exprs(parent, checked, module, defs, params, out);
			collect_fallback_exprs(index, checked, module, defs, params, out);
		}
		ExprKind::Closure {
			params: cparams,
			body,
			..
		} => {
			for p in cparams {
				if let Some(t) = &p.0.type_ {
					collect_type_node_candidates(t, out);
					bind_pattern_syntactic(
						&p.0.name,
						&t.0,
						&FxHashMap::default(),
						checked,
						module,
						defs,
						out,
					);
				}
			}
			collect_fallback_exprs(body, checked, module, defs, params, out);
		}
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
			collect_fallback_exprs(value, checked, module, defs, params, out);
		}
		ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
			collect_fallback_exprs(lhs, checked, module, defs, params, out);
			collect_fallback_exprs(rhs, checked, module, defs, params, out);
		}
		ExprKind::TypeOp { lhs, .. } | ExprKind::PatternOp { lhs, .. } => {
			collect_fallback_exprs(lhs, checked, module, defs, params, out);
		}
		ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
			if let Some(v) = value {
				collect_fallback_exprs(v, checked, module, defs, params, out);
			}
		}
		ExprKind::While {
			condition, body, ..
		} => {
			collect_fallback_exprs(condition, checked, module, defs, params, out);
			collect_fallback_exprs(body, checked, module, defs, params, out);
		}
		ExprKind::For {
			variable,
			iterable,
			body,
			..
		} => {
			collect_fallback_exprs(iterable, checked, module, defs, params, out);
			push_for_binder_candidates(variable, iterable, checked, module, defs, params, out);
			collect_fallback_exprs(body, checked, module, defs, params, out);
		}
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			collect_fallback_exprs(condition, checked, module, defs, params, out);
			collect_fallback_exprs(then, checked, module, defs, params, out);
			if let Some(o) = otherwise {
				collect_fallback_exprs(o, checked, module, defs, params, out);
			}
		}
		ExprKind::Match { value, arms } => {
			collect_fallback_exprs(value, checked, module, defs, params, out);
			let scrutinee_ty = checked.annotations.get(value.id).map(|info| info.ty);
			for arm in arms {
				push_pattern_variant_candidates(&arm.pattern, checked, module, out);
				if let Some(ty) = scrutinee_ty {
					bind_pattern_semantic(&arm.pattern, ty, checked, module, defs, params, out);
				}
				if let Some(g) = &arm.guard {
					collect_fallback_exprs(g, checked, module, defs, params, out);
				}
				collect_fallback_exprs(&arm.body, checked, module, defs, params, out);
			}
		}
		ExprKind::Block { body, .. } => {
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) => collect_fallback_exprs(e, checked, module, defs, params, out),
					Statement::Let { meta, value } => {
						push_let_binder(&meta.name, value, checked, module, defs, params, out);
						if let Some(t) = &meta.type_ {
							collect_type_node_candidates(t, out);
						}
						collect_fallback_exprs(value, checked, module, defs, params, out);
					}
				}
			}
		}
		ExprKind::Grouped(inner) => collect_fallback_exprs(inner, checked, module, defs, params, out),
	}
}

// ── In-scope names (for completion) ─────────────────────────────────────────

fn collect_decl_scope_names(decl: &Declaration, offset: usize, out: &mut ScopeNames) {
	match decl {
		Declaration::Import { .. }
		| Declaration::ExternalLet(..)
		| Declaration::ExternalFunc(..)
		| Declaration::TypeAlias { .. } => {}
		Declaration::Func { meta, body, .. } => {
			if covers(meta.name.1.to(body.span), offset) {
				let mut scopes = Vec::new();
				for p in &meta.params {
					pattern_bindings(&p.0.name.0, &mut scopes);
				}
				collect_expr_scope_names(body, offset, &mut scopes, out);
			}
		}
		Declaration::Let { value, .. } => {
			if covers(value.span, offset) {
				collect_expr_scope_names(value, offset, &mut Vec::new(), out);
			}
		}
		Declaration::Struct {
			fields,
			members,
			impls,
			..
		} => {
			for f in fields {
				if let Some(default) = &f.0.default {
					collect_expr_scope_names(default, offset, &mut Vec::new(), out);
				}
			}
			for m in members {
				collect_impl_member_scope_names(&m.0, offset, out);
			}
			for si in impls {
				for m in &si.0.members {
					collect_impl_member_scope_names(&m.0, offset, out);
				}
			}
		}
		Declaration::Enum { members, impls, .. } => {
			for m in members {
				collect_impl_member_scope_names(&m.0, offset, out);
			}
			for si in impls {
				for m in &si.0.members {
					collect_impl_member_scope_names(&m.0, offset, out);
				}
			}
		}
		Declaration::Namespace { members, .. } => {
			for m in members {
				collect_impl_member_scope_names(&m.0, offset, out);
			}
		}
		Declaration::Interface { members, .. } => {
			for m in members {
				if let InterfaceMember::Element(elem) = &m.0 {
					match &elem.0 {
						InterfaceElement::Let { value: Some(v), .. } => {
							collect_expr_scope_names(v, offset, &mut Vec::new(), out);
						}
						InterfaceElement::Func {
							meta,
							body: Some(body),
						} if covers(meta.name.1.to(body.span), offset) => {
							let mut scopes = Vec::new();
							for p in &meta.params {
								pattern_bindings(&p.0.name.0, &mut scopes);
							}
							collect_expr_scope_names(body, offset, &mut scopes, out);
						}
						_ => {}
					}
				}
			}
		}
		Declaration::Impl { members, .. } | Declaration::ImplFor { members, .. } => {
			for m in members {
				collect_impl_member_scope_names(&m.0, offset, out);
			}
		}
	}
}

fn collect_impl_member_scope_names(member: &ImplMember, offset: usize, out: &mut ScopeNames) {
	match member {
		ImplMember::ExternalLet(..) | ImplMember::ExternalFunc(..) => {}
		ImplMember::Func { meta, body, .. } => {
			if covers(meta.name.1.to(body.span), offset) {
				let mut scopes = Vec::new();
				for p in &meta.params {
					pattern_bindings(&p.0.name.0, &mut scopes);
				}
				collect_expr_scope_names(body, offset, &mut scopes, out);
			}
		}
		ImplMember::Let { value, .. } => {
			if covers(value.span, offset) {
				collect_expr_scope_names(value, offset, &mut Vec::new(), out);
			}
		}
	}
}

/// Walk down to the smallest `Expr` covering `offset`, threading the same
/// scope stack as [`walk_expr`], and record every binder on that path — the
/// names visible for completion at `offset`. Unlike `walk_expr` this never
/// "finds" a target node; it just narrows to the deepest covering child, so
/// `out` ends up holding the innermost enclosing scope's bindings.
fn collect_expr_scope_names<'a>(
	expr: &'a Expr,
	offset: usize,
	scopes: &mut Vec<(&'a str, Span)>,
	out: &mut ScopeNames,
) {
	if !covers(expr.span, offset) {
		return;
	}
	out.applicable = true;
	out.names.clear();
	out
		.names
		.extend(scopes.iter().map(|(n, _)| (*n).to_string()));

	match &expr.kind {
		ExprKind::Int(_)
		| ExprKind::UInt(_)
		| ExprKind::Float(_)
		| ExprKind::Char(_)
		| ExprKind::Boolean(_)
		| ExprKind::Identifier(_)
		| ExprKind::AnonymousParam(_)
		| ExprKind::This
		| ExprKind::Continue { .. } => {}
		ExprKind::String(parts) => {
			for p in parts {
				if let StringPart::InterpolatedExpr(e) = &p.0 {
					collect_expr_scope_names(e, offset, scopes, out);
				}
			}
		}
		ExprKind::List(items) | ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(e) | ListItem::Spread(e) => {
						collect_expr_scope_names(e, offset, scopes, out);
					}
				}
			}
		}
		ExprKind::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapEntry::Entry(k, v) => {
						collect_expr_scope_names(k, offset, scopes, out);
						collect_expr_scope_names(v, offset, scopes, out);
					}
					MapEntry::Spread(e) => collect_expr_scope_names(e, offset, scopes, out),
				}
			}
		}
		ExprKind::Range(kind) => match kind {
			RangeKind::From(e) | RangeKind::To(e) | RangeKind::ToInclusive(e) => {
				collect_expr_scope_names(e, offset, scopes, out);
			}
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				collect_expr_scope_names(min, offset, scopes, out);
				collect_expr_scope_names(max, offset, scopes, out);
			}
		},
		ExprKind::Call { func, args, .. } => {
			collect_expr_scope_names(func, offset, scopes, out);
			for arg in args {
				collect_expr_scope_names(&arg.0.value, offset, scopes, out);
			}
		}
		ExprKind::MemberAccess { parent, .. } => collect_expr_scope_names(parent, offset, scopes, out),
		ExprKind::IndexAccess { parent, index, .. } => {
			collect_expr_scope_names(parent, offset, scopes, out);
			collect_expr_scope_names(index, offset, scopes, out);
		}
		ExprKind::Closure { params, body, .. } => {
			let base = scopes.len();
			for p in params {
				pattern_bindings(&p.0.name.0, scopes);
			}
			collect_expr_scope_names(body, offset, scopes, out);
			scopes.truncate(base);
		}
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
			collect_expr_scope_names(value, offset, scopes, out);
		}
		ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
			collect_expr_scope_names(lhs, offset, scopes, out);
			collect_expr_scope_names(rhs, offset, scopes, out);
		}
		ExprKind::TypeOp { lhs, .. } | ExprKind::PatternOp { lhs, .. } => {
			collect_expr_scope_names(lhs, offset, scopes, out);
		}
		ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
			if let Some(v) = value {
				collect_expr_scope_names(v, offset, scopes, out);
			}
		}
		ExprKind::While {
			condition, body, ..
		} => {
			collect_expr_scope_names(condition, offset, scopes, out);
			collect_expr_scope_names(body, offset, scopes, out);
		}
		ExprKind::For {
			variable,
			iterable,
			body,
			..
		} => {
			collect_expr_scope_names(iterable, offset, scopes, out);
			let base = scopes.len();
			pattern_bindings(&variable.0, scopes);
			collect_expr_scope_names(body, offset, scopes, out);
			scopes.truncate(base);
		}
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			collect_expr_scope_names(condition, offset, scopes, out);
			collect_expr_scope_names(then, offset, scopes, out);
			if let Some(o) = otherwise {
				collect_expr_scope_names(o, offset, scopes, out);
			}
		}
		ExprKind::Match { value, arms } => {
			collect_expr_scope_names(value, offset, scopes, out);
			for arm in arms {
				let base = scopes.len();
				pattern_bindings(&arm.pattern.0, scopes);
				if let Some(g) = &arm.guard {
					collect_expr_scope_names(g, offset, scopes, out);
				}
				collect_expr_scope_names(&arm.body, offset, scopes, out);
				scopes.truncate(base);
			}
		}
		ExprKind::Block { body, .. } => {
			// Unlike `walk_expr` (which searches by `NodeId` identity and so
			// always reaches its target however sparse the tree), a byte
			// `offset` can fall in a gap no child `Expr` covers at all — the
			// whitespace at the start of a fresh line between two
			// statements, or trailing whitespace before the closing `}`.
			// Those are exactly the common completion positions, so each
			// statement's own `Spanned` span (not just its inner `Expr`'s)
			// decides whether to keep accumulating bindings and move on, or
			// stop and snapshot.
			let base = scopes.len();
			let mut handled = false;
			for stmt in body {
				if stmt.1.end < offset {
					if let Statement::Let { meta, .. } = &stmt.0 {
						pattern_bindings(&meta.name.0, scopes);
					}
					continue;
				}
				if covers(stmt.1, offset) {
					match &stmt.0 {
						Statement::Expr(e) => collect_expr_scope_names(e, offset, scopes, out),
						Statement::Let { meta, value } => {
							collect_expr_scope_names(value, offset, scopes, out);
							// `offset` is inside this `let` statement but
							// outside its initializer (its own binder name,
							// or trailing space before the next statement) —
							// the name is already bound at that point.
							if !covers(value.span, offset) {
								pattern_bindings(&meta.name.0, scopes);
								out.names.clear();
								out
									.names
									.extend(scopes.iter().map(|(n, _)| (*n).to_string()));
							}
						}
					}
				} else {
					// `offset` sits in the gap before this not-yet-reached
					// statement: everything earlier has already applied.
					out.names.clear();
					out
						.names
						.extend(scopes.iter().map(|(n, _)| (*n).to_string()));
				}
				handled = true;
				break;
			}
			if !handled {
				// `offset` is past every statement (trailing whitespace).
				out.names.clear();
				out
					.names
					.extend(scopes.iter().map(|(n, _)| (*n).to_string()));
			}
			scopes.truncate(base);
		}
		ExprKind::Grouped(inner) => collect_expr_scope_names(inner, offset, scopes, out),
	}
}

#[cfg(test)]
mod definition_and_scope_tests {
	use super::*;

	fn module_of(text: &str) -> Module {
		nymph_syntax::parse_module(text, "test.nym").tree
	}

	/// The byte offset of the `n`-th (0-based) occurrence of `needle` in `text`.
	fn nth_offset(text: &str, needle: &str, n: usize) -> usize {
		let mut start = 0;
		for _ in 0..n {
			let found = text[start..].find(needle).expect("occurrence not found");
			start += found + needle.len();
		}
		start + text[start..].find(needle).expect("occurrence not found")
	}

	#[test]
	fn semantic_span_containment_is_strictly_half_open() {
		let cases = [
			("start", Span::new(4, 8), 4, true),
			("interior", Span::new(4, 8), 7, true),
			("end", Span::new(4, 8), 8, false),
			("before", Span::new(4, 8), 3, false),
			("empty", Span::new(4, 4), 4, false),
			("reversed", Span::new(8, 4), 6, false),
		];

		for (name, span, offset, expected) in cases {
			assert_eq!(covers(span, offset), expected, "case {name}");
		}
	}

	#[test]
	fn public_queries_use_exact_half_open_token_boundaries() {
		let text = "func f(parameter: int): int = parameter";
		let module = module_of(text);
		let checked = crate::check_module(&module);
		let use_start = text.rfind("parameter").unwrap();
		let use_end = use_start + "parameter".len();
		let keyword_start = text.find("func").unwrap();
		let keyword_end = keyword_start + "func".len();

		for offset in [use_start, use_start + 1] {
			assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
			assert!(definition_at(&module, offset).is_some());
			assert_eq!(
				scope_names_at(&module, offset),
				vec!["parameter".to_string()]
			);
		}
		assert_eq!(type_at(&module, &checked, use_end), None);
		assert_eq!(definition_at(&module, use_end), None);
		assert_eq!(scope_names_at_exact(&module, use_end), None);

		for offset in [keyword_start, keyword_start + 1] {
			assert!(keyword_doc_at(text, offset).is_some());
		}
		assert_eq!(keyword_doc_at(text, keyword_end), None);
	}

	#[test]
	fn scope_names_preserves_vec_api_while_exact_query_reports_applicability() {
		let text = "func f(parameter: int): int = parameter";
		let module = module_of(text);
		let use_start = text.rfind("parameter").unwrap();
		let use_end = use_start + "parameter".len();

		assert_eq!(
			scope_names_at(&module, use_start),
			vec!["parameter".to_string()]
		);
		assert_eq!(
			scope_names_at_exact(&module, use_start),
			Some(vec!["parameter".to_string()])
		);
		assert_eq!(scope_names_at(&module, use_end), Vec::<String>::new());
		assert_eq!(scope_names_at_exact(&module, use_end), None);
	}

	#[test]
	fn definition_jumps_a_param_use_to_its_binder() {
		let text = "func add(a: int, b: int): int = a + b";
		let module = module_of(text);
		// Occurrences of the substring "a": 0 = `add`'s own `a`, 1 = the
		// param `a`, 2 = the `a` inside `a + b`.
		let use_offset = nth_offset(text, "a", 2);
		let binder_offset = nth_offset(text, "a", 1);

		let span = definition_at(&module, use_offset).expect("should resolve to the param binder");
		assert!(
			covers(span, binder_offset),
			"expected span to cover the `a` param binder, got {span:?}"
		);
	}

	#[test]
	fn definition_jumps_a_call_to_its_func_decl() {
		let text = "func helper(): int = 1\nfunc main(): int = helper()";
		let module = module_of(text);
		// "helper" occurs twice: the declaration name (0) and the call (1).
		let call_offset = nth_offset(text, "helper", 1) + 1;

		let span = definition_at(&module, call_offset).expect("should resolve to the func decl");
		let decl_name_offset = nth_offset(text, "helper", 0) + 1;
		assert!(
			covers(span, decl_name_offset),
			"expected span to cover `helper`'s declaration name, got {span:?}"
		);
	}

	#[test]
	fn definition_jumps_a_block_let_use_to_its_binder() {
		let text = "func main(): int = {\n  let x = 1\n  x + 2\n}";
		let module = module_of(text);
		let use_offset = text.rfind('x').unwrap();
		let binder_offset = text.find("let x").unwrap() + "let ".len();

		let span = definition_at(&module, use_offset).expect("should resolve to the let binder");
		assert!(
			covers(span, binder_offset),
			"expected span to cover the `let x` binder, got {span:?}"
		);
	}

	#[test]
	fn definition_prefers_the_innermost_shadowing_binder() {
		let text = "func main(): int = {\n  let x = 1\n  {\n    let x = 2\n    x\n  }\n}";
		let module = module_of(text);
		let use_offset = text.rfind('x').unwrap();
		let inner_binder_offset = text.rfind("let x").unwrap() + "let ".len();

		let span = definition_at(&module, use_offset).expect("should resolve to the inner `x`");
		assert!(
			covers(span, inner_binder_offset),
			"expected span to cover the inner `let x`, got {span:?}"
		);
	}

	#[test]
	fn definition_over_a_member_access_returns_none() {
		let text = "struct Point(x: int)\nfunc main(): int = Point(x = 0).x";
		let module = module_of(text);
		// The trailing `.x` member — not resolvable without the checker.
		let offset = text.rfind(".x").unwrap() + 1;

		assert_eq!(
			definition_at(&module, offset),
			None,
			"member access should never resolve to a (possibly wrong) jump"
		);
	}

	// ── BUG 2a: `this.method()` go-to-definition ────────────────────────────

	#[test]
	fn definition_jumps_a_this_method_call_to_its_method_decl() {
		let text =
			"struct Point(x: int) {\n  func get(): int = this.x\n  func run(): int = this.get()\n}";
		let module = module_of(text);
		let use_offset = text.rfind("this.get()").unwrap() + "this.".len() + 1;

		let span = definition_at(&module, use_offset).expect("should resolve to the `get` method decl");
		let decl_name_offset = text.find("func get").unwrap() + "func ".len();
		assert!(
			covers(span, decl_name_offset),
			"expected span to cover `get`'s declaration name, got {span:?}"
		);
	}

	#[test]
	fn definition_over_a_this_field_access_returns_none() {
		// `this.x` names a FIELD, not a method — no `Func` member named `x`
		// exists, so this must stay unresolved rather than guessing.
		let text = "struct Point(x: int) {\n  func get(): int = this.x\n}";
		let module = module_of(text);
		let offset = text.rfind("this.x").unwrap() + "this.".len();

		assert_eq!(definition_at(&module, offset), None);
	}

	#[test]
	fn definition_jumps_a_this_method_call_to_a_top_level_impl_method() {
		let text = "struct Point(x: int)\nimpl Point {\n  func get(): int = this.x\n}\nimpl Point {\n  func run(): int = this.get()\n}";
		let module = module_of(text);
		let use_offset = text.rfind("this.get()").unwrap() + "this.".len() + 1;

		let span = definition_at(&module, use_offset).expect("should resolve to the `get` method decl");
		let decl_name_offset = text.find("func get").unwrap() + "func ".len();
		assert!(
			covers(span, decl_name_offset),
			"expected span to cover `get`'s declaration name, got {span:?}"
		);
	}

	#[test]
	fn definition_jumps_a_type_annotation_to_its_struct_decl() {
		let text = "struct Point(x: int)\nfunc origin(): Point = Point(x = 0)";
		let module = module_of(text);
		// The `Point` in the *return type* annotation, not the call.
		let annotation_offset = text.find("): Point").unwrap() + "): ".len() + 1;

		let span =
			definition_at(&module, annotation_offset).expect("should resolve to the struct decl");
		let decl_name_offset = text.find("Point").unwrap() + 1;
		assert!(
			covers(span, decl_name_offset),
			"expected span to cover `Point`'s declaration name, got {span:?}"
		);
	}

	#[test]
	fn definition_over_an_unresolvable_name_returns_none() {
		let text = "func main(): int = nope";
		let module = module_of(text);
		let offset = text.find("nope").unwrap() + 1;

		assert_eq!(definition_at(&module, offset), None);
	}

	#[test]
	fn scope_names_include_params_and_locals_but_not_a_not_yet_declared_let() {
		let text = "func f(a: int): int = {\n  let b = a\n  b\n}";
		let module = module_of(text);
		let offset = text.rfind('b').unwrap();

		let names = scope_names_at(&module, offset);
		assert!(
			names.contains(&"a".to_string()),
			"expected param `a` in scope, got {names:?}"
		);
		assert!(
			names.contains(&"b".to_string()),
			"expected local `b` in scope, got {names:?}"
		);
	}

	#[test]
	fn scope_names_excludes_top_level_declarations() {
		// Top-level names are NOT this query's job — see its doc comment —
		// so a function body with no locals/params reports an empty scope.
		let text = "func helper(): int = 1\nfunc main(): int = 1";
		let module = module_of(text);
		let offset = text.rfind('1').unwrap();

		let names = scope_names_at(&module, offset);
		assert!(
			names.is_empty(),
			"expected no top-level names from scope_names_at, got {names:?}"
		);
	}
}

#[cfg(test)]
mod type_at_tests {
	use super::*;
	use crate::check_module;

	fn module_of(text: &str) -> Module {
		nymph_syntax::parse_module(text, "t.nym").tree
	}

	// ── BUG 1: containers must not leak the enclosing type ─────────────────

	#[test]
	fn hovering_a_var_use_returns_its_type() {
		let text = "func main(): int = {\n  let x = 1\n  x\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind('x').unwrap();

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	#[test]
	fn hovering_the_let_keyword_returns_none() {
		let text = "func main(): int = {\n  let x = 1\n  x\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("let").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	#[test]
	fn hovering_the_func_keyword_returns_none() {
		let text = "func main(): int = {\n  let x = 1\n  x\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = 1; // inside `func`

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	#[test]
	fn hovering_whitespace_inside_a_block_returns_none() {
		let text = "func main(): int = {\n  let x = 1\n  x\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		// The newline + indent between the `let` statement and `x` — no
		// expression covers it, only the enclosing `Block`.
		let offset = text.find("\n  x\n").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	#[test]
	fn hovering_the_while_keyword_returns_none() {
		let text = "func main(): void = {\n  while true { }\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("while").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	#[test]
	fn hovering_the_for_keyword_returns_none() {
		let text = "func main(): void = {\n  for i in 1..3 { }\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("for").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	#[test]
	fn hovering_a_block_brace_returns_none_not_the_blocks_value_type() {
		// A value-position `Block` IS annotated with its trailing expression's
		// type — the exact leak BUG 1 describes: hovering the opening `{`
		// (covered only by the `Block`, not by the `1` inside it) used to
		// surface that value type instead of nothing.
		let text = "func main(): int = {\n  1\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find('{').unwrap();

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	#[test]
	fn hovering_a_call_paren_returns_none_not_the_calls_return_type() {
		// `Call` is now a suppressed container (F1/F11): the callee and each
		// real argument are smaller child exprs and still resolve, but the
		// parens themselves (covered only by the `Call` span) must not leak
		// the call's return type.
		let text = "func helper(): int = 1\nfunc main(): int = helper()";
		let module = module_of(text);
		let checked = check_module(&module);
		// Land on the `()` — covered by the `Call` but not by the `helper`
		// `Identifier`.
		let offset = text.rfind("()").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	#[test]
	fn hovering_the_calls_callee_still_resolves_its_function_type() {
		// Upgraded: a call-site callee now shows the full NAMED signature
		// (`func helper(): int`), not just the unnamed `() -> int` `Fn` type
		// — see `render_named_signature`.
		let text = "func helper(): int = 1\nfunc main(): int = helper()";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind("helper").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func helper(): int".to_string())
		);
	}

	#[test]
	fn hovering_a_calls_callee_shadowed_by_a_local_still_shows_the_plain_fn_type() {
		// A function-typed LOCAL that happens to share a name with a
		// top-level `func` must not be misreported as that top-level
		// func's own named signature.
		let text =
			"func helper(): int = 1\nfunc main(): int = {\n  let helper = () -> 2\n  helper()\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind("helper").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("() -> int".to_string())
		);
	}

	// ── BUG 2: generic params render with their source name, not `T{idx}` ──

	#[test]
	fn hovering_a_generic_func_param_shows_its_source_name() {
		let text = "func id<V>(v: V): V = v";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind('v').unwrap();

		assert_eq!(type_at(&module, &checked, offset), Some("V".to_string()));
	}

	#[test]
	fn hovering_a_member_methods_own_generic_uses_the_owner_plus_own_index_space() {
		// `Pair`'s own generics are `A, B` (indices 0, 1); `pick`'s own `C` is
		// index 2 — the checker's actual index space for a struct's direct
		// member methods (owner generics first, method's own appended after).
		let text = "struct Pair<A, B>(a: A, b: B) {\n  func pick<C>(x: A, y: C): C = y\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind('y').unwrap();

		assert_eq!(type_at(&module, &checked, offset), Some("C".to_string()));
	}

	#[test]
	fn hovering_a_top_level_impls_owner_generic_through_this_shows_its_source_name() {
		let text = "struct Box<T>(val: T) {}\nimpl<T> Box<T> {\n  func keep<U>(u: U): T = this.val\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind("this.val").unwrap() + 6; // inside `.val`

		assert_eq!(type_at(&module, &checked, offset), Some("T".to_string()));
	}

	#[test]
	fn a_param_out_of_any_generic_scope_falls_back_to_the_internal_index() {
		// A bare top-level `let` has no generic scope at all; a `Param` there
		// (unreachable in practice — lets never declare generics — but this
		// pins the fallback) would render as `T{idx}`.
		assert_eq!(
			generic_scope_at(&module_of("let x = 1"), 0),
			Vec::<EcoString>::new()
		);
	}

	// ── Unchanged leaf cases: member access, index access, literal ─────────

	#[test]
	fn hovering_a_member_access_returns_the_field_type() {
		let text = "struct Point(x: int)\nfunc main(): int = Point(x = 0).x";
		let module = module_of(text);
		let checked = check_module(&module);
		// The trailing `x` after the dot — not itself an `Expr`, so only the
		// (unsuppressed) `MemberAccess` covers it.
		let offset = text.rfind(".x").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	#[test]
	fn hovering_inside_an_index_access_returns_the_element_type() {
		let text = "func main(): int = {\n  let a = #[1, 2, 3]\n  a[ 0 ]\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		// The space right after `[` — covered only by the (unsuppressed)
		// `IndexAccess`, not by `a` or the `0` index expr.
		let offset = text.find("[ 0").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	// ── F1/F11: Call/operator/collection suppression ────────────────────────

	#[test]
	fn hovering_a_labeled_call_args_name_returns_none_not_the_constructed_type() {
		let text = "struct Point(x: int)\nfunc main(): int = Point(x = 0).x";
		let module = module_of(text);
		let checked = check_module(&module);
		// The label `x` inside `Point(x = 0)` — not an `Expr` at all, so only
		// the (now-suppressed) `Call` covers it. Must never report "Point".
		let offset = text.find("(x = 0)").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	#[test]
	fn hovering_a_real_call_argument_value_still_resolves() {
		let text = "struct Point(x: int)\nfunc main(): int = Point(x = 0).x";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find('0').unwrap();

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	#[test]
	fn hovering_a_binary_operator_returns_none_not_the_result_type() {
		let text = "func main(): int = 1 + 2";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find('+').unwrap();

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	#[test]
	fn hovering_a_binary_operators_operand_still_resolves() {
		let text = "func main(): int = 1 + 2";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find('1').unwrap();

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	#[test]
	fn hovering_a_list_literals_bracket_returns_none_not_the_list_type() {
		let text = "func main(): int = {\n  let a = #[1, 2, 3]\n  a[0]\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("#[1").unwrap();

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	// ── F3: an Error-typed node must never render "<error>" ─────────────────

	#[test]
	fn hovering_an_unresolvable_identifier_returns_none_not_error() {
		let text = "func main(): int = nope";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("nope").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	// ── F4: a `let` binder hovers as its initializer's type ─────────────────

	#[test]
	fn hovering_a_let_binder_name_returns_its_initializers_type() {
		let text = "func main(): int = {\n  let x = 1\n  x\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("let x").unwrap() + 4; // the binder's own `x`

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	// ── F5: a param name hovers as its declared type ────────────────────────

	#[test]
	fn hovering_a_param_name_returns_its_declared_type() {
		let text = "func f(a: int): int = a";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("(a:").unwrap() + 1; // the param's own `a`

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	// ── F7: a type-position name renders the type it names ──────────────────

	#[test]
	fn hovering_a_primitive_return_type_annotation_renders_it() {
		let text = "func f(): int = 1";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find(": int").unwrap() + 2;

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	#[test]
	fn hovering_a_struct_field_type_annotation_renders_it() {
		let text = "struct Point(x: int)\nfunc main(): int = 1";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find(": int").unwrap() + 2;

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	#[test]
	fn hovering_a_struct_typed_return_annotation_renders_the_struct_name() {
		let text = "struct Point(x: int)\nfunc origin(): Point = Point(x = 0)";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("): Point").unwrap() + 3;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Point".to_string())
		);
	}

	// ── F6: declaration-site names hover as a sensible type/signature ───────
	// (upgraded to a full RICH structural rendering — see the module's
	// decl-renderer helpers beside `render_type_node`.)

	#[test]
	fn hovering_a_struct_decl_name_renders_its_full_structure() {
		let text = "struct Point(x: int, y: int)\nfunc origin(): Point = Point(x = 0, y = 0)";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Point").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("struct Point(x: int, y: int)".to_string())
		);
	}

	#[test]
	fn hovering_a_struct_field_decl_name_renders_the_field_name_and_type() {
		let text = "struct Point(x: int)\nfunc main(): int = 1";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("(x: int)").unwrap() + 1; // the field's own `x`

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("x: int".to_string())
		);
	}

	#[test]
	fn hovering_a_func_decl_name_renders_its_full_named_signature() {
		let text = "func add(a: int, b: int): int = a + b";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("add").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func add(a: int, b: int): int".to_string())
		);
	}

	#[test]
	fn hovering_a_mut_func_decl_name_includes_the_mut_keyword() {
		let text = "struct Counter(count: int) {\n  mut func bump(): void = {}\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("bump").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("mut func bump(): void".to_string())
		);
	}

	#[test]
	fn hovering_an_enum_decl_name_renders_every_variant_and_its_fields() {
		let text =
			"enum Shape { Circle(radius: int), Square }\nfunc origin(): Shape = Circle(radius = 1)";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Shape").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("enum Shape { Circle(radius: int), Square }".to_string())
		);
	}

	#[test]
	fn hovering_an_enum_variant_decl_name_renders_its_own_shape_qualified_by_its_enum() {
		let text = "enum Shape { Circle(radius: int) }\nfunc origin(): Shape = Circle(radius = 1)";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Circle").unwrap() + 1; // the variant's own decl name

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Shape.Circle(radius: int)".to_string())
		);
	}

	#[test]
	fn hovering_a_fieldless_enum_variant_decl_name_renders_bare() {
		let text = "enum Shape { Circle(radius: int), Square }\nfunc main(): int = 1";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Square").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Shape.Square".to_string())
		);
	}

	#[test]
	fn hovering_an_interface_decl_name_renders_its_member_signatures() {
		let text = "interface Greet {\n  func hello(): string\n}\nfunc main(): int = 1";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Greet").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("interface Greet { func hello(): string }".to_string())
		);
	}

	#[test]
	fn hovering_an_empty_interface_decl_name_renders_empty_braces() {
		let text = "interface Greet {}\nfunc main(): int = 1";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Greet").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("interface Greet {}".to_string())
		);
	}

	#[test]
	fn hovering_a_generic_param_decl_shows_its_source_bound() {
		let text = "interface Area {}\nfunc measure<T: Area>(t: T): int = 1";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("T: Area").unwrap(); // the param's own `T`

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("T: Area".to_string())
		);
	}

	#[test]
	fn hovering_an_unbounded_generic_param_decl_shows_just_its_name() {
		let text = "func id<V>(v: V): V = v";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("<V>").unwrap() + 1; // the param's own declaration `V`

		assert_eq!(type_at(&module, &checked, offset), Some("V".to_string()));
	}

	// ── Container keywords stay `None` even with the fallback in place ──────

	#[test]
	fn hovering_the_for_keyword_still_returns_none_with_fallback_enabled() {
		// The `for` keyword and its loop binder are covered only by the
		// (suppressed) `For` expr — neither a let-binder, a param, a decl
		// name, nor a type-position node — so the fallback must not pick up
		// a spurious match here.
		let text = "func main(): void = {\n  for i in 1..3 { }\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("for").unwrap() + 1;

		assert_eq!(type_at(&module, &checked, offset), None);
	}

	// ── F7: a bare enum-variant constructor use-site resolves like a struct's ──

	#[test]
	fn hovering_an_enum_variant_ctor_use_site_renders_the_variant_not_the_enum() {
		// BUG 1: a bare enum-variant name in call position (`Circle` in
		// `Circle(radius = 1)`) now shows the VARIANT's own declaration —
		// `variant_hover_at` intercepts before the primary path would render
		// the callee identifier's plain-enum type (`Shape`).
		let text = "enum Shape { Circle(radius: int) }\nfunc origin(): Shape = Circle(radius = 1)";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind("Circle").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Shape.Circle(radius: int)".to_string())
		);
	}

	#[test]
	fn hovering_a_struct_ctor_use_site_renders_the_struct_name() {
		let text = "struct Point(x: int)\nfunc origin(): Point = Point(x = 0)";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind("Point").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Point".to_string())
		);
	}

	// ── F1 (critical): a destructuring binder hovers as its OWN element ────
	// ── type, not the whole pattern's type ──────────────────────────────────

	#[test]
	fn hovering_a_destructured_let_tuple_binder_renders_its_own_element_type() {
		let text = "func main(): int = {\n  let #(a, b) = #(1, \"hi\")\n  a\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let a_offset = text.find("#(a, b)").unwrap() + 2;
		let b_offset = text.find("#(a, b)").unwrap() + 5;

		assert_eq!(
			type_at(&module, &checked, a_offset),
			Some("int".to_string())
		);
		assert_eq!(
			type_at(&module, &checked, b_offset),
			Some("string".to_string())
		);
	}

	#[test]
	fn hovering_a_destructured_func_param_tuple_binder_renders_its_own_element_type() {
		let text = "func f(#(a, b): #(int, string)): int = a";
		let module = module_of(text);
		let checked = check_module(&module);
		let a_offset = text.find("(a, b)").unwrap() + 1;
		let b_offset = text.find("(a, b)").unwrap() + 4;

		assert_eq!(
			type_at(&module, &checked, a_offset),
			Some("int".to_string())
		);
		assert_eq!(
			type_at(&module, &checked, b_offset),
			Some("string".to_string())
		);
	}

	#[test]
	fn hovering_a_destructured_let_list_binder_renders_its_element_type() {
		let text = "func main(): int = {\n  let #[a, b] = #[1, 2]\n  a\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let a_offset = text.find("#[a, b]").unwrap() + 2;
		let b_offset = text.find("#[a, b]").unwrap() + 5;

		assert_eq!(
			type_at(&module, &checked, a_offset),
			Some("int".to_string())
		);
		assert_eq!(
			type_at(&module, &checked, b_offset),
			Some("int".to_string())
		);
	}

	#[test]
	fn hovering_a_destructured_struct_pattern_field_renders_that_fields_own_type() {
		let text = "struct Point(x: int, y: string)\nfunc main(): int = {\n  let Point(x, y) = Point(x = 0, y = \"hi\")\n  x\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let x_offset = text.find("Point(x, y)").unwrap() + 6;
		let y_offset = text.find("Point(x, y)").unwrap() + 9;

		assert_eq!(
			type_at(&module, &checked, x_offset),
			Some("int".to_string())
		);
		assert_eq!(
			type_at(&module, &checked, y_offset),
			Some("string".to_string())
		);
	}

	// ── Match-arm pattern hovers: variant name + field/element binder type ──

	#[test]
	fn hovering_a_match_arm_field_bearing_variant_name_shows_its_declaration() {
		let text = "enum Shape { Circle(radius: int) }\nfunc f(s: Shape): int = match (s) { Circle(radius) -> radius }";
		let module = module_of(text);
		let checked = check_module(&module);
		// The arm's own `Circle`, not the enum declaration's.
		let offset = text.find("Circle(radius) ->").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Shape.Circle(radius: int)".to_string())
		);
	}

	#[test]
	fn hovering_a_match_arm_field_binder_shows_the_fields_declared_type() {
		let text = "enum Shape { Circle(radius: int) }\nfunc f(s: Shape): int = match (s) { Circle(radius) -> radius }";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Circle(radius) ->").unwrap() + "Circle(".len() + 1;

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	#[test]
	fn hovering_a_match_arm_nullary_variant_name_shows_its_declaration_not_the_enum() {
		let text = "enum Shape { Circle(radius: int), Square }\nfunc f(s: Shape): int = match (s) { Square -> 0, Circle(radius) -> radius }";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Square ->").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Shape.Square".to_string())
		);
	}

	#[test]
	fn hovering_a_generic_variant_field_binder_shows_the_substituted_concrete_type() {
		let text = "enum Option<T> { Some(v: T), None }\nfunc f(o: Option<int>): int = match (o) { Some(v) -> v, None -> 0 }";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Some(v) ->").unwrap() + "Some(".len();

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	#[test]
	fn hovering_a_renamed_generic_variant_field_binder_shows_the_substituted_concrete_type() {
		// Same as the shorthand-field case above, but through the `field =
		// pattern` renaming syntax (`Some(v = r)`) — the renamed binder `r`
		// must show the substituted concrete type, not the still-generic
		// declared field type.
		let text = "enum Option<T> { Some(v: T), None }\nfunc f(o: Option<int>): int = match (o) { Some(v = r) -> r, None -> 0 }";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Some(v = r) ->").unwrap() + "Some(v = ".len();

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	#[test]
	fn hovering_a_generic_variant_name_in_a_match_arm_shows_its_declared_shape() {
		// The variant-decl hover shows the enum's own written declaration
		// (still generic in `T`) — only the field/element BINDER hover
		// (previous test) substitutes the scrutinee's concrete generic args.
		let text = "enum Option<T> { Some(v: T), None }\nfunc f(o: Option<int>): int = match (o) { Some(v) -> v, None -> 0 }";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("Some(v) ->").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Option.Some(v: T)".to_string())
		);
	}

	#[test]
	fn hovering_a_match_arm_struct_pattern_field_binder_shows_its_declared_type() {
		let text =
			"struct Point(x: int, y: string)\nfunc f(p: Point): int = match (p) { Point(x, y) -> x }";
		let module = module_of(text);
		let checked = check_module(&module);
		let x_offset = text.find("Point(x, y) ->").unwrap() + "Point(".len();
		let y_offset = text.find("Point(x, y) ->").unwrap() + "Point(x, ".len();

		assert_eq!(
			type_at(&module, &checked, x_offset),
			Some("int".to_string())
		);
		assert_eq!(
			type_at(&module, &checked, y_offset),
			Some("string".to_string())
		);
	}

	// ── For-loop pattern hovers: element type ────────────────────────────────

	#[test]
	fn hovering_a_for_loop_binder_over_a_list_shows_the_element_type() {
		let text = "func main(): int = {\n  let xs = #[1, 2, 3]\n  for x in xs { x }\n  0\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("for x in").unwrap() + "for ".len();

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	// ── Inferred return type in hover signatures ────────────────────────────

	#[test]
	fn hovering_an_unannotated_funcs_name_shows_its_inferred_return_type() {
		// No `: T` annotation on `f` — the return type must still show as the
		// body's inferred `boolean`, not the old `void` default.
		let text = "func f() = true";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("func f").unwrap() + "func ".len();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func f(): boolean".to_string())
		);
	}

	#[test]
	fn hovering_an_unannotated_match_bodied_methods_name_shows_its_inferred_return_type() {
		// Mirrors the real-world bug report: `Result.is_ok_and` hovers as
		// `: void` despite its `match` body always yielding `boolean`.
		let text = "enum Result<T, E> { Ok(value: T), Error(err: E) }\nimpl<T, E> Result<T, E> { func is_ok_and(f: (T) -> boolean) = match (this) { Ok(value) -> f(value), Error(err) -> false } }";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("is_ok_and").unwrap();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func is_ok_and(f: (T) -> boolean): boolean".to_string())
		);
	}

	#[test]
	fn hovering_an_unannotated_generic_methods_name_shows_the_inferred_return_with_source_generic_names()
	 {
		// The inferred return (`Result<R, E>`) must render with the method's
		// own generic scope (source names `R`/`E`), not the internal `T{idx}`
		// fallback an empty scope would produce.
		let text = "enum Result<T, E> { Ok(value: T), Error(err: E) }\nimpl<T, E> Result<T, E> { func map<R>(f: (T) -> R) = match (this) { Ok(value) -> Ok(f(value)), Error(err) -> Error(err) } }";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("map<R>").unwrap();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func map<R>(f: (T) -> R): Result<R, E>".to_string())
		);
	}

	#[test]
	fn hovering_an_annotated_funcs_name_still_shows_its_annotation() {
		let text = "func g(): int = 1";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("func g").unwrap() + "func ".len();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func g(): int".to_string())
		);
	}

	#[test]
	fn hovering_a_truly_void_bodied_funcs_name_still_shows_void() {
		let text = "func void_expr() = while (true) {}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("void_expr").unwrap();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func void_expr(): void".to_string())
		);
	}

	#[test]
	fn hovering_the_calls_callee_shows_the_inferred_return_type_too() {
		// The call-site signature upgrade (query.rs:~95-106) must also use the
		// inferred return, not just the declaration-name hover path.
		let text = "func c() = true\nfunc main() = c()";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind('c').unwrap();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func c(): boolean".to_string())
		);
	}

	#[test]
	fn hovering_an_unannotated_top_level_funcs_name_with_a_match_body_shows_its_inferred_return_type()
	{
		// `check_dispatch`'s `Match` arm (`infer_expr.rs`) never records the
		// match expression's OWN node id (only each arm's body) — unlike an
		// impl method, a plain top-level func gets no `generalize_returns`
		// trial pass to fall back on, so `inferred_return`'s direct
		// `checked.annotations.get(body.id)` lookup used to miss entirely and
		// silently regress to `void`.
		let text = "enum Result<T, E> { Ok(value: T), Error(err: E) }\nfunc classify(r: Result<int, string>) = match (r) { Ok(v) -> true, Error(e) -> false }";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("func classify").unwrap() + "func ".len();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func classify(r: Result<int, string>): boolean".to_string())
		);
	}

	#[test]
	fn hovering_an_unannotated_top_level_funcs_name_with_a_block_body_shows_its_inferred_return_type()
	{
		// Same gap as the match-body case above, for `check_dispatch`'s
		// `Block` arm.
		let text = "func classify2() { true }";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("func classify2").unwrap() + "func ".len();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func classify2(): boolean".to_string())
		);
	}

	#[test]
	fn hovering_an_unannotated_funcs_name_with_a_nested_unresolved_infer_falls_back_to_void_not_the_internal_placeholder()
	 {
		// `#[]`'s element type is an unpinned, still-unresolved inference var
		// nested inside the recorded `List` type — the return type's own
		// top-level `TyKind` is `List`, not `Infer`, so a shallow guard
		// misses it and `render`'s `TyKind::Infer(_) => "_"` arm leaks the
		// internal placeholder into the hover text. Must fall back to the
		// same `void` every other unrenderable inferred return uses instead.
		let text = "func default_list() = #[]";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("func default_list").unwrap() + "func ".len();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("func default_list(): void".to_string())
		);
	}

	// ── BUG 1: variant-construction/reference hover shows the VARIANT ───────

	#[test]
	fn hovering_a_labeled_variant_construction_shows_the_variant_not_the_enum() {
		let text = "enum Result<T, E> { Ok(value: T), Error(err: E) }\nfunc f(): Result<int, string> = Ok(value = 1)";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind("Ok(value = 1)").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Result.Ok(value: T)".to_string())
		);
	}

	#[test]
	fn hovering_a_qualified_variant_construction_shows_the_variant_not_the_enum() {
		let text = "enum Result<T, E> { Ok(value: T), Error(err: E) }\nfunc f(): Result<int, string> = Result.Error(err = \"boom\")";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind("Result.Error(err").unwrap() + "Result.".len() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Result.Error(err: E)".to_string())
		);
	}

	#[test]
	fn hovering_a_bare_nullary_variant_reference_shows_the_variant_not_the_enum() {
		let text = "enum Opt<T> { Something(v: T), Nothing }\nfunc f(): Opt<int> = Nothing";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind("Nothing").unwrap() + 1;

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("Opt.Nothing".to_string())
		);
	}

	#[test]
	fn hovering_a_variant_construction_argument_still_shows_the_argument_type() {
		// The variant-hover intercept must be scoped to just the callee's own
		// name span — an argument under the cursor still falls through to
		// the normal per-expression render.
		let text = "enum Result<T, E> { Ok(value: T), Error(err: E) }\nfunc f(): Result<int, string> = Ok(value = 1)";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.rfind('1').unwrap();

		assert_eq!(type_at(&module, &checked, offset), Some("int".to_string()));
	}

	// ── BUG 3: `namespace func` hover keeps the `namespace` keyword ─────────

	#[test]
	fn hovering_a_namespace_funcs_name_shows_the_namespace_keyword() {
		let text = "struct Counter(n: int) {\n  namespace func make(): int = 1\n}";
		let module = module_of(text);
		let checked = check_module(&module);
		let offset = text.find("func make").unwrap() + "func ".len();

		assert_eq!(
			type_at(&module, &checked, offset),
			Some("namespace func make(): int".to_string())
		);
	}
}

#[cfg(test)]
mod member_completion_tests {
	use std::sync::Arc;

	use super::*;
	use crate::{MemberCompletionKind as Kind, ModuleAnnotations, SemanticAnalysis, check_module};

	fn analyze(source: &str) -> (SemanticAnalysis, Vec<nymph_diagnostics::Diagnostic>) {
		let parsed = nymph_syntax::parse_module(source, "member_completion.nym");
		let checked = check_module(&parsed.tree);
		let diagnostics = checked.diags.clone();
		let facts = Arc::new(checked.facts);
		(
			SemanticAnalysis {
				module: Arc::new(parsed.tree),
				annotations: Arc::new(ModuleAnnotations::from(facts.annotations.clone())),
				checked: facts,
				declarations: Arc::default(),
				import_references: Arc::default(),
			},
			diagnostics,
		)
	}

	fn at(source: &str, occurrence: &str) -> Vec<(String, Kind, String)> {
		let (analysis, _) = analyze(source);
		let start = source.rfind(occurrence).expect("completion occurrence");
		let dot = start + occurrence.find('.').expect("member dot");
		member_completions_at(&analysis, dot)
			.into_iter()
			.map(|item| (item.name.to_string(), item.kind, item.detail))
			.collect()
	}

	#[test]
	fn generic_struct_fields_are_substituted_and_aliases_are_static_values() {
		let source = "struct Box<T>(value: T) { namespace func make(): int = 1 }\ntype Alias = Box<string>\nfunc read(box: Box<string>): string = box.value\nfunc static_read(): int = Alias.make";
		assert_eq!(
			at(source, "box.value")[0],
			("value".into(), Kind::Field, "string".into())
		);
		assert_eq!(
			at(source, "Alias.make"),
			vec![("make".into(), Kind::Function, "() -> int".into())]
		);
	}

	#[test]
	fn inherent_concrete_interface_and_inherited_default_methods_are_ordered() {
		let source = "interface Child { func base(): int = 1\nfunc child(): string }\nstruct Item { func own(): boolean = true }\nimpl Child for Item { func child(): string = \"\" }\nfunc read(item: Item): int = item.own";
		let items = at(source, "item.own");
		assert_eq!(
			items.iter().map(|i| i.0.as_str()).collect::<Vec<_>>(),
			vec!["base", "child", "own"]
		);
		assert!(items.iter().all(|i| i.1 == Kind::Method));
	}

	#[test]
	fn constrained_generic_parameter_excludes_unbounded_and_non_applicable_methods() {
		let source = "interface Named { func name(): string }\ninterface Other { func other(): int }\nfunc bounded<T: Named>(value: T): string = value.name\nfunc plain<U>(value: U): U = value.nope";
		assert_eq!(
			at(source, "value.name"),
			vec![("name".into(), Kind::Method, "() -> string".into())]
		);
		assert!(at(source, "value.nope").is_empty());
	}

	#[test]
	fn mutating_methods_obey_place_capability_without_same_name_suppression() {
		let source = "struct Cell { mut func change(): int = 1 }\nstruct Other { func change(): string = \"\" }\nfunc mutable(value: mut Cell): int = value.change\nfunc immutable(value: Cell): int = value.change\nfunc owned(): int = Cell().change";
		assert!(at(source, "value.change").is_empty());
		assert_eq!(at(source, "Cell().change")[0].0, "change");
		let mutable_source = source.replacen("value.change", "value.change", 1);
		let (analysis, _) = analyze(&mutable_source);
		let first = mutable_source.find("value.change").unwrap() + "value".len();
		assert_eq!(member_completions_at(&analysis, first)[0].name, "change");

		let owned =
			"struct Cell { mut func change(): int = 1 }\nfunc take(): () -> int = Cell().change";
		let (_, diagnostics) = analyze(owned);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
	}

	#[test]
	fn receiver_and_method_generics_render_callable_detail() {
		let source = "struct Box<T> { func map<U>(f: (T) -> U): U = f(this.value) }\nfunc use_it(box: Box<int>): int = box.map";
		assert_eq!(
			at(source, "box.map"),
			vec![("map".into(), Kind::Method, "((int) -> _) -> _".into())]
		);
	}

	#[test]
	fn static_namespace_candidates_exclude_instance_members() {
		let source = "enum Choice { Pick(value: int) namespace func make(): Choice = Pick(value = 1) func instance(): int = 1 }\nfunc use_it(): Choice = Choice.make";
		let items = at(source, "Choice.make");
		assert_eq!(
			items.iter().map(|i| (&i.0, i.1)).collect::<Vec<_>>(),
			vec![
				(&"Pick".into(), Kind::Variant),
				(&"make".into(), Kind::Function)
			]
		);
		assert!(!items.iter().any(|i| i.0 == "instance"));
	}

	#[test]
	fn field_and_variant_dedupe_precedence_is_deterministic() {
		let source = "struct Pair(value: int) { func value(): string = \"\" }\nenum E { Same namespace func Same(): int = 1 }\nfunc read(pair: Pair): int = pair.value\nfunc static_read(): int = E.Same";
		assert_eq!(
			at(source, "pair.value"),
			vec![("value".into(), Kind::Field, "int".into())]
		);
		assert_eq!(at(source, "E.Same")[0].1, Kind::Variant);
	}

	#[test]
	fn malformed_receivers_are_empty_and_member_interval_is_strictly_half_open() {
		let source = "struct Point(x: int)\nfunc read(point: Point): int = point.x";
		let (analysis, _) = analyze(source);
		let dot = source.rfind('.').unwrap();
		let end = source.len();
		assert!(!member_completions_at(&analysis, dot).is_empty());
		assert!(member_completions_at(&analysis, end).is_empty());
		assert!(at("func broken(): int = unknown.member", "unknown.member").is_empty());
	}

	#[test]
	fn repeated_checking_and_queries_preserve_diagnostics_and_results() {
		let source = "interface Marker { func mark(): int }\nimpl Marker for int { func mark(): int = 1 }\nenum Choice<T: Marker> { Pick(value: T) namespace func make(value: T): Choice<T> = Pick(value = value) }\ntype IntChoice = Choice<int>\nfunc read(): IntChoice = IntChoice.Pick(value = 1)";
		let (first, first_diagnostics) = analyze(source);
		let dot = source.rfind("IntChoice.").unwrap() + "IntChoice".len();
		let expected = member_completions_at(&first, dot);
		assert_eq!(member_completions_at(&first, dot), expected);
		let (second, second_diagnostics) = analyze(source);
		assert_eq!(first_diagnostics, second_diagnostics);
		assert_eq!(member_completions_at(&second, dot), expected);
	}

	#[test]
	fn qualified_calls_record_completion_facts_without_reinferring_the_callee() {
		let source = "namespace Tools { func run<T>(value: T): T = value }\nstruct Point { func get(): int = 1 }\nstruct Vault { namespace func make(): Vault = Vault() }\nenum Choice { Pick }\ninterface Default { func default(): self }\nfunc use<R: Default>(point: Point): int = { point.get()\nVault.make()\nChoice.Pick()\nTools.run(1)\nR.default()\npoint.get() }";
		for (occurrence, expected) in [
			("point.get()", "get"),
			("Vault.make()", "make"),
			("Choice.Pick()", "Pick"),
			("Tools.run(1)", "run"),
			("R.default()", "default"),
		] {
			assert!(
				at(source, occurrence).iter().any(|item| item.0 == expected),
				"missing {expected} at {occurrence}"
			);
		}
	}

	#[test]
	fn alias_statics_and_generic_static_details_use_the_concrete_target() {
		let source = "enum Choice<T> { Pick(value: T) namespace func make(value: T): Choice<T> = Pick(value = value) }\ntype Strings = Choice<string>\nnamespace Tools { func same<T>(value: T): T = value }\nfunc use(): Strings = { let picked: Strings = Strings.Pick(value = \"x\")\nlet made: Strings = Strings.make(\"x\")\nlet text: string = Tools.same(\"x\")\npicked }";
		let items = at(source, "Strings.make(\"x\")");
		assert!(items.contains(&(
			"Pick".into(),
			Kind::Variant,
			"(string) -> Choice<string>".into()
		)));
		assert!(items.contains(&(
			"make".into(),
			Kind::Function,
			"(string) -> Choice<string>".into()
		)));
		assert_eq!(
			at(source, "Tools.same(\"x\")"),
			vec![("same".into(), Kind::Function, "(_) -> _".into())]
		);
		let (_, diagnostics) = analyze(source);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
	}

	#[test]
	fn alias_static_calls_ignore_same_named_methods_for_other_instantiations() {
		let source = "struct Box<T>(value: T)\nimpl Box<int> { namespace func make(value: int): Box<int> = Box(value = value) }\nimpl Box<string> { namespace func make(value: string): Box<string> = Box(value = value) }\ntype Strings = Box<string>\nfunc use(): Strings = Strings.make(\"ok\")";
		assert_eq!(
			at(source, "Strings.make(\"ok\")"),
			vec![(
				"make".into(),
				Kind::Function,
				"(string) -> Box<string>".into()
			)]
		);
		let (_, diagnostics) = analyze(source);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
	}

	#[test]
	fn receiver_resolved_later_still_has_member_candidates() {
		let source = "external(make) func make<T>(): T\nstruct Point(x: int)\nfunc use(): int = { let value = make()\nvalue.x\nlet point: Point = value\npoint.x }";
		assert_eq!(
			at(source, "value.x"),
			vec![("x".into(), Kind::Field, "int".into())]
		);
		let (_, diagnostics) = analyze(source);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");

		let bounded = "external(make) func make<T>(): T\ninterface Named { func name(): string }\nfunc use<T: Named>(): string = { let value = make()\nvalue.name\nlet typed: T = value\ntyped.name() }";
		assert_eq!(
			at(bounded, "value.name"),
			vec![("name".into(), Kind::Method, "() -> string".into())]
		);
		let (_, diagnostics) = analyze(bounded);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
	}
}

#[cfg(test)]
mod imported_name_tests {
	use super::*;
	use crate::{DefinitionId, ModuleIdentity, ModuleOrigin};

	fn definition(category: DeclarationCategory, name: &str) -> DefinitionId {
		DefinitionId::new(
			ModuleIdentity {
				origin: ModuleOrigin::Project("project".into()),
				project: "project".into(),
				path: "dependency".into(),
			},
			DeclarationKey::top_level(category, name),
		)
	}

	#[test]
	fn imported_names_preserve_aliases_kinds_and_omit_poison() {
		let mut bindings = FxHashMap::default();
		bindings.insert(
			"renamed".into(),
			ResolvedImportBinding::Definition(definition(DeclarationCategory::Function, "original")),
		);
		bindings.insert(
			"Shape".into(),
			ResolvedImportBinding::Definition(definition(DeclarationCategory::Struct, "Shape")),
		);
		bindings.insert(
			"dependency".into(),
			ResolvedImportBinding::Namespace(ModuleIdentity {
				origin: ModuleOrigin::Project("project".into()),
				project: "project".into(),
				path: "dependency".into(),
			}),
		);
		bindings.insert("private_or_missing".into(), ResolvedImportBinding::Poison);
		let module = nymph_syntax::parse_module("", "test.nym").tree;

		assert_eq!(
			imported_names(&bindings, &[], &module),
			vec![
				ImportedName {
					name: "Shape".into(),
					kind: ImportedNameKind::Struct,
				},
				ImportedName {
					name: "dependency".into(),
					kind: ImportedNameKind::Namespace,
				},
				ImportedName {
					name: "renamed".into(),
					kind: ImportedNameKind::Function,
				},
			]
		);
	}
}
