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
	Ident, Span, Spanned,
	decl::{Declaration, FuncDeclaration, ImplMember, InterfaceElement, InterfaceMember, Module},
	expr::{CallArg, Expr, ExprKind, ListItem, MapEntry, Statement},
	ops::{AssignOperator, BinaryOperator, PatternOperator, PrefixOperator},
	ty::{GenericArg, GenericParam},
};
use nymph_hir::hir::{
	BinOp, BuiltinResult, HirArm, HirArrayElem, HirArrayKind, HirBoundDispatchCase,
	HirBoundDispatchTarget, HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirLit, HirMapElem,
	HirMethod, HirModule, HirPat, HirRange, HirStmt, HirVariant, NumKind, ScalarCastKind, UnOp,
};
use nymph_hir::ty::{Interner, Ty, TyKind};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::anon_closure::anon_param_name;
use crate::{Annotations, Checked, DispatchKind, IterMode, Resolution};

/// Interface name → (the interface's OWN generics, its member list). Generics
/// matter here, not just for lookup by name (Slice 4J, Task 1 Finding 3 fix):
/// `push_unoverridden_defaults` needs an interface default body's owner
/// generic scope (`iface_generics`), the exact scope
/// `check_interface_default_body` (members.rs) checked it against, to keep
/// [`Lowerer::is_current_generic`] faithful to what the checker actually
/// resolved a namespaced call against.
type InterfaceTable<'m> =
	FxHashMap<EcoString, (&'m [Spanned<GenericParam>], &'m [Spanned<InterfaceMember>])>;

/// Provenance for a prelude declaration's canonical runtime owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeOwner {
	/// A compiler-created core/runtime module, never an emitted graph module.
	Compiler(EcoString),
	/// A module proven to come from the resolved project/dependency graph.
	Project(EcoString),
}

impl RuntimeOwner {
	#[must_use]
	pub fn key(&self) -> &EcoString {
		match self {
			Self::Compiler(key) | Self::Project(key) => key,
		}
	}
}

/// Consumer declarations and the ambient prelude declarations their bodies demand.
pub struct LoweredHir {
	pub module: HirModule,
	pub prelude_runtime: HirModule,
	/// Canonical owner for every demanded runtime function. The function also
	/// remains in `prelude_runtime.funcs` for compatibility with flat lowering.
	pub runtime_func_owners: FxHashMap<EcoString, RuntimeOwner>,
}

impl LoweredHir {
	fn merged(mut self) -> HirModule {
		// Ambient lets cannot depend on consumer declarations, while consumer
		// initializers may reference them. Preserve the combined lowerer's safe
		// dependency order when compatibility callers request one module.
		self.prelude_runtime.lets.extend(self.module.lets);
		self.module.lets = self.prelude_runtime.lets;
		self.module.funcs.extend(self.prelude_runtime.funcs);
		self.module.classes.extend(self.prelude_runtime.classes);
		self.module.enums.extend(self.prelude_runtime.enums);
		self.module
	}
}

/// Lower a checked module into the code-generation HIR, consulting `checked`'s
/// annotations/interner for type-directed decisions (e.g. index-access dispatch).
/// No prelude is fed — `interfaces_by_name` (below) sees only `module`'s own
/// interfaces, exactly the pre-stdlib-lowering behavior.
pub fn lower_hir(module: &Module, checked: &Checked) -> HirModule {
	lower_hir_impl(module, &[], &[], 0, checked).merged()
}

/// Lower a checked module the SAME way [`check_module_with_prelude`]
/// (`crate::prelude`) checked it: `prelude` is the identical raw (un-offset)
/// slice passed to that call, re-offset here via [`crate::prelude::offset_module`]
/// — deterministic and byte-identical to the offset clone the checker combined
/// and annotated (pinned by `prelude::tests::a_single_prelude_module_keeps_the_original_base_offset`),
/// so every `NodeId`/`Span` this walk looks up in `checked.annotations` lands on
/// the exact entry the checker recorded.
///
/// This closes two gaps `lower_hir` alone leaves open once a program resolves
/// against the prelude (stdlib body lowering slice):
///
/// - **Gap (a)** — a user `impl <PreludeInterface> for <Type>` used to panic at
///   [`Lowerer::push_unoverridden_defaults`] ("impl references unknown
///   interface"): that lookup only ever saw `module`'s own interfaces, and a
///   prelude interface's declaration lives in the prelude tree, never `module`.
///   Feeding the offset prelude's own `interface` declarations into the same
///   `interfaces_by_name` table the lookup already consults fixes this
///   directly — for an interface with no default-bodied methods (`Plus`, and
///   every other arithmetic/bitwise/`Unwrap`/`Index`/`Into` interface) the fix
///   is complete on its own (the defaults loop just finds nothing to
///   lower). An interface WITH defaults (`Equals`, `Contains`,
///   `Comparable`) needs gap (a)'s other half too: a default body may
///   reference the prelude's own `Order` enum (`Comparable`'s `less_than` &
///   co.), which nothing yet emits — see `lower_demanded_runtime_enums`
///   for the demand-driven fix.
/// - **Gap (b)** — dispatch into a prelude-OWNED body (a primitive/blanket impl
///   or an interface default reached through one) tags `DispatchKind::
///   UserImplDefaultMethod` and used to always panic loudly (codegen had no
///   representation for a method that exists nowhere in the emitted program).
///   `Resolution::impl_span` (stdlib body lowering slice) now carries
///   forward exactly the span `impl_is_unmaterialized` already classified
///   `dispatch` from, letting lowering locate that specific impl inside this
///   same offset prelude tree and — for a body that is genuinely pure Nymph
///   (not `external`, not a blanket impl targeting an unresolved generic
///   parameter) — lower it as a demand-driven, receiver-as-first-param
///   top-level function (`try_lower_runtime_dispatch`) instead of
///   panicking. A body lowering still cannot compile (external/intrinsic
///   markers, a blanket impl, a still-generic bound) keeps panicking loudly —
///   silent wrong JS is never an acceptable alternative to a loud deferral.
pub fn lower_hir_with_prelude(module: &Module, prelude: &[Module], checked: &Checked) -> HirModule {
	// No `dep_start` boundary: every prelude module is treated as ambient `core`
	// (not an emitted dep), so a struct method still panics as before — the plain
	// callers (tests, the LSP) never compile a multi-module project graph.
	lower_hir_with_prelude_and_deps(module, prelude, prelude.len(), checked)
}

/// Like [`lower_hir_with_prelude`], but the `prelude` slice is `core ++ deps`
/// where `dep_start` is the index the (emitted) dependency modules begin at.
/// A dependency module's own class IS emitted (the project driver lowers+emits
/// every module in the graph), so a call to a dep STRUCT's method lowers as an
/// ordinary `recv.method(..)` call on that emitted class, rather than being
/// lowered — unlike a `core` struct (`Range`/…), which is never emitted.
pub fn lower_hir_with_prelude_and_deps(
	module: &Module,
	prelude: &[Module],
	dep_start: usize,
	checked: &Checked,
) -> HirModule {
	lower_hir_with_prelude_runtime_and_deps(module, prelude, dep_start, checked).merged()
}

/// Lower a consumer module while keeping demanded ambient prelude declarations
/// separate for canonical runtime emission.
pub fn lower_hir_with_prelude_runtime_and_deps(
	module: &Module,
	prelude: &[Module],
	dep_start: usize,
	checked: &Checked,
) -> LoweredHir {
	let owners = (0..prelude.len())
		.map(|index| RuntimeOwner::Compiler(EcoString::from(format!("$prelude${index}"))))
		.collect::<Vec<_>>();
	lower_hir_with_prelude_runtime_and_deps_with_owners(module, prelude, &owners, dep_start, checked)
}

/// Project-aware split lowering. `prelude_owners` is parallel to `prelude` and
/// records the module that canonically owns declarations demanded from it.
pub fn lower_hir_with_prelude_runtime_and_deps_with_owners(
	module: &Module,
	prelude: &[Module],
	prelude_owners: &[RuntimeOwner],
	dep_start: usize,
	checked: &Checked,
) -> LoweredHir {
	assert_eq!(prelude.len(), prelude_owners.len());
	let offset_prelude: Vec<Module> = prelude
		.iter()
		.enumerate()
		.map(|(index, p)| crate::prelude::offset_module(p, index))
		.collect();
	let dep_start = dep_start.min(offset_prelude.len());
	lower_hir_impl(module, &offset_prelude, prelude_owners, dep_start, checked)
}

fn lower_hir_impl(
	module: &Module,
	prelude_modules: &[Module],
	prelude_owners: &[RuntimeOwner],
	dep_start: usize,
	checked: &Checked,
) -> LoweredHir {
	// A call whose callee names a struct is construction, not an ordinary call.
	// Collect the module's struct names up front so `lower_expr` can dispatch on
	// them. This mirrors the checker's own dispatch: `infer_call` treats *any*
	// identifier resolving to a struct def as construction, before trying variant/
	// method/function resolution — so lowering stays consistent with checking.
	// Cross-module import binding (Slice IB1) flattens each imported module's
	// own decls alongside `module`'s as another `prelude_modules` entry (exactly
	// like the stdlib operator prelude) — an imported `Point(…)` call must lower
	// to `New` too, so `struct_names` unions every prelude module's own struct
	// names in with `module`'s, mirroring `interfaces_by_name`'s merge just below.
	let struct_names = module
		.members
		.iter()
		.chain(prelude_modules.iter().flat_map(|m| m.members.iter()))
		.filter_map(|decl| match decl {
			Declaration::Struct { name, .. } => Some(name.0.clone()),
			_ => None,
		})
		.collect();
	// Gap (a): feed the prelude's own `interface` declarations (if any — empty
	// for `lower_hir`'s no-prelude callers) into the SAME table `module`'s own
	// interfaces populate, so `push_unoverridden_defaults`'s by-name lookup
	// finds a prelude interface exactly as it already finds a user one. Built
	// once, up front, rather than per-`push_unoverridden_defaults`-call: every
	// consumer within one `lower_module` walk needs the identical combined view.
	let mut interfaces_by_name: InterfaceTable = FxHashMap::default();
	for m in prelude_modules.iter().chain(std::iter::once(module)) {
		for decl in &m.members {
			if let Declaration::Interface {
				name,
				generics,
				members,
				..
			} = decl
			{
				interfaces_by_name.insert(name.0.clone(), (generics.as_slice(), members.as_slice()));
			}
		}
	}
	// `variant_new`'s positional-argument fallback (the checker's own
	// `check_ctor_args` supports "by label when present else positionally" for
	// EVERY variant construction — a general, pre-existing checker feature this
	// lowering never exercised until stdlib body lowering reached
	// convert.nym's `impl<T, E> Result<T, E> { func ok() = .. Option.Some(value)
	// .. }`, whose positional `Option.Some(value)` a zero-diagnostic program is
	// perfectly entitled to write): every declared enum's variant → its field
	// names in SOURCE ORDER, over `module`'s own enums AND every prelude
	// module's (a positionally-constructed variant may belong to either).
	let mut variant_fields: FxHashMap<(EcoString, EcoString), Vec<EcoString>> = FxHashMap::default();
	for m in prelude_modules.iter().chain(std::iter::once(module)) {
		for decl in &m.members {
			if let Declaration::Enum { name, variants, .. } = decl {
				for v in variants {
					let fields: Vec<EcoString> = v.0.fields.iter().map(|f| f.0.name.0.clone()).collect();
					variant_fields.insert((name.0.clone(), v.0.name.0.clone()), fields);
				}
			}
		}
	}
	let lowerer = Lowerer {
		module,
		annotations: &checked.annotations,
		interner: &checked.interner,
		external_value_marshals: &checked.external_value_marshals,
		struct_names,
		variant_fields,
		prelude_modules,
		prelude_owners,
		emitted_dep_modules: &prelude_modules[dep_start..],
		interfaces_by_name,
		scopes: RefCell::new(Vec::new()),
		rename_counters: RefCell::new(FxHashMap::default()),
		pattern_declaration_records: RefCell::new(Vec::new()),
		pattern_declaration_reuse: RefCell::new(Vec::new()),
		generics_stack: RefCell::new(Vec::new()),
		closure_depth: std::cell::Cell::new(0),
		this_sub: RefCell::new(Vec::new()),
		lowering_runtime_sibling: RefCell::new(Vec::new()),
		runtime_funcs_seen: RefCell::new(FxHashMap::default()),
		runtime_func_demands: RefCell::new(Vec::new()),
		lowered_runtime_funcs: RefCell::new(Vec::new()),
		runtime_func_owners: RefCell::new(FxHashMap::default()),
		lowering_onto_runtime_owner: std::cell::Cell::new(0),
		current_runtime_owner_lowering: RefCell::new(Vec::new()),
		runtime_enum_method_demands: RefCell::new(FxHashMap::default()),
		lowered_runtime_enum_methods: RefCell::new(FxHashMap::default()),
		runtime_struct_demands: RefCell::new(FxHashSet::default()),
	};
	lowerer.lower_module(module)
}

/// Carries the checker's output through the recursive lowering walk.
struct Lowerer<'a> {
	module: &'a Module,
	annotations: &'a Annotations,
	interner: &'a Interner,
	external_value_marshals: &'a FxHashMap<nymph_ast::Span, nymph_hir::hir::MarshalKind>,
	struct_names: FxHashSet<EcoString>,
	/// (Enum name, variant name) → that variant's declared field names, in
	/// source order — every enum `module` or any prelude module declares,
	/// built once up front. [`Self::variant_new`]'s positional-argument
	/// fallback (an un-labeled `Some(value)`, mirroring the checker's own
	/// "by label when present else positionally" `check_ctor_args`) consults
	/// this to recover the field name a bare positional argument binds to.
	variant_fields: FxHashMap<(EcoString, EcoString), Vec<EcoString>>,
	/// The offset prelude modules `lower_hir_with_prelude` reconstructed
	/// (empty for plain `lower_hir`). Consulted by
	/// `try_lower_runtime_dispatch`/`lower_demanded_runtime_enums`
	/// to locate the AST of a prelude-origin impl/interface-default/enum a
	/// `DispatchKind::UserImplDefaultMethod`/`VariantRef` needs lowered.
	prelude_modules: &'a [Module],
	prelude_owners: &'a [RuntimeOwner],
	/// The subset of `prelude_modules` that are EMITTED dependency modules (a
	/// project's imported `std/…`/`@/…` modules), as opposed to the ambient
	/// `core` prelude (never emitted). A dep module's own class IS emitted by the
	/// driver, so `try_lower_runtime_dispatch` returns `OntoClass` (an
	/// ordinary `recv.method(..)` call) for a dep STRUCT's method — a `core`
	/// struct (`Range`/…) stays unlowerable (`None`), as before.
	emitted_dep_modules: &'a [Module],
	/// Interface name → (its own generics, its member list) — `module`'s own
	/// interfaces AND (stdlib body lowering slice, gap a) every
	/// prelude module's, built once up front in `lower_hir_impl`. See that
	/// function's doc comment for why merging the prelude in here is both
	/// necessary and, for a no-default interface, sufficient.
	interfaces_by_name: InterfaceTable<'a>,
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
	/// Declaration mappings recorded while lowering the left side of a union,
	/// then reused by its right side. Sema guarantees both alternatives bind the
	/// same source names, so they must also share one emitted JS binding name.
	pattern_declaration_records: RefCell<Vec<FxHashMap<EcoString, EcoString>>>,
	pattern_declaration_reuse: RefCell<Vec<FxHashMap<EcoString, EcoString>>>,
	/// The enclosing func/method's OWN generic type-parameter names, pushed
	/// while lowering its body (Slice 4J, Task 1 Finding 3). Lowering is
	/// type-free and has no representation for a namespaced call resolved
	/// through a generic bound (`func make<T: Default>(): T = T.default()`,
	/// checked via `resolve_param_namespaced`) — `T` is a type parameter with no
	/// JS binding at all, so emitting `T.default()` verbatim would silently
	/// produce a `ReferenceError` (or worse, a runtime binding this compiler
	/// never intended) instead of the panic this compiler otherwise always
	/// gives for an unhandled construct. This stack lets the `Call` arm compare
	/// a `MemberAccess` parent identifier against the CURRENT func/method's own
	/// generics and panic loudly instead. One entry per func/method body (not
	/// merged with `scopes`, which is JS-scope/shadowing bookkeeping, not
	/// type-parameter bookkeeping).
	generics_stack: RefCell<Vec<FxHashSet<EcoString>>>,
	/// Depth of closure-body nesting currently being lowered (Slice 4L, JJ2).
	/// Zero everywhere except while lowering a `HirExpr::Closure`'s `body`. The
	/// checker types a closure body's `return` against the ENCLOSING
	/// function's `ret_ty`, not the closure's own inferred signature (it never
	/// touches `self.ret_ty` in `infer_closure`/`check_closure`), and probing
	/// confirms this accepts type-unsound programs an arrow-emitted `return`
	/// would silently miscompile (e.g. a closure inferred `(bool) -> bool`
	/// whose body does `if (b) { return 1 } true` typechecks with zero
	/// diagnostics against an enclosing `int`-returning function). Rather than
	/// give closures their own `return` target (which the checker's types
	/// don't actually support), every `return` sink checks this counter and
	/// panics loudly instead of ever emitting an arrow-scoped `return`.
	closure_depth: std::cell::Cell<u32>,
	/// Stack of receiver-param names currently substituting for `this` while
	/// lowering a prelude body as a top-level mangled function (stdlib body
	/// lowering slice, gap b) — the innermost entry is what
	/// `ExprKind::This` lowers to instead of `HirExpr::This` (which, as a
	/// top-level function's `this`, would be `undefined`/global — silent
	/// wrong JS). A stack, not a single `Cell<Option<_>>`, because a
	/// lowered default body can itself call another lowered body
	/// (e.g. `Comparable`'s `less_than` default calling `this.compare_to(o)`)
	/// — each nested lowering pushes its OWN receiver name and pops it
	/// on the way out, exactly like `generics_stack`/`closure_depth`. Empty
	/// everywhere else, including while lowering an ordinary inherent/impl
	/// method (`this` there stays `HirExpr::This`, unaffected).
	this_sub: RefCell<Vec<EcoString>>,
	/// Stack of Gap 2 sibling-dispatch frames — the innermost entry is the
	/// `(interface, tag, impl members/generics)` context of the `ImplFor`
	/// default body [`Self::lower_runtime_func`] is CURRENTLY lowering as a
	/// top-level mangled function, pushed/popped in lockstep with `this_sub`
	/// (paired 1:1 whenever `RuntimeFuncDemand::sibling_frame` is `Some`).
	///
	/// Needed because the checker types every interface default body exactly
	/// ONCE, generically, against a rigid synthetic `this` owned by the
	/// INTERFACE declaration itself (`check_interface_default_body`,
	/// members.rs/solve.rs) — so an inner `this.<method>()` call's OWN
	/// recorded `Resolution.impl_span` always points at the interface
	/// declaration's own name span, REGARDLESS of which concrete `impl
	/// <Interface> for <Type>` block the body is currently being
	/// lowered through. `try_lower_runtime_dispatch`'s ordinary
	/// span-scan matches an `ImplFor` block by its `for_interface` span
	/// (a DIFFERENT span, unique to that concrete impl block) — which an
	/// inner call's `impl_span` can never equal — so that scan alone always
	/// misses and would otherwise panic. `try_lower_runtime_dispatch`
	/// falls back to this stack's top frame when the span-scan finds
	/// nothing, resolving the sibling directly against the OUTER
	/// lowering's own known interface/tag instead. Empty everywhere
	/// else, including while lowering an inherent `Impl` body (D1: that
	/// case's inner calls already route correctly by span alone — see
	/// `RuntimeFuncDemand::sibling_frame`'s doc comment).
	lowering_runtime_sibling: RefCell<Vec<RuntimeSiblingFrame<'a>>>,
	/// Mangled top-level function names (stdlib body lowering slice,
	/// gap b) already discovered lowerable — dedups repeat demand for
	/// the same `(interface, self-type, method)` and, combined with being
	/// inserted BEFORE the body is actually lowered (`try_lower_runtime_dispatch`),
	/// guards against infinite recursion if a lowered body ever called
	/// itself indirectly.
	/// Mangled function → (canonical owner, declaration span). Repeated demands
	/// must agree on both identity components; a name alone is not identity.
	runtime_funcs_seen: RefCell<FxHashMap<EcoString, (EcoString, Span)>>,
	/// Work queue of prelude bodies discovered lowerable but not yet
	/// lowered — Pass 2 of the demand-driven scheme (see `lower_hir_with_prelude`'s
	/// doc comment): Pass 1 (the ordinary module walk) only rewrites call
	/// sites and enqueues here; queued bodies are drained AFTER that walk
	/// finishes, so each drains with an empty top-level `scopes`/`generics_stack`
	/// (mirroring how an ordinary top-level `func`/method lowers) rather than
	/// nested inside whatever user scope happened to trigger the demand.
	/// Draining can itself grow this queue (a lowered body calling
	/// another prelude body) — `drain_runtime_func_demands` loops until empty.
	runtime_func_demands: RefCell<Vec<RuntimeFuncDemand<'a>>>,
	/// Finished `HirFunc`s for every drained `runtime_func_demands` entry, in
	/// the order they were lowered — appended to `HirModule::funcs` once
	/// `lower_module`'s own walk (and every prelude body it demanded) is done.
	lowered_runtime_funcs: RefCell<Vec<HirFunc>>,
	runtime_func_owners: RefCell<FxHashMap<EcoString, RuntimeOwner>>,
	/// Nonzero while lowering an interface default body [`Self::push_unoverridden_defaults`]
	/// lowers ONTO A CONCRETE STRUCT/ENUM CLASS (gap a) — as opposed to
	/// [`Self::lower_runtime_func`] lowering one as a top-level mangled
	/// function for a PRIMITIVE self-type (gap b). The distinction matters for
	/// an inner dispatch the default body's OWN text makes back through
	/// `this` (e.g. `Comparable`'s `less_than` default calling
	/// `this.compare_to(other)`): the checker type-checks a default body
	/// exactly ONCE, generically, with `this` bound to a rigid synthetic
	/// `Param` (`check_interface_default_body`/`checking_interface_default`,
	/// members.rs/solve.rs) — so that inner call's OWN recorded `Resolution`
	/// is ALSO `UserImplDefaultMethod`, with `impl_span` set to the
	/// INTERFACE's own span (prelude-origin, so `>= SPAN_BASE`) REGARDLESS of
	/// which concrete type the whole default body later gets lowered
	/// onto. When lowering gap (a) onto struct/enum `T`, that inner call
	/// is safe to lower as an ORDINARY direct `this.method(args)` dispatch —
	/// every method a default body can reach through `this` is either a
	/// required interface method `T`'s own impl must provide directly, or
	/// another default of the SAME interface `push_unoverridden_defaults`
	/// lowers onto `T` in this very pass — so trusting it needs no
	/// `try_lower_runtime_dispatch` span lookup at all (mangled-function
	/// dispatch is the wrong shape here regardless: `T` is a real class with a
	/// real prototype, not a JS primitive). Gap (b)'s `lower_runtime_func`
	/// does NOT set this: there, `$self` may be a bare JS primitive with no
	/// such method at all, so the SAME inner-call shape must still recurse
	/// through `try_lower_runtime_dispatch` (e.g. `Comparable`'s
	/// `less_than` for `boolean` calling `$self.compare_to(other)`, which
	/// itself demands `$std$Comparable$boolean$compare_to`).
	lowering_onto_runtime_owner: std::cell::Cell<u32>,
	/// Stack of prelude ENUM names currently being lowered onto their own
	/// emitted class (this slice — named-type prelude method lowering),
	/// innermost last. Distinguishes the TWO users of `lowering_onto_runtime_owner`
	/// (see that field's doc comment): empty while `push_unoverridden_defaults`
	/// lowers an interface default onto a USER class (that class is
	/// already fully lowered, nothing to demand), non-empty with the
	/// enum's own name on top while `lower_runtime_enum` lowers ONE of
	/// its own (inline or top-level-impl) methods. Consulted at every
	/// `lowering_onto_runtime_owner`-gated inner-dispatch fast path (`this.method()`,
	/// `this as T`, `this op other`) to record a lowering DEMAND for the
	/// sibling method being called — see `runtime_enum_method_demands`.
	current_runtime_owner_lowering: RefCell<Vec<EcoString>>,
	/// Demand set driving `lower_runtime_enum`'s DEMAND-ONLY method
	/// lowering (this slice): enum name → the method names some call site
	/// actually needs lowered onto that enum's class. Populated by (a)
	/// `try_lower_runtime_dispatch` returning `RuntimeDispatch::OntoClass`
	/// for an EXTERNAL `recv.method()`/cast dispatch, and (b) the
	/// `lowering_onto_runtime_owner`-gated inner-dispatch fast path for an INNER
	/// `this.method()` reached while already lowering a prelude enum
	/// (`current_runtime_owner_lowering` non-empty).
	///
	/// Demand-only, rather than eagerly lowering every inline method
	/// (`collect_adt_methods`'s ordinary behavior for a user struct/enum), is
	/// the necessary approach here: `Option`'s own `map_or_default`/
	/// `unwrap_or_default` call `R.default()`/`T.default()` through a still-generic
	/// type parameter, which lowering has no compilable JS form for under type
	/// erasure (the same family as `a.plus(b)` on a bound generic `T`) — eagerly
	/// lowering EVERY inline method the moment `Option` is merely referenced
	/// (e.g. `let o: Option<int> = None`, which never calls a method at all)
	/// would panic on those two methods even though the program never demanded
	/// them. Demanding only what's actually called keeps that panic reachable
	/// only when a program actually calls the unlowerable method itself —
	/// the honest floor, not a blanket regression.
	runtime_enum_method_demands: RefCell<FxHashMap<EcoString, FxHashSet<EcoString>>>,
	/// The method names actually lowered onto each prelude enum's class in
	/// the MOST RECENT `lower_runtime_enum` call for that enum — compared
	/// against `runtime_enum_method_demands`'s current (possibly since-grown)
	/// demand set by `lower_demanded_runtime_enums`'s fixed-point loop
	/// to decide whether that enum needs re-lowering (lowering `is_none`
	/// demands `is_some` only as a SIDE EFFECT of lowering its body — a demand
	/// discovered too late to affect the SAME `lower_runtime_enum` call
	/// that discovered it, so the outer loop must notice the growth and run it
	/// again).
	lowered_runtime_enum_methods: RefCell<FxHashMap<EcoString, FxHashSet<EcoString>>>,
	/// Prelude STRUCTS a method call dispatched `OntoClass` against (a method on a
	/// prelude struct receiver — including an inherited interface default like an
	/// adapter's `fold`). Unlike a `New`-constructed adapter (found by
	/// `lower_demanded_runtime_classes`'s body scan), a struct reached ONLY
	/// through a method call leaves no `HirExpr::New` trace, so its demand is recorded
	/// here and folded into that same fixed-point pass — otherwise the `recv.method(..)`
	/// call would hit an unemitted class.
	runtime_struct_demands: RefCell<FxHashSet<EcoString>>,
}

/// Where a `DispatchKind::UserImplDefaultMethod` resolution's prelude-origin
/// body actually lives, once [`Lowerer::try_lower_runtime_dispatch`]
/// locates it (this slice extends gap (b) — see that function's doc comment
/// for the full decision tree).
enum RuntimeDispatch {
	/// A demand-driven top-level MANGLED function (the original gap (b)
	/// shape, for a primitive/`List`/`Map` self-type with no emitted class of
	/// its own) — call it as `<mangled>(recv, args…)`.
	TopLevel(EcoString),
	/// A method that lowers ONTO the named prelude ENUM's own emitted
	/// class (this slice, named-type prelude method lowering) — call
	/// it as a plain `recv.method(args…)`, exactly like a user struct/enum's
	/// own method. Doesn't carry the enum name: `Lowerer::demand_onto_class`,
	/// the only place that constructs this variant, already recorded the
	/// lowering demand (keyed by enum name) before returning it — no
	/// consumer needs it again.
	OntoClass { method: EcoString },
	/// A method call that resolved through an `external(name)` marker present
	/// in [`nymph_hir::linkage::REGISTRY`] (Gap 3, L0/L1) — call it as
	/// `HirExpr::ExternCall { module: linked.module, symbol: linked.symbol,
	/// args: [receiver, ..args] }` instead of panicking. Carries the ALREADY
	/// receiver-tag-disambiguated [`nymph_hir::linkage::Linked`] (not the bare
	/// marker) — this is the ONE place (`Declaration::Impl`'s `ExternalFunc`
	/// arm, below) that has BOTH the marker and the concrete receiver `type_`
	/// in scope at once, so it is the only place that CAN resolve an ambiguous
	/// marker like `get` correctly; every consumer below just copies the
	/// already-resolved pair into `HirExpr::ExternCall`, never re-`lookup`s.
	LinkedExtern(&'static nymph_hir::linkage::Linked),
}

/// One entry in `Lowerer::runtime_func_demands`: everything
/// `drain_runtime_func_demands` needs to lower a demanded prelude body into a
/// `HirFunc`, without re-walking the prelude AST to rediscover it.
struct RuntimeFuncDemand<'a> {
	/// The mangled name to emit this body under (`$std$<Interface>$<SelfTypeTag>$<method>`).
	mangled: EcoString,
	canonical_owner: RuntimeOwner,
	/// The owning impl's (or, for an interface-default fallback, the
	/// interface's own) generic scope — mirrors `lower_method`'s
	/// `owner_generics` parameter; see `push_unoverridden_defaults`'s doc
	/// comment for why the CALLER must pass the same scope the checker used.
	owner_generics: &'a [Spanned<GenericParam>],
	meta: &'a FuncDeclaration,
	body: &'a Expr,
	/// Gap 2 (sibling-interface-method dispatch inside a lowered
	/// top-level default body): `Some` when this body was lowered
	/// through an `ImplFor` block (`Declaration::ImplFor`, i.e. it carries an
	/// interface name segment in its mangled name) — `None` for an inherent
	/// `Impl` body, which needs no frame because its inner sibling calls
	/// already route correctly by span alone (D1: an inherent method's own
	/// name span IS its `impl_span`, so the ordinary span-scan in
	/// `try_lower_runtime_dispatch` finds it directly). See
	/// `Lowerer::lowering_runtime_sibling`'s doc comment for why an
	/// `ImplFor` body needs this at all.
	sibling_frame: Option<RuntimeSiblingFrame<'a>>,
}

/// Gap 2's sibling-dispatch context, carried from the OUTER call that
/// lowered an `ImplFor` default body (`Lowerer::lower_impl_for_method`)
/// through to `Lowerer::lower_runtime_func`, which pushes it onto
/// `Lowerer::lowering_runtime_sibling` for exactly the body's own lowering
/// duration — see that field's doc comment for why an inner `this.<method>()`
/// call inside the body can't find its own concrete impl by span alone and
/// needs this instead.
#[derive(Clone)]
struct RuntimeSiblingFrame<'a> {
	/// The interface segment of the mangled scheme this body lowered
	/// under (`$std$<iface_name>$<tag>$<method>`) — every sibling call inside
	/// the body resolves to a method of this SAME interface (the checker
	/// types a default body's `this` as one rigid synthetic instance of its
	/// OWN interface, so an inner `this.method()` call can never resolve to a
	/// different interface's method).
	iface_name: EcoString,
	/// The canonical self-type tag (`inherent_self_type_tag`) this body's
	/// concrete receiver lowered under — reused verbatim so a sibling
	/// call mangles to the IDENTICAL name a direct outer call to that sibling
	/// would have produced (see `tag_consistency` in the task brief).
	tag: EcoString,
	/// The `impl<T> <iface_name> for <ReceiverType> { .. }` block's own
	/// members — consulted first (mirrors `try_lower_runtime_dispatch`'s
	/// own `own_member`-before-interface-default preference order) so a
	/// sibling call finds an override this SAME impl block provides before
	/// falling back to the interface's default.
	members: &'a [Spanned<ImplMember>],
	/// That impl block's own generics — the scope a sibling resolved through
	/// `members` (not the interface-default fallback) must be lowered
	/// against, mirroring the OUTER call's own `owner_generics`.
	impl_generics: &'a [Spanned<GenericParam>],
}

/// One JS lexical scope's bindings, for Y2 shadowing rename.
#[derive(Default)]
struct Scope {
	/// Original source name → the name currently in effect for it in this scope.
	current: FxHashMap<EcoString, EcoString>,
}

impl<'a> Lowerer<'a> {
	/// Push a fresh, empty JS scope (Slice 4E, Y2).
	fn push_scope(&self) {
		self.scopes.borrow_mut().push(Scope::default());
	}

	/// Pop the innermost JS scope.
	fn pop_scope(&self) {
		self.scopes.borrow_mut().pop();
	}

	/// Push the CURRENT func/method's full generic type-parameter scope: the
	/// OWNING struct/enum/impl-block's own generics (empty for a top-level
	/// `func`, which has no owner) plus this func/method's own (Slice 4J, Task
	/// 1 Finding 3 fix). Must be paired with [`Self::pop_generics`].
	///
	/// Both halves matter: the checker resolves a namespaced call
	/// (`T.default()`) against EVERY active param scope innermost-first
	/// (`lookup_param`, check.rs), and `build_param_scope(owner_generics)` is
	/// pushed onto that stack right alongside the method's own generics for
	/// every inherent/namespaced/`mut func` body (`check_method_body`,
	/// members.rs) — so a struct/enum-owned generic like `T` on `struct
	/// Box<T: Default>` type-checks a `T.default()` call inside one of
	/// `Box`'s own methods with zero diagnostics. Tracking only the method's
	/// own generics here (the pre-fix behavior) left that owner-generic case
	/// invisible to `is_current_generic`, so it fell through to ordinary
	/// `HirExpr::Call`/`HirExpr::Field` lowering and silently emitted a bare,
	/// unbound `T.default()` in the output JS instead of the loud panic this
	/// guard exists to give.
	fn push_generics(
		&self,
		owner_generics: &[Spanned<GenericParam>],
		generics: &[Spanned<GenericParam>],
	) {
		let names = owner_generics
			.iter()
			.chain(generics)
			.map(|g| g.0.name.0.clone())
			.collect();
		self.generics_stack.borrow_mut().push(names);
	}

	/// Pop the innermost generics frame.
	fn pop_generics(&self) {
		self.generics_stack.borrow_mut().pop();
	}

	/// Is `name` one of the CURRENT (innermost) func/method's own generic
	/// type-parameter names? Only the innermost frame matters here: nested
	/// func/method lowering doesn't exist yet (no closures, Slice 4J FF3), so
	/// there is at most one meaningful frame at any point, but checking only
	/// the innermost keeps the intent explicit either way.
	fn is_current_generic(&self, name: &EcoString) -> bool {
		self
			.generics_stack
			.borrow()
			.last()
			.is_some_and(|g| g.contains(name))
	}

	/// Is `receiver`'s inferred type a bare type parameter (after peeling a `mut`
	/// view)? Such a receiver is only known through a generic bound, so a method
	/// call on it must lower to a plain dynamic `recv.method(args)` — the concrete
	/// impl is chosen by whatever object flows in at runtime (type erasure).
	fn receiver_is_still_generic(&self, receiver: &Expr) -> bool {
		self
			.annotations
			.get(receiver.id)
			.map(|info| Self::peel_mut(self.interner, info.ty))
			.is_some_and(|ty| matches!(self.interner.kind(ty), TyKind::Param(_)))
	}

	fn lower_bound_dispatch(
		&self,
		res: &Resolution,
		receiver: &Expr,
		argument: &Expr,
	) -> Option<HirExpr> {
		use nymph_ast::ty::Type;

		let interface_span = res.impl_span.unwrap_or_else(|| {
			panic!(
				"generic-bound dispatch for `{}` has no source span",
				res.method
			)
		});
		let modules = self
			.prelude_modules
			.iter()
			.chain(std::iter::once(self.module));
		let interface = modules.clone().find_map(|module| {
			match module.members.iter().find(
				|decl| matches!(decl, Declaration::Interface { name, .. } if crate::prelude::same_source_span(name.1, interface_span)),
			) {
				Some(Declaration::Interface { name, .. }) => Some(name.0.clone()),
				_ => None,
			}
		}).or_else(|| {
			// Import rewriting preserves source identity but can rebuild a module in
			// a differently assembled prelude graph. Fall back to the canonical
			// method declaration; checker resolution guarantees this method belongs
			// to the bound interface.
			modules.clone().find_map(|module| {
				module.members.iter().find_map(|decl| match decl {
					Declaration::Interface { name, members, .. }
						if members.iter().any(|member| matches!(
							&member.0,
							InterfaceMember::Element(element)
								if matches!(&element.0, InterfaceElement::Func { meta, .. } if meta.name.0 == res.method)
						)) => Some(name.0.clone()),
					_ => None,
				})
			})
		}).unwrap_or_else(|| {
			panic!(
				"generic-bound dispatch for `{}` has unknown interface span {interface_span:?}",
				res.method
			)
		});
		let interface_module = self.interface_owner(&interface);
		if interface != "Comparable" && interface_module.key() != &self.module.path {
			return None;
		}

		let mut cases: Vec<HirBoundDispatchCase> = Vec::new();
		for decl in modules.flat_map(|module| module.members.iter()) {
			let Declaration::ImplFor {
				generics,
				mutable,
				type_,
				for_interface,
				members,
				..
			} = decl
			else {
				continue;
			};
			if for_interface.0.0 != interface {
				continue;
			}
			let Some(receiver_tag) = inherent_self_type_tag(&type_.0, *mutable) else {
				continue;
			};
			let Some(other) = for_interface
				.1
				.iter()
				.find(|arg| arg.0.name.as_ref().is_some_and(|name| name.0 == "Other"))
				.or_else(|| for_interface.1.first())
			else {
				continue;
			};
			let Some(argument_tag) = (match &other.0.value.0 {
				Type::SelfType => Some(receiver_tag.clone()),
				type_ => inherent_self_type_tag(type_, false),
			}) else {
				continue;
			};
			// This lowering shape represents one still-generic type flowing as
			// both receiver and argument. Heterogeneous `Comparable<Other>` impls
			// belong to a different instantiation and, because runtime helper names
			// are receiver-keyed, must not collide with the homogeneous case.
			if receiver_tag != argument_tag {
				continue;
			}

			let target = if let Some(marker) = members.iter().find_map(|member| match &member.0 {
				ImplMember::ExternalFunc(_, marker, meta) if meta.name.0 == res.method => Some(marker),
				_ => None,
			}) {
				let Some(linked) = nymph_hir::linkage::lookup(marker, Some(receiver_tag.as_str())) else {
					continue;
				};
				HirBoundDispatchTarget::Extern {
					module: linked.module,
					symbol: linked.symbol,
				}
			} else {
				let Some((owner_generics, meta, body)) =
					self.resolve_impl_for_source(&interface, members, generics, &res.method)
				else {
					continue;
				};
				if self.body_calls_unlinked_external(body) {
					continue;
				}
				match self.finish_runtime_impl_lowering(
					&interface,
					&receiver_tag,
					members,
					generics,
					owner_generics,
					meta,
					body,
					&res.method,
				) {
					RuntimeDispatch::TopLevel(name) => HirBoundDispatchTarget::TopLevel {
						module: interface_module.key().clone(),
						name,
					},
					RuntimeDispatch::LinkedExtern(linked) => HirBoundDispatchTarget::Extern {
						module: linked.module,
						symbol: linked.symbol,
					},
					RuntimeDispatch::OntoClass { .. } => continue,
				}
			};

			if let Some(existing) = cases.iter().find(|case| {
				case.receiver_tag == runtime_type_tag(&receiver_tag)
					&& case.argument_tag == runtime_type_tag(&argument_tag)
			}) {
				assert_eq!(
					existing.target, target,
					"conflicting generic dispatch targets for {interface}.{} on ({receiver_tag}, {argument_tag})",
					res.method
				);
				continue;
			}
			cases.push(HirBoundDispatchCase {
				receiver_tag: runtime_type_tag(&receiver_tag),
				argument_tag: runtime_type_tag(&argument_tag),
				target,
			});
		}
		cases
			.sort_by(|a, b| (&a.receiver_tag, &a.argument_tag).cmp(&(&b.receiver_tag, &b.argument_tag)));
		Some(HirExpr::BoundDispatch {
			interface,
			method: res.method.clone(),
			receiver: Box::new(self.lower_expr(receiver)),
			argument: Box::new(self.lower_expr(argument)),
			cases,
		})
	}

	/// Does any prelude impl back a method named `method` with an `external` body
	/// (`ImplMember::ExternalFunc`)? If so, a still-generic receiver calling that
	/// method is NOT safe to lower as a plain `recv.method(args)`: the concrete
	/// runtime value could be a primitive/collection whose impl is an intrinsic with
	/// no such JS method (e.g. `Plus::plus` on `int` is `external(plus)`, not a class
	/// method). Such a call stays a loud deferral rather than a silent miscompile.
	/// The genuinely-safe generic dispatches (an adapter's `this.source.next()` under
	/// `S: Iterator`) name methods with no `external` prelude backing at all.
	fn method_is_externally_backed_in_prelude(&self, method: &str) -> bool {
		fn members_have_external(members: &[Spanned<ImplMember>], method: &str) -> bool {
			members
				.iter()
				.any(|m| matches!(&m.0, ImplMember::ExternalFunc(_, _, meta) if meta.name.0 == method))
		}
		self.prelude_modules.iter().any(|module| {
			module.members.iter().any(|decl| match decl {
				Declaration::ExternalFunc(_, _, meta) => meta.name.0 == method,
				Declaration::Impl { members, .. } | Declaration::ImplFor { members, .. } => {
					members_have_external(members, method)
				}
				Declaration::Struct { members, impls, .. } | Declaration::Enum { members, impls, .. } => {
					members_have_external(members, method)
						|| impls
							.iter()
							.any(|si| members_have_external(&si.0.members, method))
				}
				_ => false,
			})
		})
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
		if let Some(mapped) = self
			.pattern_declaration_reuse
			.borrow()
			.last()
			.and_then(|bindings| bindings.get(name))
			.cloned()
		{
			self
				.scopes
				.borrow_mut()
				.last_mut()
				.expect("pattern declaration outside a scope")
				.current
				.insert(name.clone(), mapped.clone());
			for record in self.pattern_declaration_records.borrow_mut().iter_mut() {
				record.insert(name.clone(), mapped.clone());
			}
			return mapped;
		}
		let mut scopes = self.scopes.borrow_mut();
		let shadows_active_binding = scopes.iter().any(|s| s.current.contains_key(name));
		// Named-type prelude method lowering: a Nymph parameter/`let`
		// name that happens to be a JS RESERVED WORD (`default` — the one that
		// actually appears in real stdlib source, e.g. `Unwrap.unwrap(default:
		// T)`/`Option.map_or(default, f)`, mirroring Rust's `Option`/`Result`
		// naming convention) is perfectly legal Nymph (never reserved by this
		// language's own keyword set) but would emit as an outright JS
		// `SyntaxError` verbatim (`function unwrap(default) {` doesn't even
		// parse) — never reachable before this slice, since no prelude body
		// using `default` as a parameter was ever actually lowered (each fell
		// into either gap (b)'s pre-existing mangled-function path, which
		// never lowers a NAMED-enum receiver body at all, or straight into the
		// "prelude-only impl" panic). Treated exactly like a shadowing
		// collision — same rename machinery, same `$N` suffix — rather than a
		// separate mechanism, since both need the identical "give me a fresh,
		// never-colliding name" operation.
		let needs_rename = shadows_active_binding || is_js_reserved_word(name);
		let scope = scopes
			.last_mut()
			.expect("slice-4e lowering: declare() called outside any pushed scope");
		let declared = if needs_rename {
			let mut counters = self.rename_counters.borrow_mut();
			let suffix = counters.entry(name.clone()).or_insert(0);
			*suffix += 1;
			let renamed: EcoString = format!("{name}${suffix}").into();
			scope.current.insert(name.clone(), renamed.clone());
			renamed
		} else {
			scope.current.insert(name.clone(), name.clone());
			name.clone()
		};
		for record in self.pattern_declaration_records.borrow_mut().iter_mut() {
			record.insert(name.clone(), declared.clone());
		}
		declared
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

	fn canonical_owner_of(&self, meta: &FuncDeclaration) -> RuntimeOwner {
		for (module, owner) in self.prelude_modules.iter().zip(self.prelude_owners) {
			let found = module.members.iter().any(|decl| match decl {
				Declaration::Func {
					meta: candidate, ..
				} => candidate.name.1 == meta.name.1,
				Declaration::Impl { members, .. } | Declaration::ImplFor { members, .. } => {
					members.iter().any(|member| match &member.0 {
						ImplMember::Func {
							meta: candidate, ..
						} => candidate.name.1 == meta.name.1,
						_ => false,
					})
				}
				Declaration::Interface { members, .. } => members.iter().any(|member| match &member.0 {
					InterfaceMember::Element(element) => match &element.0 {
						InterfaceElement::Func {
							meta: candidate, ..
						} => candidate.name.1 == meta.name.1,
						_ => false,
					},
					_ => false,
				}),
				_ => false,
			});
			if found {
				return owner.clone();
			}
		}
		panic!(
			"demanded runtime function `{}` has no canonical owner",
			meta.name.0
		)
	}

	fn interface_owner(&self, interface: &EcoString) -> RuntimeOwner {
		self
			.prelude_modules
			.iter()
			.zip(self.prelude_owners)
			.find_map(|(module, owner)| {
				module
					.members
					.iter()
					.any(|decl| matches!(decl, Declaration::Interface { name, .. } if name.0 == *interface))
					.then(|| owner.clone())
			})
			.or_else(|| {
				self
					.module
					.members
					.iter()
					.any(|decl| matches!(decl, Declaration::Interface { name, .. } if name.0 == *interface))
					.then(|| RuntimeOwner::Compiler(self.module.path.clone()))
			})
			.unwrap_or_else(|| panic!("interface `{interface}` has no canonical owner"))
	}

	fn register_runtime_func(
		&self,
		mangled: &EcoString,
		owner: &RuntimeOwner,
		meta: &FuncDeclaration,
	) -> bool {
		let identity = (owner.key().clone(), meta.name.1);
		let mut seen = self.runtime_funcs_seen.borrow_mut();
		if let Some(existing) = seen.get(mangled) {
			assert_eq!(
				existing, &identity,
				"conflicting source identity for demanded runtime function `{mangled}`"
			);
			false
		} else {
			seen.insert(mangled.clone(), identity);
			true
		}
	}

	fn lower_module(&self, module: &Module) -> LoweredHir {
		use nymph_ast::decl::ImplMember;
		use nymph_ast::ty::Type;

		// Interface bodies, for lowering un-overridden default methods onto
		// implementing struct classes (Slice 4C-b) — `self.interfaces_by_name`
		// (built once in `lower_hir_impl`) already covers both `module`'s own
		// interfaces AND, when lowering via `lower_hir_with_prelude`, every
		// prelude module's (stdlib body lowering slice, gap a). Resolution
		// is by bare name within this flattened view — stdlib isn't cross-module
		// linked in yet, so no real cross-module lookup is needed (mirrors the
		// checker's own `finish_interface_impl`, which resolves the same way via
		// `defs.get`). Keyed to `(generics, members)`: an interface's OWN
		// generics matter too (Slice 4J, Task 1 Finding 3 fix) — see that
		// field's doc comment for why.
		let interfaces_by_name = &self.interfaces_by_name;

		// First pass: collect instance methods from top-level `impl <Named>` blocks
		// (inherent, 4A) and top-level `impl <Interface> for <Named>` blocks
		// (interface impls, 4B/D5, now also lowering un-overridden interface
		// defaults per Slice 4C-b), keyed by the target type name. Non-`func`
		// members (namespaced statics, nested impls, `mut func`) are deferred and
		// panic loudly rather than silently disappearing.
		let mut methods_by_type: FxHashMap<EcoString, Vec<HirMethod>> = FxHashMap::default();
		for decl in &module.members {
			match decl {
				Declaration::Impl {
					generics,
					type_,
					members,
					..
				} => {
					// Inherent impl: no interface, no defaults to lower. A
					// non-`Reference` target silently contributes nothing here, same as
					// before Slice 4C-b — unchanged, out of this slice's scope.
					if let Type::Reference { name, .. } = &type_.0 {
						let entry = methods_by_type.entry(name.0.clone()).or_default();
						for member in members {
							match &member.0 {
								ImplMember::Func { meta, body, .. } => {
									// A `mut func` in a top-level `impl` block is an ordinary
									// instance method (like a struct-body `mut func`). A
									// `namespace func` static, however, has no channel from a
									// top-level impl block into the class's `statics` yet — the
									// checker models it as a static (members.rs), so lowering it
									// as an instance method would be silent wrong-JS. Defer it
									// loudly instead (statics belong in the type's own body).
									assert!(
										meta.kind != nymph_ast::decl::FuncKind::Namespace,
										"lowering does not yet support a `namespace func` in a top-level `impl` block (declare the static in the type body instead): {}",
										meta.name.0,
									);
									entry.push(self.lower_method(generics, meta, body));
								}
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
						interfaces_by_name,
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
		let mut external_values: std::collections::BTreeMap<_, EcoString> =
			std::collections::BTreeMap::new();
		for decl in &module.members {
			match decl {
				// A top-level `let`/`let mut` (Slice 4E, Y3). No scope is pushed while
				// lowering its value: the module-level scope stack is empty for the
				// whole module walk, so `resolve()` on any identifier inside falls
				// through to its bare source name — exactly right, since module-level
				// funcs/classes/enums/other top-level lets are never renamed either.
				Declaration::Let { meta, value, .. } => lets.push(HirLet {
					name: param_name(&meta.name),
					mutable: meta.is_mutable(),
					value: self.lower_expr(value),
				}),
				Declaration::ExternalLet(_, marker, meta) => {
					let mut let_ = self.lower_external_let(marker, meta);
					let HirExpr::ExternValue {
						module,
						symbol,
						marshal,
					} = let_.value
					else {
						unreachable!()
					};
					let identity = (module, symbol, marshal);
					if let Some(canonical) = external_values.get(&identity) {
						let_.value = HirExpr::Local(canonical.clone());
					} else {
						external_values.insert(identity, let_.name.clone());
					}
					lets.push(let_);
				}
				Declaration::Func { meta, body, .. } => funcs.push(self.lower_func(meta, body)),
				Declaration::Struct {
					name,
					generics,
					fields,
					members,
					impls,
					..
				} => {
					// Methods from top-level impls, the struct's own inner `func`s /
					// `namespace func` statics / `mut func` methods, and nested
					// `impl <Interface> { .. }` blocks inside the struct body (also
					// lowering that interface's un-overridden defaults, Slice 4C-b).
					let (methods, statics) = self.collect_adt_methods(
						&name.0,
						generics,
						members,
						impls,
						interfaces_by_name,
						&mut methods_by_type,
						None,
					);
					classes.push(HirClass {
						name: name.0.clone(),
						fields: fields.iter().map(|f| f.0.name.0.clone()).collect(),
						methods,
						statics,
					});
				}
				Declaration::Enum {
					name,
					generics,
					variants,
					members,
					impls,
					..
				} => {
					// Slice 4D: enums consume `methods_by_type` (top-level `impl`/`impl
					// … for`) and their own inner members through the exact same path
					// as structs — enum-body inherent funcs, `namespace func` statics,
					// `mut func` methods, and nested `impl <Interface> { .. }` blocks.
					let (methods, statics) = self.collect_adt_methods(
						&name.0,
						generics,
						members,
						impls,
						interfaces_by_name,
						&mut methods_by_type,
						None,
					);
					let variants: Vec<HirVariant> = variants
						.iter()
						.map(|v| HirVariant {
							name: v.0.name.0.clone(),
							fields: v.0.fields.iter().map(|f| f.0.name.0.clone()).collect(),
						})
						.collect();
					self.assert_no_variant_static_collision(&name.0, &variants, &statics);
					enums.push(HirEnum {
						name: name.0.clone(),
						variants,
						methods,
						statics,
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

		// Gap (b), Pass 2 (stdlib body lowering slice): the module walk
		// above only REWRITES a lowerable `UserImplDefaultMethod` call site
		// and enqueues the demanded body — actually lower every queued body now,
		// with a clean top-level scope stack (this can itself enqueue further
		// demands, e.g. a lowered `Comparable` default calling
		// `this.compare_to(o)`; `drain_runtime_func_demands` loops to a
		// fixed point).
		self.drain_runtime_func_demands();
		let mut runtime_funcs: Vec<_> = self.lowered_runtime_funcs.borrow_mut().drain(..).collect();

		// Gap (a)'s other half: any interface-default body lowered onto a
		// user class above (`push_unoverridden_defaults`, fed prelude
		// interfaces) — or any gap (b) body just lowered — may reference a
		// prelude enum (`Comparable`'s defaults all reach for `Order`) that
		// nothing has emitted anywhere in `enums` yet; a user program never
		// declares it (it isn't theirs to declare), so leaving it unlowered
		// would compile clean to a JS `ReferenceError` on the undefined `Order`
		// binding — silently worse than the loud panic this compiler otherwise
		// always prefers. Scan everything just lowered for a `VariantRef` naming
		// an enum this module doesn't already declare, and lower it
		// on-demand from the prelude, to a fixed point (an enum's own methods,
		// though none in `ops/mod.nym` today, could in principle reference
		// another prelude enum).
		// Gap (a)-for-structs (iterator adapters): an interface-default method
		// lowered onto a user class may construct an ambient adapter struct
		// (`c.map(f)` → `MapAdapter(source = this, f = f)`) whose class is emitted
		// nowhere. Lower referenced prelude structs FIRST, so an adapter method
		// that reaches a prelude enum method (`Option::map`) records that demand before
		// the enum pass below runs.
		let runtime_classes = self.lower_demanded_runtime_classes(
			Vec::new(),
			&lets,
			&funcs,
			&classes,
			&enums,
			&runtime_funcs,
		);
		// Lowering a demanded prelude class can itself discover generic-bound
		// primitive dispatchers (for example `RangeBounds.contains` comparing
		// its generic index values). Drain those newly demanded top-level
		// implementations before scanning for their enum dependencies.
		self.drain_runtime_func_demands();
		runtime_funcs.extend(self.lowered_runtime_funcs.borrow_mut().drain(..));
		self.lower_demanded_runtime_free_funcs(&mut runtime_funcs);
		let runtime_enums = self.lower_demanded_runtime_enums(
			Vec::new(),
			&lets,
			&funcs,
			&classes,
			&enums,
			&runtime_funcs,
			&runtime_classes,
		);

		// Stdlib body lowering slice's missed case (core/std split
		// follow-up): a bare identifier naming a prelude top-level `let`
		// (`std/math`'s `pi`/`tau`/`e`/`phi`/`max_int`/`min_int` — plain,
		// literal-initialized, never `external`) lowers through the ordinary
		// `ExprKind::Identifier` arm's `resolve()` fallback exactly like any
		// other unbound-in-scope module-level name (`HirExpr::Local(name)`,
		// bare, unrenamed — see `resolve`'s doc comment), with NOTHING having
		// ever demand-lowered the constant's own `const` binding anywhere
		// in the emitted module: a user program compiles with zero
		// diagnostics to a bare reference that throws `ReferenceError` at
		// runtime. Mirrors `lower_demanded_runtime_enums` exactly
		// (scan every body just lowered, including the enums just
		// lowered above, for a free `Local` this module doesn't already
		// bind as one of its own top-level `let`s, and lower it from the
		// prelude on demand) rather than pre-declaring every prelude constant
		// unconditionally, since almost no program references most of them.
		let runtime_lets = self.lower_demanded_runtime_lets(
			Vec::new(),
			&lets,
			&funcs,
			&classes,
			&enums,
			&runtime_funcs,
			&runtime_classes,
			&runtime_enums,
		);

		LoweredHir {
			module: HirModule {
				lets: reorder_lets_by_dependency(lets, &funcs),
				funcs,
				classes,
				enums,
			},
			prelude_runtime: HirModule {
				lets: reorder_lets_by_dependency(runtime_lets, &runtime_funcs),
				funcs: runtime_funcs,
				classes: runtime_classes,
				enums: runtime_enums,
			},
			runtime_func_owners: self.runtime_func_owners.borrow().clone(),
		}
	}

	/// Scan `lets`' own values, every `func`/class-method/enum-method body
	/// just lowered, for a free `HirExpr::Local` name none of them already
	/// binds as a top-level `let` of its own, and — if that name matches a
	/// plain top-level `let` in `self.prelude_modules` (`Declaration::Let`,
	/// never `ExternalLet`; see [`Self::lower_runtime_let`]'s doc
	/// comment for why `external` is out of scope here) — lower it into
	/// a new `HirLet` and fold it in, to a fixed point (a lowered
	/// constant's own value could in principle reference another prelude
	/// constant, though none in `std/math` today do). A name matching nothing
	/// found is left alone here exactly like `lower_demanded_runtime_
	/// enums` leaves an unresolvable enum reference alone: a genuine
	/// checker/lowering mismatch on a zero-diagnostic program is a real bug,
	/// not a gap this fixed point should paper over, and will surface as
	/// emit's own undefined-reference behavior if it's ever hit.
	fn lower_demanded_runtime_lets(
		&self,
		mut lets: Vec<HirLet>,
		module_lets: &[HirLet],
		module_funcs: &[HirFunc],
		module_classes: &[HirClass],
		module_enums: &[HirEnum],
		runtime_funcs: &[HirFunc],
		runtime_classes: &[HirClass],
		runtime_enums: &[HirEnum],
	) -> Vec<HirLet> {
		loop {
			let mut referenced = FxHashSet::default();
			for l in module_lets.iter().chain(&lets) {
				collect_locals(&l.value, &mut referenced);
			}
			for f in module_funcs.iter().chain(runtime_funcs) {
				collect_locals(&f.body, &mut referenced);
			}
			for c in module_classes.iter().chain(runtime_classes) {
				for m in c.methods.iter().chain(&c.statics) {
					collect_locals(&m.body, &mut referenced);
				}
			}
			for e in module_enums.iter().chain(runtime_enums) {
				for m in e.methods.iter().chain(&e.statics) {
					collect_locals(&m.body, &mut referenced);
				}
			}
			let known: FxHashSet<&EcoString> = module_lets.iter().chain(&lets).map(|l| &l.name).collect();
			let missing: Vec<EcoString> = referenced
				.into_iter()
				// A mangled `$m{tag}$…` name belongs to a real project module (the
				// import-binding rewrite pass), emitted on its own turn — never a
				// bare prelude constant name, so never worth a lookup here (mirrors
				// `lower_demanded_runtime_enums`'s identical guard).
				.filter(|name| !known.contains(name) && !name.starts_with('$'))
				.collect();
			if missing.is_empty() {
				return lets;
			}
			let mut changed = false;
			for name in missing {
				if let Some(new_let) = self.lower_runtime_let(&name) {
					lets.push(new_let);
					changed = true;
				}
			}
			if !changed {
				// Every still-missing name was looked for and not found in the
				// prelude either — left alone for the same reason
				// `lower_demanded_runtime_enums` leaves its own
				// still-missing names alone (see this function's doc comment).
				return lets;
			}
		}
	}

	/// Locate `name` among `self.prelude_modules`' top-level plain `let`
	/// declarations (`Declaration::Let` — never `Declaration::ExternalLet`: an
	/// external let's value is a JS-side binding with no Nymph body to lower,
	/// a separate, not-yet-covered gap this fix does not claim to close) and
	/// lower its value exactly like `lower_module`'s own top-level
	/// `Declaration::Let` arm: no scope pushed (the module-level scope stack
	/// is empty here too, same as for the module's own top-level lets), name
	/// kept bare (a prelude top-level name is never renamed, exactly like a
	/// module's own).
	fn lower_runtime_let(&self, name: &EcoString) -> Option<HirLet> {
		for module in self.prelude_modules {
			for decl in &module.members {
				if let Declaration::ExternalLet(_, marker, meta) = decl
					&& param_name(&meta.name) == *name
				{
					return Some(self.lower_external_let(marker, meta));
				}
				if let Declaration::Let { meta, value, .. } = decl
					&& param_name(&meta.name) == *name
				{
					return Some(HirLet {
						name: name.clone(),
						mutable: meta.is_mutable(),
						value: self.lower_expr(value),
					});
				}
			}
		}
		None
	}

	fn lower_external_let(&self, marker: &str, meta: &nymph_ast::decl::LetDeclaration) -> HirLet {
		let value_link =
			nymph_hir::linkage::lookup_value(marker).expect("checked external value linkage must lower");
		let linked = &value_link.linked;
		HirLet {
			name: param_name(&meta.name),
			mutable: false,
			value: HirExpr::ExternValue {
				module: linked.module,
				symbol: linked.symbol,
				marshal: self.external_value_marshals[&meta.name.1],
			},
		}
	}

	/// Locate a TOP-LEVEL `external` func declaration among
	/// `self.prelude_modules` whose Nymph NAME (not marker) is `name`, and
	/// return its linkage if the marker is registered (`receiver_tag: None`
	/// — a top-level `external` func has no receiver at all). This is the
	/// free-function counterpart of the method-call `LinkedExtern` dispatch
	/// (`try_lower_runtime_dispatch`): a bare `print(x)` is `ExprKind::
	/// Call { func: Identifier("print"), .. }`, never a `MemberAccess`, so it
	/// skips every method-dispatch arm above and would otherwise fall to the
	/// final `else` as a plain `HirExpr::Call` to a name with no JS binding
	/// (an `external` func has no body) — silent-wrong-JS, a runtime
	/// `ReferenceError`. Matching is by the MANGLED name (the project rewrite
	/// gives both the call site and the external decl the same `$m{tag}$name`
	/// tag; `external(marker)`'s marker string itself is left untouched by
	/// rewrite), so the registry key stays the bare marker written in the
	/// `.nym` source. Mangled names are globally unique, so at most one
	/// prelude module can match.
	fn lookup_free_fn_external(
		&self,
		name: &EcoString,
	) -> Option<&'static nymph_hir::linkage::Linked> {
		self.prelude_modules.iter().find_map(|m| {
			m.members.iter().find_map(|decl| match decl {
				Declaration::ExternalFunc(_, marker, meta) if &meta.name.0 == name => {
					nymph_hir::linkage::lookup(marker, None)
				}
				_ => None,
			})
		})
	}

	/// Whether `name` resolves to a top-level `external` func in the ambient
	/// prelude — regardless of whether its marker is registered in
	/// [`nymph_hir::linkage`]. Used at the free-function call site to tell
	/// "not a prelude external, so an ordinary call" apart from "a prelude
	/// external whose marker is *missing* from the registry". The latter must
	/// panic loudly (like every other `LinkedExtern` dispatch arm) rather than
	/// silently fall through to a `HirExpr::Call` on a symbol with no JS
	/// binding — a mis-registered stdlib external is a build-time bug, not a
	/// runtime `ReferenceError`.
	fn is_prelude_external_fn(&self, name: &EcoString) -> bool {
		self.prelude_modules.iter().any(|m| {
			m.members
				.iter()
				.any(|decl| matches!(decl, Declaration::ExternalFunc(_, _, meta) if &meta.name.0 == name))
		})
	}

	/// Drain [`Self::runtime_func_demands`] to a fixed point, lowering each
	/// queued body into a `HirFunc` (appended to
	/// [`Self::lowered_runtime_funcs`]) — see that field's doc comment for
	/// why this must run with an empty top-level scope, after the ordinary
	/// module walk, rather than inline at the demand site.
	fn drain_runtime_func_demands(&self) {
		loop {
			let next = self.runtime_func_demands.borrow_mut().pop();
			let Some(pending) = next else {
				break;
			};
			let f = self.lower_runtime_func(&pending);
			let old = self
				.runtime_func_owners
				.borrow_mut()
				.insert(f.name.clone(), pending.canonical_owner.clone());
			assert!(old.is_none_or(|owner| owner == pending.canonical_owner));
			self.lowered_runtime_funcs.borrow_mut().push(f);
		}
	}

	fn lower_demanded_runtime_free_funcs(&self, funcs: &mut Vec<HirFunc>) {
		loop {
			let existing: FxHashSet<_> = funcs.iter().map(|func| func.name.clone()).collect();
			let mut referenced = FxHashSet::default();
			for func in funcs.iter() {
				collect_locals(&func.body, &mut referenced);
			}
			let demanded = self
				.prelude_modules
				.iter()
				.zip(self.prelude_owners)
				.find_map(|(module, owner)| {
					module.members.iter().find_map(|decl| match decl {
						Declaration::Func { meta, body, .. }
							if referenced.contains(&meta.name.0) && !existing.contains(&meta.name.0) =>
						{
							Some((meta, body, owner))
						}
						_ => None,
					})
				});
			let Some((meta, body, owner)) = demanded else {
				break;
			};
			let func = self.lower_func(meta, body);
			let old = self
				.runtime_func_owners
				.borrow_mut()
				.insert(func.name.clone(), owner.clone());
			assert!(old.is_none_or(|existing| existing == *owner));
			funcs.push(func);
			self.drain_runtime_func_demands();
			funcs.extend(self.lowered_runtime_funcs.borrow_mut().drain(..));
		}
	}

	/// Lower one queued [`RuntimeFuncDemand`] into a top-level `HirFunc`
	/// (stdlib body lowering slice, gap b): `$self` (declared via the
	/// ordinary [`Self::declare`] machinery, so it can never collide with the
	/// body's own params/locals — see [`Self::try_lower_runtime_dispatch`]'s
	/// doc comment) becomes the receiver, pushed as a `this`-substitution
	/// (`this_sub`) for the body's own duration so `ExprKind::This` resolves to
	/// it instead of the meaningless top-level `HirExpr::This`. Otherwise
	/// mirrors [`Self::lower_method`] exactly (own scope, own generics frame,
	/// same param/body lowering) — a lowered prelude body is checked and
	/// shaped exactly like any other method body, just re-targeted to a
	/// top-level function instead of a class method.
	fn lower_runtime_func(&self, pending: &RuntimeFuncDemand<'a>) -> HirFunc {
		self.push_scope();
		self.push_generics(pending.owner_generics, &pending.meta.generics);
		let self_param = self.declare(&EcoString::from("$self"));
		let mut params = vec![self_param.clone()];
		params.extend(
			pending
				.meta
				.params
				.iter()
				.map(|p| self.declare(&param_name(&p.0.name))),
		);
		self.this_sub.borrow_mut().push(self_param);
		// Gap 2: while lowering an `ImplFor`-lowered default body, push
		// its own sibling-dispatch frame — see
		// `Lowerer::lowering_runtime_sibling`'s doc comment — so an
		// inner `this.<method>()` call can resolve directly against this SAME
		// impl instead of failing the ordinary span-scan. `None` for an
		// inherent `Impl` body (D1 needs no frame), so this is a no-op there.
		if let Some(frame) = &pending.sibling_frame {
			self
				.lowering_runtime_sibling
				.borrow_mut()
				.push(frame.clone());
		}
		let body = self.lower_func_body(pending.body);
		if pending.sibling_frame.is_some() {
			self.lowering_runtime_sibling.borrow_mut().pop();
		}
		self.this_sub.borrow_mut().pop();
		self.pop_generics();
		self.pop_scope();
		HirFunc {
			name: pending.mangled.clone(),
			params,
			body,
		}
	}

	/// Gap (a)'s enum half (see `lower_module`'s tail doc comment): repeatedly
	/// scan every body just lowered for a `VariantRef`/`VariantNew` naming an
	/// enum not already in `enums`, and lower (lower) that enum from
	/// `self.prelude_modules` if it's found there — to a fixed point, so a
	/// freshly lowered enum's own methods get the same treatment. A
	/// reference naming neither a declared enum nor a prelude one would mean
	/// the checker recorded a variant resolution lowering can't account for —
	/// a real checker/lowering mismatch, not an expected gap — so that case is
	/// left alone here (not silently lowered as something it isn't) and
	/// will surface as emit's own undefined-enum panic if it's ever hit.
	///
	/// This slice extends the fixed point two ways, both driven by
	/// `runtime_enum_method_demands` (named-type prelude method
	/// lowering, demand-only):
	/// - A prelude enum can be "referenced" purely through a method-call
	///   DEMAND with no `VariantRef`/`VariantNew` anywhere in the lowered
	///   program at all (e.g. a function parameter `o: Option<int>` calling
	///   `o.is_some()`, never itself constructing an `Option`) —
	///   `runtime_enum_method_demands`'s keys are unioned into `referenced`
	///   too, or that call's promised `recv.method()` would compile clean
	///   against a class never emitted.
	/// - An ALREADY-lowered enum's demand set can have GROWN since its
	///   last lowering (lowering `is_none`'s body demands `is_some`
	///   only as a side effect of lowering `is_none` ITSELF — too late to
	///   affect the very `lower_runtime_enum` call that discovered it);
	///   `grown` detects this by comparing the current demand set against
	///   `lowered_runtime_enum_methods`' record of what actually got
	///   lowered last time, and re-lowers (replacing the stale entry).
	fn lower_demanded_runtime_enums(
		&self,
		mut enums: Vec<HirEnum>,
		module_lets: &[HirLet],
		module_funcs: &[HirFunc],
		module_classes: &[HirClass],
		module_enums: &[HirEnum],
		runtime_funcs: &[HirFunc],
		runtime_classes: &[HirClass],
	) -> Vec<HirEnum> {
		loop {
			let mut referenced = FxHashSet::default();
			for l in module_lets {
				collect_variant_ref_enums(&l.value, &mut referenced);
			}
			for f in module_funcs.iter().chain(runtime_funcs) {
				collect_variant_ref_enums(&f.body, &mut referenced);
			}
			for c in module_classes.iter().chain(runtime_classes) {
				for m in c.methods.iter().chain(&c.statics) {
					collect_variant_ref_enums(&m.body, &mut referenced);
				}
			}
			for e in module_enums.iter().chain(&enums) {
				for m in e.methods.iter().chain(&e.statics) {
					collect_variant_ref_enums(&m.body, &mut referenced);
				}
			}
			for enum_name in self.runtime_enum_method_demands.borrow().keys() {
				referenced.insert(enum_name.clone());
			}
			let known: FxHashSet<&EcoString> =
				module_enums.iter().chain(&enums).map(|e| &e.name).collect();
			let missing: Vec<EcoString> = referenced
				.into_iter()
				// A mangled `$m{tag}$…` name belongs to a real project module (the
				// import-binding rewrite pass) that is emitted on its OWN turn and
				// referenced here by that mangled name — re-lowering it would
				// emit its declaration a second time and crash Node with a
				// redeclaration error. Only un-mangled ambient stdlib-prelude enums
				// (e.g. `Order`, `Option`, `Result`), which are emitted nowhere
				// else, are lowered.
				.filter(|name| !known.contains(name) && !name.starts_with('$'))
				.collect();
			let grown: Vec<EcoString> = enums
				.iter()
				.filter(|e| {
					let demand = self.runtime_enum_method_demands.borrow();
					let Some(demanded) = demand.get(&e.name) else {
						return false;
					};
					let lowered = self.lowered_runtime_enum_methods.borrow();
					let done = lowered.get(&e.name);
					!done.is_some_and(|done| demanded.is_subset(done))
				})
				.map(|e| e.name.clone())
				.collect();
			if missing.is_empty() && grown.is_empty() {
				return enums;
			}
			let mut changed = false;
			for name in missing.into_iter().chain(grown) {
				if let Some(e) = self.lower_runtime_enum(&name) {
					enums.retain(|e2| e2.name != name);
					enums.push(e);
					changed = true;
				}
			}
			if !changed {
				// Every still-missing name was looked for and not found in the
				// prelude either — see the doc comment above for why this is
				// left alone rather than looped on forever.
				return enums;
			}
		}
	}

	/// Locate `name` among `self.prelude_modules`' top-level `enum`
	/// declarations and lower it exactly like an ordinary module enum
	/// (`lower_module`'s own `Declaration::Enum` arm) — reusing
	/// `collect_adt_methods` for consistency — except DEMAND-ONLY (this
	/// slice, named-type prelude method lowering): only the methods
	/// named in `runtime_enum_method_demands[name]` at the moment of this
	/// call are lowered, whether inline (this enum's own `func`s) or from a
	/// TOP-LEVEL `impl <Interface> for <name>`/`impl <name> { .. }` block
	/// (Sub-problem #4 — `collect_adt_methods` alone never sees these; they're
	/// separate top-level `Declaration`s, collected in the second loop below).
	/// Eagerly lowering every inline method regardless of demand (this
	/// function's pre-this-slice behavior, still correct for `ops/mod.nym`'s
	/// `Order`, which has none) is NOT safe for every prelude enum: `Option`'s
	/// own `map_or_default`/`unwrap_or_default` call `R.default()`/
	/// `T.default()` through a still-generic type parameter, which has no
	/// compilable JS form under type erasure (the same family as `a.plus(b)`
	/// on a bound generic `T`) — eagerly lowering them merely because
	/// `Option` was referenced AT ALL (e.g. `let o: Option<int> = None`,
	/// which never calls a method) would panic on a program that never
	/// actually demanded either. See `runtime_enum_method_demands`'s doc
	/// comment.
	///
	/// While lowering this enum's own methods, `lowering_onto_runtime_owner` is
	/// bumped and `name` is pushed onto `current_runtime_owner_lowering`
	/// (Sub-problem #1, inner dispatch) — every inner `this.method()`/`this
	/// as T`/`this op other` dispatch reached while lowering these bodies is
	/// then safe as a plain class-method call (mirrors
	/// `push_unoverridden_defaults`'s identical use of `lowering_onto_runtime_owner`
	/// for a USER class), and records its own sibling demand
	/// (`record_inner_runtime_enum_method_demand`) so a GROWN demand set is caught by
	/// `lower_demanded_runtime_enums`'s fixed-point loop.
	///
	/// Returns `None` when `name` isn't a prelude enum at all (see the
	/// caller's doc comment).
	fn lower_runtime_enum(&self, name: &EcoString) -> Option<HirEnum> {
		use nymph_ast::ty::Type;

		let (generics, variants, members, impls) = self.prelude_modules.iter().find_map(|m| {
			m.members.iter().find_map(|decl| match decl {
				Declaration::Enum {
					name: n,
					generics,
					variants,
					members,
					impls,
					..
				} if n.0 == *name => Some((
					generics.as_slice(),
					variants.as_slice(),
					members.as_slice(),
					impls.as_slice(),
				)),
				_ => None,
			})
		})?;

		// Snapshot the demand set NOW — any growth discovered WHILE lowering
		// these bodies (inner dispatch demanding a sibling method) is caught
		// on the NEXT `lower_demanded_runtime_enums` round instead
		// (`lowered_runtime_enum_methods`'s doc comment), not this call.
		let demand: FxHashSet<EcoString> = self
			.runtime_enum_method_demands
			.borrow()
			.get(name)
			.cloned()
			.unwrap_or_default();

		self
			.current_runtime_owner_lowering
			.borrow_mut()
			.push(name.clone());
		self
			.lowering_onto_runtime_owner
			.set(self.lowering_onto_runtime_owner.get() + 1);

		let mut methods_by_type: FxHashMap<EcoString, Vec<HirMethod>> = FxHashMap::default();
		let (mut methods, mut statics) = self.collect_adt_methods(
			name,
			generics,
			members,
			impls,
			&self.interfaces_by_name,
			&mut methods_by_type,
			Some(&demand),
		);
		assert!(
			methods_by_type.is_empty(),
			"stdlib body lowering: prelude enum `{name}`'s OWN nested `impl Iface {{ .. }}` block populated `methods_by_type` for a DIFFERENT type name — unexpected (`collect_adt_methods` only ever feeds it from `impls` belonging to `{name}` itself)"
		);

		// Sub-problem #4: a TOP-LEVEL `impl <Interface> for <name>`/`impl
		// <name> { .. }` targeting this enum (`Option`'s own `Unwrap`/
		// `Default` impls, convert.nym's `ok`/`err`/`ok_or`/`ok_or_else`) is a
		// SEPARATE top-level `Declaration` `collect_adt_methods` above never
		// sees at all (that only ever collects INLINE members + this enum's
		// OWN nested `impl Iface { .. }` blocks) — scan every prelude
		// module's top-level declarations for one targeting `name`,
		// demand-gated exactly like the inline pass just above. (Un-overridden
		// interface DEFAULT methods on such an impl — e.g. a hypothetical
		// un-overridden `Unwrap` default — are NOT collected here: `Unwrap`
		// itself declares none, so this is a real but so-far-harmless scope
		// limit, not a silent gap this slice's payoff exercises.)
		for module in self.prelude_modules {
			for decl in &module.members {
				match decl {
					Declaration::ImplFor {
						generics: impl_generics,
						type_,
						members: impl_members,
						..
					} => {
						let Type::Reference { name: target, .. } = &type_.0 else {
							continue;
						};
						if target.0 != *name {
							continue;
						}
						self.collect_top_level_impl_methods(
							name,
							&demand,
							impl_generics,
							impl_members,
							&mut methods,
							&mut statics,
						);
					}
					Declaration::Impl {
						generics: impl_generics,
						type_,
						members: impl_members,
						..
					} => {
						let Type::Reference { name: target, .. } = &type_.0 else {
							continue;
						};
						if target.0 != *name {
							continue;
						}
						self.collect_top_level_impl_methods(
							name,
							&demand,
							impl_generics,
							impl_members,
							&mut methods,
							&mut statics,
						);
					}
					_ => {}
				}
			}
		}

		self
			.lowering_onto_runtime_owner
			.set(self.lowering_onto_runtime_owner.get() - 1);
		self.current_runtime_owner_lowering.borrow_mut().pop();

		// Record exactly which methods THIS round actually lowered —
		// compared against `runtime_enum_method_demands`'s (possibly
		// since-grown) demand set by `lower_demanded_runtime_enums`'s
		// fixed-point loop to decide whether this enum needs re-lowering.
		let lowered_names: FxHashSet<EcoString> = methods
			.iter()
			.chain(&statics)
			.map(|m| m.name.clone())
			.collect();
		self
			.lowered_runtime_enum_methods
			.borrow_mut()
			.insert(name.clone(), lowered_names);

		// V4, re-asserted across the combined inline + top-level-impl method
		// list (`collect_adt_methods` already checked its own slice of it).
		self.assert_no_duplicate_methods(name, &methods);
		self.assert_no_duplicate_methods(name, &statics);

		let lowered_variants: Vec<HirVariant> = variants
			.iter()
			.map(|v| HirVariant {
				name: v.0.name.0.clone(),
				fields: v
					.0
					.fields
					.iter()
					.map(|f| f.0.name.0.clone())
					.collect::<Vec<_>>(),
			})
			.collect();
		self.assert_no_variant_static_collision(name, &lowered_variants, &statics);
		Some(HirEnum {
			name: name.clone(),
			variants: lowered_variants,
			methods,
			statics,
		})
	}

	/// Scan every already-lowered body (including the classes/enums lowered so
	/// far) for a `HirExpr::New` constructing a struct this module doesn't already
	/// emit a class for — an ambient iterator adapter (`MapAdapter`, `FilterAdapter`)
	/// referenced from an interface-default method that was lowered onto a
	/// concrete class (`c.map(f)` → `MapAdapter(source = this, f = f)`). A user
	/// program never declares such a struct (it isn't theirs), so leaving it
	/// unlowered compiles clean to a JS `ReferenceError` on the undefined class —
	/// the same silent failure `lower_demanded_runtime_enums` closes for
	/// enums. Lower each from the prelude to a fixed point (an adapter's own
	/// method may construct another adapter).
	fn lower_demanded_runtime_classes(
		&self,
		mut classes: Vec<HirClass>,
		module_lets: &[HirLet],
		module_funcs: &[HirFunc],
		module_classes: &[HirClass],
		module_enums: &[HirEnum],
		runtime_funcs: &[HirFunc],
	) -> Vec<HirClass> {
		loop {
			let mut referenced = FxHashSet::default();
			for l in module_lets {
				collect_variant_ref_enums(&l.value, &mut referenced);
			}
			for f in module_funcs.iter().chain(runtime_funcs) {
				collect_variant_ref_enums(&f.body, &mut referenced);
			}
			for c in module_classes.iter().chain(&classes) {
				for m in c.methods.iter().chain(&c.statics) {
					collect_variant_ref_enums(&m.body, &mut referenced);
				}
			}
			for e in module_enums {
				for m in e.methods.iter().chain(&e.statics) {
					collect_variant_ref_enums(&m.body, &mut referenced);
				}
			}
			// A prelude struct reached only through a method call (`OntoClass`) leaves no
			// `New` for the body scan, so fold in the demand set the dispatch recorded.
			for name in self.runtime_struct_demands.borrow().iter() {
				referenced.insert(name.clone());
			}
			let known: FxHashSet<&EcoString> = module_classes
				.iter()
				.chain(&classes)
				.map(|c| &c.name)
				.collect();
			// `referenced` also holds enum names (shared scan) and mangled project-module
			// names; `lower_runtime_struct` returns `None` for anything that isn't a
			// plain prelude struct, so both are skipped.
			let missing: Vec<EcoString> = referenced
				.into_iter()
				.filter(|name| !known.contains(name) && !name.starts_with('$'))
				.collect();
			if missing.is_empty() {
				return classes;
			}
			let mut changed = false;
			for name in missing {
				if let Some(class) = self.lower_runtime_struct(&name) {
					classes.push(class);
					changed = true;
				}
			}
			if !changed {
				return classes;
			}
		}
	}

	/// Locate `name` among `self.prelude_modules`' top-level `struct` declarations and
	/// lower it into a `HirClass` exactly like `lower_module`'s own
	/// `Declaration::Struct` arm — its inline members and nested `impl <Interface>
	/// { .. }` blocks (an adapter's `impl Iterator<R> { mut func next() = .. }`),
	/// eagerly (all methods, `demand: None`): unlike a prelude ENUM, an adapter struct
	/// has no still-generic `T.default()`-style method that can't be compiled, so there
	/// is nothing to demand-gate. `lowering_onto_runtime_owner` is bumped while lowering the
	/// methods so an inner `this.method()` call resolves as a plain class-method call,
	/// and any prelude enum method it reaches (`Option::map` in
	/// `this.source.next().map(this.f)`) records its own demand for the enum pass that
	/// runs next. Returns `None` when `name` isn't a prelude struct.
	fn lower_runtime_struct(&self, name: &EcoString) -> Option<HirClass> {
		let (generics, fields, members, impls) = self.prelude_modules.iter().find_map(|m| {
			m.members.iter().find_map(|decl| match decl {
				Declaration::Struct {
					name: n,
					generics,
					fields,
					members,
					impls,
					..
				} if n.0 == *name => Some((
					generics.as_slice(),
					fields.as_slice(),
					members.as_slice(),
					impls.as_slice(),
				)),
				_ => None,
			})
		})?;

		self
			.lowering_onto_runtime_owner
			.set(self.lowering_onto_runtime_owner.get() + 1);

		let mut methods_by_type: FxHashMap<EcoString, Vec<HirMethod>> = FxHashMap::default();
		let (methods, statics) = self.collect_adt_methods(
			name,
			generics,
			members,
			impls,
			&self.interfaces_by_name,
			&mut methods_by_type,
			None,
		);

		self
			.lowering_onto_runtime_owner
			.set(self.lowering_onto_runtime_owner.get() - 1);

		self.assert_no_duplicate_methods(name, &methods);
		self.assert_no_duplicate_methods(name, &statics);

		Some(HirClass {
			name: name.clone(),
			fields: fields.iter().map(|f| f.0.name.0.clone()).collect(),
			methods,
			statics,
		})
	}

	/// Sub-problem #4 helper for [`Self::lower_runtime_enum`]: lower the
	/// DEMANDED (`demand`) methods of one top-level `impl <Interface> for
	/// <enum_name>`/`impl <enum_name> { .. }` block's `impl_members` into
	/// `methods`/`statics`, exactly like `collect_adt_methods`'s own flat-member
	/// loop (instance vs. `namespace func` static, same demand-gating) — kept
	/// as its own method (rather than a closure) purely to avoid borrowing
	/// `methods`/`statics` mutably while also calling back into `self`.
	fn collect_top_level_impl_methods(
		&self,
		enum_name: &EcoString,
		demand: &FxHashSet<EcoString>,
		impl_generics: &[Spanned<GenericParam>],
		impl_members: &[Spanned<nymph_ast::decl::ImplMember>],
		methods: &mut Vec<HirMethod>,
		statics: &mut Vec<HirMethod>,
	) {
		use nymph_ast::decl::{FuncKind, ImplMember};

		for member in impl_members {
			let ImplMember::Func { meta, body, .. } = &member.0 else {
				panic!(
					"stdlib body lowering: prelude enum `{enum_name}`'s top-level impl has a non-`func` member ({:?}) — demand-only enum lowering does not yet support it",
					member.0
				);
			};
			if !demand.contains(&meta.name.0) {
				continue;
			}
			if meta.kind == FuncKind::Namespace {
				statics.push(self.lower_method(impl_generics, meta, body));
			} else {
				methods.push(self.lower_method(impl_generics, meta, body));
			}
		}
	}

	/// Lower a top-level `impl <Interface> for <Type> { … }`'s own methods into
	/// `methods_by_type[Type]`, then lower (append) that interface's
	/// un-overridden default-bodied methods (Slice 4C-b, V1: impl-provided methods
	/// first in source order, then defaults in interface source order).
	fn push_impl_for_methods(
		&self,
		generics: &[Spanned<GenericParam>],
		type_: &Spanned<nymph_ast::ty::Type>,
		for_interface: &(Ident, Vec<Spanned<GenericArg>>),
		members: &[Spanned<nymph_ast::decl::ImplMember>],
		interfaces_by_name: &InterfaceTable,
		methods_by_type: &mut FxHashMap<EcoString, Vec<HirMethod>>,
	) {
		use nymph_ast::decl::ImplMember;
		use nymph_ast::ty::Type;

		let Type::Reference { name, .. } = &type_.0 else {
			// A structural target (e.g. `impl Plus<...> for #[int] { .. }` /
			// map.nym's `impl<K,V> Plus<..> for #{K:V}`) type-checks today (the
			// checker resolves its operator as a real `UserImpl`), and
			// `methods_by_type` has no representation for attaching methods to
			// anything but a named struct/enum class — but that's fine, because
			// `methods_by_type` is ONLY ever consumed by a `Declaration::Struct`/
			// `Enum` arm keyed on ITS OWN type name (`collect_adt_methods`'s
			// `methods_by_type.remove(type_name)`); a structural target can never
			// match one of those, so an entry keyed here would just sit unused
			// (and trip the end-of-`lower_module` "neither struct nor enum"
			// assert). The actual per-CALL-SITE lowering for a structural
			// receiver goes through the entirely separate, registry-aware
			// `try_lower_runtime_dispatch` (its own `ImplFor` arm handles
			// this exact shape via `inherent_self_type_tag`, independent of
			// `methods_by_type`), so silently contributing nothing here is exactly
			// as safe as the sibling `Declaration::Impl` arm's identical skip
			// (`lower_module`, "silently contributes nothing … same as before
			// Slice 4C-b") — this is that same skip, just for an `ImplFor` block
			// instead of a bare `Impl` block.
			return;
		};

		// A blanket impl (`impl<T> Iface for T`) parses its target as a bare
		// `Type::Reference` naming the impl's own generic parameter. Left
		// unchecked, that name could coincide with an unrelated real struct in the
		// module and silently attach the blanket's methods to it; refuse instead
		// (V5: blanket impls stay a loud deferral, never lowered).
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
					let method = self.lower_method(generics, meta, body);
					Self::push_protocol_impl_alias(&for_interface.0.0, &method, entry);
					entry.push(method);
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
		interfaces_by_name: &InterfaceTable,
		out: &mut Vec<HirMethod>,
	) {
		use nymph_ast::decl::{InterfaceElement, InterfaceMember};

		let Some((iface_generics, members)) = interfaces_by_name.get(&iface_name.0) else {
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
			// The interface's OWN generics are this default body's owner scope
			// (Slice 4J, Task 1 Finding 3 fix) — `check_interface_default_body`
			// (members.rs) checks it exactly once, generically, against
			// `iface_generics` plus the method's own, never against any
			// implementing struct/enum's generics (a default body lowered
			// onto multiple types can't depend on which one it lands on).
			//
			// `lowering_onto_runtime_owner` (stdlib body lowering slice,
			// gap a) is bumped for the DURATION of lowering this one default
			// body: an inner dispatch it makes back through `this` must lower
			// as an ordinary direct call on `this`, not attempt (gap b's)
			// top-level mangled-function lowering — see that field's
			// doc comment.
			self
				.lowering_onto_runtime_owner
				.set(self.lowering_onto_runtime_owner.get() + 1);
			out.push(self.lower_method(iface_generics, meta, body));
			self
				.lowering_onto_runtime_owner
				.set(self.lowering_onto_runtime_owner.get() - 1);
		}
	}

	/// Gap (b) of the stdlib body lowering slice: given a `Resolution`
	/// tagged `DispatchKind::UserImplDefaultMethod`, decide whether the
	/// prelude-origin impl/interface-default body it points at (via
	/// `res.impl_span`) is one this compiler can actually lower as a
	/// demand-driven top-level function — and if so, enqueue it (dedup'd) and
	/// return the mangled name to call instead of panicking.
	///
	/// Two shapes of prelude impl reach here, each with its own match key
	/// (this is the collections-lowering extension: originally this
	/// only scanned `Declaration::ImplFor` — an INTERFACE impl, matched by the
	/// interface-ident span — because every stdlib operator/`Comparable`
	/// method lived in one; every real stdlib COLLECTION method instead lives
	/// in a plain inherent `impl<T> #[T] { .. }` / `impl<T> mut #[T] { .. }` /
	/// `impl<K,V> #{K:V} { .. }` block, which has no interface to span at all):
	/// - `Declaration::ImplFor { for_interface, .. }`: `res.impl_span` is the
	///   INTERFACE ident's own span (`solve.rs`'s `commit_method`/`ImplDirect`
	///   /`InterfaceDefault` sourcing) — matched via `for_interface.0.1`.
	/// - `Declaration::Impl { members, .. }` (an INHERENT impl, no interface):
	///   `res.impl_span` is the resolved METHOD NAME's OWN span instead
	///   (`resolve_inherent`/`InherentRegistry::method_span`, members.rs —
	///   there is no impl-level span to key on since a bare `impl Type { .. }`
	///   commits no `ImplDef`), so the match key here is `meta.name.1`, one
	///   level deeper than the `ImplFor` branch's.
	///
	/// A dispatch stays unlowerable (returns `None`, so every call site
	/// keeps its existing loud panic) for any of:
	/// - `res.impl_span` is `None` (a `BuiltinEager`/`BuiltinShortCircuit`/
	///   `UserImpl` dispatch never reaches here at all, but kept total) or
	///   this lowering has no prelude modules at all (`lower_hir`'s plain,
	///   no-prelude callers) — nothing to look the span up in either way.
	/// - The span doesn't match any TOP-LEVEL `impl`/`impl … for …` in
	///   `self.prelude_modules` — covers a still-generic `GenericBound`
	///   dispatch (`impl_span` is the INTERFACE's own span there, e.g. a
	///   `func f<T: Plus>(a: T, b: T) = a.plus(b)`-shaped bound satisfied only
	///   through the prelude): there is no single concrete self-type to key a
	///   mangled function on, so this intentionally does NOT attempt to
	///   "unwrap" that case — it stays exactly as loud as before this slice.
	/// - The impl's target type isn't one this slice's tag inventory covers
	///   (`primitive_type_tag`'s six primitives for `ImplFor`; those six PLUS
	///   `List`/`Map` for a bare `Impl`, via `inherent_self_type_tag`): covers
	///   a blanket impl (`impl<T> Iface for T`, whose target is a bare
	///   `Type::Reference` naming the impl's own generic parameter, e.g.
	///   `Comparable`'s `minmax` or `Equals`'s blanket) and a named struct/enum
	///   receiver — mirrors `push_impl_for_methods`'s V5 (blanket impls stay a
	///   loud deferral, never lowered), for the identical reason: no
	///   single self-type to tag the mangled function with cleanly.
	/// - The matched member is itself `external`/`external(name)` (no real
	///   Nymph body — an `ImplMember::ExternalFunc`, never
	///   `ImplMember::Func`), or (`ImplFor` only) neither the impl's own
	///   members NOR the interface's own default body (falling back, exactly
	///   like `push_unoverridden_defaults`) provide `res.method` at all —
	///   covers every stdlib collection intrinsic (`list.push`/`get`/
	///   `length`/…, `map.get`/`insert`/`size`/…) and every primitive
	///   arithmetic/`compare_to_*` external — nothing here would EMIT
	///   anything for those even if lowering tried, so leaving them loud is
	///   the honest floor.
	/// - The matched body, though real Nymph source, itself transitively calls
	///   an unlinked external (`body_calls_unlinked_external`, extended by
	///   this slice to also catch external INSTANCE methods, not just
	///   top-level `external` functions) — covers `list`/`map`'s own
	///   `is_empty`/`is_not_empty` (`this.length() == 0` / `this.size() == 0`),
	///   which have real bodies but reach an intrinsic one call deep.
	///
	/// `this_receiver` gates Gap 2's sibling-frame fallback at this function's
	/// tail (below): `true` only when the CALLER already established (via
	/// `Self::is_this_receiver`) that this dispatch's own receiver expression
	/// is literally `this` — mirroring `is_this_receiver`'s own doc comment
	/// (written for the analogous gap-a `lowering_onto_runtime_owner` fast path):
	/// a default body dispatching through some OTHER expression that merely
	/// happens to satisfy the SAME interface bound (e.g. a second parameter
	/// also bound to `SomeIface`) is a genuinely different receiver that may
	/// be bound to a different concrete type than the one the OUTER
	/// lowering is running for — trusting the frame's tag for it would
	/// silently mangle a call to the WRONG concrete impl. Every call site
	/// below always has a concrete receiver expression to check, so this is
	/// never itself optional.
	fn try_lower_runtime_dispatch(
		&self,
		res: &Resolution,
		this_receiver: bool,
	) -> Option<RuntimeDispatch> {
		use nymph_ast::ty::Type;

		let span = res.impl_span?;
		if self.prelude_modules.is_empty() {
			return None;
		}
		// This slice's extension: an INLINE method declared directly inside a
		// prelude `enum` body (`Option`'s own `is_some`, `is_none`, `map`, …)
		// resolves through `resolve_inherent` with `impl_span` = the method's
		// own name span — the identical span shape `Declaration::Impl`'s
		// `meta.name.1` match key below uses for a top-level inherent impl,
		// just scoped to the enum's OWN `members` instead of a separate
		// top-level declaration. Checked first, ahead of the `ImplFor`/`Impl`
		// loop below, since an inline method's span can never collide with a
		// top-level declaration's (distinct source positions).
		//
		// Deliberately ENUM-only, never `Declaration::Struct`: unlike an enum
		// (`lower_runtime_enum`), there is no prelude-STRUCT
		// lowering anywhere in this compiler — returning `OntoClass`
		// for a struct's inline method would promise a `recv.method()` call
		// against a class this compiler never emits, exactly the
		// silent-wrong-JS failure mode this compiler never accepts. A named
		// struct receiver must keep panicking exactly as before this slice
		// (pinned by `inherent_prelude_struct_receiver_still_stays_a_loud_defer`).
		for module in self.prelude_modules {
			for decl in &module.members {
				if let Declaration::Enum {
					name: enum_name,
					members,
					..
				} = decl
				{
					let hit = members
						.iter()
						.any(|m| matches!(&m.0, ImplMember::Func { meta, .. } if meta.name.1 == span));
					if hit {
						return Some(self.demand_onto_class(enum_name.0.clone(), res.method.clone()));
					}
				}
			}
		}
		// An EMITTED dependency module's STRUCT method (an imported `Set`/
		// `LinkedList`/`Tree`/`Complex`, direct member OR nested `impl Iface { .. }`):
		// the struct's class is emitted by the driver on that module's own turn, so a
		// call to its method lowers as an ordinary `recv.method(..)` call on that class
		// — `OntoClass`, with NO lowering demand (the body is already emitted).
		for module in self.emitted_dep_modules {
			for decl in &module.members {
				if let Declaration::Struct { members, impls, .. } = decl {
					// A DIRECT member method (`mut func insert`) is resolved by its own
					// name span; a method reached through a nested `impl Iface { .. }`
					// block is resolved by the INTERFACE's span (mirrors the top-level
					// `ImplFor` branch's `for_interface.0.1` match key), so accept either.
					let hit = members
						.iter()
						.any(|m| matches!(&m.0, ImplMember::Func { meta, .. } if meta.name.1 == span))
						|| impls.iter().any(|si| {
							si.0.interface.0.1 == span
								|| si
									.0
									.members
									.iter()
									.any(|m| matches!(&m.0, ImplMember::Func { meta, .. } if meta.name.1 == span))
						});
					if hit {
						return Some(RuntimeDispatch::OntoClass {
							method: res.method.clone(),
						});
					}
				}
			}
		}
		// An ambient `core` prelude struct's method reached through a nested
		// `impl Iface { .. }` block by the INTERFACE's span — i.e. an INHERITED interface
		// DEFAULT the struct doesn't override (an iterator adapter's `fold`/`to_list`/
		// `count`/`map` on `Mapped`). The struct's class is emitted on demand (task #24),
		// and a default body is pure Nymph over the interface's surface (never the
		// struct's own `external`/generic-operator members), so a plain `recv.method(..)`
		// call is safe; record a demand so the class is lowered even when the struct
		// is reached ONLY through this call (no `HirExpr::New` for the body scan to find).
		// Restricted to a method the struct's impl does NOT itself provide as a member —
		// i.e. a pure INHERITED default (`fold`/`to_list`/…), never an abstract interface
		// method the struct implements with its OWN body. A struct's own body may
		// transitively call an unlinked `external` (`Set::insert`) or use an erased-generic
		// operator (`Range::contains`'s `this.start <= item`, ALSO reached through its
		// interface span since `contains` is abstract in `RangeBounds`), neither of which
		// is lowerable — those keep the loud defer below.
		for module in self.prelude_modules {
			for decl in &module.members {
				if let Declaration::Struct { name, impls, .. } = decl
					&& impls.iter().any(|si| {
						si.0.interface.0.1 == span
							&& !si
								.0
								.members
								.iter()
								.any(|m| matches!(&m.0, ImplMember::Func { meta, .. } if meta.name.0 == res.method))
					}) {
					self
						.runtime_struct_demands
						.borrow_mut()
						.insert(name.0.clone());
					return Some(RuntimeDispatch::OntoClass {
						method: res.method.clone(),
					});
				}
			}
		}
		for module in self.prelude_modules {
			for decl in &module.members {
				match decl {
					Declaration::ImplFor {
						generics,
						mutable,
						type_,
						for_interface,
						members,
						..
					} => {
						if for_interface.0.1 != span {
							continue;
						}
						// Found the exact impl the checker resolved through —
						// definitive either way (a `Span` is a unique source
						// position; two DIFFERENT impls, even across distinct
						// prelude modules, can never share one after
						// `offset_module`'s per-module offsetting), so every exit
						// from here on is final; no need to keep scanning.
						let iface_name: &Ident = &for_interface.0;
						// An impl's own external member overrides an interface
						// default exactly like an impl-owned Nymph body does. Check
						// it before `resolve_impl_for_source`, which otherwise skips
						// external members and falls back to the default body.
						if let Some(marker) = members.iter().find_map(|m| match &m.0 {
							ImplMember::ExternalFunc(_, marker, meta) if meta.name.0 == res.method => {
								Some(marker)
							}
							_ => None,
						}) {
							let receiver_tag = inherent_self_type_tag(&type_.0, *mutable);
							return nymph_hir::linkage::lookup(marker, receiver_tag.as_deref())
								.map(RuntimeDispatch::LinkedExtern);
						}
						// The impl's own members first (mirrors `method_signature`'s
						// preference order in `solve.rs`) — e.g. `Negate for int`'s
						// own `negate`, or `Comparable for boolean`'s own
						// `compare_to`; falls back to the interface's own default
						// body, exactly like `push_unoverridden_defaults`. Shared
						// with Gap 2's inner-sibling-call fallback below via
						// `resolve_impl_for_source`.
						let Some((owner_generics, meta, body)) = self.resolve_impl_for_source(
							&iface_name.0,
							members,
							generics.as_slice(),
							&res.method,
						) else {
							// The interface declares no default for it either (a
							// `MethodSource` this compiler didn't expect for a
							// concrete impl) — genuinely unlowerable.
							return None;
						};
						if self.body_calls_unlinked_external(body) {
							return None;
						}
						if let Some(tag) = inherent_self_type_tag(&type_.0, *mutable) {
							return Some(self.finish_runtime_impl_lowering(
								&iface_name.0,
								&tag,
								members,
								generics.as_slice(),
								owner_generics,
								meta,
								body,
								&res.method,
							));
						}
						// Not a primitive target — this slice's extension: a
						// NAMED prelude enum (e.g. `impl<T> Unwrap<Output = T>
						// for Option<T>`) lowers ONTO that enum's own
						// class instead of a mangled top-level function. Guarded
						// by `is_prelude_enum` (not just "is a bare
						// `Type::Reference`") so a blanket impl's target (a
						// `Type::Reference` naming the impl's OWN generic
						// parameter, e.g. `Comparable`'s blanket) stays exactly
						// as loud as before — a generic parameter's name is
						// never also a real prelude enum's.
						if let Type::Reference {
							name: target_name, ..
						} = &type_.0
							&& self.is_prelude_enum(&target_name.0)
						{
							return Some(self.demand_onto_class(target_name.0.clone(), res.method.clone()));
						}
						return None;
					}
					Declaration::Impl {
						generics,
						mutable,
						type_,
						members,
						..
					} => {
						// Inherent-impl match key: the resolved METHOD's OWN
						// name span (`resolve_inherent`/`method_span`), not any
						// impl-level span — a bare `impl Type { .. }` has no
						// interface to span. `find_map` also has to identify an
						// `ExternalFunc` hit (no body at all) as distinct from no
						// hit — `Ok`/`Err` tells those apart without a second
						// pass over `members`.
						let hit = members.iter().find_map(|m| match &m.0 {
							ImplMember::Func { meta, body, .. } if meta.name.1 == span => Some(Ok((meta, body))),
							ImplMember::ExternalFunc(_, marker, meta) if meta.name.1 == span => Some(Err(marker)),
							_ => None,
						});
						let Some(hit) = hit else {
							continue;
						};
						// The span is a unique source position, so this IS the
						// exact member the checker resolved through — every exit
						// from here on is final, same as the `ImplFor` branch.
						let (meta, body) = match hit {
							Ok(mb) => mb,
							Err(marker) => {
								// `external`/`external(name)` — every stdlib
								// collection intrinsic (`list.push`/`get`/`length`/…,
								// `map.get`/`insert`/`size`/…) lands here. Gap 3
								// (L0/L1): if the marker IS in the linkage registry
								// FOR THIS RECEIVER, it lowers as a
								// linked-external call instead of a loud defer — no
								// JS binding is emitted for any OTHER `external`
								// name/receiver pairing until it too gains a
								// registry entry, so lowering a call to one of
								// those would compile clean and throw at runtime.
								// This IS the one place with both the marker AND the
								// concrete receiver `type_` in scope, so it's the
								// only place that can disambiguate an ambiguous
								// marker like `get` (shared by `List` and `Map`,
								// different JS implementations) — see
								// `nymph_hir::linkage`'s own doc comment.
								let receiver_tag = inherent_self_type_tag(&type_.0, *mutable);
								return nymph_hir::linkage::lookup(marker, receiver_tag.as_deref())
									.map(RuntimeDispatch::LinkedExtern);
							}
						};
						if let Some(tag) = inherent_self_type_tag(&type_.0, *mutable) {
							let mangled: EcoString = format!("$std$${tag}${}", res.method).into();
							let owner = self.canonical_owner_of(meta);
							if self.body_calls_unlinked_external(body) {
								// A real Nymph body that itself transitively calls an
								// external instance method (e.g. `list`/`map`'s own
								// `is_empty` calling `this.length()`/`this.size()`) is
								// just as unlowerable as if it were `external`
								// itself — see `body_calls_unlinked_external`'s doc
								// comment.
								return None;
							}
							if !self.register_runtime_func(&mangled, &owner, meta) {
								return Some(RuntimeDispatch::TopLevel(mangled));
							}
							self
								.runtime_func_demands
								.borrow_mut()
								.push(RuntimeFuncDemand {
									mangled: mangled.clone(),
									canonical_owner: owner,
									owner_generics: generics.as_slice(),
									meta,
									body,
									// D1: an inherent method's own name span IS its
									// `impl_span`, so an inner sibling call already
									// routes correctly by span alone — no frame needed.
									sibling_frame: None,
								});
							return Some(RuntimeDispatch::TopLevel(mangled));
						}
						// This slice's extension: a top-level inherent impl
						// targeting a named prelude enum (e.g. `impl<T:
						// Default> Option<T> { func unwrap_or_default() = .. }`,
						// or convert.nym's `impl<T, E> Result<T, E> { func
						// ok() = .. }`) lowers onto that enum's own
						// class, same as the `ImplFor` branch above.
						if self.body_calls_unlinked_external(body) {
							return None;
						}
						if let Type::Reference {
							name: target_name, ..
						} = &type_.0
							&& self.is_prelude_enum(&target_name.0)
						{
							return Some(self.demand_onto_class(target_name.0.clone(), res.method.clone()));
						}
						return None;
					}
					_ => continue,
				}
			}
		}
		// Gap 2: the span-scan above locates the exact `ImplFor`/`Impl` block a
		// call resolves through only when `res.impl_span` names that block
		// directly — which is true for the OUTER call to a lowered
		// default (`for_interface`'s own span) and for any inherent-impl call
		// (D1: a method's own name span IS its `impl_span`), but NOT for an
		// INNER `this.<method>()` call written inside an interface default
		// body itself: the checker types every default body exactly once,
		// generically, against a rigid synthetic `this` owned by the
		// INTERFACE declaration (`check_interface_default_body`), so that
		// inner call's `impl_span` always points at the interface's own name
		// span — never at any concrete `impl .. for ..` block's span, no
		// matter which concrete type the whole body is currently being
		// lowered onto. `Lowerer::lower_runtime_func` pushes the OUTER
		// lowering's own `(interface, tag, impl members/generics)`
		// context onto `lowering_runtime_sibling` for exactly the body's
		// lowering duration — fall back to that here, resolving the sibling
		// directly against the SAME impl block the outer call already found,
		// instead of failing. Gated on `this_receiver` (see this function's
		// own doc comment) — a call through some OTHER expression that merely
		// satisfies the same interface bound must NOT trust the frame's
		// concrete tag. Empty (and this is skipped) everywhere else,
		// including while lowering an inherent `Impl` body, which never
		// pushes a frame in the first place (D1 needs none).
		if !this_receiver {
			return None;
		}
		let frame = self.lowering_runtime_sibling.borrow().last().map(|f| {
			(
				f.iface_name.clone(),
				f.tag.clone(),
				f.members,
				f.impl_generics,
			)
		});
		if let Some((iface_name, tag, members, impl_generics)) = frame {
			if let Some(marker) = members.iter().find_map(|m| match &m.0 {
				ImplMember::ExternalFunc(_, marker, meta) if meta.name.0 == res.method => Some(marker),
				_ => None,
			}) {
				return nymph_hir::linkage::lookup(marker, Some(tag.as_str()))
					.map(RuntimeDispatch::LinkedExtern);
			}
			let (owner_generics, meta, body) =
				self.resolve_impl_for_source(&iface_name, members, impl_generics, &res.method)?;
			if self.body_calls_unlinked_external(body) {
				// A sibling that is itself external, or transitively calls an
				// unlinked external, stays exactly as loud a defer as any
				// other unlowerable body (Gap 3 stays isolated) — never
				// silently lowered.
				return None;
			}
			return Some(self.finish_runtime_impl_lowering(
				&iface_name,
				&tag,
				members,
				impl_generics,
				owner_generics,
				meta,
				body,
				&res.method,
			));
		}
		None
	}

	/// Resolve `method` against an `ImplFor` block's own `members` first,
	/// falling back to `iface_name`'s own interface-default body — exactly
	/// `push_unoverridden_defaults`'s preference order. Shared by
	/// `try_lower_runtime_dispatch`'s `ImplFor` arm (the OUTER call) and
	/// its Gap 2 sibling-frame fallback above (an INNER `this.<method>()`
	/// call inside a lowered default body) so both resolve a method name
	/// against the identical impl the SAME way. Returns `None` for an
	/// `external`/`external(name)` member (no `ImplMember::Func` with a real
	/// body matched `method`) or when the interface declares no default for
	/// it either — genuinely unlowerable, same as before this slice.
	fn resolve_impl_for_source(
		&self,
		iface_name: &EcoString,
		members: &'a [Spanned<ImplMember>],
		impl_generics: &'a [Spanned<GenericParam>],
		method: &EcoString,
	) -> Option<(&'a [Spanned<GenericParam>], &'a FuncDeclaration, &'a Expr)> {
		let own_member = members.iter().find_map(|m| match &m.0 {
			ImplMember::Func { meta, body, .. } if meta.name.0 == *method => {
				Some((impl_generics, meta, body))
			}
			_ => None,
		});
		own_member.or_else(|| {
			self
				.interfaces_by_name
				.get(iface_name)
				.and_then(|(iface_generics, iface_members)| {
					use nymph_ast::decl::InterfaceElement;
					iface_members.iter().find_map(|m| {
						let InterfaceMember::Element(element) = &m.0 else {
							return None;
						};
						let InterfaceElement::Func {
							meta,
							body: Some(body),
						} = &element.0
						else {
							return None;
						};
						(meta.name.0 == *method).then_some((*iface_generics, meta, body))
					})
				})
		})
	}

	/// Mangle `method` under the canonical `$std$<iface_name>$<tag>$<method>`
	/// scheme, dedup via `runtime_funcs_seen`, and — unless already
	/// seen/queued — enqueue `RuntimeFuncDemand` (carrying a
	/// `RuntimeSiblingFrame` so `lower_runtime_func` can push Gap 2's own
	/// sibling-dispatch context while lowering IT), returning
	/// `RuntimeDispatch::TopLevel` either way. Shared by
	/// `try_lower_runtime_dispatch`'s `ImplFor` arm and Gap 2's
	/// sibling-frame fallback — both, by construction, only ever call this
	/// AFTER `resolve_impl_for_source` + `body_calls_unlinked_external` have
	/// already confirmed `meta`/`body` are real and external-free.
	// 8 real parameters pushes this over clippy's default 7-argument
	// threshold — mirrors the `#[allow]` already on `collect_adt_methods`
	// (and, before it, `members.rs`'s `commit_inherent`/`resolve_inherent`)
	// for the identical reason: a single coherent operation whose parameters
	// are each independently necessary (both mangling-scheme segments, the
	// two DIFFERENT generic scopes `RuntimeFuncDemand`/`RuntimeSiblingFrame`
	// need, and the already-resolved `meta`/`body`), not a sign this should
	// be split up.
	#[allow(clippy::too_many_arguments)]
	fn finish_runtime_impl_lowering(
		&self,
		iface_name: &EcoString,
		tag: &str,
		members: &'a [Spanned<ImplMember>],
		impl_generics: &'a [Spanned<GenericParam>],
		owner_generics: &'a [Spanned<GenericParam>],
		meta: &'a FuncDeclaration,
		body: &'a Expr,
		method: &EcoString,
	) -> RuntimeDispatch {
		let mangled: EcoString = format!("$std${iface_name}${tag}${method}").into();
		let canonical_owner = self.interface_owner(iface_name);
		if !self.register_runtime_func(&mangled, &canonical_owner, meta) {
			return RuntimeDispatch::TopLevel(mangled);
		}
		self
			.runtime_func_demands
			.borrow_mut()
			.push(RuntimeFuncDemand {
				mangled: mangled.clone(),
				canonical_owner,
				owner_generics,
				meta,
				body,
				sibling_frame: Some(RuntimeSiblingFrame {
					iface_name: iface_name.clone(),
					tag: tag.into(),
					members,
					impl_generics,
				}),
			});
		RuntimeDispatch::TopLevel(mangled)
	}

	/// Whether `name` is a prelude ENUM's own declared name — used to gate
	/// [`Self::try_lower_runtime_dispatch`]'s named-type extension so it
	/// only ever promises `RuntimeDispatch::OntoClass` for a receiver this
	/// compiler can actually lower a class for (`lower_runtime_enum`
	/// only ever handles `Declaration::Enum`; there is no prelude-STRUCT
	/// equivalent). Also correctly excludes a blanket impl's target (a
	/// `Type::Reference` naming the impl's OWN generic parameter) — a generic
	/// parameter's name is never also a real prelude enum's.
	fn is_prelude_enum(&self, name: &EcoString) -> bool {
		self.prelude_modules.iter().any(|m| {
			m.members
				.iter()
				.any(|decl| matches!(decl, Declaration::Enum { name: n, .. } if n.0 == *name))
		})
	}

	/// Record a lowering DEMAND for `method` onto prelude enum
	/// `enum_name`'s class (`runtime_enum_method_demands`) and return the
	/// `RuntimeDispatch::OntoClass` value every one of
	/// [`Self::try_lower_runtime_dispatch`]'s named-enum exits
	/// constructs — centralizing the demand-recording here (rather than at
	/// each of that function's three named-enum return sites, or at every
	/// call site consuming its result) keeps "returning `OntoClass` always
	/// means the demand is already recorded" a single invariant instead of a
	/// convention every caller must remember.
	fn demand_onto_class(&self, enum_name: EcoString, method: EcoString) -> RuntimeDispatch {
		self
			.runtime_enum_method_demands
			.borrow_mut()
			.entry(enum_name)
			.or_default()
			.insert(method.clone());
		RuntimeDispatch::OntoClass { method }
	}

	/// While lowering a prelude enum's OWN method body — `lower_runtime_enum`
	/// bumped `lowering_onto_runtime_owner` and pushed the enum's name onto
	/// `current_runtime_owner_lowering` for the duration — an inner `this.method()`/
	/// `this as T`/`this op other` dispatch that falls through to the ordinary
	/// class-method fast path (every `lowering_onto_runtime_owner`-gated call site)
	/// needs the SIBLING method's demand recorded too, so demand-only
	/// `lower_runtime_enum` knows to lower it (Sub-problem #1, inner
	/// dispatch). A no-op when NOT currently lowering a prelude enum:
	/// `push_unoverridden_defaults` lowering an interface default onto a
	/// plain USER class leaves `current_runtime_owner_lowering` empty — that
	/// class's methods are already fully, eagerly lowered, nothing to
	/// demand.
	fn record_inner_runtime_enum_method_demand(&self, method: &EcoString) {
		if let Some(enum_name) = self.current_runtime_owner_lowering.borrow().last() {
			self
				.runtime_enum_method_demands
				.borrow_mut()
				.entry(enum_name.clone())
				.or_default()
				.insert(method.clone());
		}
	}

	/// Whether `body` — a candidate prelude impl/interface-default body
	/// [`Self::try_lower_runtime_dispatch`] is deciding whether to
	/// lower — itself calls an unlinked `external`/`external(name)`
	/// anywhere within it, either as a bare top-level function (e.g.
	/// `Comparable for string`'s own `compare_to`, which calls the free
	/// `external(compare_to_string) func compare_to_string(..)` declared at
	/// `stdlib/src/ops/mod.nym:277`) OR as an external INSTANCE method
	/// reached through `this`/a receiver (e.g. `stdlib/src/collections/
	/// list.nym`'s `is_empty`, whose body is `this.length() == 0` and
	/// `length` is itself `external(length)` in the SAME impl block — the
	/// collections-lowering extension's addition, so a pure-Nymph body
	/// one call deep from an intrinsic doesn't get lowered and then
	/// panic/throw mid-body instead of deferring cleanly here). Such a call
	/// can never be emitted correctly by this lowering: an `external`
	/// declaration is a checker-side intrinsic signature only — "stdlib
	/// linkage" (binding it to a real JS implementation, e.g.
	/// `stdlib/src/ops/comparison.ts`'s `compare_to_string`, or
	/// `stdlib/src/collections/list.ts`'s free-function intrinsics taking the
	/// receiver as an explicit first arg) is a still-future slice, so nothing
	/// anywhere in this compiler emits a JS binding for it under that name.
	/// Materializing the CALLING body anyway would compile clean and then
	/// throw a JS `ReferenceError` on the missing name at runtime — exactly
	/// the silent-wrong-JS class of bug this compiler never accepts as a
	/// substitute for a loud deferral (returning `false` here keeps
	/// `try_lower_runtime_dispatch` itself loud instead). Only
	/// `self.prelude_modules`' own top-level AND per-impl-member `external`
	/// names are collected — the only ones a PRELUDE body could possibly
	/// reach for un-qualified/through `this`; a user module never declares one
	/// of its own into a prelude body's scope. Matching is by bare NAME, not
	/// by receiver type (mirrors the pre-existing top-level-function
	/// matching) — deliberately conservative: a false-positive-shaped name
	/// collision only defers a MORE bodies loudly, never mis-lowers one.
	///
	/// Gap 3 (L0/L1) extension: a marker whose `external(MARKER)` is present
	/// in [`nymph_hir::linkage::REGISTRY`] FOR THIS RECEIVER is skipped here —
	/// it now has a real JS binding, so a body that only reaches it no longer
	/// counts as calling an unlinked external. Gated on the MARKER (the
	/// `external(name)` key), not the METHOD name this function collects
	/// (`meta.name.0`) — they coincide for `length` today, but must not be
	/// conflated in general (a future marker could differ from its method
	/// name). The receiver tag matters here exactly as much as it does at the
	/// `LinkedExtern` construction site (`try_lower_runtime_dispatch`):
	/// `map.nym`'s OWN `external(get)` must NOT be treated as linked just
	/// because `list.nym`'s `get` is — a top-level `Declaration::ExternalFunc`
	/// has no receiver at all (tag `None`, matching only an UNAMBIGUOUS
	/// registry entry), an `Impl`/`ImplFor`'s tag comes from its own
	/// `type_`/`mutable`, exactly like `inherent_self_type_tag`'s other
	/// callers.
	fn body_calls_unlinked_external(&self, body: &Expr) -> bool {
		let mut external_names: FxHashSet<&EcoString> = FxHashSet::default();
		for module in self.prelude_modules {
			for decl in &module.members {
				match decl {
					Declaration::ExternalFunc(_, marker, meta) => {
						if nymph_hir::linkage::lookup(marker, None).is_none() {
							external_names.insert(&meta.name.0);
						}
					}
					Declaration::Impl {
						type_,
						mutable,
						members,
						..
					}
					| Declaration::ImplFor {
						type_,
						mutable,
						members,
						..
					} => {
						let receiver_tag = inherent_self_type_tag(&type_.0, *mutable);
						for m in members {
							if let ImplMember::ExternalFunc(_, marker, meta) = &m.0
								&& nymph_hir::linkage::lookup(marker, receiver_tag.as_deref()).is_none()
							{
								external_names.insert(&meta.name.0);
							}
						}
					}
					_ => {}
				}
			}
		}
		expr_calls_any_name(body, &external_names)
	}

	/// Collect a struct's or enum's full method list: entries already gathered
	/// into `methods_by_type` from top-level `impl <Name>`/`impl <Interface> for
	/// <Name>` blocks, plus the type's own inner members (inherent `func`s,
	/// `namespace func` statics, `mut func` methods) and its nested
	/// `impl <Interface> { .. }` blocks, each lowering that interface's
	/// un-overridden defaults, Slice 4C-b). Struct and enum bodies share the
	/// identical `(members, impls)` AST shape, so this one path serves both
	/// (Slice 4D, X2).
	/// Returns `(instance methods, static/namespaced methods)`.
	// `demand` (this slice, named-type prelude method lowering) is the
	// 7th real parameter pushing this over clippy's default 7-argument
	// threshold — mirrors the same `#[allow]` already on `members.rs`'s
	// `commit_inherent`/`resolve_inherent` for the identical reason (a single
	// coherent operation whose parameters are each independently necessary,
	// not a sign this should be split up).
	#[allow(clippy::too_many_arguments)]
	fn collect_adt_methods(
		&self,
		type_name: &EcoString,
		owner_generics: &[Spanned<GenericParam>],
		members: &[Spanned<nymph_ast::decl::ImplMember>],
		impls: &[Spanned<nymph_ast::decl::StructImpl>],
		interfaces_by_name: &InterfaceTable,
		methods_by_type: &mut FxHashMap<EcoString, Vec<HirMethod>>,
		demand: Option<&FxHashSet<EcoString>>,
	) -> (Vec<HirMethod>, Vec<HirMethod>) {
		use nymph_ast::decl::{FuncKind, ImplMember};

		let mut methods = methods_by_type.remove(type_name).unwrap_or_default();
		let mut statics = Vec::new();
		// Flat members: instance `func` and `mut func` become instance methods;
		// `namespace func` becomes a static. A `namespace func` is checked with
		// `self_ty: None` (a bare namespaced body never sees `this`, rejected
		// loudly by `TypeError::ThisOutsideMethod` otherwise), and `mut func`
		// carries no extra checker restriction beyond an ordinary instance method
		// (Task #1, mutable types, will add real enforcement) — so `lower_method`
		// is checker-faithful for all three (it only lowers a `this` the body
		// actually contains).
		//
		// `demand`, when `Some` (this slice, `lower_runtime_enum` only —
		// every OTHER caller passes `None`, meaning "lower every member
		// eagerly", exactly the pre-existing behavior for a user-declared
		// struct/enum), restricts this to DEMAND-ONLY lowering: skip any inline
		// member not named in the demand set. See `runtime_enum_method_demands`'s
		// doc comment for why demand-only lowering is necessary at all
		// (`Option`'s own `map_or_default`/`unwrap_or_default` have no
		// compilable JS form and must not be lowered merely because `Option`
		// itself was referenced).
		for member in members {
			match &member.0 {
				ImplMember::Func { meta, body, .. } => {
					if demand.is_some_and(|d| !d.contains(&meta.name.0)) {
						continue;
					}
					if meta.kind == FuncKind::Namespace {
						statics.push(self.lower_method(owner_generics, meta, body));
					} else {
						methods.push(self.lower_method(owner_generics, meta, body));
					}
				}
				other => {
					panic!("slice-4a lowering does not yet handle struct inner member {other:?}")
				}
			}
		}
		// Nested `impl Iface { .. }` blocks: an override's own scope is the OWNING
		// struct/enum's generics chained with the impl block's own (Slice 4J,
		// Task 1 Finding 3 fix) — mirrors `collect_inner_impl`'s `combined`
		// (iface.rs), the exact scope the checker used to check these bodies.
		for m in impls {
			let interface = &m.0.interface;
			let impl_generics = &m.0.generics;
			let impl_members = &m.0.members;
			let combined: Vec<Spanned<GenericParam>> = owner_generics
				.iter()
				.chain(impl_generics)
				.cloned()
				.collect();
			let mut overridden: FxHashSet<EcoString> = FxHashSet::default();
			for member in impl_members {
				match &member.0 {
					ImplMember::Func { meta, body, .. } => {
						overridden.insert(meta.name.0.clone());
						let method = self.lower_method(&combined, meta, body);
						Self::push_protocol_impl_alias(&interface.0.0, &method, &mut methods);
						methods.push(method);
					}
					other => panic!("slice-4b lowering does not yet handle impl member {other:?}"),
				}
			}
			self.push_unoverridden_defaults(&interface.0, &overridden, interfaces_by_name, &mut methods);
		}
		// V4: two interfaces (or an override and a same-named default)
		// lowering the same method name on one type is a real ambiguity
		// codegen cannot silently resolve (JS would just let the last one win) —
		// panic loudly, naming the type and method.
		self.assert_no_duplicate_methods(type_name, &methods);
		self.assert_no_duplicate_methods(type_name, &statics);
		(methods, statics)
	}

	/// V4: panic loudly, naming the struct and the offending method, if two
	/// lowered/overridden methods share a name on one class — two interfaces
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

	/// Slice 4J, Task 1: an enum's namespaced static named the same as one of
	/// its own variants would put two entries under the same key on the
	/// object every `Type.name` call site resolves against (`E.V` for a
	/// variant, `E.func()` for a static — both properties of the exact same
	/// returned object, see `emit_enum`), and JS would let the later one
	/// silently win. Panic loudly instead, naming the enum and the collision —
	/// mirrors [`Self::assert_no_duplicate_methods`]'s "never miscompile
	/// silently" floor, extended to the one hazard specific to enums (structs
	/// have no variants to collide with).
	fn assert_no_variant_static_collision(
		&self,
		enum_name: &EcoString,
		variants: &[HirVariant],
		statics: &[HirMethod],
	) {
		let variant_names: FxHashSet<&EcoString> = variants.iter().map(|v| &v.name).collect();
		for s in statics {
			assert!(
				!variant_names.contains(&s.name),
				"slice-4j lowering: enum `{enum_name}` has a namespaced function named `{}` that collides with a variant of the same name",
				s.name
			);
		}
	}

	fn lower_func(&self, meta: &FuncDeclaration, body: &Expr) -> HirFunc {
		// Params and the body block's own `let`s share ONE JS scope (emit flattens
		// both into the same function body, see `emit_func`) — push a single scope,
		// seed it with the params, then lower the body into that same scope rather
		// than letting it push its own (Slice 4E, Y2).
		self.push_scope();
		self.push_generics(&[], &meta.generics);
		let params = meta
			.params
			.iter()
			.map(|p| self.declare(&param_name(&p.0.name)))
			.collect();
		let body = self.lower_func_body(body);
		self.pop_generics();
		self.pop_scope();
		HirFunc {
			name: meta.name.0.clone(),
			params,
			body,
		}
	}

	/// Lower one inherent instance method (mirrors [`Self::lower_func`]). `this` in
	/// the body lowers to [`HirExpr::This`]. Also covers namespaced (`static`)
	/// functions and `mut func` methods — same AST shape (`FuncDeclaration` +
	/// body), same lowering (Slice 4J).
	///
	/// `owner_generics` is the full generic scope the checker put in effect
	/// while checking this method's body BEYOND the method's own generics —
	/// the struct/enum's own declared generics for an inherent/namespaced/
	/// `mut func` member (`check_method_body`'s `owner_generics`, members.rs),
	/// the struct/enum's generics chained with a nested `impl Iface { .. }`
	/// block's own (`collect_inner_impl`'s `combined`, iface.rs), a top-level
	/// `impl<G> Type { .. }`/`impl<G> Iface for Type { .. }` block's own
	/// generics, or an interface's own generics for a lowered default
	/// body (`check_interface_default_body`'s `iface_generics`, members.rs).
	/// Every caller must pass the SAME scope the checker used, or a namespaced
	/// call through an owner-generic type parameter silently lowers to
	/// unbound JS instead of tripping [`Self::is_current_generic`] (Slice 4J,
	/// Task 1 Finding 3 fix).
	fn lower_method(
		&self,
		owner_generics: &[Spanned<GenericParam>],
		meta: &FuncDeclaration,
		body: &Expr,
	) -> HirMethod {
		self.push_scope();
		self.push_generics(owner_generics, &meta.generics);
		let params = meta
			.params
			.iter()
			.map(|p| self.declare(&param_name(&p.0.name)))
			.collect();
		let body = self.lower_func_body(body);
		self.pop_generics();
		self.pop_scope();
		HirMethod {
			name: meta.name.0.clone(),
			params,
			body,
		}
	}

	fn push_protocol_impl_alias(interface: &str, method: &HirMethod, out: &mut Vec<HirMethod>) {
		let alias = match (interface, method.name.as_str()) {
			("Display", "display") => "$nymph$display",
			("Debug", "debug") => "$nymph$debug",
			_ => return,
		};
		let mut protocol_method = method.clone();
		protocol_method.name = alias.into();
		out.push(protocol_method);
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

	/// Lower a closure expression (Slice 4L, JJ1) into `HirExpr::Closure`. Mirrors
	/// `lower_func`/`lower_func_body`: one JS scope seeded with the params, then
	/// the body lowered into that same scope (a `Block` body shares it rather than
	/// pushing its own, so a body `let` and a param share one JS scope exactly the
	/// way emit flattens an arrow's body). Free variables inside `body` resolve
	/// through the scope stack past this new frame into whatever OUTER scopes are
	/// still active — which is exactly capture-by-reference, and for free for a
	/// shadowed outer binding's Y2 rename (an outer `x` renamed to `x$1` resolves
	/// to `x$1` inside the closure too).
	///
	/// Deliberately does NOT push a `generics_stack` frame (Slice 4J, Task 1
	/// Finding 3's machinery is untouched here): `is_current_generic` checks only
	/// the innermost frame, and the checker resolves a namespaced call
	/// (`T.default()`) inside a closure body against the ENCLOSING func/method's
	/// generic scope (probe: `() -> T.default()` inside a generic func
	/// typechecks clean) — pushing an empty closure frame would blind that
	/// namespaced-call guard and silently emit unbound JS instead of the loud
	/// panic it exists to give.
	///
	/// `closure_depth` is bumped around the body lowering so every `return` sink
	/// (`lower_block`'s statement interception, `lower_branch`'s unbraced wrap,
	/// and `lower_expr`'s own subexpression-position `Return` arm, which already
	/// panics unconditionally) panics rather than silently emitting a `return`
	/// the checker never actually typed against this closure (see the
	/// `closure_depth` field doc for why).
	fn lower_closure(
		&self,
		params: &[Spanned<nymph_ast::expr::ClosureParam>],
		body: &Expr,
	) -> HirExpr {
		for p in params {
			assert!(
				!p.0.spread,
				"slice-4l lowering does not support a spread closure parameter (the checker never reads `ClosureParam::spread` either)"
			);
		}
		self.push_scope();
		let params = params
			.iter()
			.map(|p| self.declare(&param_name(&p.0.name)))
			.collect();
		self.closure_depth.set(self.closure_depth.get() + 1);
		let body = self.lower_func_body(body);
		self.closure_depth.set(self.closure_depth.get() - 1);
		self.pop_scope();
		HirExpr::Closure {
			params,
			body: Box::new(body),
		}
	}

	/// Thin wrapper around [`Self::lower_expr_inner`]: intercepts a committed
	/// anonymous-closure (`$N`) boundary (Slice: `$N` anonymous closure params)
	/// BEFORE the ordinary per-kind lowering, wrapping `expr` as a synthesized
	/// closure instead — see [`Self::lower_anon_closure`]. The split mirrors
	/// `Checker::check`/`Checker::infer` vs. `check_dispatch`/`infer_dispatch`
	/// in `infer_expr.rs`: `lower_anon_closure` must dispatch `expr`'s OWN kind
	/// through `lower_expr_inner` directly, not back through this wrapper, or
	/// it would just re-hit this same interception and recurse forever.
	fn lower_expr(&self, expr: &Expr) -> HirExpr {
		match self.annotations.anon_boundary_arity(expr.id) {
			Some(arity) => self.lower_anon_closure(expr, arity),
			None => self.lower_expr_inner(expr),
		}
	}

	/// Lower the closure committed at `expr` (see [`Self::lower_expr`]):
	/// mirrors [`Self::lower_closure`] exactly — one fresh JS scope seeded
	/// with `arity` synthesized `anon$0`, `anon$1`, … params, `closure_depth`
	/// bumped around the body (so a stray `return` inside still panics loudly
	/// rather than silently emit an arrow-scoped one — see that field's doc
	/// comment), then `expr` itself lowered as the closure's body through
	/// `lower_expr_inner` (its OWN kind, e.g. the `BinaryOp` a boundary like
	/// `$ % 2 == 0` actually is) rather than through `lower_expr` again.
	fn lower_anon_closure(&self, expr: &Expr, arity: u8) -> HirExpr {
		self.push_scope();
		let params = (0..arity)
			.map(|i| self.declare(&anon_param_name(i)))
			.collect();
		self.closure_depth.set(self.closure_depth.get() + 1);
		let body = self.lower_expr_inner(expr);
		self.closure_depth.set(self.closure_depth.get() - 1);
		self.pop_scope();
		HirExpr::Closure {
			params,
			body: Box::new(body),
		}
	}

	fn lower_expr_inner(&self, expr: &Expr) -> HirExpr {
		match &expr.kind {
			ExprKind::Int(v) => HirExpr::Num(v.0 as f64, self.num_kind(expr.id, NumKind::Int)),
			ExprKind::UInt(v) => HirExpr::Num(v.0 as f64, self.num_kind(expr.id, NumKind::UInt)),
			ExprKind::Float(v) => HirExpr::Num(v.0.into_inner(), self.num_kind(expr.id, NumKind::Float)),
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
			// While lowering a lowered prelude body as a top-level mangled
			// function (stdlib body lowering slice, gap b), `this`
			// substitutes to that function's own receiver param instead of the
			// meaningless top-level `HirExpr::This` — see `this_sub`'s doc comment.
			ExprKind::This => match self.this_sub.borrow().last() {
				Some(name) => HirExpr::Local(name.clone()),
				None => HirExpr::This,
			},
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
				}
				// A namespaced/static call (`Type.func(..)`) through a generic type
				// parameter of the enclosing func/method (`func make<T: Default>():
				// T = T.default()`, checked via `resolve_param_namespaced` against
				// `T`'s bound) — `T` names no struct/enum and has no JS binding at
				// all; falling through to the generic `HirExpr::Call` arm below
				// would silently lower `parent` (an `Identifier("T")`) into
				// `HirExpr::Local("T")` and emit a bare, unbound `T.default()` in
				// the output JS (Slice 4J, Task 1 Finding 3 — a pre-existing
				// silent-wrong-JS hole, confirmed by probe). Panic loudly instead.
				else if let ExprKind::MemberAccess { parent, member, .. } = &func.kind
					&& let ExprKind::Identifier(name) = &parent.kind
					&& self.is_current_generic(&name.0)
				{
					panic!(
						"slice-4j lowering does not yet support a namespaced call through a generic type parameter (`{}.{}(..)`) — `{}` has no JS binding",
						name.0, member.0, name.0
					)
				}
				// A plain method call (`receiver.method(args…)`) the checker resolved
				// through the interface solver (Finding 2, stdlib linkage groundwork):
				// consult the same `Resolution`/`DispatchKind` operator dispatch already
				// reads (`lower_operator`/`lower_prefix_op`) before trusting the method
				// exists. `dispatch_kind_for_method_call` (`infer_expr.rs`) only ever
				// tags a plain call `UserImplDefaultMethod` when the matched impl was
				// cloned from an offset prelude module (never lowered by
				// `compile_with_prelude`, which lowers only the user's own AST) —
				// unlike an operator, a still-generic (`GenericBound`) receiver is safe
				// here (type erasure + duck typing: the emitted call needs only the
				// literal method name, already known regardless of dispatch source).
				// Refuse loudly rather than emit a call to a method that doesn't exist
				// on the emitted class. A call with no recorded `Resolution` at all
				// (every other call shape: constructors, namespaced calls, plain
				// function calls) falls through unchanged.
				else if let ExprKind::MemberAccess { parent, .. } = &func.kind
					&& let Some(res) = self.annotations.resolution_of(expr.id)
					&& res.dispatch == DispatchKind::UserImplDefaultMethod
					// While lowering a default body ONTO A CONCRETE CLASS (gap
					// a), an inner `this.method(..)` call falls through to the
					// ordinary `Field`+`Call` lowering below instead — see
					// `lowering_onto_runtime_owner`'s doc comment for why that's safe
					// and gap b's mangled-function path is the wrong shape here.
					// Gated on the receiver actually BEING `this` (`is_this_receiver`)
					// — a default body dispatching through its own non-`this`
					// generic parameter (e.g. `other.plus(other)`) is NOT safe to
					// treat as an ordinary class method call even while
					// lowering onto a class, since that parameter may be bound
					// to a primitive with no such JS method at all.
					&& self.lowering_onto_runtime_owner.get() > 0
					&& Self::is_this_receiver(parent)
				{
					// This slice: if we're additionally lowering a PRELUDE
					// ENUM (not a plain user class), record a demand for the
					// sibling method too — see `record_inner_runtime_enum_method_demand`'s
					// doc comment.
					self.record_inner_runtime_enum_method_demand(&res.method);
					HirExpr::Call {
						callee: Box::new(self.lower_expr(func)),
						args: args.iter().map(|a| self.lower_expr(&a.0.value)).collect(),
					}
				} else if let ExprKind::MemberAccess { parent, .. } = &func.kind
					&& let Some(res) = self.annotations.resolution_of(expr.id)
					&& matches!(
						res.dispatch,
						DispatchKind::UserImpl | DispatchKind::UserImplDefaultMethod
					) && !Self::is_this_receiver(parent)
					&& self.receiver_is_still_generic(parent)
					&& let [argument] = args.as_slice()
				{
					self
						.lower_bound_dispatch(res, parent, &argument.0.value)
						.unwrap_or_else(|| {
							panic!(
								"cannot lower generic-bound dispatch for method `{}`",
								res.method,
							)
						})
				} else if let ExprKind::MemberAccess { parent, .. } = &func.kind
					&& let Some(res) = self.annotations.resolution_of(expr.id)
					&& res.dispatch == DispatchKind::UserImplDefaultMethod
					&& !Self::is_this_receiver(parent)
					&& self.receiver_is_still_generic(parent)
					&& !self.method_is_externally_backed_in_prelude(&res.method)
				{
					// A method call dispatched through a STILL-GENERIC bound whose receiver
					// is some concrete sub-expression (a field or parameter), NOT `this` —
					// its type head is a type parameter (`S: Iterator`'s
					// `this.source.next()`). There is no single concrete prelude impl to
					// lower, but none is needed: under type erasure the emitted JS is a
					// plain `recv.method(args)` and the concrete object supplied at runtime
					// carries the real method (duck typing) — exactly the "safe" case the
					// operator-dispatch comment above describes. Emit the direct call rather
					// than routing into the concrete-lowering path (which returns
					// `None` and panics). `this`-receivered sibling calls are EXCLUDED: while
					// lowering a prelude default body onto a class or as a mangled
					// top-level function, `this` carries the synthetic `Self` param and must
					// stay on the two branches around this one (they record the sibling
					// demand / build the mangled call), not be short-circuited here.
					HirExpr::Call {
						callee: Box::new(self.lower_expr(func)),
						args: args.iter().map(|a| self.lower_expr(&a.0.value)).collect(),
					}
				} else if let ExprKind::MemberAccess { parent, .. } = &func.kind
					&& let Some(res) = self.annotations.resolution_of(expr.id)
					&& res.dispatch == DispatchKind::UserImplDefaultMethod
				{
					// Stdlib body lowering slice, gap b: a prelude-origin
					// dispatch that lowers to real, self-contained Nymph code (not
					// external/intrinsic, not a still-generic/blanket bound)
					// rewrites to a call on the demand-lowered top-level
					// mangled function (`RuntimeDispatch::TopLevel`) or a plain
					// method call on the demand-lowered named-enum class
					// (`RuntimeDispatch::OntoClass`, this slice) instead of
					// panicking — see `try_lower_runtime_dispatch`'s doc
					// comment for exactly which bodies qualify.
					match self.try_lower_runtime_dispatch(res, Self::is_this_receiver(parent)) {
						Some(RuntimeDispatch::TopLevel(mangled)) => {
							let mut call_args = vec![self.lower_expr(parent)];
							call_args.extend(args.iter().map(|a| self.lower_expr(&a.0.value)));
							HirExpr::Call {
								callee: Box::new(HirExpr::Local(mangled)),
								args: call_args,
							}
						}
						Some(RuntimeDispatch::OntoClass { .. }) => HirExpr::Call {
							callee: Box::new(self.lower_expr(func)),
							args: args.iter().map(|a| self.lower_expr(&a.0.value)).collect(),
						},
						// Gap 3 (L0/L1): a call resolved through a LINKED external
						// lowers to `HirExpr::ExternCall` instead of panicking —
						// `$_this`-first, exactly the shape `RuntimeDispatch::
						// TopLevel`'s mangled-call arm above already builds.
						Some(RuntimeDispatch::LinkedExtern(linked)) => {
							let mut call_args = vec![self.lower_expr(parent)];
							call_args.extend(args.iter().map(|a| self.lower_expr(&a.0.value)));
							HirExpr::ExternCall {
								module: linked.module,
								symbol: linked.symbol,
								args: call_args,
							}
						}
						None => panic!(
							"slice-stdlib-linkage lowering does not yet support dispatching a method call to a method resolved through a prelude-only impl (never lowered onto a class): `{}`",
							res.method
						),
					}
				}
				// A bare call to a top-level `external` func LINKED in the
				// registry (Gap 3, free-function extension) — a `print(x)`-
				// shaped call, receiver-less, so no synthetic `$_this` arg is
				// prepended (unlike the method-call `LinkedExtern` arms above).
				// Checked LAST, after every `MemberAccess`-shaped guard (which
				// can never match an `Identifier` callee anyway) and right
				// before the generic fallback it would otherwise silently hit.
				else if let ExprKind::Identifier(name) = &func.kind
					&& self.is_prelude_external_fn(&name.0)
				{
					// It IS a prelude `external` func, so it MUST be linked: an
					// unregistered marker means the emitted call would reference
					// a JS symbol that has no binding anywhere in the bundle.
					// Panic loudly (a mis-registered stdlib external is a
					// build-time bug), exactly like the method-path
					// `LinkedExtern` arms — never silently fall through to the
					// generic call below.
					match self.lookup_free_fn_external(&name.0) {
						Some(linked) => HirExpr::ExternCall {
							module: linked.module,
							symbol: linked.symbol,
							args: args.iter().map(|a| self.lower_expr(&a.0.value)).collect(),
						},
						None => panic!(
							"lowering: free-function external `{}` is declared in the ambient \
							 prelude but its linkage marker is not registered in \
							 nymph_hir::linkage — register a JS binding for it (otherwise the \
							 emitted call references an undefined symbol)",
							name.0
						),
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
			ExprKind::Tuple(items) => self.lower_tuple(items),
			ExprKind::List(items) => self.lower_list(items),
			ExprKind::Map(entries) => self.lower_map(entries),
			ExprKind::IndexAccess { parent, index, .. } => {
				self.lower_index_access(expr.id, parent, index)
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
			// `value as Type` (Slice 4K) dispatches per its recorded `Resolution`,
			// mirroring `lower_operator`: `BuiltinEager` is a built-in scalar/identity
			// conversion, whose exact JS mapping `lower_scalar_cast` picks from the
			// recorded operand/target types; `UserImpl` dispatches to the resolved
			// `Into`-named interface's own zero-arg method — `res.method`, read off
			// the interface's declared methods by `check_cast` rather than assumed to
			// be literally "into" (a local `interface Into<Other> { func convert(): Other }`
			// dispatches to `convert`; see `check_cast`'s doc). Any other dispatch, or
			// no resolution at all (an unresolved cast that should have been
			// diagnosed by the checker, e.g. `TypeError::CastRequiresInto`/
			// `CannotCast`/`IntoInterfaceMalformed`), panics loudly rather than
			// silently emitting the bare operand — never a lowering deferral.
			ExprKind::TypeOp { lhs, .. } => match self.annotations.resolution_of(expr.id) {
				Some(res) if res.dispatch == DispatchKind::BuiltinEager => {
					self.lower_scalar_cast(expr.id, lhs)
				}
				Some(res) if res.dispatch == DispatchKind::UserImpl => HirExpr::Call {
					callee: Box::new(HirExpr::Field {
						recv: Box::new(self.lower_expr(lhs)),
						name: res.method.clone(),
					}),
					args: vec![],
				},
				Some(res)
					if res.dispatch == DispatchKind::UserImplDefaultMethod
						&& self.lowering_onto_runtime_owner.get() > 0
						&& Self::is_this_receiver(lhs) =>
				{
					self.record_inner_runtime_enum_method_demand(&res.method);
					HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(self.lower_expr(lhs)),
							name: res.method.clone(),
						}),
						args: vec![],
					}
				}
				// Stdlib body lowering slice: fixes a live silent-miscompile
				// (`b as string` through the prelude's `Into for boolean` used to be
				// misclassified `UserImpl` — see `check_cast`'s doc comment in
				// `infer_expr.rs` — and compiled straight to `operand.into()`, a
				// `TypeError` on a JS primitive with no such method). Now correctly
				// tagged `UserImplDefaultMethod`; lower the prelude `into` body
				// as a mangled top-level function, or a plain method call on a
				// demand-lowered named-enum class (this slice), exactly like the
				// operator/method-call sites, or stay loud if it can't be.
				Some(res) if res.dispatch == DispatchKind::UserImplDefaultMethod => {
					match self.try_lower_runtime_dispatch(res, Self::is_this_receiver(lhs)) {
						Some(RuntimeDispatch::TopLevel(mangled)) => HirExpr::Call {
							callee: Box::new(HirExpr::Local(mangled)),
							args: vec![self.lower_expr(lhs)],
						},
						Some(RuntimeDispatch::OntoClass { method, .. }) => HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(self.lower_expr(lhs)),
								name: method,
							}),
							args: vec![],
						},
						// Gap 3 (L0/L1): see the plain-method-call arm above for
						// the same `LinkedExtern` shape.
						Some(RuntimeDispatch::LinkedExtern(linked)) => HirExpr::ExternCall {
							module: linked.module,
							symbol: linked.symbol,
							args: vec![self.lower_expr(lhs)],
						},
						None => panic!(
							"slice-stdlib-linkage lowering does not yet support dispatching a cast to a method resolved through a prelude-only impl (never lowered anywhere): `{}`",
							res.method
						),
					}
				}
				Some(res) => panic!(
					"slice-4k lowering does not yet dispatch cast to {:?}",
					res.dispatch
				),
				None => panic!(
					"slice-4k lowering: no cast resolution recorded for `as` — an unresolved cast must be a checker bug on a zero-diagnostic program"
				),
			},
			// `value is Pattern` / `value !is Pattern` (Slice 4K) desugars to a
			// one-arm boolean `match`: the pattern arm yields `true`/`false` (swapped
			// for `!is`), and a trailing `Wildcard` arm yields the other, making
			// runtime fallthrough impossible — no exhaustiveness machinery needed (that
			// only runs on genuine AST `Match` nodes in the checker, never on this
			// lowering-time desugar). Pattern bindings do NOT escape the expression:
			// the checker scopes them the same way (`infer_kind`'s `PatternOp` arm
			// pushes/pops a scope around `check_pattern`), and this mirrors the
			// `Match` arm below's own scope discipline (push/lower_pattern/pop) even
			// though the arm bodies here never reference a pattern binding.
			ExprKind::PatternOp { lhs, op, rhs } => {
				let scrutinee = Box::new(self.lower_expr(lhs));
				self.push_scope();
				let pat = self.lower_pattern(rhs);
				self.pop_scope();
				let (matched, unmatched) = match op {
					PatternOperator::Is => (HirExpr::Bool(true), HirExpr::Bool(false)),
					PatternOperator::NotIs => (HirExpr::Bool(false), HirExpr::Bool(true)),
				};
				HirExpr::Match {
					scrutinee,
					arms: vec![
						HirArm {
							pat,
							guard: None,
							body: matched,
						},
						HirArm {
							pat: HirPat::Wildcard,
							guard: None,
							body: unmatched,
						},
					],
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
			// A string literal, text-only or interpolated (Slice 4H, BB1) — cooked
			// text runs fold into `HirExpr::Str` segments, joined with lowered
			// interpoland subexpressions via JS `+` (see `lower_string_expr`).
			ExprKind::String(parts) => self.lower_string_expr(parts),
			// `for (<pat> in <range>) <body>` desugars entirely here into a
			// `Block { let mut $i = min; let $max = max; while (…) { .. } }` (Slice
			// 4H, BB2) — see `lower_for`. The label is discarded exactly like
			// `While` above: the parser never actually produces one yet
			// (`parse_for` hardcodes `label: None`), and neither `break` nor
			// `continue` lower (no HIR shape, no arm below — either panics loudly
			// in this same catch-all), so a labeled jump could never have reached
			// codegen regardless of what we did with the label here.
			ExprKind::For {
				variable,
				iterable,
				body,
				..
			} => self.lower_for(variable, iterable, body),
			// A range reached HERE is in ordinary VALUE position — not consumed
			// directly as a `for`-loop source (that shape is special-cased inside
			// `lower_for`, which destructures the iterable's `ExprKind::Range`
			// itself before ever calling `lower_expr` on it). Nothing in the
			// language can consume a first-class range value today (Slice 4H
			// investigation: match-range patterns test bounds against the
			// scrutinee, never a range object; the checker types a value-position
			// range as an unconstrained fresh inference variable that unifies with
			// anything, so miscompiling it silently is the worst possible
			// outcome). Panic loudly rather than invent an unused object ABI.
			ExprKind::Range(_) => panic!(
				"slice-4h lowering: range expressions are only supported as for-loop sources, not as a general value"
			),
			// A closure expression (Slice 4L, JJ1). `generics` is always empty (both
			// parse paths — the paren form and the single-ident form — hardcode
			// `Vec::new()` for it), and `return_type` is consumed only by the
			// checker (lowering is type-free) — neither is read here.
			ExprKind::Closure { params, body, .. } => self.lower_closure(params, body),
			// `$N` (Slice: `$N` anonymous closure params) — reached only for a
			// param the checker resolved through `Checker::anon_ctx` (`idx`
			// default 0), i.e. one strictly INSIDE a committed boundary node.
			// Resolves through the SAME JS-scope stack `lower_anon_closure`
			// just declared its `anon$0`/`anon$1`/… params into, exactly like
			// any other local reference — capture-by-reference and Y2 rename
			// both fall out for free.
			ExprKind::AnonymousParam(idx) => {
				HirExpr::Local(self.resolve(&anon_param_name(idx.unwrap_or(0))))
			}
			other => panic!("slice-2a lowering does not yet handle {other:?}"),
		}
	}

	/// Lower a checked `receiver[key]`: structural collections keep their
	/// dedicated runtime operations, while custom `Index` implementations follow
	/// the same dispatch/lowering paths as an explicit `.index(key)` call.
	fn lower_index_access(&self, id: nymph_ast::NodeId, parent: &Expr, index: &Expr) -> HirExpr {
		let recv_ty = self
			.annotations
			.get(parent.id)
			.map(|info| Self::peel_mut(self.interner, info.ty));
		match recv_ty.map(|ty| self.interner.kind(ty)) {
			Some(TyKind::Map(..)) => HirExpr::MapGet {
				recv: Box::new(self.lower_expr(parent)),
				key: Box::new(self.lower_expr(index)),
			},
			Some(TyKind::List(_) | TyKind::Tuple(_)) => HirExpr::Index {
				recv: Box::new(self.lower_expr(parent)),
				index: Box::new(self.lower_expr(index)),
			},
			_ => match self.annotations.resolution_of(id) {
				Some(res) if res.dispatch == DispatchKind::UserImpl => HirExpr::Call {
					callee: Box::new(HirExpr::Field {
						recv: Box::new(self.lower_expr(parent)),
						name: res.method.clone(),
					}),
					args: vec![self.lower_expr(index)],
				},
				Some(res)
					if res.dispatch == DispatchKind::UserImplDefaultMethod
						&& self.lowering_onto_runtime_owner.get() > 0
						&& Self::is_this_receiver(parent) =>
				{
					self.record_inner_runtime_enum_method_demand(&res.method);
					HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(self.lower_expr(parent)),
							name: res.method.clone(),
						}),
						args: vec![self.lower_expr(index)],
					}
				}
				Some(res)
					if res.dispatch == DispatchKind::UserImplDefaultMethod
						&& !Self::is_this_receiver(parent)
						&& self.receiver_is_still_generic(parent) =>
				{
					HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(self.lower_expr(parent)),
							name: res.method.clone(),
						}),
						args: vec![self.lower_expr(index)],
					}
				}
				Some(res) if res.dispatch == DispatchKind::UserImplDefaultMethod => {
					match self.try_lower_runtime_dispatch(res, Self::is_this_receiver(parent)) {
						Some(RuntimeDispatch::TopLevel(mangled)) => HirExpr::Call {
							callee: Box::new(HirExpr::Local(mangled)),
							args: vec![self.lower_expr(parent), self.lower_expr(index)],
						},
						Some(RuntimeDispatch::OntoClass { method, .. }) => HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(self.lower_expr(parent)),
								name: method,
							}),
							args: vec![self.lower_expr(index)],
						},
						Some(RuntimeDispatch::LinkedExtern(linked)) => HirExpr::ExternCall {
							module: linked.module,
							symbol: linked.symbol,
							args: vec![self.lower_expr(parent), self.lower_expr(index)],
						},
						None => panic!(
							"lowering does not support the resolved custom index implementation `{}`",
							res.method
						),
					}
				}
				Some(res) => unreachable!(
					"custom index access resolved to non-method dispatch {:?}",
					res.dispatch
				),
				None => panic!("lowering: no Index resolution recorded for custom index access"),
			},
		}
	}

	/// Lower a string literal's parts (Slice 4H, BB1). Contiguous `Text`/
	/// `EscapeSequence` runs cook into one `HirExpr::Str` segment (via
	/// `push_cooked_escape`); each `InterpolatedExpr` boundary flushes the
	/// pending buffer and lowers the interpoland through the Display intrinsic.
	/// Codegen unwraps each resulting NString, concatenates raw JS strings, and
	/// boxes the completed interpolation exactly once.
	fn lower_string_expr(&self, parts: &[Spanned<nymph_ast::expr::StringPart>]) -> HirExpr {
		use nymph_ast::expr::StringPart;

		let mut segments = Vec::new();
		let mut buf = EcoString::new();
		let mut interpolated = false;
		for part in parts {
			match &part.0 {
				StringPart::Text(t) => buf.push_str(t),
				StringPart::EscapeSequence(esc) => push_cooked_escape(&mut buf, *esc),
				StringPart::InterpolatedExpr(e) => {
					interpolated = true;
					if !buf.is_empty() {
						segments.push(HirExpr::Str(std::mem::take(&mut buf)));
					}
					segments.push(HirExpr::ExternCall {
						module: "std/display",
						symbol: "display",
						args: vec![self.lower_expr(e)],
					});
				}
			}
		}
		if !interpolated {
			return HirExpr::Str(buf);
		}
		if !buf.is_empty() {
			segments.push(HirExpr::Str(buf));
		}
		HirExpr::InterpolatedString(segments)
	}

	/// Desugar `for (<pat> in <src>) <body>` through the iterator protocol.
	/// Syntactic ranges are lowered as `NymphRange`; other collections use
	/// the protocol selected by the checker.
	///
	/// Ordinary collections desugar through `Iterator`/`Iterable`
	///   (`lower_for_protocol`), reading back which of the two the checker
	///   matched (`IterMode`, recorded on `iterable`'s own node id by
	///   `resolve_iterable_source`).
	fn lower_for(
		&self,
		variable: &Spanned<nymph_ast::expr::Pattern>,
		iterable: &Expr,
		body: &Expr,
	) -> HirExpr {
		if let ExprKind::Range(kind) = &iterable.kind {
			return self.lower_for_range_protocol(variable, kind, body);
		}
		self.lower_for_protocol(variable, iterable, body)
	}

	fn lower_for_range_protocol(
		&self,
		variable: &Spanned<nymph_ast::expr::Pattern>,
		kind: &nymph_ast::expr::RangeKind,
		body: &Expr,
	) -> HirExpr {
		use nymph_ast::expr::RangeKind;
		let (min, max, inclusive) = match kind {
			RangeKind::Exclusive { min, max } => (min, max, false),
			RangeKind::Inclusive { min, max } => (min, max, true),
			RangeKind::From(_) => {
				panic!("slice-4h lowering does not support an unbounded `from` range as a for-loop source")
			}
			RangeKind::To(_) | RangeKind::ToInclusive(_) => {
				panic!("slice-4h lowering does not support a start-less range as a for-loop source")
			}
		};
		let min_unwrapped = Self::peel_grouped(min);
		let integer = self.annotations.get(min_unwrapped.id).is_some_and(|info| {
			matches!(
				self.interner.kind(Self::peel_mut(self.interner, info.ty)),
				TyKind::Int | TyKind::UInt
			)
		});
		assert!(
			integer,
			"range lowering only supports `int` and `uint` bounds"
		);
		let source = HirExpr::New {
			class: "NymphRange".into(),
			fields: vec![
				("start".into(), self.lower_expr(min)),
				("end".into(), self.lower_expr(max)),
				("inclusive".into(), HirExpr::Bool(inclusive)),
			],
		};
		let it_value = Self::it_value_for(IterMode::ViaIter, source);
		self.push_scope();
		let stmts = self.drain_loop_stmts(it_value, |s| {
			let pat = s.lower_pattern(variable);
			let body = s.lower_branch(body);
			(pat, body)
		});
		self.pop_scope();
		HirExpr::Block { stmts, tail: None }
	}

	/// The general `Iterator`/`Iterable` protocol desugar (RR1/RR2): every
	/// `for`-loop source that is not a syntactic range
	/// reaches here. HIR has no `Break`/`Continue` node, so the loop-exit signal
	/// is a plain `mut` boolean flag rather than the more natural
	/// `while (true) { match { .. -> break } }` shape:
	///
	/// ```text
	/// let $it = <src>.iter()   // IterMode::ViaIter
	///        or <src>          // IterMode::Direct
	/// let mut $go = true
	/// while ($go) {
	///   match ($it.next()) {
	///     Some(<pat>) -> <body>,
	///     None -> $go = false,
	///   }
	/// }
	/// ```
	///
	/// The loop pattern is a genuine `match` arm
	/// pattern (`HirPat`, via the ordinary `lower_pattern`), so ANY pattern
	/// shape the language supports is legal in this position, same as any other
	/// `match` arm.
	///
	/// `.iter()`/`.next()` are ordinary `HirExpr::Call { callee: Field, args: [] }`
	/// nodes — for a user ADT source these compile straight to real emitted
	/// class methods (`recv.iter()` / `$it.next()`), no prelude-lowering
	/// hook needed, the same way any other user method call already lowers.
	fn lower_for_protocol(
		&self,
		variable: &Spanned<nymph_ast::expr::Pattern>,
		iterable: &Expr,
		body: &Expr,
	) -> HirExpr {
		let mode = self.annotations.iter_mode_of(iterable.id).unwrap_or_else(|| {
			panic!(
				"iterator-for-loops lowering: no `IterMode` recorded for a non-range, non-list for-loop source at {:?} — checker and lowering disagree",
				iterable.span
			)
		});

		let src = self.lower_expr(iterable);
		let it_value = Self::it_value_for(mode, src);

		// The whole for-loop is one JS scope, holding the iterator and the
		// loop-continues flag (plus, via `drain_loop_stmts`, the loop pattern's
		// own nested scope).
		self.push_scope();
		let stmts = self.drain_loop_stmts(it_value, |s| {
			let pat = s.lower_pattern(variable);
			let body = s.lower_branch(body);
			(pat, body)
		});
		self.pop_scope();

		HirExpr::Block { stmts, tail: None }
	}

	/// Build the iterator-or-iterable-via-`.iter()` source `lower_for_protocol`
	/// and the SS1 spread drain (`lower_spread_source`) both feed their own
	/// drain loop: `src` itself for [`IterMode::Direct`], `src.iter()` for
	/// [`IterMode::ViaIter`].
	fn it_value_for(mode: IterMode, src: HirExpr) -> HirExpr {
		match mode {
			IterMode::Direct => src,
			IterMode::ViaIter => HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(src),
					name: "iter".into(),
				}),
				args: vec![],
			},
		}
	}

	/// Build the `$it`/`$go`/`while` drain trio shared by [`Self::lower_for_protocol`]
	/// (Track A's `for`-loop desugar) and [`Self::drain_to_array`] (SS1's spread
	/// drain):
	///
	/// ```text
	/// let $it = <it_value>
	/// let mut $go = true
	/// while ($go) {
	///   match ($it.next()) {
	///     Some(<pat>) -> <body>,
	///     None -> $go = false,
	///   }
	/// }
	/// ```
	///
	/// `build_some` supplies the `Some`-arm's pattern and body, run inside its
	/// OWN pushed/popped scope — a closure rather than two pre-computed values,
	/// so `$it`/`$go` are always declared BEFORE that scope is pushed, exactly
	/// matching `lower_for_protocol`'s original inline declare order (this
	/// extraction changes no observable behavior; every existing for-loop e2e
	/// stays byte-equivalent). Returns the three `HirStmt`s for the CALLER to
	/// embed in its own `Block` — this method pushes/pops only the body's inner
	/// scope, never the outer one, so callers control whether `$it`/`$go`
	/// themselves share a scope with something else (e.g. `drain_to_array`'s
	/// `$acc`).
	fn drain_loop_stmts(
		&self,
		it_value: HirExpr,
		build_some: impl FnOnce(&Self) -> (HirPat, HirExpr),
	) -> Vec<HirStmt> {
		let it_name = self.declare(&EcoString::from("$it"));
		let go_name = self.declare(&EcoString::from("$go"));

		// One scope covers the loop pattern's bindings and the body together,
		// mirroring how `ExprKind::Match`'s own arms scope pattern+guard+body as
		// a unit above.
		self.push_scope();
		let (pat, body) = build_some(self);
		self.pop_scope();

		let next_call = HirExpr::Call {
			callee: Box::new(HirExpr::Field {
				recv: Box::new(HirExpr::Local(it_name.clone())),
				name: "next".into(),
			}),
			args: vec![],
		};
		let match_expr = HirExpr::Match {
			scrutinee: Box::new(next_call),
			arms: vec![
				HirArm {
					pat: HirPat::Variant {
						enum_name: "Option".into(),
						variant: "Some".into(),
						fields: vec![("value".into(), pat)],
					},
					guard: None,
					body,
				},
				HirArm {
					pat: HirPat::Variant {
						enum_name: "Option".into(),
						variant: "None".into(),
						fields: Vec::new(),
					},
					guard: None,
					body: HirExpr::Assign {
						target: Box::new(HirExpr::Local(go_name.clone())),
						value: Box::new(HirExpr::Bool(false)),
					},
				},
			],
		};
		let while_expr = HirExpr::While {
			cond: Box::new(HirExpr::Local(go_name.clone())),
			body: Box::new(HirExpr::Block {
				stmts: vec![HirStmt::Expr(match_expr)],
				tail: None,
			}),
		};

		vec![
			HirStmt::Let {
				name: it_name,
				mutable: false,
				value: it_value,
			},
			HirStmt::Let {
				name: go_name,
				mutable: true,
				value: HirExpr::Bool(true),
			},
			HirStmt::Expr(while_expr),
		]
	}

	/// Drain an `Iterator`/`Iterable` protocol source into a real JS array
	/// (SS1): shared by a list spread (`#[...src]`) and a non-map map spread
	/// (`#{...src}`, where `src` is an iterable of `#(K, V)` pairs) — the drain
	/// loop itself is agnostic to whether each drained element is a scalar or a
	/// pair, so one helper serves both:
	///
	/// ```text
	/// let $acc = []
	/// let $it = <it_value>
	/// let mut $go = true
	/// while ($go) {
	///   match ($it.next()) {
	///     Some($x) -> $acc.push($x),
	///     None -> $go = false,
	///   }
	/// }
	/// $acc   // the Block's tail
	/// ```
	///
	/// `it_value` is the ALREADY-built iterator-or-iterable-via-`.iter()` source
	/// (see [`Self::it_value_for`]) — same shape [`Self::lower_for_protocol`]
	/// feeds its own drain. Returns a `Block`; `emit_expr`'s existing
	/// subexpression-position `Block` arm wraps it in an IIFE, so no new emit
	/// code is needed for the drain itself.
	fn drain_to_array(&self, it_value: HirExpr) -> HirExpr {
		self.push_scope();
		let acc_name = self.declare(&EcoString::from("$acc"));
		let mut stmts = vec![HirStmt::Let {
			name: acc_name.clone(),
			mutable: false,
			value: HirExpr::Array {
				kind: HirArrayKind::Raw,
				items: vec![],
			},
		}];
		stmts.extend(self.drain_loop_stmts(it_value, |s| {
			let x_name = s.declare(&EcoString::from("$x"));
			let push_call = HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(HirExpr::Local(acc_name.clone())),
					name: "push".into(),
				}),
				args: vec![HirExpr::Local(x_name.clone())],
			};
			(
				HirPat::Binding {
					name: x_name,
					sub: None,
				},
				push_call,
			)
		}));
		self.pop_scope();
		HirExpr::Block {
			stmts,
			tail: Some(Box::new(HirExpr::Local(acc_name))),
		}
	}

	/// Lower a spread source `e` (`#[...e]` list item, or a non-map `#{...e}`
	/// map entry) to the JS-array-valued expression the spread splices: a boxed
	/// list contributes its `.v` payload directly; anything else is drained into a
	/// real array through the `Iterator`/`Iterable` protocol
	/// ([`Self::drain_to_array`]), reading back which mode the checker matched
	/// ([`IterMode`]) exactly like [`Self::lower_for_protocol`] does.
	///
	/// Panics loudly if neither a list-like type nor an `IterMode` was recorded
	/// — e.g. a `Range` spread source (`#[...0..5]`), which the checker types
	/// through its own short-circuit that never consults `Iterator`/`Iterable`
	/// and so never records either annotation. An out-of-scope edge (the
	/// for-loop is the intended range consumer) this never silently miscompiles.
	fn lower_spread_source(&self, e: &Expr) -> HirExpr {
		let source_ty = self
			.annotations
			.get(e.id)
			.map(|info| Self::peel_mut(self.interner, info.ty));
		if source_ty.is_some_and(|ty| matches!(self.interner.kind(ty), TyKind::List(_))) {
			return HirExpr::Field {
				recv: Box::new(self.lower_expr(e)),
				name: "v".into(),
			};
		}
		let mode = self.annotations.iter_mode_of(e.id).unwrap_or_else(|| {
			panic!(
				"spread lowering: no `IterMode` recorded for a non-list spread source at {:?} — checker and lowering disagree",
				e.span
			)
		});
		let src = self.lower_expr(e);
		let it_value = Self::it_value_for(mode, src);
		self.drain_to_array(it_value)
	}

	/// Peel through zero or more `ExprKind::Grouped` wrapper layers (i.e.
	/// parentheses) down to the innermost non-`Grouped` expression. The
	/// checker's `check()` recurses through `Grouped` without ever recording an
	/// annotation for the `Grouped` node's own id, so any annotation lookup keyed
	/// on an arbitrary expression's id must peel through parens first, or a
	/// parenthesized subexpression looks annotation-less (`None`) even though
	/// it's perfectly well-typed.
	fn peel_grouped(mut e: &Expr) -> &Expr {
		while let ExprKind::Grouped(inner) = &e.kind {
			e = inner;
		}
		e
	}

	/// Peel a top-level `mut` view off a recorded type before using it purely to
	/// pick a codegen shape (map-vs-list dispatch, numeric-range/cast kind, …).
	/// Lowering is type-free — codegen must be identical whether or not the
	/// checker let this value through as `mut` — so every type-directed
	/// dispatch decision here must ignore `mut` exactly as the emitted JS does.
	fn peel_mut(interner: &Interner, ty: Ty) -> Ty {
		match interner.kind(ty) {
			TyKind::Mut(inner) => *inner,
			_ => ty,
		}
	}

	/// The [`NumKind`] a numeric literal node must be boxed as (uniform value
	/// boxing, slice #2). HIR is type-free, so the int/uint/float distinction —
	/// which selects the `NInt`/`NUint`/`NFloat` wrapper — is recovered here from
	/// the checker's inferred type for the literal's own node, exactly the
	/// `info.ty` → `interner.kind` channel every other type-directed lowering
	/// decision uses (`peel_mut` above). The checker's type is authoritative over
	/// the syntactic form because a literal's kind can be re-decided by context
	/// (`let x: float = 5` types the syntactically-`int` `5` as `float`). Only
	/// when no usable type was recorded (a literal the checker never annotated, or
	/// one still typed as an unsolved inference variable / an error) does it fall
	/// back to `syntactic`, the kind the lexer gave the token.
	fn num_kind(&self, id: nymph_ast::NodeId, syntactic: NumKind) -> NumKind {
		match self.annotations.get(id) {
			Some(info) => match self.interner.kind(Self::peel_mut(self.interner, info.ty)) {
				TyKind::Int => NumKind::Int,
				TyKind::UInt => NumKind::UInt,
				TyKind::Float => NumKind::Float,
				_ => syntactic,
			},
			None => syntactic,
		}
	}

	/// Recover the result box selected by the checker for a built-in operator.
	/// Built-in operators are the only operator HIR nodes that reach native JS;
	/// user implementations have already become method calls.
	fn builtin_result(&self, id: nymph_ast::NodeId, operand_id: nymph_ast::NodeId) -> BuiltinResult {
		let info = self
			.annotations
			.get(id)
			.unwrap_or_else(|| panic!("no result type recorded for built-in operator {id:?}"));
		// Compound assignment nodes are statement-like and therefore annotated as
		// `void`; their inner operation returns the target's type.
		let ty = if matches!(self.interner.kind(info.ty), TyKind::Void) {
			self
				.annotations
				.get(operand_id)
				.unwrap_or_else(|| panic!("no operand type recorded for built-in operator {id:?}"))
				.ty
		} else {
			info.ty
		};
		match self.interner.kind(Self::peel_mut(self.interner, ty)) {
			TyKind::Int => BuiltinResult::Int,
			TyKind::UInt => BuiltinResult::UInt,
			TyKind::Float => BuiltinResult::Float,
			TyKind::Char => BuiltinResult::Char,
			TyKind::String => BuiltinResult::String,
			TyKind::Boolean => BuiltinResult::Boolean,
			kind => panic!("unsupported built-in operator result type {kind:?}"),
		}
	}

	fn builtin_binary_result(&self, id: nymph_ast::NodeId, op: BinOp, lhs: &Expr) -> BuiltinResult {
		let lhs = Self::peel_grouped(lhs);
		let identity_comparison = matches!(op, BinOp::Eq | BinOp::Ne)
			&& self.annotations.get(lhs.id).is_some_and(|info| {
				!matches!(
					self.interner.kind(Self::peel_mut(self.interner, info.ty)),
					TyKind::Int
						| TyKind::UInt
						| TyKind::Float
						| TyKind::Char
						| TyKind::String
						| TyKind::Boolean
				)
			});
		if identity_comparison {
			BuiltinResult::IdentityBoolean
		} else {
			self.builtin_result(id, lhs.id)
		}
	}

	/// Whether `e` (after peeling any parens) is literally `this` — the ONLY
	/// receiver shape for which `lowering_onto_runtime_owner`'s fast path (an
	/// ordinary `<recv>.method(args)` JS call, bypassing
	/// `try_lower_runtime_dispatch`/loud-panic entirely) is sound. See
	/// `lowering_onto_runtime_owner`'s own doc comment: it is nonzero for the
	/// FULL duration of lowering one interface default body onto a concrete
	/// class, and that body's dispatches are safe to treat as "this method
	/// exists on the class" only when the receiver actually IS `this` — a
	/// default body dispatching through its OWN other generic parameter
	/// (`Comparable<Other>`'s `Other`, say) is a completely different
	/// receiver that may be bound to a primitive with no such JS method at
	/// all, and must fall through to the same lower-or-panic handling
	/// gap (b) already applies everywhere else.
	fn is_this_receiver(e: &Expr) -> bool {
		matches!(Self::peel_grouped(e).kind, ExprKind::This)
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
			// Slice 4L, JJ2: same closure-body guard as `lower_block`'s statement
			// interception above — an unbraced branch (`if (cond) return n`) hits
			// this arm instead, so it needs the identical check.
			assert!(
				self.closure_depth.get() == 0,
				"slice-4l lowering: `return` inside a closure body is not supported"
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
	///
	/// `Pipe`/`In`/`NotIn`/`Unwrap` (Slice 4I) branch here *before* reaching
	/// `lower_operator`: `lower_binop` has no native `BinOp` for any of the four (it
	/// panics on them), so calling it eagerly as `lower_operator`'s argument — as
	/// every other operator does — would panic even on a node the checker resolved
	/// cleanly. `Pipe` needs no `Resolution` at all (DD1: a structural `Call`);
	/// `In`/`NotIn` need a *swapped* receiver/argument (`c.contains(a)`, DD2) that
	/// `lower_operator`'s `lhs.method(rhs)` shape can't express; `Unwrap` fits
	/// `lower_operator`'s ordinary shape exactly (`recv.unwrap(fallback)`, DD3) but
	/// still needs its own arm since it has no native `BinOp` to pass through.
	fn lower_binary(
		&self,
		id: nymph_ast::NodeId,
		lhs: &Expr,
		op: BinaryOperator,
		rhs: &Expr,
	) -> HirExpr {
		match op {
			// DD1: `x |> f` lowers structurally to a `Call` — the checker already
			// type-checked every hazardous callee shape away (a bare variant/struct
			// name or bound method value as the RHS is a checker error before
			// lowering ever runs), so no special-casing of `rhs`'s shape is needed
			// here, unlike `ExprKind::Call`'s variant-factory/`New` special cases.
			//
			// Evaluation order: JS evaluates a call's callee before its arguments,
			// so `lhsFn() |> rhsFn()` runs `rhsFn` FIRST — reversed from source
			// order. Nymph documents no evaluation-order guarantee, and hoisting
			// the LHS into a temp would cost an IIFE per pipe (HIR has no sequence
			// expression), so RHS-first is the accepted semantics — the same
			// ruling as `in`/`!in`'s receiver swap below. Revisit if HIR ever
			// grows a let/sequence expression.
			BinaryOperator::Pipe => {
				return HirExpr::Call {
					callee: Box::new(self.lower_expr(rhs)),
					args: vec![self.lower_expr(lhs)],
				};
			}
			// DD2: `a in c` / `a !in c` ≡ `c.contains(a)` / `c.not_contains(a)` — the
			// RHS is the receiver, the LHS the sole argument, swapped relative to
			// every other binary operator. This changes evaluation order (the RHS
			// collection is evaluated before the LHS item); Nymph has no documented
			// left-to-right evaluation guarantee for `in` (the reference docs are a
			// stub) and HIR has no let/sequence expression to preserve source order
			// cheaply (only `Block`, which would cost an IIFE), so RHS-before-LHS is
			// accepted and documented here rather than engineered around.
			BinaryOperator::In | BinaryOperator::NotIn => {
				return match self.annotations.resolution_of(id) {
					Some(res) if res.dispatch == DispatchKind::UserImpl => HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(self.lower_expr(rhs)),
							name: res.method.clone(),
						}),
						args: vec![self.lower_expr(lhs)],
					},
					Some(res)
						if res.dispatch == DispatchKind::UserImplDefaultMethod
							&& self.lowering_onto_runtime_owner.get() > 0
							&& Self::is_this_receiver(rhs) =>
					{
						self.record_inner_runtime_enum_method_demand(&res.method);
						HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(self.lower_expr(rhs)),
								name: res.method.clone(),
							}),
							args: vec![self.lower_expr(lhs)],
						}
					}
					Some(res) if res.dispatch == DispatchKind::UserImplDefaultMethod => {
						match self.try_lower_runtime_dispatch(res, Self::is_this_receiver(rhs)) {
							Some(RuntimeDispatch::TopLevel(mangled)) => HirExpr::Call {
								callee: Box::new(HirExpr::Local(mangled)),
								args: vec![self.lower_expr(rhs), self.lower_expr(lhs)],
							},
							Some(RuntimeDispatch::OntoClass { method, .. }) => HirExpr::Call {
								callee: Box::new(HirExpr::Field {
									recv: Box::new(self.lower_expr(rhs)),
									name: method,
								}),
								args: vec![self.lower_expr(lhs)],
							},
							// Gap 3 (L0/L1): `in`/`!in`'s receiver is `rhs` (the
							// collection); mirrors `RuntimeDispatch::TopLevel`'s
							// arg order above.
							Some(RuntimeDispatch::LinkedExtern(linked)) => HirExpr::ExternCall {
								module: linked.module,
								symbol: linked.symbol,
								args: vec![self.lower_expr(rhs), self.lower_expr(lhs)],
							},
							None => panic!(
								"slice-4i lowering does not yet dispatch operator to interface default method {}",
								res.method
							),
						}
					}
					Some(res) => panic!(
						"slice-4i lowering does not yet dispatch operator to interface default method {}",
						res.method
					),
					None => panic!("slice-4i lowering: no operator resolution recorded for binary op {op:?}"),
				};
			}
			// DD3: Nymph has no optional runtime representation (no `T?` syntax, no
			// `TyKind::Optional`, no builtin `Option`/`Result` short-circuit default),
			// so `??` always resolves to an ordinary eager `recv.unwrap(fallback)`
			// call — the same `lhs.method(rhs)` shape `lower_operator` already
			// produces for `UserImpl`, just with no native `BinOp` to fall back to.
			BinaryOperator::Unwrap => {
				return match self.annotations.resolution_of(id) {
					Some(res) if res.dispatch == DispatchKind::UserImpl => HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(self.lower_expr(lhs)),
							name: res.method.clone(),
						}),
						args: vec![self.lower_expr(rhs)],
					},
					Some(res)
						if res.dispatch == DispatchKind::UserImplDefaultMethod
							&& self.lowering_onto_runtime_owner.get() > 0
							&& Self::is_this_receiver(lhs) =>
					{
						self.record_inner_runtime_enum_method_demand(&res.method);
						HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(self.lower_expr(lhs)),
								name: res.method.clone(),
							}),
							args: vec![self.lower_expr(rhs)],
						}
					}
					Some(res) if res.dispatch == DispatchKind::UserImplDefaultMethod => {
						match self.try_lower_runtime_dispatch(res, Self::is_this_receiver(lhs)) {
							Some(RuntimeDispatch::TopLevel(mangled)) => HirExpr::Call {
								callee: Box::new(HirExpr::Local(mangled)),
								args: vec![self.lower_expr(lhs), self.lower_expr(rhs)],
							},
							Some(RuntimeDispatch::OntoClass { method, .. }) => HirExpr::Call {
								callee: Box::new(HirExpr::Field {
									recv: Box::new(self.lower_expr(lhs)),
									name: method,
								}),
								args: vec![self.lower_expr(rhs)],
							},
							// Gap 3 (L0/L1): `??`'s receiver is `lhs`; mirrors
							// `RuntimeDispatch::TopLevel`'s arg order above.
							Some(RuntimeDispatch::LinkedExtern(linked)) => HirExpr::ExternCall {
								module: linked.module,
								symbol: linked.symbol,
								args: vec![self.lower_expr(lhs), self.lower_expr(rhs)],
							},
							None => panic!(
								"slice-4i lowering does not yet dispatch operator to interface default method {}",
								res.method
							),
						}
					}
					Some(res) => panic!(
						"slice-4i lowering does not yet dispatch operator to interface default method {}",
						res.method
					),
					None => panic!("slice-4i lowering: no operator resolution recorded for binary op {op:?}"),
				};
			}
			_ => {}
		}
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
	/// resolution both panic loudly — codegen cannot yet lower interface
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
					result: self.builtin_binary_result(id, native, lhs),
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
			Some(res)
				if res.dispatch == DispatchKind::UserImplDefaultMethod
					&& self.lowering_onto_runtime_owner.get() > 0
					&& Self::is_this_receiver(lhs) =>
			{
				self.record_inner_runtime_enum_method_demand(&res.method);
				HirExpr::Call {
					callee: Box::new(HirExpr::Field {
						recv: Box::new(self.lower_expr(lhs)),
						name: res.method.clone(),
					}),
					args: vec![self.lower_expr(rhs)],
				}
			}
			Some(res)
				if res.dispatch == DispatchKind::UserImplDefaultMethod
					&& self.receiver_is_still_generic(lhs) =>
			{
				self.lower_bound_dispatch(res, lhs, rhs).unwrap_or_else(|| {
					match self.try_lower_runtime_dispatch(res, false) {
						Some(RuntimeDispatch::TopLevel(name)) => HirExpr::Call {
							callee: Box::new(HirExpr::Local(name)),
							args: vec![self.lower_expr(lhs), self.lower_expr(rhs)],
						},
						Some(RuntimeDispatch::LinkedExtern(linked)) => HirExpr::ExternCall {
							module: linked.module,
							symbol: linked.symbol,
							args: vec![self.lower_expr(lhs), self.lower_expr(rhs)],
						},
						Some(RuntimeDispatch::OntoClass { method, .. }) => HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(self.lower_expr(lhs)),
								name: method,
							}),
							args: vec![self.lower_expr(rhs)],
						},
						None => panic!(
							"lowering does not yet dispatch operator to interface default method `{}`",
							res.method
						),
					}
				})
			}
			// Stdlib body lowering slice, gap b: see
			// `try_lower_runtime_dispatch`'s doc comment.
			Some(res) if res.dispatch == DispatchKind::UserImplDefaultMethod => {
				match self.try_lower_runtime_dispatch(res, Self::is_this_receiver(lhs)) {
					Some(RuntimeDispatch::TopLevel(mangled)) => HirExpr::Call {
						callee: Box::new(HirExpr::Local(mangled)),
						args: vec![self.lower_expr(lhs), self.lower_expr(rhs)],
					},
					Some(RuntimeDispatch::OntoClass { method, .. }) => HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(self.lower_expr(lhs)),
							name: method,
						}),
						args: vec![self.lower_expr(rhs)],
					},
					// Gap 3 (L0/L1): binary operator receiver is `lhs`; mirrors
					// `RuntimeDispatch::TopLevel`'s arg order above.
					Some(RuntimeDispatch::LinkedExtern(linked)) => HirExpr::ExternCall {
						module: linked.module,
						symbol: linked.symbol,
						args: vec![self.lower_expr(lhs), self.lower_expr(rhs)],
					},
					None => panic!(
						"slice-4b lowering does not yet dispatch operator to interface default method {}",
						res.method
					),
				}
			}
			// Unreachable in practice (`BuiltinEager`/`BuiltinShortCircuit`/`UserImpl`/
			// `UserImplDefaultMethod` above exhaust `DispatchKind`'s four variants),
			// but a guarded match still needs a total fallback arm for `Some(_)`.
			Some(res) => unreachable!(
				"lower_operator: DispatchKind fully covered above, got {:?} for method {}",
				res.dispatch, res.method
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
	/// lower interface default methods, and an unresolved node is a checker
	/// bug we want to see immediately rather than silently miscompile.
	fn lower_prefix_op(&self, id: nymph_ast::NodeId, op: PrefixOperator, value: &Expr) -> HirExpr {
		match self.annotations.resolution_of(id) {
			Some(res) if res.dispatch == DispatchKind::BuiltinEager => HirExpr::Unary {
				op: lower_prefix(op),
				result: self.builtin_result(id, value.id),
				operand: Box::new(self.lower_expr(value)),
			},
			Some(res) if res.dispatch == DispatchKind::UserImpl => HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(self.lower_expr(value)),
					name: res.method.clone(),
				}),
				args: vec![],
			},
			Some(res)
				if res.dispatch == DispatchKind::UserImplDefaultMethod
					&& self.lowering_onto_runtime_owner.get() > 0
					&& Self::is_this_receiver(value) =>
			{
				self.record_inner_runtime_enum_method_demand(&res.method);
				HirExpr::Call {
					callee: Box::new(HirExpr::Field {
						recv: Box::new(self.lower_expr(value)),
						name: res.method.clone(),
					}),
					args: vec![],
				}
			}
			Some(res) if res.dispatch == DispatchKind::BuiltinShortCircuit => panic!(
				"slice-4c lowering: BuiltinShortCircuit is unreachable for a prefix operator (method {})",
				res.method
			),
			// Stdlib body lowering slice, gap b: see
			// `try_lower_runtime_dispatch`'s doc comment.
			Some(res) if res.dispatch == DispatchKind::UserImplDefaultMethod => {
				match self.try_lower_runtime_dispatch(res, Self::is_this_receiver(value)) {
					Some(RuntimeDispatch::TopLevel(mangled)) => HirExpr::Call {
						callee: Box::new(HirExpr::Local(mangled)),
						args: vec![self.lower_expr(value)],
					},
					Some(RuntimeDispatch::OntoClass { method, .. }) => HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(self.lower_expr(value)),
							name: method,
						}),
						args: vec![],
					},
					// Gap 3 (L0/L1): a prefix operator's receiver is `value`, no
					// extra args.
					Some(RuntimeDispatch::LinkedExtern(linked)) => HirExpr::ExternCall {
						module: linked.module,
						symbol: linked.symbol,
						args: vec![self.lower_expr(value)],
					},
					None => panic!(
						"slice-4c lowering does not yet dispatch operator to interface default method {}",
						res.method
					),
				}
			}
			Some(res) => unreachable!(
				"lower_prefix_op: DispatchKind fully covered above, got {:?} for method {}",
				res.dispatch, res.method
			),
			None => panic!("slice-4c lowering: no operator resolution recorded for prefix op {op:?}"),
		}
	}

	/// Lower a built-in (`DispatchKind::BuiltinEager`) `as` cast's exact JS mapping
	/// (Slice 4K, extended by the saturating-cast change). Codegen is type-free, so
	/// this is the one place lowering *reads* types purely to pick a shape —
	/// mirroring `recv_is_map`'s precedent above — rather than threading a
	/// type-directed decision into `emit.rs`. Both the operand's (`lhs`) and the
	/// cast node's (`id`) types were recorded (and zonked) by `check_cast`/`infer`,
	/// so they're read back here instead of re-inferring. Identity casts (`Foo as
	/// Foo`) and the remaining same-"JS number" numeric casts (`int`/`uint` →
	/// `float`, `uint` → `int`) need no runtime operation at all — the operand
	/// lowers unchanged; every other numeric pairing (`float`/`int` → `uint`,
	/// `float` → `int`) now saturates (NaN/Infinity semantics, Nymph's own — never
	/// JS's/Rust's), and a conversion touching `char` needs an actual JS call.
	fn lower_scalar_cast(&self, id: nymph_ast::NodeId, lhs: &Expr) -> HirExpr {
		let operand = self.lower_expr(lhs);
		let src_ty = self
			.annotations
			.get(lhs.id)
			.unwrap_or_else(|| panic!("slice-4k lowering: cast operand has no recorded type"))
			.ty;
		let target_ty = self
			.annotations
			.get(id)
			.unwrap_or_else(|| panic!("slice-4k lowering: cast node has no recorded type"))
			.ty;
		let src_ty = Self::peel_mut(self.interner, src_ty);
		let target_ty = Self::peel_mut(self.interner, target_ty);
		match (self.interner.kind(src_ty), self.interner.kind(target_ty)) {
			(TyKind::Int, TyKind::Int) => HirExpr::ScalarCast {
				kind: ScalarCastKind::IdentityInt,
				operand: Box::new(operand),
			},
			(TyKind::UInt, TyKind::UInt) => HirExpr::ScalarCast {
				kind: ScalarCastKind::IdentityUInt,
				operand: Box::new(operand),
			},
			(TyKind::Float, TyKind::Float) => HirExpr::ScalarCast {
				kind: ScalarCastKind::IdentityFloat,
				operand: Box::new(operand),
			},
			(TyKind::Char, TyKind::Char) => HirExpr::ScalarCast {
				kind: ScalarCastKind::IdentityChar,
				operand: Box::new(operand),
			},
			(TyKind::UInt, TyKind::Int) => HirExpr::ScalarCast {
				kind: ScalarCastKind::ToInt,
				operand: Box::new(operand),
			},
			(TyKind::Int | TyKind::UInt, TyKind::Float) => HirExpr::ScalarCast {
				kind: ScalarCastKind::ToFloat,
				operand: Box::new(operand),
			},
			(TyKind::Float, TyKind::Int) => HirExpr::ScalarCast {
				kind: ScalarCastKind::SaturatingToInt,
				operand: Box::new(operand),
			},
			// `int as uint` used to be a no-op (int/uint/float were treated as one
			// "same JS number" family); the abs-first saturating rule makes it a real
			// runtime operation, so it joins `float as uint` here instead of falling
			// through to the identity-cast catch-all below.
			(TyKind::Float | TyKind::Int, TyKind::UInt) => HirExpr::ScalarCast {
				kind: ScalarCastKind::SaturatingToUInt,
				operand: Box::new(operand),
			},
			(TyKind::Char, TyKind::Int) => HirExpr::ScalarCast {
				kind: ScalarCastKind::CharToInt,
				operand: Box::new(operand),
			},
			(TyKind::Char, TyKind::UInt) => HirExpr::ScalarCast {
				kind: ScalarCastKind::CharToUInt,
				operand: Box::new(operand),
			},
			(TyKind::Char, TyKind::Float) => HirExpr::ScalarCast {
				kind: ScalarCastKind::CharToFloat,
				operand: Box::new(operand),
			},
			(TyKind::Int | TyKind::UInt, TyKind::Char) => HirExpr::ScalarCast {
				kind: ScalarCastKind::NumToChar,
				operand: Box::new(operand),
			},
			(TyKind::Float, TyKind::Char) => HirExpr::ScalarCast {
				kind: ScalarCastKind::FloatToChar,
				operand: Box::new(operand),
			},
			// Identity (`src == target`, any type) and the remaining same-"JS
			// number" numeric pairings (`int`/`uint` → `float`, `uint` → `int`) —
			// no runtime conversion needed.
			_ => operand,
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
			Pattern::Int(v) => HirPat::Lit(HirLit::Num(v.0 as f64, NumKind::Int)),
			Pattern::UInt(v) => HirPat::Lit(HirLit::Num(v.0 as f64, NumKind::UInt)),
			Pattern::Float(v) => HirPat::Lit(HirLit::Num(v.0.into_inner(), NumKind::Float)),
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
			Pattern::Tuple(entries) => {
				let has_rest = entries
					.iter()
					.any(|e| matches!(e.0, ListPatternEntry::Rest(_)));
				if has_rest {
					// A tuple's elements are heterogeneous, but at runtime it's a plain JS
					// array — reuse `HirPat::List`'s prefix/rest/suffix machinery (and its
					// emit) wholesale rather than giving `HirPat::Tuple` its own rest shape.
					// The resulting `length >=` test is redundant (a well-typed tuple's
					// arity is static and already enforced by the checker) but harmless.
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
								assert!(rest.is_none(), "tuple pattern has at most one `...` rest");
								rest = Some(name.as_ref().map(|n| self.declare(&n.0)));
							}
						}
					}
					HirPat::List {
						kind: HirArrayKind::Tuple,
						prefix,
						rest,
						suffix,
					}
				} else {
					HirPat::Tuple(self.lower_pattern_items(entries))
				}
			}
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
					kind: HirArrayKind::List,
					prefix,
					rest,
					suffix,
				}
			}
			Pattern::Map(entries) => {
				use nymph_ast::expr::MapPatternEntry;
				let mut lowered = Vec::new();
				let mut rest: Option<Option<ecow::EcoString>> = None;
				for entry in entries {
					match &entry.0 {
						MapPatternEntry::Entry(k, v) => {
							lowered.push((lower_lit_pattern(k), self.lower_pattern(v)));
						}
						MapPatternEntry::Rest(name) => {
							assert!(rest.is_none(), "map pattern has at most one `...` rest");
							rest = Some(name.as_ref().map(|n| self.declare(&n.0)));
						}
					}
				}
				HirPat::Map {
					entries: lowered,
					rest,
				}
			}
			Pattern::Range(kind) => HirPat::Range(lower_range_pattern(kind)),
			Pattern::Union(a, b) => {
				self
					.pattern_declaration_records
					.borrow_mut()
					.push(FxHashMap::default());
				let a = self.lower_pattern(a);
				let bindings = self
					.pattern_declaration_records
					.borrow_mut()
					.pop()
					.expect("union declaration record was pushed");
				self.pattern_declaration_reuse.borrow_mut().push(bindings);
				let b = self.lower_pattern(b);
				self.pattern_declaration_reuse.borrow_mut().pop();
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
				StructPatternField::Named(name) => {
					// A bare identifier the checker reinterpreted as a positional sub-pattern
					// on a single-field constructor carries a recorded field name; the name is
					// then a nullary-variant pattern or a plain binding (as in `lower_pattern`'s
					// `Binding` arm), not a field shorthand.
					match self.annotations.positional_field_of(f.1).cloned() {
						Some(fname) => {
							let sub = match self.annotations.pattern_variant_of(f.1).cloned() {
								Some(res) => HirPat::Variant {
									enum_name: res.enum_name.clone(),
									variant: res.variant.clone(),
									fields: Vec::new(),
								},
								None => HirPat::Binding {
									name: self.declare(&name.0),
									sub: None,
								},
							};
							Some((fname, sub))
						}
						None => Some((
							name.0.clone(),
							HirPat::Binding {
								name: self.declare(&name.0),
								sub: None,
							},
						)),
					}
				}
				// A positional sub-pattern: the checker recorded (by the field's span)
				// which single field it binds — pair that field name with the lowered
				// sub-pattern, exactly as a `field = pattern` (`Value`) would.
				StructPatternField::Positional(value) => self
					.annotations
					.positional_field_of(f.1)
					.map(|fname| (fname.clone(), self.lower_pattern(value))),
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
	/// `VariantNew`. Returns `None` when the node is not a variant construction (an
	/// ordinary call/struct).
	///
	/// An un-labeled argument (`Some(value)`, as opposed to `Some(value = value)`)
	/// resolves POSITIONALLY against `variant_fields`' declared field order —
	/// mirroring `infer_variant_ctor`/`check_ctor_args`'s own "by label when
	/// present else positionally" semantics (`infer_expr.rs`), which a
	/// zero-diagnostic program is fully entitled to have used. This slice
	/// (named-type prelude method lowering) needs it: convert.nym's
	/// `ok`/`err` (`impl<T, E> Result<T, E> { .. }`) build `Option.Some(value)`
	/// positionally, unreachable before this slice (their enclosing methods
	/// panicked at dispatch, never lowering the body at all) — 2C's original
	/// "labeled fields only" restriction is otherwise unrelated to this slice's
	/// prelude-lowering machinery, but blocks its `Result.ok()`/`.err()`
	/// payoff outright without this fix.
	fn variant_new(
		&self,
		id: nymph_ast::NodeId,
		args: &[nymph_ast::Spanned<CallArg>],
	) -> Option<HirExpr> {
		let res = self.annotations.variant_of(id)?;
		let fields = args
			.iter()
			.enumerate()
			.map(|(i, a)| {
				let label = match &a.0.name {
					Some(label) => label.0.clone(),
					None => {
						let names = self
							.variant_fields
							.get(&(res.enum_name.clone(), res.variant.clone()))
							.unwrap_or_else(|| {
								panic!(
									"slice-2c lowering: positional variant construction for `{}.{}` needs its declared field list, but no `enum {}` was found in this module or its prelude",
									res.enum_name, res.variant, res.enum_name
								)
							});
						names.get(i).cloned().unwrap_or_else(|| {
							panic!(
								"slice-2c lowering: positional argument {i} has no corresponding declared field on `{}.{}`",
								res.enum_name, res.variant
							)
						})
					}
				};
				(label, self.lower_expr(&a.0.value))
			})
			.collect();
		Some(HirExpr::VariantNew {
			enum_name: res.enum_name.clone(),
			variant: res.variant.clone(),
			fields,
		})
	}

	/// Lower spread-free list/tuple items.
	fn lower_items(&self, items: &[nymph_ast::Spanned<ListItem>]) -> Vec<HirExpr> {
		items
			.iter()
			.map(|item| match &item.0 {
				ListItem::Expr(e) => self.lower_expr(e),
				ListItem::Spread(_) => {
					unreachable!("spread-bearing collections use their spread lowering path")
				}
			})
			.collect()
	}

	/// Lower a list literal `#[...]`. A spread-free list lowers to a typed
	/// `HirExpr::Array` via [`Self::lower_items`]. Any spread element
	/// (`#[a, ...xs, b]`) routes through
	/// [`HirExpr::ArraySpread`] instead: each plain item lowers as usual, each
	/// spread source lowers via [`Self::lower_spread_source`] (native splice for
	/// an already-JS-array source, a protocol drain otherwise) — the JS spread
	/// syntax codegen emits for a [`HirArrayElem::Spread`] preserves left-to-
	/// right source order and JS array-spread semantics either way, so a
	/// mid-list spread (`#[a, ...xs, b]`) splices in-position correctly.
	fn lower_list(&self, items: &[nymph_ast::Spanned<ListItem>]) -> HirExpr {
		if !items.iter().any(|i| matches!(i.0, ListItem::Spread(_))) {
			return HirExpr::Array {
				kind: HirArrayKind::List,
				items: self.lower_items(items),
			};
		}
		let elems = items
			.iter()
			.map(|item| match &item.0 {
				ListItem::Expr(e) => HirArrayElem::Item(self.lower_expr(e)),
				ListItem::Spread(e) => HirArrayElem::Spread(self.lower_spread_source(e)),
			})
			.collect();
		HirExpr::ArraySpread {
			kind: HirArrayKind::List,
			elems,
		}
	}

	fn lower_tuple(&self, items: &[nymph_ast::Spanned<ListItem>]) -> HirExpr {
		if !items.iter().any(|i| matches!(i.0, ListItem::Spread(_))) {
			return HirExpr::Array {
				kind: HirArrayKind::Tuple,
				items: self.lower_items(items),
			};
		}
		let elems = items
			.iter()
			.map(|item| match &item.0 {
				ListItem::Expr(e) => HirArrayElem::Item(self.lower_expr(e)),
				ListItem::Spread(e) => HirArrayElem::Spread(HirExpr::Field {
					recv: Box::new(self.lower_expr(e)),
					name: "v".into(),
				}),
			})
			.collect();
		HirExpr::ArraySpread {
			kind: HirArrayKind::Tuple,
			elems,
		}
	}

	/// Lower a map literal's entries — the spread-free fast path both
	/// [`Self::lower_map`] and (pre-SS1) every map literal used; identical
	/// output to before SS1.
	fn lower_map_entries(&self, entries: &[nymph_ast::Spanned<MapEntry>]) -> Vec<(HirExpr, HirExpr)> {
		entries
			.iter()
			.map(|entry| match &entry.0 {
				MapEntry::Entry(k, v) => (self.lower_expr(k), self.lower_expr(v)),
				MapEntry::Spread(_) => panic!("slice-2a lowering does not yet handle spread map entries"),
			})
			.collect()
	}

	/// Lower a map literal `#{...}`. A spread-free map lowers exactly as before
	/// SS1 (`HirExpr::MapLit`, zero behavior change). Any spread entry
	/// (`#{...m, k: v}`) routes through [`HirExpr::MapSpread`] instead: each
	/// plain entry lowers as usual, each spread source dispatches on its
	/// recorded type — an `NMap` source lowers directly (`lower_expr`; it
	/// iterates as `[k, v]` pairs, so it splices straight into the new map's
	/// entries array with no drain), anything else (a
	/// non-map `Iterator`/`Iterable<#(K, V)>` source) drains through
	/// [`Self::lower_spread_source`] exactly like a list spread's non-native
	/// case. `NMap` processes its entries array in order, so entries
	/// emit left-to-right in source order and a later duplicate key overwrites
	/// an earlier one (SS4) with no extra handling needed here.
	fn lower_map(&self, entries: &[nymph_ast::Spanned<MapEntry>]) -> HirExpr {
		if !entries.iter().any(|e| matches!(e.0, MapEntry::Spread(_))) {
			return HirExpr::MapLit(self.lower_map_entries(entries));
		}
		let elems = entries
			.iter()
			.map(|entry| match &entry.0 {
				MapEntry::Entry(k, v) => HirMapElem::Entry(self.lower_expr(k), self.lower_expr(v)),
				MapEntry::Spread(e) => {
					let source_ty = self
						.annotations
						.get(e.id)
						.map(|info| Self::peel_mut(self.interner, info.ty));
					let is_native_map =
						source_ty.is_some_and(|ty| matches!(self.interner.kind(ty), TyKind::Map(..)));
					let lowered = if is_native_map {
						self.lower_expr(e)
					} else {
						self.lower_spread_source(e)
					};
					HirMapElem::Spread(lowered)
				}
			})
			.collect();
		HirExpr::MapSpread(elems)
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
						mutable: meta.is_mutable(),
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
					// Slice 4L, JJ2: a `return` lexically inside a closure body is
					// rejected here rather than lowered — see the `closure_depth`
					// field doc for why an arrow-emitted `return` would be unsound.
					assert!(
						self.closure_depth.get() == 0,
						"slice-4l lowering: `return` inside a closure body is not supported"
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
/// relevant name sets. The one exception is `HirExpr::Closure`: its own
/// params are bound names, not free-variable references, so they are
/// excluded from what gets reported (see that arm) rather than treated like
/// every other unfiltered `Local`. A generic structural walk over every
/// `HirExpr`/`HirStmt` shape — kept exhaustive (no wildcard arm) so a future
/// HIR addition that can reference a `Local` doesn't silently fall through
/// unanalyzed.
fn collect_locals(expr: &HirExpr, out: &mut FxHashSet<EcoString>) {
	match expr {
		HirExpr::Local(name) => {
			out.insert(name.clone());
		}
		HirExpr::Num(..)
		| HirExpr::Str(_)
		| HirExpr::Bool(_)
		| HirExpr::Char(_)
		| HirExpr::ExternValue { .. }
		| HirExpr::This
		| HirExpr::VariantRef { .. } => {}
		HirExpr::InterpolatedString(segments) => {
			for segment in segments {
				collect_locals(segment, out);
			}
		}
		HirExpr::Call { callee, args } => {
			collect_locals(callee, out);
			for a in args {
				collect_locals(a, out);
			}
		}
		// Gap 3 (L0): `name` is an `external(..)` marker, not a `Local`
		// reference — only `args` (receiver + call args) can carry one.
		HirExpr::ExternCall { args, .. } => {
			for a in args {
				collect_locals(a, out);
			}
		}
		HirExpr::BoundDispatch {
			receiver, argument, ..
		} => {
			collect_locals(receiver, out);
			collect_locals(argument, out);
		}
		HirExpr::Array { items, .. } => {
			for item in items {
				collect_locals(item, out);
			}
		}
		HirExpr::ArraySpread { elems, .. } => {
			for elem in elems {
				match elem {
					HirArrayElem::Item(e) | HirArrayElem::Spread(e) => collect_locals(e, out),
				}
			}
		}
		HirExpr::MapLit(pairs) => {
			for (k, v) in pairs {
				collect_locals(k, out);
				collect_locals(v, out);
			}
		}
		HirExpr::MapSpread(elems) => {
			for elem in elems {
				match elem {
					HirMapElem::Entry(k, v) => {
						collect_locals(k, out);
						collect_locals(v, out);
					}
					HirMapElem::Spread(e) => collect_locals(e, out),
				}
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
		HirExpr::ScalarCast { operand, .. } => collect_locals(operand, out),
		// A closure's params are BOUND names, not free-variable references — they
		// must NOT be reported to the caller as `Local`s the way a genuine free
		// variable is, or the Y3 module-let dependency analysis
		// (`reorder_lets_by_dependency`) mistakes a closure param that happens to
		// share a top-level `let`'s name for a real dependency on it (up to a
		// false "circular dependency" panic on legal code — Slice 4L fix). Collect
		// the body into a scratch set first, then remove the closure's own params
		// before merging into `out`, so only genuine free variables the body
		// references (which may still legitimately name a top-level `let`/`func`)
		// propagate outward.
		HirExpr::Closure { params, body } => {
			let mut inner = FxHashSet::default();
			collect_locals(body, &mut inner);
			for p in params {
				inner.remove(p);
			}
			out.extend(inner);
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

/// Collect every `HirExpr::VariantRef`'s `enum_name` reachable anywhere within
/// `expr` into `out` (stdlib body lowering slice, gap a's enum half —
/// see `Lowerer::lower_demanded_runtime_enums`). Mirrors
/// [`collect_locals`]'s exhaustive structural walk (kept exhaustive, no
/// wildcard arm, for the identical reason: a future `HirExpr` shape that can
/// hold a `VariantRef` must not silently go unwalked). Pattern-position
/// variant refs (`HirPat::Variant`) are covered too, via
/// [`collect_variant_ref_enums_pat`], even though none of `ops/mod.nym`'s
/// lowerable bodies happen to match on one today.
fn collect_variant_ref_enums(expr: &HirExpr, out: &mut FxHashSet<EcoString>) {
	match expr {
		HirExpr::VariantRef { enum_name, .. } => {
			out.insert(enum_name.clone());
		}
		HirExpr::Num(..)
		| HirExpr::Str(_)
		| HirExpr::Bool(_)
		| HirExpr::Char(_)
		| HirExpr::ExternValue { .. }
		| HirExpr::Local(_)
		| HirExpr::This => {}
		HirExpr::InterpolatedString(segments) => {
			for segment in segments {
				collect_variant_ref_enums(segment, out);
			}
		}
		HirExpr::Call { callee, args } => {
			collect_variant_ref_enums(callee, out);
			for a in args {
				collect_variant_ref_enums(a, out);
			}
		}
		// Gap 3 (L0): `name` is an `external(..)` marker naming a linked JS
		// function, never a prelude enum — only `args` can reference one.
		HirExpr::ExternCall { args, .. } => {
			for a in args {
				collect_variant_ref_enums(a, out);
			}
		}
		HirExpr::BoundDispatch {
			receiver, argument, ..
		} => {
			collect_variant_ref_enums(receiver, out);
			collect_variant_ref_enums(argument, out);
		}
		HirExpr::Array { items, .. } => {
			for item in items {
				collect_variant_ref_enums(item, out);
			}
		}
		HirExpr::ArraySpread { elems, .. } => {
			for elem in elems {
				match elem {
					HirArrayElem::Item(e) | HirArrayElem::Spread(e) => collect_variant_ref_enums(e, out),
				}
			}
		}
		HirExpr::MapLit(pairs) => {
			for (k, v) in pairs {
				collect_variant_ref_enums(k, out);
				collect_variant_ref_enums(v, out);
			}
		}
		HirExpr::MapSpread(elems) => {
			for elem in elems {
				match elem {
					HirMapElem::Entry(k, v) => {
						collect_variant_ref_enums(k, out);
						collect_variant_ref_enums(v, out);
					}
					HirMapElem::Spread(e) => collect_variant_ref_enums(e, out),
				}
			}
		}
		HirExpr::Index { recv, index } => {
			collect_variant_ref_enums(recv, out);
			collect_variant_ref_enums(index, out);
		}
		HirExpr::MapGet { recv, key } => {
			collect_variant_ref_enums(recv, out);
			collect_variant_ref_enums(key, out);
		}
		HirExpr::New { class, fields } => {
			// The constructed struct's own name is a type reference that may need
			// prelude lowering (an ambient iterator adapter like `MapAdapter`,
			// emitted nowhere until demanded) — surfaced here alongside enum names;
			// the enum-lowering loop ignores any name that isn't a prelude enum,
			// and `lower_demanded_runtime_classes` picks up the struct ones.
			out.insert(class.clone());
			for (_, v) in fields {
				collect_variant_ref_enums(v, out);
			}
		}
		// A prelude enum referenced ONLY through construction (`Some(value = 1)`,
		// `Result.Ok(..)`) — never a bare variant reference/pattern, never a
		// method call — must still register as "referenced": with no
		// `HirExpr::VariantRef`/pattern anywhere, `enum_name` was the only trace
		// this walk would otherwise ever see of it, and skipping it here would
		// leave `Result`/`Option` unlowered while still constructing one of
		// its variants — a runtime `ReferenceError` on the undefined enum object,
		// not a loud compile-time panic (this fix predates any test exercising it
		// directly, caught by inspection while auditing every `HirExpr` variant
		// this walk visits).
		HirExpr::VariantNew {
			enum_name, fields, ..
		} => {
			out.insert(enum_name.clone());
			for (_, v) in fields {
				collect_variant_ref_enums(v, out);
			}
		}
		HirExpr::Field { recv, .. } => collect_variant_ref_enums(recv, out),
		HirExpr::Binary { lhs, rhs, .. } => {
			collect_variant_ref_enums(lhs, out);
			collect_variant_ref_enums(rhs, out);
		}
		HirExpr::Unary { operand, .. } => collect_variant_ref_enums(operand, out),
		HirExpr::Assign { target, value } => {
			collect_variant_ref_enums(target, out);
			collect_variant_ref_enums(value, out);
		}
		HirExpr::Block { stmts, tail } => {
			for stmt in stmts {
				match stmt {
					HirStmt::Let { value, .. } => collect_variant_ref_enums(value, out),
					HirStmt::Expr(e) => collect_variant_ref_enums(e, out),
					HirStmt::Return(v) => {
						if let Some(v) = v {
							collect_variant_ref_enums(v, out);
						}
					}
				}
			}
			if let Some(t) = tail {
				collect_variant_ref_enums(t, out);
			}
		}
		HirExpr::If {
			cond,
			then,
			otherwise,
		} => {
			collect_variant_ref_enums(cond, out);
			collect_variant_ref_enums(then, out);
			if let Some(o) = otherwise {
				collect_variant_ref_enums(o, out);
			}
		}
		HirExpr::While { cond, body } => {
			collect_variant_ref_enums(cond, out);
			collect_variant_ref_enums(body, out);
		}
		HirExpr::Match { scrutinee, arms } => {
			collect_variant_ref_enums(scrutinee, out);
			for arm in arms {
				collect_variant_ref_enums_pat(&arm.pat, out);
				if let Some(g) = &arm.guard {
					collect_variant_ref_enums(g, out);
				}
				collect_variant_ref_enums(&arm.body, out);
			}
		}
		HirExpr::ScalarCast { operand, .. } => collect_variant_ref_enums(operand, out),
		HirExpr::Closure { body, .. } => collect_variant_ref_enums(body, out),
	}
}

/// The `HirPat` counterpart of [`collect_variant_ref_enums`].
fn collect_variant_ref_enums_pat(pat: &HirPat, out: &mut FxHashSet<EcoString>) {
	match pat {
		HirPat::Wildcard | HirPat::Lit(_) | HirPat::Range(_) => {}
		HirPat::Binding { sub, .. } => {
			if let Some(s) = sub {
				collect_variant_ref_enums_pat(s, out);
			}
		}
		HirPat::Variant {
			enum_name, fields, ..
		} => {
			out.insert(enum_name.clone());
			for (_, f) in fields {
				collect_variant_ref_enums_pat(f, out);
			}
		}
		HirPat::Struct { fields } => {
			for (_, f) in fields {
				collect_variant_ref_enums_pat(f, out);
			}
		}
		HirPat::Tuple(items) => {
			for i in items {
				collect_variant_ref_enums_pat(i, out);
			}
		}
		HirPat::List { prefix, suffix, .. } => {
			for i in prefix.iter().chain(suffix) {
				collect_variant_ref_enums_pat(i, out);
			}
		}
		HirPat::Map { entries, .. } => {
			for (_, v) in entries {
				collect_variant_ref_enums_pat(v, out);
			}
		}
		HirPat::Or(a, b) => {
			collect_variant_ref_enums_pat(a, out);
			collect_variant_ref_enums_pat(b, out);
		}
	}
}

/// Whether `expr` (a raw AST node, pre-lowering — used only on prelude
/// bodies `try_lower_runtime_dispatch` is vetting, via
/// `Lowerer::body_calls_unlinked_external`) contains a `Call` naming anything
/// in `names` anywhere within it, recursing through every sub-expression this
/// AST can hold (block statements, closures, match arms/guards, string
/// interpolation, etc.) — the exhaustive-match counterpart of
/// [`collect_variant_ref_enums`] for the AST rather than [`HirExpr`]. Two
/// callee shapes count: a bare `Identifier` (`foo(..)`, a top-level
/// `external` free function) AND a `MemberAccess` callee's OWN member name
/// (`x.foo(..)`, an external INSTANCE method reached through a
/// receiver/`this` — the collections-lowering extension's addition;
/// `body_calls_unlinked_external` now also collects per-impl-member
/// `external` names into `names`, so e.g. `this.length()` inside `is_empty`'s
/// body must be caught the same way a bare `external` call would be). Only
/// the callee's own name is compared either way — `x.foo(..)`'s `x` (the
/// receiver sub-expression) is separately recursed into just below, exactly
/// like a bare call's own `func`.
fn expr_calls_any_name(expr: &Expr, names: &FxHashSet<&EcoString>) -> bool {
	use nymph_ast::expr::{RangeKind, StringPart};

	match &expr.kind {
		ExprKind::Call { func, args, .. } => {
			let is_named_external = match &func.kind {
				ExprKind::Identifier(name) => names.contains(&name.0),
				ExprKind::MemberAccess { member, .. } => names.contains(&member.0),
				_ => false,
			};
			is_named_external
				|| expr_calls_any_name(func, names)
				|| args.iter().any(|a| expr_calls_any_name(&a.0.value, names))
		}
		ExprKind::MemberAccess { parent, .. } => expr_calls_any_name(parent, names),
		ExprKind::IndexAccess { parent, index, .. } => {
			expr_calls_any_name(parent, names) || expr_calls_any_name(index, names)
		}
		ExprKind::Closure { body, .. } => expr_calls_any_name(body, names),
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
			expr_calls_any_name(value, names)
		}
		ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
			expr_calls_any_name(lhs, names) || expr_calls_any_name(rhs, names)
		}
		ExprKind::TypeOp { lhs, .. } | ExprKind::PatternOp { lhs, .. } => {
			expr_calls_any_name(lhs, names)
		}
		ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => value
			.as_deref()
			.is_some_and(|v| expr_calls_any_name(v, names)),
		ExprKind::While {
			condition, body, ..
		} => expr_calls_any_name(condition, names) || expr_calls_any_name(body, names),
		ExprKind::For { iterable, body, .. } => {
			expr_calls_any_name(iterable, names) || expr_calls_any_name(body, names)
		}
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			expr_calls_any_name(condition, names)
				|| expr_calls_any_name(then, names)
				|| otherwise
					.as_deref()
					.is_some_and(|o| expr_calls_any_name(o, names))
		}
		ExprKind::Match { value, arms } => {
			expr_calls_any_name(value, names)
				|| arms.iter().any(|arm| {
					arm
						.guard
						.as_ref()
						.is_some_and(|g| expr_calls_any_name(g, names))
						|| expr_calls_any_name(&arm.body, names)
				})
		}
		ExprKind::Block { body, .. } => body.iter().any(|s| match &s.0 {
			Statement::Expr(e) => expr_calls_any_name(e, names),
			Statement::Let { value, .. } => expr_calls_any_name(value, names),
		}),
		ExprKind::Grouped(inner) => expr_calls_any_name(inner, names),
		ExprKind::List(items) | ExprKind::Tuple(items) => items.iter().any(|i| match &i.0 {
			ListItem::Expr(e) | ListItem::Spread(e) => expr_calls_any_name(e, names),
		}),
		ExprKind::Map(entries) => entries.iter().any(|e| match &e.0 {
			MapEntry::Entry(k, v) => expr_calls_any_name(k, names) || expr_calls_any_name(v, names),
			MapEntry::Spread(e) => expr_calls_any_name(e, names),
		}),
		ExprKind::Range(kind) => match kind {
			RangeKind::From(e) | RangeKind::To(e) | RangeKind::ToInclusive(e) => {
				expr_calls_any_name(e, names)
			}
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				expr_calls_any_name(min, names) || expr_calls_any_name(max, names)
			}
		},
		ExprKind::String(parts) => parts.iter().any(|p| match &p.0 {
			StringPart::InterpolatedExpr(e) => expr_calls_any_name(e, names),
			StringPart::Text(_) | StringPart::EscapeSequence(_) => false,
		}),
		ExprKind::Int(_)
		| ExprKind::UInt(_)
		| ExprKind::Float(_)
		| ExprKind::Char(_)
		| ExprKind::Boolean(_)
		| ExprKind::Identifier(_)
		| ExprKind::AnonymousParam(_)
		| ExprKind::Continue { .. }
		| ExprKind::This => false,
	}
}

/// Reorder top-level `let`s into a valid module-init order via Kahn's topological sort
/// plus a worklist fixpoint for transitive function-call dependencies.
///
/// Each `let` must be emitted after every OTHER top-level `let` its own initializer
/// needs at module-init time — either by directly naming it, or by calling a function
/// that (transitively) reads it. JS module-scope `const`/`let` is TDZ (unlike a hoisted
/// `function` declaration), so naive source-order emission throws `ReferenceError` when
/// a let references a LATER let, directly or through a call chain. Ties keep source order
/// (stable sort). Genuine cycles panic. The fixpoint resolves transitive call-graph
/// dependencies (avoiding the memoization bugs of a recursion-guarded DFS under mutual
/// recursion), then Kahn's algorithm emits lets in dependency order.
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
		Pattern::Int(v) => HirLit::Num(v.0 as f64, NumKind::Int),
		Pattern::UInt(v) => HirLit::Num(v.0 as f64, NumKind::UInt),
		Pattern::Float(v) => HirLit::Num(v.0.into_inner(), NumKind::Float),
		Pattern::Boolean(b) => HirLit::Bool(b.0),
		Pattern::Char(c) => HirLit::Char(c.0),
		Pattern::String(parts) => HirLit::Str(lower_string_pattern(parts)),
		Pattern::Grouped(inner) => lower_lit_pattern(inner),
		other => panic!("slice-3b expects a literal pattern (map key / range bound), got {other:?}"),
	}
}

/// Cook a single escape sequence's expansion into `buf` (Slice 4H). Every
/// variant but `Interpolation` (`\${`) expands to one concrete char via
/// [`nymph_ast::expr::StringEscape::to_char`]; `Interpolation` has no single-char
/// expansion (it exists so `${` can appear literally in text without starting
/// real interpolation) and cooks to the two literal characters `${`.
fn push_cooked_escape(buf: &mut EcoString, esc: nymph_ast::expr::StringEscape) {
	match esc.to_char() {
		Some(c) => buf.push(c),
		None => buf.push_str("${"),
	}
}

/// Concatenate a string pattern's parts, cooking any escapes (Slice 4H —
/// previously text-only; `StringPatternPart` has no interpolation variant, so
/// unlike the expression-side `lower_string_expr` this never needs to fold in a
/// subexpression).
fn lower_string_pattern(
	parts: &[nymph_ast::Spanned<nymph_ast::expr::StringPatternPart>],
) -> ecow::EcoString {
	use nymph_ast::expr::StringPatternPart;
	let mut s = ecow::EcoString::new();
	for part in parts {
		match &part.0 {
			StringPatternPart::Text(t) => s.push_str(t),
			StringPatternPart::EscapeSequence(esc) => push_cooked_escape(&mut s, *esc),
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

/// Whether `name` is a JS reserved word this compiler's own keyword set
/// doesn't already exclude from being written as a Nymph identifier — see
/// [`Lowerer::declare`]'s doc comment for why this matters (`default`, used
/// as a parameter name throughout `option.nym`/`result.nym`/`ops/mod.nym`, is
/// the one that actually surfaces in real stdlib source). Deliberately the
/// full ECMAScript reserved-word list (strict-mode keywords, future-reserved
/// words, and the three literal keywords), not just `default`: cheap to keep
/// complete, and every OTHER entry is equally unreachable as a Nymph
/// identifier today only because nothing has tried yet, not because Nymph's
/// own keyword set already reserves it too.
fn is_js_reserved_word(name: &str) -> bool {
	matches!(
		name,
		"break"
			| "case"
			| "catch"
			| "class"
			| "const"
			| "continue"
			| "debugger"
			| "default"
			| "delete"
			| "do"
			| "else"
			| "export"
			| "extends"
			| "finally"
			| "for"
			| "function"
			| "if"
			| "import"
			| "in"
			| "instanceof"
			| "new"
			| "return"
			| "super"
			| "switch"
			| "this"
			| "throw"
			| "try"
			| "typeof"
			| "var"
			| "void"
			| "while"
			| "with"
			| "yield"
			| "let"
			| "static"
			| "enum"
			| "await"
			| "implements"
			| "package"
			| "private"
			| "protected"
			| "public"
			| "interface"
			| "null"
			| "true"
			| "false"
	)
}

/// The mangled-function self-type tag for a prelude impl's target `Type`
/// (stdlib body lowering slice, gap b) — `Some` only for the primitive
/// types the AST represents as their OWN dedicated `Type` variant (never
/// `Type::Reference`), which is exactly the set of concrete, non-blanket
/// primitive impls `ops/mod.nym` provides (`int`/`uint`/`float`/`char`/
/// `string`/`boolean`). `None` for everything else — a blanket impl's target
/// (`Type::Reference` naming the impl's own generic parameter), a named
/// struct/enum (never how a PRELUDE impl targets a primitive; user impls
/// never reach this function at all), or a structural type
/// (`#()`/list/tuple/map/function) this slice's target inventory has no
/// lowerable body for anyway — `try_lower_runtime_dispatch`
/// treats `None` as "stay loud", matching `push_impl_for_methods`'s existing
/// V5 blanket-impl doctrine.
fn primitive_type_tag(ty: &nymph_ast::ty::Type) -> Option<&'static str> {
	use nymph_ast::ty::Type;
	match ty {
		Type::Int => Some("int"),
		Type::UInt => Some("uint"),
		Type::Float => Some("float"),
		Type::Char => Some("char"),
		Type::String => Some("string"),
		Type::Boolean => Some("boolean"),
		_ => None,
	}
}

/// The mangled-function self-type tag for a prelude INHERENT impl's target
/// `Type` (`Declaration::Impl`, the collections-lowering extension of
/// `try_lower_runtime_dispatch`) — a superset of `primitive_type_tag`
/// that also covers the two STRUCTURAL types every real stdlib collection
/// method is declared on: `List` (`impl<T> #[T] { .. }` / `impl<T> mut #[T]
/// { .. }`) and `Map` (`impl<K,V> #{K:V} { .. }` / `impl<K,V> mut #{K:V} {
/// .. }`). `primitive_type_tag` itself deliberately excludes these (its own
/// doc comment: "a structural type this slice's target inventory has no
/// lowerable body for anyway") — this function IS that later inventory,
/// scoped narrowly to `List`/`Map` since that's the concrete, testable
/// payoff (a named struct/enum receiver, or a blanket impl's own generic
/// parameter, both still return `None` — no single JS class either shape can
/// be tagged with generically without a collision/soundness risk, so both
/// stay a loud deferral, matching V5's existing blanket-impl doctrine).
///
/// `mutable` (the `Declaration::Impl`'s own `mutable` flag — `impl<T> mut
/// #[T]` vs `impl<T> #[T]`) folds into the tag so the two impl blocks never
/// collide under the SAME mangled name for a same-named method: the real
/// stdlib duplicates several names (`length`/`get`, `size`/`get`, …) across
/// its `mut`/non-`mut` impls of the identical receiver type. `Type::Mut` is
/// also peeled defensively first, in case a future parse shape ever wraps
/// `type_` itself in a `mut` VIEW rather than recording it only via the
/// declaration's own flag (today's parser does the latter — see
/// `parser::decl::parse_impl` — so this loop runs zero iterations against
/// real stdlib source, but costs nothing to keep honest either way).
fn inherent_self_type_tag(type_: &nymph_ast::ty::Type, mutable: bool) -> Option<EcoString> {
	use nymph_ast::ty::Type;

	let mut mutable = mutable;
	let mut ty = type_;
	while let Type::Mut(inner) = ty {
		mutable = true;
		ty = &inner.0;
	}
	let base: &str = match ty {
		Type::List(_) => "list",
		Type::Map(..) => "map",
		other => return primitive_type_tag(other).map(EcoString::from),
	};
	Some(if mutable {
		format!("mut_{base}").into()
	} else {
		base.into()
	})
}

fn runtime_type_tag(lowering_tag: &str) -> EcoString {
	match lowering_tag {
		"boolean" => "nymph.bool".into(),
		other => format!("nymph.{other}").into(),
	}
}
