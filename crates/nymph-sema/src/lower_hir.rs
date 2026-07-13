//! Structural lowering of the AST into the code-generation HIR.
//!
//! Slice 1 was a pure syntactic walk that consumed neither type annotations nor
//! the interner, because JS needs no type information to emit correct
//! scalar/control-flow code (see the slice-1 plan's Design Decisions). Slice 2A
//! starts consuming the checker's output: index-access lowering must know whether
//! the receiver is a `Map` (→ `HirExpr::MapGet`) or a list/tuple (→ `HirExpr::Index`),
//! which is only recorded in the checker's `Annotations` side-table. `lower_hir` now
//! takes the full `Checked` result and threads `&Annotations`/`&Interner` down through
//! a `Lowerer` so later slices can add further type-directed lowering without another
//! signature change.

use std::cell::RefCell;

use ecow::EcoString;
use nymph_ast::{
	Ident, Spanned,
	decl::{Declaration, FuncDeclaration, Module},
	expr::{CallArg, Expr, ExprKind, ListItem, MapEntry, Statement},
	ops::{AssignOperator, BinaryOperator, PrefixOperator},
	ty::{GenericArg, GenericParam},
};
use nymph_hir::hir::{
	BinOp, HirArm, HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirLit, HirMethod, HirModule, HirPat,
	HirRange, HirStmt, HirVariant, UnOp,
};
use nymph_hir::ty::{Interner, TyKind};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{Annotations, Checked, DispatchKind};

/// Lower a checked module into the code-generation HIR, consulting `checked`'s
/// annotations/interner for type-directed decisions (e.g. index-access dispatch).
pub fn lower_hir(module: &Module, checked: &Checked) -> HirModule {
	// A call whose callee names a struct is construction, not an ordinary call.
	// Collect the module's struct names up front so `lower_expr` can dispatch on
	// them. This mirrors the checker's own dispatch: `infer_call` treats *any*
	// identifier resolving to a struct def as construction, before trying variant/
	// method/function resolution — so lowering stays consistent with checking.
	// ASSUMPTION: every constructible struct is declared in this module. That holds
	// for the current single-module pipeline; when cross-module imports are wired,
	// this set must also include imported struct names (otherwise an imported
	// `Point(…)` would lower to a plain call instead of `New`).
	let struct_names = module
		.members
		.iter()
		.filter_map(|decl| match decl {
			Declaration::Struct { name, .. } => Some(name.0.clone()),
			_ => None,
		})
		.collect();
	let lowerer = Lowerer {
		annotations: &checked.annotations,
		interner: &checked.interner,
		struct_names,
		scopes: RefCell::new(Vec::new()),
		rename_counters: RefCell::new(FxHashMap::default()),
	};
	lowerer.lower_module(module)
}

/// Carries the checker's output through the recursive lowering walk.
struct Lowerer<'a> {
	annotations: &'a Annotations,
	interner: &'a Interner,
	struct_names: FxHashSet<EcoString>,
	/// The JS-scope stack for `let`-shadowing rename (Slice 4E, Y2). A `RefCell`
	/// keeps every lowering method `&self` (mirroring `Emitter`'s `Cell<u32>`
	/// gensym counter) despite the many iterator-chain closures that capture
	/// `self` throughout this walk. One entry per JS scope: function/method body
	/// (seeded with params, merged with the body block — emit flattens both into
	/// one function body), every other `HirExpr::Block`, and each match arm
	/// (pattern binds + guard + body together).
	scopes: RefCell<Vec<Scope>>,
	/// Per-source-name monotonic `$N` suffix counter, shared across the WHOLE
	/// scope stack (not per `Scope`) — see [`Self::declare`] for why a rename
	/// must be globally unique per name rather than merely unique within one
	/// scope (Slice 4E, Y2 fix: a nested-block redeclaration renaming to the
	/// same suffix an ancestor scope already renamed to would just reintroduce
	/// the identical TDZ hazard one level deeper).
	rename_counters: RefCell<FxHashMap<EcoString, u32>>,
}

/// One JS lexical scope's bindings, for Y2 shadowing rename.
#[derive(Default)]
struct Scope {
	/// Original source name → the name currently in effect for it in this scope.
	current: FxHashMap<EcoString, EcoString>,
}

impl Lowerer<'_> {
	/// Push a fresh, empty JS scope (Slice 4E, Y2).
	fn push_scope(&self) {
		self.scopes.borrow_mut().push(Scope::default());
	}

	/// Pop the innermost JS scope.
	fn pop_scope(&self) {
		self.scopes.borrow_mut().pop();
	}

	/// Bind `name` in the CURRENT (innermost) JS scope, returning the name to
	/// actually emit for this declaration: `name` itself if it isn't currently
	/// bound in ANY active scope (this one or an ancestor), or a fresh `name$1`,
	/// `name$2`, … when it is (Slice 4E, Y2). `$` cannot appear in a Nymph
	/// identifier (confirmed against the lexer), so a renamed binding can never
	/// collide with a real user name.
	///
	/// The check spans the WHOLE scope stack, not just the current scope: a
	/// nested block/if-branch/while-body/match-arm-body gets its own, separate
	/// `Scope` here, but emit still gives it its own JS `BlockStatement`/IIFE —
	/// which means a nested `let` that reuses a name still bound in an ANCESTOR
	/// scope is exactly as dangerous as a same-scope redeclaration would be,
	/// because JS hoists a block's own `const`/`let` for the whole block (TDZ):
	/// if the new declaration keeps the unrenamed source name, its own
	/// initializer reading that same name (e.g. `let i = i + 100` inside a
	/// nested block shadowing an outer `i`) resolves to the new, not-yet-
	/// initialized binding instead of the outer one, throwing `ReferenceError:
	/// Cannot access 'i' before initialization` at runtime. Renaming on ANY
	/// active-scope collision (not just a proven read-before-declare hazard)
	/// sidesteps that analysis entirely and is always safe, just occasionally
	/// renames when the specific initializer wouldn't have needed it.
	///
	/// The suffix counter lives on the `Lowerer`, not the `Scope` (see
	/// `rename_counters`), so it hands out a name that's unique across the
	/// WHOLE scope stack, not just within one `Scope` — a per-`Scope` counter
	/// could otherwise pick the same suffix an ancestor scope already renamed
	/// to (e.g. two levels of `let i = i + …` shadowing each other), which
	/// would just reproduce the identical TDZ hazard one level deeper. Must be
	/// called with at least one scope pushed.
	fn declare(&self, name: &EcoString) -> EcoString {
		let mut scopes = self.scopes.borrow_mut();
		let shadows_active_binding = scopes.iter().any(|s| s.current.contains_key(name));
		let scope = scopes
			.last_mut()
			.expect("slice-4e lowering: declare() called outside any pushed scope");
		if shadows_active_binding {
			let mut counters = self.rename_counters.borrow_mut();
			let suffix = counters.entry(name.clone()).or_insert(0);
			*suffix += 1;
			let renamed: EcoString = format!("{name}${suffix}").into();
			scope.current.insert(name.clone(), renamed.clone());
			renamed
		} else {
			scope.current.insert(name.clone(), name.clone());
			name.clone()
		}
	}

	/// Resolve an identifier reference through the JS-scope stack, innermost
	/// first, to whatever name is currently bound for it (itself, or a Y2 rename).
	/// Falls through to `name` unchanged when no pushed scope binds it — module-
	/// level functions/classes/enums/top-level `let`s are never pushed onto the
	/// scope stack, so this is exactly how a reference to one of those resolves.
	fn resolve(&self, name: &EcoString) -> EcoString {
		let scopes = self.scopes.borrow();
		for scope in scopes.iter().rev() {
			if let Some(mapped) = scope.current.get(name) {
				return mapped.clone();
			}
		}
		name.clone()
	}

	fn lower_module(&self, module: &Module) -> HirModule {
		use nymph_ast::decl::{ImplMember, InterfaceMember};
		use nymph_ast::ty::Type;

		// Interface bodies, for materializing un-overridden default methods onto
		// implementing struct classes (Slice 4C-b). Resolution is by bare name
		// within this flattened module — stdlib isn't linked in yet, so no
		// cross-module lookup is needed (mirrors the checker's own
		// `finish_interface_impl`, which resolves the same way via `defs.get`).
		let mut interfaces_by_name: FxHashMap<EcoString, &[Spanned<InterfaceMember>]> =
			FxHashMap::default();
		for decl in &module.members {
			if let Declaration::Interface { name, members, .. } = decl {
				interfaces_by_name.insert(name.0.clone(), members.as_slice());
			}
		}

		// First pass: collect instance methods from top-level `impl <Named>` blocks
		// (inherent, 4A) and top-level `impl <Interface> for <Named>` blocks
		// (interface impls, 4B/D5, now also materializing un-overridden interface
		// defaults per Slice 4C-b), keyed by the target type name. Non-`func`
		// members (namespaced statics, nested impls, `impl mut`) are deferred and
		// panic loudly rather than silently disappearing.
		let mut methods_by_type: FxHashMap<EcoString, Vec<HirMethod>> = FxHashMap::default();
		for decl in &module.members {
			match decl {
				Declaration::Impl { type_, members, .. } => {
					// Inherent impl: no interface, no defaults to materialize. A
					// non-`Reference` target silently contributes nothing here, same as
					// before Slice 4C-b — unchanged, out of this slice's scope.
					if let Type::Reference { name, .. } = &type_.0 {
						let entry = methods_by_type.entry(name.0.clone()).or_default();
						for member in members {
							match &member.0 {
								ImplMember::Func { meta, body, .. } => entry.push(self.lower_method(meta, body)),
								other => panic!("slice-4b lowering does not yet handle impl member {other:?}"),
							}
						}
					}
				}
				Declaration::ImplFor {
					generics,
					type_,
					for_interface,
					members,
					..
				} => {
					self.push_impl_for_methods(
						generics,
						type_,
						for_interface,
						members,
						&interfaces_by_name,
						&mut methods_by_type,
					);
				}
				_ => {}
			}
		}

		let mut lets = Vec::new();
		let mut funcs = Vec::new();
		let mut classes = Vec::new();
		let mut enums = Vec::new();
		for decl in &module.members {
			match decl {
				// A top-level `let`/`let mut` (Slice 4E, Y3). No scope is pushed while
				// lowering its value: the module-level scope stack is empty for the
				// whole module walk, so `resolve()` on any identifier inside falls
				// through to its bare source name — exactly right, since module-level
				// funcs/classes/enums/other top-level lets are never renamed either.
				Declaration::Let { meta, value, .. } => lets.push(HirLet {
					name: param_name(&meta.name),
					mutable: meta.mutable,
					value: self.lower_expr(value),
				}),
				Declaration::Func { meta, body, .. } => funcs.push(self.lower_func(meta, body)),
				Declaration::Struct {
					name,
					fields,
					members,
					..
				} => {
					// Methods from top-level impls, the struct's own inner `func`s, and
					// nested `impl <Interface> { .. }` blocks inside the struct body
					// (also materializing that interface's un-overridden defaults,
					// Slice 4C-b).
					let methods =
						self.collect_adt_methods(&name.0, members, &interfaces_by_name, &mut methods_by_type);
					classes.push(HirClass {
						name: name.0.clone(),
						fields: fields.iter().map(|f| f.0.name.0.clone()).collect(),
						methods,
					});
				}
				Declaration::Enum {
					name,
					variants,
					members,
					..
				} => {
					// Slice 4D: enums consume `methods_by_type` (top-level `impl`/`impl
					// … for`) and their own inner members through the exact same path
					// as structs — enum-body inherent funcs and nested `impl <Interface>
					// { .. }` blocks are the same `StructInnerMember` shape.
					let methods =
						self.collect_adt_methods(&name.0, members, &interfaces_by_name, &mut methods_by_type);
					enums.push(HirEnum {
						name: name.0.clone(),
						variants: variants
							.iter()
							.map(|v| HirVariant {
								name: v.0.name.0.clone(),
								fields: v.0.fields.iter().map(|f| f.0.name.0.clone()).collect(),
							})
							.collect(),
						methods,
					});
				}
				_ => {}
			}
		}
		assert!(
			methods_by_type.is_empty(),
			"slice-4d lowering does not yet support inherent or interface-impl methods on types that are neither struct nor enum; found impls for: {:?}",
			methods_by_type.keys().collect::<Vec<_>>()
		);
		HirModule {
			lets: reorder_lets_by_dependency(lets, &funcs),
			funcs,
			classes,
			enums,
		}
	}

	/// Lower a top-level `impl <Interface> for <Type> { … }`'s own methods into
	/// `methods_by_type[Type]`, then materialize (append) that interface's
	/// un-overridden default-bodied methods (Slice 4C-b, V1: impl-provided methods
	/// first in source order, then defaults in interface source order).
	fn push_impl_for_methods(
		&self,
		generics: &[Spanned<GenericParam>],
		type_: &Spanned<nymph_ast::ty::Type>,
		for_interface: &(Ident, Vec<Spanned<GenericArg>>),
		members: &[Spanned<nymph_ast::decl::ImplMember>],
		interfaces_by_name: &FxHashMap<EcoString, &[Spanned<nymph_ast::decl::InterfaceMember>]>,
		methods_by_type: &mut FxHashMap<EcoString, Vec<HirMethod>>,
	) {
		use nymph_ast::decl::ImplMember;
		use nymph_ast::ty::Type;

		let Type::Reference { name, .. } = &type_.0 else {
			// A structural target (e.g. `impl Plus<...> for #[int] { .. }`) type-checks
			// today (the checker resolves its operator as a real `UserImpl`), but this
			// lowering has no representation for attaching methods to anything but a
			// named struct class — silently dropping it would let the checker's
			// resolution point at a method that was never emitted. Loud is the floor.
			panic!(
				"slice-4c-b lowering does not yet support `impl {} for {:?}` targeting a non-named type",
				for_interface.0.0, type_.0
			);
		};

		// A blanket impl (`impl<T> Iface for T`) parses its target as a bare
		// `Type::Reference` naming the impl's own generic parameter. Left
		// unchecked, that name could coincide with an unrelated real struct in the
		// module and silently attach the blanket's methods to it; refuse instead
		// (V5: blanket impls stay a loud deferral, never materialized).
		if generics.iter().any(|g| g.0.name.0 == name.0) {
			panic!(
				"slice-4c-b lowering does not yet support blanket impls (`impl<{0}> {1} for {0}`)",
				name.0, for_interface.0.0
			);
		}

		let entry = methods_by_type.entry(name.0.clone()).or_default();
		let mut overridden: FxHashSet<EcoString> = FxHashSet::default();
		for member in members {
			match &member.0 {
				ImplMember::Func { meta, body, .. } => {
					overridden.insert(meta.name.0.clone());
					entry.push(self.lower_method(meta, body));
				}
				other => panic!("slice-4b lowering does not yet handle impl member {other:?}"),
			}
		}
		self.push_unoverridden_defaults(&for_interface.0, &overridden, interfaces_by_name, entry);
	}

	/// Append `iface_name`'s default-bodied methods that aren't in `overridden` to
	/// `out`, each lowered via the same [`Self::lower_method`] path an impl's own
	/// methods use (Slice 4C-b, V1). The interface's default body is checked once,
	/// generically (`check_interface_default_bodies`), and its annotations are
	/// impl-independent (no per-impl type information is consulted while lowering
	/// an operator/variant/map-index dispatch), so lowering the same AST body once
	/// per implementing impl is sound — see the Slice 4C-b plan's investigation
	/// brief ("annotations_shape").
	fn push_unoverridden_defaults(
		&self,
		iface_name: &Ident,
		overridden: &FxHashSet<EcoString>,
		interfaces_by_name: &FxHashMap<EcoString, &[Spanned<nymph_ast::decl::InterfaceMember>]>,
		out: &mut Vec<HirMethod>,
	) {
		use nymph_ast::decl::{InterfaceElement, InterfaceMember};

		let Some(members) = interfaces_by_name.get(&iface_name.0) else {
			// The checker already rejects an `impl … for …`/`impl … { … }` naming an
			// undefined or non-interface type (`TypeError::NotAnInterface`) before
			// lowering ever runs, so a zero-diagnostic program always has this entry.
			panic!(
				"slice-4c-b lowering: impl references unknown interface `{}`",
				iface_name.0
			);
		};
		for m in *members {
			let InterfaceMember::Element(element) = &m.0 else {
				continue;
			};
			let InterfaceElement::Func {
				meta,
				body: Some(body),
			} = &element.0
			else {
				continue;
			};
			if overridden.contains(&meta.name.0) {
				continue;
			}
			out.push(self.lower_method(meta, body));
		}
	}

	/// Collect a struct's or enum's full method list: entries already gathered
	/// into `methods_by_type` from top-level `impl <Name>`/`impl <Interface> for
	/// <Name>` blocks, plus the type's own inner members (inherent `func`s and
	/// nested `impl <Interface> { .. }` blocks, each materializing that
	/// interface's un-overridden defaults, Slice 4C-b). Struct and enum bodies
	/// share the identical `StructInnerMember` AST shape, so this one path
	/// serves both (Slice 4D, X2).
	fn collect_adt_methods(
		&self,
		type_name: &EcoString,
		members: &[Spanned<nymph_ast::decl::StructInnerMember>],
		interfaces_by_name: &FxHashMap<EcoString, &[Spanned<nymph_ast::decl::InterfaceMember>]>,
		methods_by_type: &mut FxHashMap<EcoString, Vec<HirMethod>>,
	) -> Vec<HirMethod> {
		use nymph_ast::decl::{ImplMember, StructInnerMember};

		let mut methods = methods_by_type.remove(type_name).unwrap_or_default();
		for member in members {
			match &member.0 {
				StructInnerMember::Member(inner) => match &inner.0 {
					ImplMember::Func { meta, body, .. } => methods.push(self.lower_method(meta, body)),
					other => {
						panic!("slice-4a lowering does not yet handle struct inner member {other:?}")
					}
				},
				StructInnerMember::Impl {
					interface, members, ..
				} => {
					let mut overridden: FxHashSet<EcoString> = FxHashSet::default();
					for member in members {
						match &member.0 {
							ImplMember::Func { meta, body, .. } => {
								overridden.insert(meta.name.0.clone());
								methods.push(self.lower_method(meta, body));
							}
							other => panic!("slice-4b lowering does not yet handle impl member {other:?}"),
						}
					}
					self.push_unoverridden_defaults(
						&interface.0,
						&overridden,
						interfaces_by_name,
						&mut methods,
					);
				}
				other => {
					panic!("slice-4a lowering does not yet handle struct inner member {other:?}")
				}
			}
		}
		// V4: two interfaces (or an override and a same-named default)
		// materializing the same method name on one type is a real ambiguity
		// codegen cannot silently resolve (JS would just let the last one win) —
		// panic loudly, naming the type and method.
		self.assert_no_duplicate_methods(type_name, &methods);
		methods
	}

	/// V4: panic loudly, naming the struct and the offending method, if two
	/// materialized/overridden methods share a name on one class — two interfaces
	/// both defaulting the same method name, an override colliding with the other
	/// interface's default, or two overrides sharing a name. JS would let the last
	/// one silently win; this compiler never miscompiles silently instead.
	fn assert_no_duplicate_methods(&self, struct_name: &EcoString, methods: &[HirMethod]) {
		let mut seen: FxHashSet<&EcoString> = FxHashSet::default();
		for m in methods {
			assert!(
				seen.insert(&m.name),
				"slice-4c-b lowering: struct `{struct_name}` has multiple methods named `{}` (conflicting interface defaults/overrides)",
				m.name
			);
		}
	}

	fn lower_func(&self, meta: &FuncDeclaration, body: &Expr) -> HirFunc {
		// Params and the body block's own `let`s share ONE JS scope (emit flattens
		// both into the same function body, see `emit_func`) — push a single scope,
		// seed it with the params, then lower the body into that same scope rather
		// than letting it push its own (Slice 4E, Y2).
		self.push_scope();
		let params = meta
			.params
			.iter()
			.map(|p| self.declare(&param_name(&p.0.name)))
			.collect();
		let body = self.lower_func_body(body);
		self.pop_scope();
		HirFunc {
			name: meta.name.0.clone(),
			params,
			body,
		}
	}

	/// Lower one inherent instance method (mirrors [`Self::lower_func`]). `this` in
	/// the body lowers to [`HirExpr::This`].
	fn lower_method(&self, meta: &FuncDeclaration, body: &Expr) -> HirMethod {
		self.push_scope();
		let params = meta
			.params
			.iter()
			.map(|p| self.declare(&param_name(&p.0.name)))
			.collect();
		let body = self.lower_func_body(body);
		self.pop_scope();
		HirMethod {
			name: meta.name.0.clone(),
			params,
			body,
		}
	}

	/// Lower a function/method body expression into the scope its params were
	/// just seeded into, WITHOUT letting a block body push a second, separate
	/// scope for itself (that only applies to a block reached generically via
	/// [`Self::lower_expr`] — every other, genuinely nested, block). A non-block
	/// body (`= expr`) has no `let`s of its own anyway, so it just lowers normally.
	fn lower_func_body(&self, body: &Expr) -> HirExpr {
		match &body.kind {
			ExprKind::Block { body: stmts, .. } => self.lower_block(stmts, false),
			_ => self.lower_expr(body),
		}
	}

	fn lower_expr(&self, expr: &Expr) -> HirExpr {
		match &expr.kind {
			ExprKind::Int(v) => HirExpr::Num(v.0 as f64),
			ExprKind::UInt(v) => HirExpr::Num(v.0 as f64),
			ExprKind::Float(v) => HirExpr::Num(v.0.into_inner()),
			ExprKind::Boolean(b) => HirExpr::Bool(b.0),
			ExprKind::Char(c) => HirExpr::Char(c.0),
			ExprKind::Identifier(name) => match self.annotations.variant_of(expr.id) {
				// A bare name resolving to a variant (`None`, or `Some` as a value) →
				// the variant binding `Enum.Variant`.
				Some(res) => HirExpr::VariantRef {
					enum_name: res.enum_name.clone(),
					variant: res.variant.clone(),
				},
				// A plain local reference resolves through the JS-scope stack (Slice
				// 4E, Y2) — itself unless it's currently shadowed by a same-scope
				// rename; falls through to the bare name for anything never pushed
				// onto the stack (module-level funcs/classes/enums/top-level lets).
				None => HirExpr::Local(self.resolve(&name.0)),
			},
			ExprKind::This => HirExpr::This,
			ExprKind::Grouped(inner) => self.lower_expr(inner),
			ExprKind::Call { func, args, .. } => {
				// A call the checker resolved to a variant is variant construction →
				// `VariantNew` (bare `Some(…)` or qualified `Opt.Some(…)`).
				if let Some(variant_new) = self.variant_new(expr.id, args) {
					variant_new
				}
				// A call whose callee names a struct is construction → `New`. 2B supports
				// labeled fields only; positional construction is deferred.
				else if let ExprKind::Identifier(name) = &func.kind
					&& self.struct_names.contains(&name.0)
				{
					let fields = args
						.iter()
						.map(|a| {
							let label =
								a.0.name.as_ref().unwrap_or_else(|| {
									panic!("slice-2b struct construction requires labeled fields")
								});
							(label.0.clone(), self.lower_expr(&a.0.value))
						})
						.collect();
					HirExpr::New {
						class: name.0.clone(),
						fields,
					}
				} else {
					HirExpr::Call {
						callee: Box::new(self.lower_expr(func)),
						args: args.iter().map(|a| self.lower_expr(&a.0.value)).collect(),
					}
				}
			}
			ExprKind::MemberAccess { parent, member, .. } => {
				match self.annotations.variant_of(expr.id) {
					// A qualified nullary reference `Opt.None` → the variant binding.
					Some(res) => HirExpr::VariantRef {
						enum_name: res.enum_name.clone(),
						variant: res.variant.clone(),
					},
					None => HirExpr::Field {
						recv: Box::new(self.lower_expr(parent)),
						name: member.0.clone(),
					},
				}
			}
			ExprKind::Tuple(items) => HirExpr::Array(self.lower_items(items)),
			ExprKind::List(items) => HirExpr::Array(self.lower_items(items)),
			ExprKind::Map(entries) => HirExpr::MapLit(self.lower_map_entries(entries)),
			ExprKind::IndexAccess { parent, index, .. } => {
				// Dispatch on the receiver's recorded type: Map → get, else subscript.
				let recv = self.lower_expr(parent);
				let index = self.lower_expr(index);
				let recv_is_map = self
					.annotations
					.get(parent.id)
					.is_some_and(|info| matches!(self.interner.kind(info.ty), TyKind::Map(..)));
				if recv_is_map {
					HirExpr::MapGet {
						recv: Box::new(recv),
						key: Box::new(index),
					}
				} else {
					HirExpr::Index {
						recv: Box::new(recv),
						index: Box::new(index),
					}
				}
			}
			ExprKind::BinaryOp { lhs, op, rhs } => self.lower_binary(expr.id, lhs, *op, rhs),
			ExprKind::PrefixOp { op, value } => self.lower_prefix_op(expr.id, *op, value),
			ExprKind::AssignOp { lhs, op, rhs } => {
				// A compound assignment `a op= b` desugars to `a = a op b`, dispatched
				// per its recorded `Resolution` just like a `BinaryOp` node (Finding 1);
				// a plain `=` (or `~=`, which has no binary form) assigns the value
				// directly, with no operator resolution involved.
				let value = match assign_binop(*op) {
					None => self.lower_expr(rhs),
					Some(binop) => {
						// The lhs would otherwise be lowered twice here: once as the
						// operator's own operand (via `lower_operator`, mirroring `a op
						// b`), once as the `Assign` target below. That's only safe for an
						// identifier target (re-reading a plain local has no side effect);
						// codegen only supports `HirExpr::Local` assignment targets anyway
						// (see the `unreachable!` in `emit.rs`), so panic here — loudly,
						// with a clearer message — rather than let a field/index target
						// silently double-evaluate its receiver chain. When field/index
						// compound-assign targets land, they'll need a hoisted receiver
						// temp (`let $t = a.b; $t.x = $t.x.plus(v)`).
						if !matches!(lhs.kind, ExprKind::Identifier(_)) {
							panic!(
								"slice-4b lowering: compound-assign targets must be identifiers (got {:?})",
								lhs.kind
							);
						}
						self.lower_operator(expr.id, binop, lhs, rhs, || {
							format!(
								"slice-4b lowering: no operator resolution recorded for compound assign {op:?}"
							)
						})
					}
				};
				HirExpr::Assign {
					target: Box::new(self.lower_expr(lhs)),
					value: Box::new(value),
				}
			}
			// Any block reached HERE (generically, via an ordinary subexpression
			// position — if/else branches, a while body, a match arm body, or a
			// plain nested `{ .. }`) is a genuinely separate JS scope from its
			// enclosing one (emit wraps each in its own `BlockStatement`/IIFE), so it
			// pushes its own scope — unlike a function/method's OWN body block,
			// which `lower_func_body` lowers directly via `lower_block(_, false)`
			// into the scope its params already seeded (Slice 4E, Y2).
			ExprKind::Block { body, .. } => self.lower_block(body, true),
			ExprKind::Return { value: _, label } => {
				// `return` is statement-flavored (`HirStmt::Return`); reaching it HERE
				// means it showed up in genuine expression position — an unbraced
				// match-arm body, an if/let-init operand, etc. — which lowering has no
				// representation for. `lower_block` intercepts every `Statement::Expr`
				// wrapping a `Return` before it ever reaches `lower_expr`, so the only
				// way here is a subexpression position; panic loudly rather than
				// silently drop or misplace it (Slice 4E, Y1).
				assert!(
					label.is_none(),
					"slice-4e lowering does not yet support labeled `return`"
				);
				panic!(
					"slice-4e lowering: `return` is only supported in statement position (inside a block), not as a subexpression"
				);
			}
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => HirExpr::If {
				cond: Box::new(self.lower_expr(condition)),
				then: Box::new(self.lower_branch(then)),
				otherwise: otherwise.as_ref().map(|e| Box::new(self.lower_branch(e))),
			},
			ExprKind::While {
				condition, body, ..
			} => HirExpr::While {
				cond: Box::new(self.lower_expr(condition)),
				body: Box::new(self.lower_branch(body)),
			},
			ExprKind::Match { value, arms } => {
				let scrutinee = Box::new(self.lower_expr(value));
				let arms = arms
					.iter()
					.map(|arm| {
						// One JS scope per arm covering its pattern binds, guard, and body
						// together (Slice 4E, Y2) — a conservative merge: emit actually
						// nests the arm body's own block one level deeper than the
						// pattern-bind block (see `match_arm` in emit.rs), so a body `let`
						// reusing a pattern-bound name is legal JS shadowing that doesn't
						// strictly need a rename, but treating them as one scope is safe
						// (just occasionally renames when it didn't have to) and far
						// simpler than mirroring emit's exact nested-block shape here.
						self.push_scope();
						let pat = self.lower_pattern(&arm.pattern);
						let guard = arm.guard.as_ref().map(|g| self.lower_expr(g));
						let body = self.lower_expr(&arm.body);
						self.pop_scope();
						HirArm { pat, guard, body }
					})
					.collect();
				HirExpr::Match { scrutinee, arms }
			}
			other => panic!("slice-2a lowering does not yet handle {other:?}"),
		}
	}

	/// Lower an `if`/`while` branch expression (`then`/`otherwise`/`body`),
	/// special-casing a directly-unbraced `return` (Slice 4E, Y1 follow-up): the
	/// parser accepts a bare `return` as the whole then-branch/while-body with no
	/// surrounding `{ .. }` (unbraced if/while branches are ordinary expression
	/// positions — see the parser's `control_flow_expressions` tests), but
	/// `lower_block`'s statement-level interception only ever sees a `Return`
	/// that is itself a full statement of SOME block's own statement list. An
	/// unbraced branch never reaches `lower_block` at all, so without this it
	/// falls through to `lower_expr`'s subexpression-position `Return` arm and
	/// panics unconditionally, even though the corpus's already-supported braced
	/// shape (`if (cond) { return n }`) lowers this exact same branch fine.
	/// Wrapping it in a single-statement `Block` (mirroring what `lower_block`
	/// already produces for the braced form) makes the two shapes lower
	/// identically. This does NOT relax the Y1 scope guard: emit's
	/// `in_iife_subexpr` check still panics loudly if the enclosing if/while
	/// itself ends up in a genuine subexpression (IIFE-wrapped) position — that
	/// check is orthogonal to how the branch was lowered.
	fn lower_branch(&self, e: &Expr) -> HirExpr {
		if let ExprKind::Return { value, label } = &e.kind {
			assert!(
				label.is_none(),
				"slice-4e lowering does not yet support labeled `return`"
			);
			let value = value.as_ref().map(|v| self.lower_expr(v));
			HirExpr::Block {
				stmts: vec![HirStmt::Return(value)],
				tail: None,
			}
		} else {
			self.lower_expr(e)
		}
	}

	/// Lower a `BinaryOp` node per its recorded [`crate::Resolution`] (Slice 4B, D4).
	/// Thin wrapper over [`Self::lower_operator`] that just picks the native `BinOp`
	/// and the panic message for an unresolved node; see that method for the actual
	/// dispatch (shared with compound-assignment lowering, Finding 1).
	fn lower_binary(
		&self,
		id: nymph_ast::NodeId,
		lhs: &Expr,
		op: BinaryOperator,
		rhs: &Expr,
	) -> HirExpr {
		self.lower_operator(id, lower_binop(op), lhs, rhs, || {
			format!("slice-4b lowering: no operator resolution recorded for binary op {op:?}")
		})
	}

	/// Lower an operator-shaped node — a `BinaryOp`, or the desugared `place op
	/// value` inside a compound assignment (Finding 1) — per its recorded
	/// [`crate::Resolution`] (Slice 4B, D4). `BuiltinEager`/`BuiltinShortCircuit`
	/// keep the existing native-JS `HirExpr::Binary` path (`native` supplies the
	/// operator for it); `UserImpl` dispatches to a method call on the lhs
	/// (`lhs.method(rhs)`, mirroring how method calls elsewhere in this file lower
	/// to `Call { callee: Field { .. }, .. }`). `UserImplDefaultMethod` and a missing
	/// resolution both panic loudly — codegen cannot yet materialize interface
	/// default methods, and an unresolved node is a checker bug we want to see
	/// immediately rather than silently miscompile. `missing_resolution_msg` lets
	/// each call site name its own AST shape in that last panic.
	fn lower_operator(
		&self,
		id: nymph_ast::NodeId,
		native: BinOp,
		lhs: &Expr,
		rhs: &Expr,
		missing_resolution_msg: impl FnOnce() -> String,
	) -> HirExpr {
		match self.annotations.resolution_of(id) {
			Some(res)
				if matches!(
					res.dispatch,
					DispatchKind::BuiltinEager | DispatchKind::BuiltinShortCircuit
				) =>
			{
				HirExpr::Binary {
					op: native,
					lhs: Box::new(self.lower_expr(lhs)),
					rhs: Box::new(self.lower_expr(rhs)),
				}
			}
			Some(res) if res.dispatch == DispatchKind::UserImpl => HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(self.lower_expr(lhs)),
					name: res.method.clone(),
				}),
				args: vec![self.lower_expr(rhs)],
			},
			Some(res) => panic!(
				"slice-4b lowering does not yet dispatch operator to interface default method {}",
				res.method
			),
			None => panic!("{}", missing_resolution_msg()),
		}
	}

	/// Lower a `PrefixOp` node per its recorded [`crate::Resolution`] (Slice 4C-a,
	/// U3) — the unary counterpart of [`Self::lower_operator`]. `BuiltinEager` keeps
	/// the existing native-JS `HirExpr::Unary` path (`lower_prefix` supplies the
	/// operator for it); `UserImpl` dispatches to a zero-argument method call on the
	/// operand (`value.method()`, mirroring `lower_operator`'s `lhs.method(rhs)`).
	/// `UserImplDefaultMethod`, `BuiltinShortCircuit` (never produced for a unary
	/// operator — `&&`/`||` are the only short-circuiting operators and both are
	/// binary), and a missing resolution all panic loudly — codegen cannot yet
	/// materialize interface default methods, and an unresolved node is a checker
	/// bug we want to see immediately rather than silently miscompile.
	fn lower_prefix_op(&self, id: nymph_ast::NodeId, op: PrefixOperator, value: &Expr) -> HirExpr {
		match self.annotations.resolution_of(id) {
			Some(res) if res.dispatch == DispatchKind::BuiltinEager => HirExpr::Unary {
				op: lower_prefix(op),
				operand: Box::new(self.lower_expr(value)),
			},
			Some(res) if res.dispatch == DispatchKind::UserImpl => HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(self.lower_expr(value)),
					name: res.method.clone(),
				}),
				args: vec![],
			},
			Some(res) if res.dispatch == DispatchKind::BuiltinShortCircuit => panic!(
				"slice-4c lowering: BuiltinShortCircuit is unreachable for a prefix operator (method {})",
				res.method
			),
			Some(res) => panic!(
				"slice-4c lowering does not yet dispatch operator to interface default method {}",
				res.method
			),
			None => panic!("slice-4c lowering: no operator resolution recorded for prefix op {op:?}"),
		}
	}

	/// Lower an AST pattern into a `HirPat`. 3B handles the full pattern surface:
	/// scalar/string literals, bindings, placeholders, variant/struct/tuple/list/map/
	/// range/union patterns. Deferred edges panic loudly: map-rest, non-literal map
	/// keys, interpolated/escaped string patterns.
	fn lower_pattern(&self, pat: &nymph_ast::Spanned<nymph_ast::expr::Pattern>) -> HirPat {
		use nymph_ast::expr::{ListPatternEntry, Pattern};
		match &pat.0 {
			Pattern::Placeholder => HirPat::Wildcard,
			Pattern::Int(v) => HirPat::Lit(HirLit::Num(v.0 as f64)),
			Pattern::UInt(v) => HirPat::Lit(HirLit::Num(v.0 as f64)),
			Pattern::Float(v) => HirPat::Lit(HirLit::Num(v.0.into_inner())),
			Pattern::Boolean(b) => HirPat::Lit(HirLit::Bool(b.0)),
			Pattern::Char(c) => HirPat::Lit(HirLit::Char(c.0)),
			Pattern::String(parts) => HirPat::Lit(HirLit::Str(lower_string_pattern(parts))),
			Pattern::Grouped(inner) => self.lower_pattern(inner),
			Pattern::Binding { name, inner } => {
				// A bare name recorded as a variant is a nullary variant pattern; else a
				// binding, optionally with a sub-pattern.
				if let Some(res) = self.annotations.pattern_variant_of(pat.1) {
					HirPat::Variant {
						enum_name: res.enum_name.clone(),
						variant: res.variant.clone(),
						fields: Vec::new(),
					}
				} else {
					let sub = match &inner.0 {
						Pattern::Placeholder => None,
						_ => Some(Box::new(self.lower_pattern(inner))),
					};
					HirPat::Binding {
						name: self.declare(&name.0),
						sub,
					}
				}
			}
			Pattern::Struct { fields, .. } => {
				let lowered = self.lower_struct_fields(fields);
				// A `Pattern::Struct` recorded as a variant is a variant pattern; otherwise
				// it is a struct pattern (irrefutable, binds fields only).
				match self.annotations.pattern_variant_of(pat.1) {
					Some(res) => HirPat::Variant {
						enum_name: res.enum_name.clone(),
						variant: res.variant.clone(),
						fields: lowered,
					},
					None => HirPat::Struct { fields: lowered },
				}
			}
			Pattern::Tuple(entries) => HirPat::Tuple(self.lower_pattern_items(entries)),
			Pattern::List(entries) => {
				let mut prefix = Vec::new();
				let mut suffix = Vec::new();
				let mut rest: Option<Option<ecow::EcoString>> = None;
				for entry in entries {
					match &entry.0 {
						ListPatternEntry::Item(p) => {
							if rest.is_none() {
								prefix.push(self.lower_pattern(p));
							} else {
								suffix.push(self.lower_pattern(p));
							}
						}
						ListPatternEntry::Rest(name) => {
							assert!(rest.is_none(), "list pattern has at most one `...` rest");
							rest = Some(name.as_ref().map(|n| self.declare(&n.0)));
						}
					}
				}
				HirPat::List {
					prefix,
					rest,
					suffix,
				}
			}
			Pattern::Map(entries) => {
				use nymph_ast::expr::MapPatternEntry;
				let lowered = entries
					.iter()
					.map(|entry| match &entry.0 {
						MapPatternEntry::Entry(k, v) => (lower_lit_pattern(k), self.lower_pattern(v)),
						MapPatternEntry::Rest(_) => {
							panic!("slice-3b lowering does not yet handle map-pattern rest")
						}
					})
					.collect();
				HirPat::Map(lowered)
			}
			Pattern::Range(kind) => HirPat::Range(lower_range_pattern(kind)),
			Pattern::Union(a, b) => {
				// A union whose sides bind would need cross-branch consistent-name analysis
				// (which the checker doesn't yet do); 3B rejects it here rather than in
				// codegen so the failure is a clear lowering panic like every other deferral.
				let a = self.lower_pattern(a);
				let b = self.lower_pattern(b);
				assert!(
					!pat_binds(&a) && !pat_binds(&b),
					"slice-3b lowering does not yet handle union patterns that bind"
				);
				HirPat::Or(Box::new(a), Box::new(b))
			}
		}
	}

	/// Lower a struct/variant pattern's fields into `(name, sub-pattern)` pairs.
	fn lower_struct_fields(
		&self,
		fields: &[nymph_ast::Spanned<nymph_ast::expr::StructPatternField>],
	) -> Vec<(ecow::EcoString, HirPat)> {
		use nymph_ast::expr::StructPatternField;
		fields
			.iter()
			.filter_map(|f| match &f.0 {
				StructPatternField::Value { name, value } => {
					Some((name.0.clone(), self.lower_pattern(value)))
				}
				StructPatternField::Named(name) => Some((
					name.0.clone(),
					HirPat::Binding {
						name: self.declare(&name.0),
						sub: None,
					},
				)),
				StructPatternField::Rest => None,
			})
			.collect()
	}

	/// Lower tuple-pattern items (no rest allowed in a tuple).
	fn lower_pattern_items(
		&self,
		entries: &[nymph_ast::Spanned<nymph_ast::expr::ListPatternEntry>],
	) -> Vec<HirPat> {
		use nymph_ast::expr::ListPatternEntry;
		entries
			.iter()
			.map(|entry| match &entry.0 {
				ListPatternEntry::Item(p) => self.lower_pattern(p),
				ListPatternEntry::Rest(_) => panic!("slice-3b lowering does not handle tuple rest"),
			})
			.collect()
	}

	/// If the checker resolved node `id` to a variant, lower a construction call to
	/// `VariantNew`. 2C supports labeled fields only (positional deferred). Returns
	/// `None` when the node is not a variant construction (an ordinary call/struct).
	fn variant_new(
		&self,
		id: nymph_ast::NodeId,
		args: &[nymph_ast::Spanned<CallArg>],
	) -> Option<HirExpr> {
		let res = self.annotations.variant_of(id)?;
		let fields = args
			.iter()
			.map(|a| {
				let label = a
					.0
					.name
					.as_ref()
					.unwrap_or_else(|| panic!("slice-2c variant construction requires labeled fields"));
				(label.0.clone(), self.lower_expr(&a.0.value))
			})
			.collect();
		Some(HirExpr::VariantNew {
			enum_name: res.enum_name.clone(),
			variant: res.variant.clone(),
			fields,
		})
	}

	/// Lower a list/tuple literal's items. 2A does not yet support spread elements.
	fn lower_items(&self, items: &[nymph_ast::Spanned<ListItem>]) -> Vec<HirExpr> {
		items
			.iter()
			.map(|item| match &item.0 {
				ListItem::Expr(e) => self.lower_expr(e),
				ListItem::Spread(_) => panic!("slice-2a lowering does not yet handle spread list items"),
			})
			.collect()
	}

	/// Lower a map literal's entries. 2A does not yet support spread entries.
	fn lower_map_entries(&self, entries: &[nymph_ast::Spanned<MapEntry>]) -> Vec<(HirExpr, HirExpr)> {
		entries
			.iter()
			.map(|entry| match &entry.0 {
				MapEntry::Entry(k, v) => (self.lower_expr(k), self.lower_expr(v)),
				MapEntry::Spread(_) => panic!("slice-2a lowering does not yet handle spread map entries"),
			})
			.collect()
	}

	/// Lower a block's statements. `new_scope` selects whether this call pushes
	/// its OWN JS scope (every ordinary nested block) or lowers directly into
	/// the caller's already-pushed scope (a function/method's own body block,
	/// merged with its params by [`Self::lower_func_body`] — Slice 4E, Y2).
	fn lower_block(&self, body: &[nymph_ast::Spanned<Statement>], new_scope: bool) -> HirExpr {
		if new_scope {
			self.push_scope();
		}
		let mut stmts = Vec::new();
		let mut tail = None;
		for (i, stmt) in body.iter().enumerate() {
			let is_last = i + 1 == body.len();
			match &stmt.0 {
				Statement::Let { meta, value } => {
					// The value lowers (and resolves its own identifiers) against the
					// PRIOR binding for this name, before `declare` registers the new
					// one — `let x = x + 1` must read the old `x` on its right-hand
					// side (Slice 4E, Y2).
					let name = param_name(&meta.name);
					let value = self.lower_expr(value);
					let name = self.declare(&name);
					stmts.push(HirStmt::Let {
						name,
						mutable: meta.mutable,
						value,
					});
				}
				// `return` is statement-flavored regardless of source position (last
				// statement or not): it never becomes a block's tail EXPRESSION, even
				// when it's the block's last statement — the exact corpus shape (an
				// if-branch block whose only statement is `return n`), since emit has
				// no way to represent "return" as a value (Slice 4E, Y1).
				Statement::Expr(e) if matches!(e.kind, ExprKind::Return { .. }) => {
					let ExprKind::Return { value, label } = &e.kind else {
						unreachable!("matched above");
					};
					assert!(
						label.is_none(),
						"slice-4e lowering does not yet support labeled `return`"
					);
					let value = value.as_ref().map(|v| self.lower_expr(v));
					stmts.push(HirStmt::Return(value));
				}
				Statement::Expr(e) => {
					if is_last {
						tail = Some(Box::new(self.lower_expr(e)));
					} else {
						stmts.push(HirStmt::Expr(self.lower_expr(e)));
					}
				}
			}
		}
		if new_scope {
			self.pop_scope();
		}
		HirExpr::Block { stmts, tail }
	}
}

/// Collect every `HirExpr::Local` name referenced anywhere within `expr` into
/// `out` (Slice 4E, Y3 module-let dependency analysis) — used to find, for a
/// top-level `let`'s initializer or a function's body, every top-level
/// `let`/`func` name it touches. Unfiltered (collects ALL locals, not just
/// ones known to be top-level lets/funcs); callers intersect against the
/// relevant name sets. A generic structural walk over every `HirExpr`/
/// `HirStmt` shape — kept exhaustive (no wildcard arm) so a future HIR
/// addition that can reference a `Local` doesn't silently fall through
/// unanalyzed.
fn collect_locals(expr: &HirExpr, out: &mut FxHashSet<EcoString>) {
	match expr {
		HirExpr::Local(name) => {
			out.insert(name.clone());
		}
		HirExpr::Num(_)
		| HirExpr::Str(_)
		| HirExpr::Bool(_)
		| HirExpr::Char(_)
		| HirExpr::This
		| HirExpr::VariantRef { .. } => {}
		HirExpr::Call { callee, args } => {
			collect_locals(callee, out);
			for a in args {
				collect_locals(a, out);
			}
		}
		HirExpr::Array(items) => {
			for item in items {
				collect_locals(item, out);
			}
		}
		HirExpr::MapLit(pairs) => {
			for (k, v) in pairs {
				collect_locals(k, out);
				collect_locals(v, out);
			}
		}
		HirExpr::Index { recv, index } => {
			collect_locals(recv, out);
			collect_locals(index, out);
		}
		HirExpr::MapGet { recv, key } => {
			collect_locals(recv, out);
			collect_locals(key, out);
		}
		HirExpr::New { fields, .. } | HirExpr::VariantNew { fields, .. } => {
			for (_, v) in fields {
				collect_locals(v, out);
			}
		}
		HirExpr::Field { recv, .. } => collect_locals(recv, out),
		HirExpr::Binary { lhs, rhs, .. } => {
			collect_locals(lhs, out);
			collect_locals(rhs, out);
		}
		HirExpr::Unary { operand, .. } => collect_locals(operand, out),
		HirExpr::Assign { target, value } => {
			collect_locals(target, out);
			collect_locals(value, out);
		}
		HirExpr::Block { stmts, tail } => {
			for stmt in stmts {
				collect_locals_stmt(stmt, out);
			}
			if let Some(t) = tail {
				collect_locals(t, out);
			}
		}
		HirExpr::If {
			cond,
			then,
			otherwise,
		} => {
			collect_locals(cond, out);
			collect_locals(then, out);
			if let Some(o) = otherwise {
				collect_locals(o, out);
			}
		}
		HirExpr::While { cond, body } => {
			collect_locals(cond, out);
			collect_locals(body, out);
		}
		HirExpr::Match { scrutinee, arms } => {
			collect_locals(scrutinee, out);
			for arm in arms {
				if let Some(g) = &arm.guard {
					collect_locals(g, out);
				}
				collect_locals(&arm.body, out);
			}
		}
	}
}

/// The `HirStmt` counterpart of [`collect_locals`].
fn collect_locals_stmt(stmt: &HirStmt, out: &mut FxHashSet<EcoString>) {
	match stmt {
		HirStmt::Let { value, .. } => collect_locals(value, out),
		HirStmt::Expr(e) => collect_locals(e, out),
		HirStmt::Return(v) => {
			if let Some(v) = v {
				collect_locals(v, out);
			}
		}
	}
}

/// Reorder top-level `let`s into a valid module-init order (Slice 4E, Y3 fix):
/// each `let` must be emitted after every OTHER top-level `let` its own
/// initializer needs at module-init time — either by directly naming it, or
/// by calling a function that (transitively) reads it. JS module-scope
/// `const`/`let` is TDZ (unlike a hoisted `function` declaration), so naive
/// source-order emission can throw `ReferenceError: Cannot access '<x>'
/// before initialization` when a let references a LATER let, directly or
/// through a function-call chain. Ties (no dependency either way) keep
/// source order — the reordering is a stable, minimal-movement pass, not an
/// arbitrary topological sort. A genuine cycle between top-level lets (`let a
/// = b + 1; let b = a + 1;`) has no valid JS order at all — panic loudly
/// rather than silently pick one and let it throw at runtime instead.
fn reorder_lets_by_dependency(lets: Vec<HirLet>, funcs: &[HirFunc]) -> Vec<HirLet> {
	let let_names: FxHashSet<EcoString> = lets.iter().map(|l| l.name.clone()).collect();
	let func_names: FxHashSet<EcoString> = funcs.iter().map(|f| f.name.clone()).collect();

	// Each function's DIRECT top-level-let references and DIRECT calls to other
	// top-level functions — one flat pass over each body, no recursion-guard
	// subtleties. (A function's OWN direct locals may name lets, other funcs,
	// or both; split here so the fixpoint below only ever has to union sets,
	// never re-walk a body.)
	let mut direct_lets: FxHashMap<EcoString, FxHashSet<EcoString>> = FxHashMap::default();
	let mut direct_calls: FxHashMap<EcoString, FxHashSet<EcoString>> = FxHashMap::default();
	for f in funcs {
		let mut refs = FxHashSet::default();
		collect_locals(&f.body, &mut refs);
		let lets_here = refs
			.iter()
			.filter(|n| let_names.contains(*n))
			.cloned()
			.collect();
		let calls_here = refs
			.iter()
			.filter(|n| func_names.contains(*n) && *n != &f.name)
			.cloned()
			.collect();
		direct_lets.insert(f.name.clone(), lets_here);
		direct_calls.insert(f.name.clone(), calls_here);
	}

	// Resolve each function's TRANSITIVE top-level-let dependencies via a
	// WORKLIST FIXPOINT over the function call graph, rather than a
	// memoized DFS with an `in_progress` recursion guard. The DFS approach is
	// unsound under MUTUAL recursion: when `f` calls `g` and `g` calls back to
	// `f`, the guard trips on the back-edge (`f` is already being resolved
	// higher up the same call chain), that edge contributes `{}`, and — the
	// real bug — `f`'s result is then PERMANENTLY memoized with that
	// incomplete set, even though `g`'s own deps (discovered moments later)
	// were never folded back in.
	//
	// The fixpoint sidesteps this entirely: seed every function's dep set
	// with just its own direct let-refs, then repeatedly union in every
	// callee's CURRENT dep set until a full pass changes nothing. Sets only
	// ever grow (monotonic), so this always terminates, and a whole call
	// cycle naturally converges to the union of every function on it — no
	// edge is ever finalized early. The call graph here is tiny, so an
	// O(n^2)-ish number of passes is a non-issue.
	let mut resolved: FxHashMap<EcoString, FxHashSet<EcoString>> = direct_lets.clone();
	loop {
		let mut changed = false;
		for name in &func_names {
			let callee_deps: FxHashSet<EcoString> = direct_calls[name]
				.iter()
				.flat_map(|callee| resolved.get(callee).cloned().unwrap_or_default())
				.collect();
			let entry = resolved.entry(name.clone()).or_default();
			for d in callee_deps {
				changed |= entry.insert(d);
			}
		}
		if !changed {
			break;
		}
	}

	// Each let's dependency set on OTHER top-level lets: direct references plus,
	// for any function it calls/reads, that function's resolved transitive
	// let-dependencies.
	let mut deps: FxHashMap<EcoString, FxHashSet<EcoString>> = FxHashMap::default();
	for l in &lets {
		let mut direct = FxHashSet::default();
		collect_locals(&l.value, &mut direct);
		let mut out = FxHashSet::default();
		for n in &direct {
			if let_names.contains(n) {
				if n != &l.name {
					out.insert(n.clone());
				}
			} else if let Some(fdeps) = resolved.get(n) {
				out.extend(fdeps.iter().filter(|d| *d != &l.name).cloned());
			}
		}
		deps.insert(l.name.clone(), out);
	}

	// Kahn's algorithm: repeatedly emit the first remaining (source-order) let
	// whose dependencies are all already emitted — a stable topological sort
	// that reduces to plain source order whenever no reordering is needed.
	let mut remaining: Vec<HirLet> = lets;
	let mut emitted_names: FxHashSet<EcoString> = FxHashSet::default();
	let mut ordered: Vec<HirLet> = Vec::with_capacity(remaining.len());
	while !remaining.is_empty() {
		let ready = remaining.iter().position(|l| {
			deps
				.get(&l.name)
				.map(|d| d.iter().all(|dep| emitted_names.contains(dep)))
				.unwrap_or(true)
		});
		let Some(idx) = ready else {
			let names: Vec<&str> = remaining.iter().map(|l| l.name.as_str()).collect();
			panic!(
				"slice-4e lowering: circular top-level `let` dependency among {names:?} — no valid module-init order exists"
			);
		};
		let l = remaining.remove(idx);
		emitted_names.insert(l.name.clone());
		ordered.push(l);
	}
	ordered
}

/// The bound name of a simple parameter pattern. Slice 1 supports plain-identifier
/// parameters; destructuring parameters arrive with pattern lowering (Slice 3).
fn param_name(pattern: &nymph_ast::Spanned<nymph_ast::expr::Pattern>) -> ecow::EcoString {
	match &pattern.0 {
		nymph_ast::expr::Pattern::Binding { name, .. } => name.0.clone(),
		other => panic!("slice-1 lowering supports only identifier params, got {other:?}"),
	}
}

/// Lower a literal pattern to a `HirLit` (for map keys and range bounds). Panics on
/// a non-literal pattern (3B only supports literal keys/bounds).
fn lower_lit_pattern(pat: &nymph_ast::Spanned<nymph_ast::expr::Pattern>) -> HirLit {
	use nymph_ast::expr::Pattern;
	match &pat.0 {
		Pattern::Int(v) => HirLit::Num(v.0 as f64),
		Pattern::UInt(v) => HirLit::Num(v.0 as f64),
		Pattern::Float(v) => HirLit::Num(v.0.into_inner()),
		Pattern::Boolean(b) => HirLit::Bool(b.0),
		Pattern::Char(c) => HirLit::Char(c.0),
		Pattern::String(parts) => HirLit::Str(lower_string_pattern(parts)),
		Pattern::Grouped(inner) => lower_lit_pattern(inner),
		other => panic!("slice-3b expects a literal pattern (map key / range bound), got {other:?}"),
	}
}

/// Whether a lowered pattern introduces any binding — used to reject binding
/// unions, which 3B does not support.
fn pat_binds(pat: &HirPat) -> bool {
	match pat {
		HirPat::Wildcard | HirPat::Lit(_) | HirPat::Range(_) => false,
		HirPat::Binding { .. } => true,
		HirPat::Variant { fields, .. } | HirPat::Struct { fields } => {
			fields.iter().any(|(_, p)| pat_binds(p))
		}
		HirPat::Tuple(ps) => ps.iter().any(pat_binds),
		HirPat::List {
			prefix,
			rest,
			suffix,
		} => matches!(rest, Some(Some(_))) || prefix.iter().chain(suffix).any(pat_binds),
		HirPat::Map(entries) => entries.iter().any(|(_, p)| pat_binds(p)),
		HirPat::Or(a, b) => pat_binds(a) || pat_binds(b),
	}
}

/// Concatenate a string pattern's text parts. 3B string patterns are text-only.
fn lower_string_pattern(
	parts: &[nymph_ast::Spanned<nymph_ast::expr::StringPatternPart>],
) -> ecow::EcoString {
	use nymph_ast::expr::StringPatternPart;
	let mut s = ecow::EcoString::new();
	for part in parts {
		match &part.0 {
			StringPatternPart::Text(t) => s.push_str(t),
			StringPatternPart::EscapeSequence(_) => {
				panic!("slice-3b string patterns are text-only (escapes not yet lowered)")
			}
		}
	}
	s
}

/// Lower a range pattern's bounds into a `HirRange`.
fn lower_range_pattern(kind: &nymph_ast::expr::RangePatternKind) -> HirRange {
	use nymph_ast::expr::RangePatternKind as R;
	match kind {
		R::From(p) => HirRange::From(lower_lit_pattern(p)),
		R::To(p) => HirRange::To(lower_lit_pattern(p)),
		R::ToInclusive(p) => HirRange::ToInclusive(lower_lit_pattern(p)),
		R::Exclusive { min, max } => HirRange::Exclusive {
			min: lower_lit_pattern(min),
			max: lower_lit_pattern(max),
		},
		R::Inclusive { min, max } => HirRange::Inclusive {
			min: lower_lit_pattern(min),
			max: lower_lit_pattern(max),
		},
	}
}

fn lower_binop(op: BinaryOperator) -> BinOp {
	use BinaryOperator as B;
	match op {
		B::Plus => BinOp::Add,
		B::Minus => BinOp::Sub,
		B::Times => BinOp::Mul,
		B::Divide => BinOp::Div,
		B::Remainder => BinOp::Rem,
		B::Power => BinOp::Pow,
		B::Equals => BinOp::Eq,
		B::NotEquals => BinOp::Ne,
		B::LessThan => BinOp::Lt,
		B::LessThanEquals => BinOp::Le,
		B::GreaterThan => BinOp::Gt,
		B::GreaterThanEquals => BinOp::Ge,
		B::BoolAnd => BinOp::And,
		B::BoolOr => BinOp::Or,
		B::BitAnd => BinOp::BitAnd,
		B::BitOr => BinOp::BitOr,
		B::BitXor => BinOp::BitXor,
		B::LeftShift => BinOp::Shl,
		B::RightShift => BinOp::Shr,
		other => panic!("slice-1 lowering does not yet handle operator {other:?}"),
	}
}

/// The binary operator a compound assignment desugars to, or `None` for a plain `=`.
fn assign_binop(op: AssignOperator) -> Option<BinOp> {
	use AssignOperator as A;
	Some(match op {
		A::Assign => return None,
		A::PlusAssign => BinOp::Add,
		A::MinusAssign => BinOp::Sub,
		A::TimesAssign => BinOp::Mul,
		A::DivideAssign => BinOp::Div,
		A::RemainderAssign => BinOp::Rem,
		A::PowerAssign => BinOp::Pow,
		A::LeftShiftAssign => BinOp::Shl,
		A::RightShiftAssign => BinOp::Shr,
		A::BitAndAssign => BinOp::BitAnd,
		A::BitXorAssign => BinOp::BitXor,
		A::BitOrAssign => BinOp::BitOr,
		A::BoolAndAssign => BinOp::And,
		A::BoolOrAssign => BinOp::Or,
		other => panic!("slice-1 lowering does not yet handle {other:?}"),
	})
}

fn lower_prefix(op: PrefixOperator) -> UnOp {
	match op {
		PrefixOperator::Negate => UnOp::Neg,
		PrefixOperator::BoolNot => UnOp::Not,
		PrefixOperator::BitNot => UnOp::BitNot,
	}
}
