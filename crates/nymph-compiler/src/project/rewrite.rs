//! AST desugaring for import binding (IB1, phase 2).
//!
//! Every module in the project graph gets ONE stable per-project tag (its
//! index in the dependency-first topological order) and every one of its own
//! top-level declarations is renamed to `$m{tag}$<name>` — globally unique
//! across the whole project, so flattening any number of modules together
//! for [`nymph_sema::check_module_with_prelude`] can never collide, exactly
//! mirroring how the stdlib operator prelude already avoids colliding with a
//! user module (see `nymph-sema/src/prelude.rs`).
//!
//! An import's namespace access (`math.sin(x)`) and `with`-list names
//! (`sin` from `with (sin)`) are both resolved here, syntactically, as a
//! rewrite of the IMPORTING module's own AST — not as a checker-side
//! resolution — because free-standing namespace/import binding has no home
//! in the (fenced) type checker. `math.sin` and (aliased-or-not) `sin`
//! REPLACE the reference with a bare `Identifier` naming `sin`'s mangled
//! top-level name in the target module (`$m{target_tag}$sin`) — which, once
//! the target module's OWN processed form is flattened alongside this one as
//! a "prelude" slice entry, is exactly the name its own top-level `func sin`
//! was renamed to. No decl injection, no per-consumer copy of the target
//! module: one canonical renaming per module, reused by every importer.
//!
//! Known, deliberate limitations (documented, not silently wrong): a local
//! variable/parameter that happens to share a name with an import's namespace
//! or a `with`-bound name is not detected as shadowing — the rewrite doesn't
//! track lexical scope, so it always rewrites a matching bare identifier.
//!
//! `Pattern::Struct::path` (a struct/enum-variant pattern, e.g.
//! `Point(x, y)`, `Color.Red`, `math.Point(x, y)`) IS rewritten by
//! [`rewrite_pattern`], mirroring `rewrite_expr`'s identifier/member-access
//! handling: a one-segment path is looked up in `ctx.renames` exactly like a
//! value identifier (covers both this module's own struct/enum names and a
//! `with`-bound imported name); a two-segment path is either a namespace
//! member access (`math.Point`, resolved via `ctx.namespaces` down to the
//! target's single mangled name) or a same-module qualified enum-variant path
//! (`Color.Red`), in which case only the type-name segment is rewritten — a
//! variant's own name is never mangled (see `rewrite_enum_variant`), so
//! `resolve_pattern_path` in `nymph-sema` still looks it up by its original
//! name. A namespace-qualified *variant* path (`math.Red`) is out of scope
//! (packages/cross-module variant access is IB2 territory) and is left
//! unrewritten, same as before.

use std::cell::RefCell;

use ecow::EcoString;
use nymph_ast::{
	Ident, Spanned,
	decl::{
		Declaration, EnumVariant, FuncDeclaration, FuncParam, ImplMember, InterfaceElement,
		InterfaceMember, LetDeclaration, Module, StructField, StructImpl, Visibility,
	},
	expr::{
		CallArg, ClosureParam, Expr, ExprKind, ListItem, ListPatternEntry, MapEntry, MapPatternEntry,
		MatchArm, Pattern, RangeKind, RangePatternKind, Statement, StringPart, StructPatternField,
	},
	ty::{GenericArg, GenericParam, Type},
};
use nymph_diagnostics::Diagnostic;
use rustc_hash::FxHashMap;

/// One module's declared top-level names, with their (normalized — `None` ⇒
/// `Internal`) visibility, computed from its RAW (pre-rewrite) AST. Consulted
/// whenever some OTHER module imports this one, to validate a `with`-name or
/// a namespace member access.
#[derive(Clone)]
pub(crate) struct DeclaredName {
	pub name: EcoString,
	pub vis: Visibility,
	/// Whether this declaration kind actually lowers to a JS runtime binding
	/// under `nymph_sema::lower_hir` — mirrors that module's per-module walk
	/// EXACTLY: only `Func`/`Let`/`Struct`/`Enum` push into `funcs`/`lets`/
	/// `classes`/`enums` and thus reach `nymph_codegen::emit`. `Namespace`,
	/// `Interface`, `TypeAlias`, `ExternalFunc`, and `ExternalLet` all fall
	/// through that walk's `_ => {}` arm and emit no top-level JS declaration
	/// at all (an `external`/`external(name)` func or let is a declaration-only
	/// intrinsic-linkage marker, never given its own JS body).
	///
	/// `declared_names()` itself still lists every kind — this flag is
	/// consulted ONLY where a declared name must correspond to a real JS
	/// identifier (the bundler's synthesized `import { .. }`/`export { .. };`
	/// lines, see `super::wrap_module_js`), not for `with`-name/namespace
	/// resolution (a `with`-bound interface/type-alias name is still a
	/// legitimate TYPE-position reference, just never a value the bundle
	/// needs to link).
	pub has_runtime_binding: bool,
}

/// Collect every top-level name `module` declares, mirroring
/// `nymph_sema`'s own `build_def_map`/`top_level_names` name derivation
/// (imports and impls introduce no name of their own).
pub(crate) fn declared_names(module: &Module) -> Vec<DeclaredName> {
	let mut out = Vec::new();
	let mut push = |name: &EcoString, vis: Option<Visibility>, has_runtime_binding: bool| {
		out.push(DeclaredName {
			name: name.clone(),
			vis: vis.unwrap_or(Visibility::Internal),
			has_runtime_binding,
		});
	};
	for decl in &module.members {
		match decl {
			Declaration::Func {
				visibility, meta, ..
			} => push(&meta.name.0, *visibility, true),
			Declaration::ExternalFunc(vis, _, meta) => push(&meta.name.0, *vis, false),
			Declaration::Let {
				visibility, meta, ..
			} => {
				if let Pattern::Binding { name, .. } = &meta.name.0 {
					push(&name.0, *visibility, true);
				}
			}
			Declaration::ExternalLet(vis, _, meta) => {
				if let Pattern::Binding { name, .. } = &meta.name.0 {
					push(&name.0, *vis, false);
				}
			}
			Declaration::Struct {
				visibility, name, ..
			}
			| Declaration::Enum {
				visibility, name, ..
			} => push(&name.0, *visibility, true),
			Declaration::Namespace {
				visibility, name, ..
			}
			| Declaration::Interface {
				visibility, name, ..
			} => push(&name.0, *visibility, false),
			Declaration::TypeAlias {
				visibility, meta, ..
			} => push(&meta.name.0, *visibility, false),
			Declaration::Import { .. } | Declaration::Impl { .. } | Declaration::ImplFor { .. } => {}
		}
	}
	out
}

/// Where an import's namespace name (`math`, or its `as` alias) points.
pub(crate) struct NsInfo {
	pub target_key: String,
	pub target_tag: usize,
}

/// Everything [`rewrite_module`] needs for one module: the flat rename map
/// (this module's own top-level names → `$m{tag}$name`, PLUS every bound
/// `with`-name → its target's mangled name) and the namespace table for
/// `ns.member` rewriting. `declared` is every module's declared-name table
/// (by canonical key), used to validate a namespace member access.
pub(crate) struct RewriteCtx<'a> {
	pub renames: FxHashMap<EcoString, EcoString>,
	pub namespaces: FxHashMap<EcoString, NsInfo>,
	pub declared: &'a FxHashMap<String, Vec<DeclaredName>>,
	pub diags: RefCell<Vec<Diagnostic>>,
}

impl RewriteCtx<'_> {
	fn rename(&self, name: &EcoString) -> Option<EcoString> {
		self.renames.get(name).cloned()
	}

	/// Validate `member` against `ns`'s target module's declared surface and
	/// return the mangled name to reference instead, pushing a diagnostic (and
	/// returning `None`) if `member` doesn't exist there or is `private`.
	fn resolve_namespace_member(&self, ns: &NsInfo, member: &Ident) -> Option<EcoString> {
		let declared = self.declared.get(&ns.target_key)?;
		match declared.iter().find(|d| d.name == member.0) {
			None => {
				self.diags.borrow_mut().push(Diagnostic::error(
					"IMPORT-UNRESOLVED-NAME".into(),
					format!("module `{}` has no member `{}`", ns.target_key, member.0),
					member.1,
				));
				None
			}
			Some(d) if d.vis == Visibility::Private => {
				self.diags.borrow_mut().push(Diagnostic::error(
					"IMPORT-PRIVATE-NAME".into(),
					format!(
						"`{}` is private to module `{}` and cannot be imported",
						member.0, ns.target_key
					),
					member.1,
				));
				None
			}
			Some(_) => Some(format!("$m{}${}", ns.target_tag, member.0).into()),
		}
	}
}

fn rewrite_own_name(name: Ident, ctx: &RewriteCtx) -> Ident {
	match ctx.rename(&name.0) {
		Some(m) => Spanned(m, name.1),
		None => name,
	}
}

/// Rewrite `module`'s own (non-`Import`) declarations: every top-level name
/// is renamed via `ctx`, and every value/type reference to a renamed name
/// (this module's own siblings, or an import's namespace/`with` binding) is
/// rewritten throughout every body/signature.
pub(crate) fn rewrite_module(module: &Module, ctx: &RewriteCtx) -> Module {
	Module {
		members: module
			.members
			.iter()
			.filter(|d| !matches!(d, Declaration::Import { .. }))
			.cloned()
			.map(|d| rewrite_declaration(d, ctx))
			.collect(),
		path: module.path.clone(),
	}
}

fn rewrite_declaration(decl: Declaration, ctx: &RewriteCtx) -> Declaration {
	match decl {
		Declaration::Import { .. } => unreachable!("filtered out by rewrite_module"),
		Declaration::Let {
			visibility,
			meta,
			value,
		} => Declaration::Let {
			visibility,
			meta: rewrite_let_decl_toplevel(meta, ctx),
			value: rewrite_expr(value, ctx),
		},
		Declaration::ExternalLet(vis, s, meta) => {
			Declaration::ExternalLet(vis, s, rewrite_let_decl_toplevel(meta, ctx))
		}
		Declaration::Func {
			visibility,
			meta,
			body,
		} => {
			let mut meta = rewrite_func_decl_body(meta, ctx);
			meta.name = rewrite_own_name(meta.name, ctx);
			Declaration::Func {
				visibility,
				meta,
				body: rewrite_expr(body, ctx),
			}
		}
		Declaration::ExternalFunc(vis, s, meta) => {
			let mut meta = rewrite_func_decl_body(meta, ctx);
			meta.name = rewrite_own_name(meta.name, ctx);
			Declaration::ExternalFunc(vis, s, meta)
		}
		Declaration::TypeAlias {
			visibility,
			meta,
			value,
		} => Declaration::TypeAlias {
			visibility,
			meta: rewrite_type_alias_decl(meta, ctx),
			value: rewrite_spanned_type(value, ctx),
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
			name: rewrite_own_name(name, ctx),
			generics: generics
				.into_iter()
				.map(|g| rewrite_generic_param(g, ctx))
				.collect(),
			fields: fields
				.into_iter()
				.map(|f| rewrite_struct_field(f, ctx))
				.collect(),
			members: members
				.into_iter()
				.map(|m| rewrite_impl_member(m, ctx))
				.collect(),
			impls: impls
				.into_iter()
				.map(|i| rewrite_struct_impl(i, ctx))
				.collect(),
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
			name: rewrite_own_name(name, ctx),
			generics: generics
				.into_iter()
				.map(|g| rewrite_generic_param(g, ctx))
				.collect(),
			variants: variants
				.into_iter()
				.map(|v| rewrite_enum_variant(v, ctx))
				.collect(),
			members: members
				.into_iter()
				.map(|m| rewrite_impl_member(m, ctx))
				.collect(),
			impls: impls
				.into_iter()
				.map(|i| rewrite_struct_impl(i, ctx))
				.collect(),
		},
		Declaration::Namespace {
			visibility,
			name,
			members,
		} => Declaration::Namespace {
			visibility,
			name: rewrite_own_name(name, ctx),
			members: members
				.into_iter()
				.map(|m| rewrite_impl_member(m, ctx))
				.collect(),
		},
		Declaration::Interface {
			visibility,
			name,
			generics,
			super_interfaces,
			members,
		} => Declaration::Interface {
			visibility,
			name: rewrite_own_name(name, ctx),
			generics: generics
				.into_iter()
				.map(|g| rewrite_generic_param(g, ctx))
				.collect(),
			super_interfaces: super_interfaces
				.into_iter()
				.map(|si| {
					Spanned(
						(
							rewrite_ref_ident(si.0.0, ctx),
							si.0
								.1
								.into_iter()
								.map(|g| rewrite_generic_arg(g, ctx))
								.collect(),
						),
						si.1,
					)
				})
				.collect(),
			members: members
				.into_iter()
				.map(|m| rewrite_interface_member(m, ctx))
				.collect(),
		},
		Declaration::Impl {
			visibility,
			generics,
			mutable,
			type_,
			members,
		} => Declaration::Impl {
			visibility,
			generics: generics
				.into_iter()
				.map(|g| rewrite_generic_param(g, ctx))
				.collect(),
			mutable,
			type_: rewrite_spanned_type(type_, ctx),
			members: members
				.into_iter()
				.map(|m| rewrite_impl_member(m, ctx))
				.collect(),
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
			generics: generics
				.into_iter()
				.map(|g| rewrite_generic_param(g, ctx))
				.collect(),
			mutable,
			type_: rewrite_spanned_type(type_, ctx),
			for_interface: (
				rewrite_ref_ident(for_interface.0, ctx),
				for_interface
					.1
					.into_iter()
					.map(|g| rewrite_generic_arg(g, ctx))
					.collect(),
			),
			members: members
				.into_iter()
				.map(|m| rewrite_impl_member(m, ctx))
				.collect(),
		},
	}
}

/// A top-level `let`'s bound name, if it's a plain binding (mirrors
/// `nymph_sema::def`'s `binding_name`) — anything else (a destructuring
/// pattern) declares no single name, so nothing to rename. Either way, any
/// struct/enum-variant path nested in the pattern (e.g. `let Point(x, y) =
/// ..`) is a REFERENCE, not a declaration, and must be rewritten like any
/// other pattern (see [`rewrite_pattern`]).
fn rewrite_let_decl_toplevel(meta: LetDeclaration, ctx: &RewriteCtx) -> LetDeclaration {
	let name = match meta.name {
		Spanned(Pattern::Binding { name, inner }, span) => Spanned(
			Pattern::Binding {
				name: rewrite_own_name(name, ctx),
				inner: Box::new(rewrite_pattern(*inner, ctx)),
			},
			span,
		),
		other => rewrite_pattern(other, ctx),
	};
	LetDeclaration {
		kind: meta.kind,
		name,
		type_: meta.type_.map(|t| rewrite_spanned_type(t, ctx)),
	}
}

/// A `let`'s bound name INSIDE a body (a local, not a top-level decl) — its
/// own binding name(s) are never renamed, but any struct/enum-variant path
/// nested in the pattern is (see [`rewrite_pattern`]), same as its type
/// annotation (it may reference an imported type).
fn rewrite_let_decl_local(meta: LetDeclaration, ctx: &RewriteCtx) -> LetDeclaration {
	LetDeclaration {
		kind: meta.kind,
		name: rewrite_pattern(meta.name, ctx),
		type_: meta.type_.map(|t| rewrite_spanned_type(t, ctx)),
	}
}

/// Rewrite every struct/enum-variant path reference nested anywhere in a
/// pattern — the pattern counterpart of `rewrite_expr`'s
/// identifier/member-access rewriting. A pattern's own BINDING names (a plain
/// `name` or a struct-field shorthand `name`) are never touched here: they
/// declare a new local, not a reference.
fn rewrite_pattern(p: Spanned<Pattern>, ctx: &RewriteCtx) -> Spanned<Pattern> {
	let span = p.1;
	let kind = match p.0 {
		Pattern::Binding { name, inner } => Pattern::Binding {
			name,
			inner: Box::new(rewrite_pattern(*inner, ctx)),
		},
		Pattern::List(entries) => Pattern::List(rewrite_pattern_list_entries(entries, ctx)),
		Pattern::Tuple(entries) => Pattern::Tuple(rewrite_pattern_list_entries(entries, ctx)),
		Pattern::Map(entries) => Pattern::Map(
			entries
				.into_iter()
				.map(|e| {
					Spanned(
						match e.0 {
							MapPatternEntry::Entry(k, v) => {
								MapPatternEntry::Entry(rewrite_pattern(k, ctx), rewrite_pattern(v, ctx))
							}
							MapPatternEntry::Rest(name) => MapPatternEntry::Rest(name),
						},
						e.1,
					)
				})
				.collect(),
		),
		Pattern::Range(r) => Pattern::Range(rewrite_range_pattern_kind(r, ctx)),
		Pattern::Struct { path, fields } => Pattern::Struct {
			path: rewrite_pattern_path(path, ctx),
			fields: fields
				.into_iter()
				.map(|f| {
					Spanned(
						match f.0 {
							StructPatternField::Value { name, value } => StructPatternField::Value {
								name,
								value: rewrite_pattern(value, ctx),
							},
							StructPatternField::Positional(value) => {
								StructPatternField::Positional(rewrite_pattern(value, ctx))
							}
							other @ (StructPatternField::Named(_) | StructPatternField::Rest) => other,
						},
						f.1,
					)
				})
				.collect(),
		},
		Pattern::Union(a, b) => Pattern::Union(
			Box::new(rewrite_pattern(*a, ctx)),
			Box::new(rewrite_pattern(*b, ctx)),
		),
		Pattern::Grouped(inner) => Pattern::Grouped(Box::new(rewrite_pattern(*inner, ctx))),
		other @ (Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Placeholder) => other,
	};
	Spanned(kind, span)
}

fn rewrite_pattern_list_entries(
	entries: Vec<Spanned<ListPatternEntry>>,
	ctx: &RewriteCtx,
) -> Vec<Spanned<ListPatternEntry>> {
	entries
		.into_iter()
		.map(|e| {
			Spanned(
				match e.0 {
					ListPatternEntry::Item(p) => ListPatternEntry::Item(rewrite_pattern(p, ctx)),
					ListPatternEntry::Rest(name) => ListPatternEntry::Rest(name),
				},
				e.1,
			)
		})
		.collect()
}

fn rewrite_range_pattern_kind(r: RangePatternKind, ctx: &RewriteCtx) -> RangePatternKind {
	match r {
		RangePatternKind::From(p) => RangePatternKind::From(Box::new(rewrite_pattern(*p, ctx))),
		RangePatternKind::To(p) => RangePatternKind::To(Box::new(rewrite_pattern(*p, ctx))),
		RangePatternKind::Exclusive { min, max } => RangePatternKind::Exclusive {
			min: Box::new(rewrite_pattern(*min, ctx)),
			max: Box::new(rewrite_pattern(*max, ctx)),
		},
		RangePatternKind::ToInclusive(p) => {
			RangePatternKind::ToInclusive(Box::new(rewrite_pattern(*p, ctx)))
		}
		RangePatternKind::Inclusive { min, max } => RangePatternKind::Inclusive {
			min: Box::new(rewrite_pattern(*min, ctx)),
			max: Box::new(rewrite_pattern(*max, ctx)),
		},
	}
}

/// Rewrite a struct/enum-variant pattern path, mirroring `rewrite_expr`'s
/// `MemberAccess` handling: a one-segment path is a bare reference (this
/// module's own struct/enum name, or a `with`-bound imported name), looked up
/// in `ctx.renames` exactly like a value identifier. A two-segment path is
/// either a namespace member access (`math.Point`, resolved down to the
/// target's single mangled name via `ctx.namespaces`) or a same-module
/// qualified enum-variant path (`Color.Red`) — in the latter case only the
/// type name is rewritten, since a variant's own name is never mangled.
fn rewrite_pattern_path(path: Vec<Ident>, ctx: &RewriteCtx) -> Vec<Ident> {
	match path.as_slice() {
		[single] => match ctx.rename(&single.0) {
			Some(m) => vec![Spanned(m, single.1)],
			None => path,
		},
		[first, second] => {
			if let Some(ns) = ctx.namespaces.get(&first.0) {
				return match ctx.resolve_namespace_member(ns, second) {
					Some(mangled) => vec![Spanned(mangled, second.1)],
					// Diagnostic already pushed (unresolved/private member) — leave the
					// path unchanged so the rest of the walk still sees a well-formed
					// tree rather than aborting the whole pass.
					None => path,
				};
			}
			match ctx.rename(&first.0) {
				Some(m) => vec![Spanned(m, first.1), second.clone()],
				None => path,
			}
		}
		_ => path,
	}
}

/// A reference to a type/interface name — same substitution as a value
/// identifier (one shared namespace, see `RewriteCtx::renames`).
fn rewrite_ref_ident(name: Ident, ctx: &RewriteCtx) -> Ident {
	match ctx.rename(&name.0) {
		Some(m) => Spanned(m, name.1),
		None => name,
	}
}

fn rewrite_type(t: Type, ctx: &RewriteCtx) -> Type {
	match t {
		Type::Reference { name, generics } => Type::Reference {
			name: rewrite_ref_ident(name, ctx),
			generics: generics
				.into_iter()
				.map(|g| rewrite_generic_arg(g, ctx))
				.collect(),
		},
		Type::Intersection(a, b) => Type::Intersection(
			Box::new(rewrite_spanned_type(*a, ctx)),
			Box::new(rewrite_spanned_type(*b, ctx)),
		),
		Type::List(inner) => Type::List(Box::new(rewrite_spanned_type(*inner, ctx))),
		Type::Tuple(elems) => Type::Tuple(
			elems
				.into_iter()
				.map(|t| rewrite_spanned_type(t, ctx))
				.collect(),
		),
		Type::Map(k, v) => Type::Map(
			Box::new(rewrite_spanned_type(*k, ctx)),
			Box::new(rewrite_spanned_type(*v, ctx)),
		),
		Type::Function {
			params,
			return_type,
		} => Type::Function {
			params: params
				.into_iter()
				.map(|(n, t)| (n, rewrite_spanned_type(t, ctx)))
				.collect(),
			return_type: Box::new(rewrite_spanned_type(*return_type, ctx)),
		},
		Type::Grouped(inner) => Type::Grouped(Box::new(rewrite_spanned_type(*inner, ctx))),
		Type::Mut(inner) => Type::Mut(Box::new(rewrite_spanned_type(*inner, ctx))),
		other @ (Type::Int
		| Type::UInt
		| Type::Float
		| Type::Char
		| Type::String
		| Type::Boolean
		| Type::Void
		| Type::Never
		| Type::SelfType
		| Type::Infer) => other,
	}
}

fn rewrite_spanned_type(t: Spanned<Type>, ctx: &RewriteCtx) -> Spanned<Type> {
	Spanned(rewrite_type(t.0, ctx), t.1)
}

fn rewrite_generic_arg(g: Spanned<GenericArg>, ctx: &RewriteCtx) -> Spanned<GenericArg> {
	Spanned(
		GenericArg {
			value: rewrite_spanned_type(g.0.value, ctx),
			name: g.0.name,
		},
		g.1,
	)
}

fn rewrite_generic_param(g: Spanned<GenericParam>, ctx: &RewriteCtx) -> Spanned<GenericParam> {
	Spanned(
		GenericParam {
			name: g.0.name,
			constraint: g.0.constraint.map(|t| rewrite_spanned_type(t, ctx)),
			default: g.0.default.map(|t| rewrite_spanned_type(t, ctx)),
		},
		g.1,
	)
}

fn rewrite_func_decl_body(f: FuncDeclaration, ctx: &RewriteCtx) -> FuncDeclaration {
	FuncDeclaration {
		name: f.name,
		kind: f.kind,
		generics: f
			.generics
			.into_iter()
			.map(|g| rewrite_generic_param(g, ctx))
			.collect(),
		params: f
			.params
			.into_iter()
			.map(|p| rewrite_func_param(p, ctx))
			.collect(),
		return_type: f.return_type.map(|t| rewrite_spanned_type(t, ctx)),
	}
}

fn rewrite_func_param(p: Spanned<FuncParam>, ctx: &RewriteCtx) -> Spanned<FuncParam> {
	Spanned(
		FuncParam {
			name: p.0.name,
			type_: rewrite_spanned_type(p.0.type_, ctx),
			mutable: p.0.mutable,
			spread: p.0.spread,
		},
		p.1,
	)
}

fn rewrite_struct_field(f: Spanned<StructField>, ctx: &RewriteCtx) -> Spanned<StructField> {
	Spanned(
		StructField {
			visibility: f.0.visibility,
			name: f.0.name,
			type_: rewrite_spanned_type(f.0.type_, ctx),
			default: f.0.default.map(|e| rewrite_expr(e, ctx)),
		},
		f.1,
	)
}

fn rewrite_enum_variant(v: Spanned<EnumVariant>, ctx: &RewriteCtx) -> Spanned<EnumVariant> {
	Spanned(
		EnumVariant {
			name: v.0.name,
			fields: v
				.0
				.fields
				.into_iter()
				.map(|f| rewrite_struct_field(f, ctx))
				.collect(),
		},
		v.1,
	)
}

fn rewrite_impl_member(m: Spanned<ImplMember>, ctx: &RewriteCtx) -> Spanned<ImplMember> {
	Spanned(
		match m.0 {
			ImplMember::Let {
				visibility,
				meta,
				value,
			} => ImplMember::Let {
				visibility,
				meta: rewrite_let_decl_local(meta, ctx),
				value: rewrite_expr(value, ctx),
			},
			ImplMember::ExternalLet(vis, s, meta) => {
				ImplMember::ExternalLet(vis, s, rewrite_let_decl_local(meta, ctx))
			}
			ImplMember::Func {
				visibility,
				meta,
				body,
			} => ImplMember::Func {
				visibility,
				meta: rewrite_func_decl_body(meta, ctx),
				body: rewrite_expr(body, ctx),
			},
			ImplMember::ExternalFunc(vis, s, meta) => {
				ImplMember::ExternalFunc(vis, s, rewrite_func_decl_body(meta, ctx))
			}
		},
		m.1,
	)
}

fn rewrite_interface_element(
	e: Spanned<InterfaceElement>,
	ctx: &RewriteCtx,
) -> Spanned<InterfaceElement> {
	Spanned(
		match e.0 {
			InterfaceElement::Let { meta, value } => InterfaceElement::Let {
				meta: rewrite_let_decl_local(meta, ctx),
				value: value.map(|e| rewrite_expr(e, ctx)),
			},
			InterfaceElement::Func { meta, body } => InterfaceElement::Func {
				meta: rewrite_func_decl_body(meta, ctx),
				body: body.map(|e| rewrite_expr(e, ctx)),
			},
		},
		e.1,
	)
}

fn rewrite_interface_member(
	m: Spanned<InterfaceMember>,
	ctx: &RewriteCtx,
) -> Spanned<InterfaceMember> {
	Spanned(
		match m.0 {
			InterfaceMember::Element(e) => {
				InterfaceMember::Element(Box::new(rewrite_interface_element(*e, ctx)))
			}
			InterfaceMember::Impl {
				interface,
				generics,
				members,
			} => InterfaceMember::Impl {
				interface: (
					rewrite_ref_ident(interface.0, ctx),
					interface
						.1
						.into_iter()
						.map(|g| rewrite_generic_arg(g, ctx))
						.collect(),
				),
				generics: generics
					.into_iter()
					.map(|g| rewrite_generic_param(g, ctx))
					.collect(),
				members: members
					.into_iter()
					.map(|m| rewrite_impl_member(m, ctx))
					.collect(),
			},
		},
		m.1,
	)
}

fn rewrite_struct_impl(m: Spanned<StructImpl>, ctx: &RewriteCtx) -> Spanned<StructImpl> {
	Spanned(
		StructImpl {
			interface: (
				rewrite_ref_ident(m.0.interface.0, ctx),
				m.0
					.interface
					.1
					.into_iter()
					.map(|g| rewrite_generic_arg(g, ctx))
					.collect(),
			),
			generics: m
				.0
				.generics
				.into_iter()
				.map(|g| rewrite_generic_param(g, ctx))
				.collect(),
			members: m
				.0
				.members
				.into_iter()
				.map(|mm| rewrite_impl_member(mm, ctx))
				.collect(),
		},
		m.1,
	)
}

// `e` is deliberately taken and returned boxed: every AST call site holds a
// `Box<Expr>` field, so unboxing to `Expr` would only push the (identical)
// allocate-transform-reallocate work onto each caller, not eliminate it —
// mirrors `nymph-sema/src/prelude.rs`'s `box_expr` for the identical reason.
#[allow(clippy::boxed_local)]
fn rewrite_box_expr(e: Box<Expr>, ctx: &RewriteCtx) -> Box<Expr> {
	Box::new(rewrite_expr(*e, ctx))
}

fn rewrite_opt_box_expr(e: Option<Box<Expr>>, ctx: &RewriteCtx) -> Option<Box<Expr>> {
	e.map(|e| rewrite_box_expr(e, ctx))
}

fn rewrite_expr(e: Expr, ctx: &RewriteCtx) -> Expr {
	let span = e.span;
	let id = e.id;
	match e.kind {
		ExprKind::Identifier(name) => {
			let kind = match ctx.rename(&name.0) {
				Some(m) => ExprKind::Identifier(Spanned(m, name.1)),
				None => ExprKind::Identifier(name),
			};
			Expr::new(kind, span, id)
		}
		ExprKind::MemberAccess {
			parent,
			member,
			optional,
		} => {
			if let ExprKind::Identifier(ns_ident) = &parent.kind
				&& let Some(ns) = ctx.namespaces.get(&ns_ident.0)
				&& let Some(mangled) = ctx.resolve_namespace_member(ns, &member)
			{
				return Expr::new(ExprKind::Identifier(Spanned(mangled, member.1)), span, id);
			}
			// Either not a namespace access at all, or the diagnostic was already
			// pushed (unresolved/private member) — fall through to an inert
			// rewrite so the rest of the walk (and any downstream check) still
			// sees a well-formed tree rather than aborting the whole pass.
			Expr::new(
				ExprKind::MemberAccess {
					parent: rewrite_box_expr(parent, ctx),
					member,
					optional,
				},
				span,
				id,
			)
		}
		ExprKind::Int(v) => Expr::new(ExprKind::Int(v), span, id),
		ExprKind::UInt(v) => Expr::new(ExprKind::UInt(v), span, id),
		ExprKind::Float(v) => Expr::new(ExprKind::Float(v), span, id),
		ExprKind::Char(v) => Expr::new(ExprKind::Char(v), span, id),
		ExprKind::Boolean(v) => Expr::new(ExprKind::Boolean(v), span, id),
		ExprKind::This => Expr::new(ExprKind::This, span, id),
		ExprKind::Continue { label } => Expr::new(ExprKind::Continue { label }, span, id),
		ExprKind::AnonymousParam(p) => Expr::new(ExprKind::AnonymousParam(p), span, id),
		ExprKind::String(parts) => Expr::new(
			ExprKind::String(
				parts
					.into_iter()
					.map(|p| {
						Spanned(
							match p.0 {
								StringPart::InterpolatedExpr(e) => {
									StringPart::InterpolatedExpr(rewrite_expr(e, ctx))
								}
								other => other,
							},
							p.1,
						)
					})
					.collect(),
			),
			span,
			id,
		),
		ExprKind::List(items) => Expr::new(ExprKind::List(rewrite_list_items(items, ctx)), span, id),
		ExprKind::Tuple(items) => Expr::new(ExprKind::Tuple(rewrite_list_items(items, ctx)), span, id),
		ExprKind::Map(entries) => Expr::new(
			ExprKind::Map(
				entries
					.into_iter()
					.map(|e| {
						Spanned(
							match e.0 {
								MapEntry::Entry(k, v) => {
									MapEntry::Entry(rewrite_expr(k, ctx), rewrite_expr(v, ctx))
								}
								MapEntry::Spread(e) => MapEntry::Spread(rewrite_expr(e, ctx)),
							},
							e.1,
						)
					})
					.collect(),
			),
			span,
			id,
		),
		ExprKind::Range(r) => Expr::new(ExprKind::Range(rewrite_range_kind(r, ctx)), span, id),
		ExprKind::Call {
			func,
			generics,
			args,
		} => Expr::new(
			ExprKind::Call {
				func: rewrite_box_expr(func, ctx),
				generics: generics
					.into_iter()
					.map(|g| rewrite_generic_arg(g, ctx))
					.collect(),
				args: args
					.into_iter()
					.map(|a| {
						Spanned(
							CallArg {
								value: rewrite_expr(a.0.value, ctx),
								name: a.0.name,
								spread: a.0.spread,
							},
							a.1,
						)
					})
					.collect(),
			},
			span,
			id,
		),
		ExprKind::IndexAccess {
			parent,
			index,
			optional,
		} => Expr::new(
			ExprKind::IndexAccess {
				parent: rewrite_box_expr(parent, ctx),
				index: rewrite_box_expr(index, ctx),
				optional,
			},
			span,
			id,
		),
		ExprKind::Closure {
			params,
			generics,
			return_type,
			body,
		} => Expr::new(
			ExprKind::Closure {
				params: params
					.into_iter()
					.map(|p| {
						Spanned(
							ClosureParam {
								name: p.0.name,
								type_: p.0.type_.map(|t| rewrite_spanned_type(t, ctx)),
								mutable: p.0.mutable,
								spread: p.0.spread,
							},
							p.1,
						)
					})
					.collect(),
				generics: generics
					.into_iter()
					.map(|g| rewrite_generic_param(g, ctx))
					.collect(),
				return_type: return_type.map(|t| rewrite_spanned_type(t, ctx)),
				body: rewrite_box_expr(body, ctx),
			},
			span,
			id,
		),
		ExprKind::PrefixOp { op, value } => Expr::new(
			ExprKind::PrefixOp {
				op,
				value: rewrite_box_expr(value, ctx),
			},
			span,
			id,
		),
		ExprKind::PostfixOp { op, value } => Expr::new(
			ExprKind::PostfixOp {
				op,
				value: rewrite_box_expr(value, ctx),
			},
			span,
			id,
		),
		ExprKind::BinaryOp { lhs, op, rhs } => Expr::new(
			ExprKind::BinaryOp {
				lhs: rewrite_box_expr(lhs, ctx),
				op,
				rhs: rewrite_box_expr(rhs, ctx),
			},
			span,
			id,
		),
		ExprKind::TypeOp { lhs, op, rhs } => Expr::new(
			ExprKind::TypeOp {
				lhs: rewrite_box_expr(lhs, ctx),
				op,
				rhs: rewrite_spanned_type(rhs, ctx),
			},
			span,
			id,
		),
		ExprKind::PatternOp { lhs, op, rhs } => Expr::new(
			ExprKind::PatternOp {
				lhs: rewrite_box_expr(lhs, ctx),
				op,
				rhs,
			},
			span,
			id,
		),
		ExprKind::AssignOp { lhs, op, rhs } => Expr::new(
			ExprKind::AssignOp {
				lhs: rewrite_box_expr(lhs, ctx),
				op,
				rhs: rewrite_box_expr(rhs, ctx),
			},
			span,
			id,
		),
		ExprKind::Return { value, label } => Expr::new(
			ExprKind::Return {
				value: rewrite_opt_box_expr(value, ctx),
				label,
			},
			span,
			id,
		),
		ExprKind::Break { value, label } => Expr::new(
			ExprKind::Break {
				value: rewrite_opt_box_expr(value, ctx),
				label,
			},
			span,
			id,
		),
		ExprKind::While {
			condition,
			body,
			label,
		} => Expr::new(
			ExprKind::While {
				condition: rewrite_box_expr(condition, ctx),
				body: rewrite_box_expr(body, ctx),
				label,
			},
			span,
			id,
		),
		ExprKind::For {
			variable,
			iterable,
			body,
			label,
		} => Expr::new(
			ExprKind::For {
				variable: rewrite_pattern(variable, ctx),
				iterable: rewrite_box_expr(iterable, ctx),
				body: rewrite_box_expr(body, ctx),
				label,
			},
			span,
			id,
		),
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => Expr::new(
			ExprKind::If {
				condition: rewrite_box_expr(condition, ctx),
				then: rewrite_box_expr(then, ctx),
				otherwise: rewrite_opt_box_expr(otherwise, ctx),
			},
			span,
			id,
		),
		ExprKind::Match { value, arms } => Expr::new(
			ExprKind::Match {
				value: rewrite_box_expr(value, ctx),
				arms: arms
					.into_iter()
					.map(|a| MatchArm {
						pattern: rewrite_pattern(a.pattern, ctx),
						guard: a.guard.map(|e| rewrite_expr(e, ctx)),
						body: rewrite_expr(a.body, ctx),
					})
					.collect(),
			},
			span,
			id,
		),
		ExprKind::Block { body, label } => Expr::new(
			ExprKind::Block {
				body: body
					.into_iter()
					.map(|s| {
						Spanned(
							match s.0 {
								Statement::Expr(e) => Statement::Expr(rewrite_expr(e, ctx)),
								Statement::Let { meta, value } => Statement::Let {
									meta: rewrite_let_decl_local(meta, ctx),
									value: rewrite_expr(value, ctx),
								},
							},
							s.1,
						)
					})
					.collect(),
				label,
			},
			span,
			id,
		),
		ExprKind::Grouped(inner) => {
			Expr::new(ExprKind::Grouped(rewrite_box_expr(inner, ctx)), span, id)
		}
	}
}

fn rewrite_list_items(items: Vec<Spanned<ListItem>>, ctx: &RewriteCtx) -> Vec<Spanned<ListItem>> {
	items
		.into_iter()
		.map(|i| {
			Spanned(
				match i.0 {
					ListItem::Expr(e) => ListItem::Expr(rewrite_expr(e, ctx)),
					ListItem::Spread(e) => ListItem::Spread(rewrite_expr(e, ctx)),
				},
				i.1,
			)
		})
		.collect()
}

fn rewrite_range_kind(r: RangeKind, ctx: &RewriteCtx) -> RangeKind {
	match r {
		RangeKind::From(e) => RangeKind::From(rewrite_box_expr(e, ctx)),
		RangeKind::To(e) => RangeKind::To(rewrite_box_expr(e, ctx)),
		RangeKind::Exclusive { min, max } => RangeKind::Exclusive {
			min: rewrite_box_expr(min, ctx),
			max: rewrite_box_expr(max, ctx),
		},
		RangeKind::ToInclusive(e) => RangeKind::ToInclusive(rewrite_box_expr(e, ctx)),
		RangeKind::Inclusive { min, max } => RangeKind::Inclusive {
			min: rewrite_box_expr(min, ctx),
			max: rewrite_box_expr(max, ctx),
		},
	}
}

fn rewrite_type_alias_decl(
	meta: nymph_ast::decl::TypeAliasDeclaration,
	ctx: &RewriteCtx,
) -> nymph_ast::decl::TypeAliasDeclaration {
	nymph_ast::decl::TypeAliasDeclaration {
		name: rewrite_own_name(meta.name, ctx),
		generics: meta
			.generics
			.into_iter()
			.map(|g| rewrite_generic_param(g, ctx))
			.collect(),
	}
}
