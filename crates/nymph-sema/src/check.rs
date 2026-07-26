//! The type checker's core state and driver.
//!
//! `Checker` owns the interner, the resolved [`DefMap`], the lowered [`Signatures`],
//! the unification table, and the accumulated diagnostics, plus the transient
//! per-body state (local scopes, the active generic-parameter scope, the current
//! `self`/return types). The inference rules themselves live in `infer_expr.rs`,
//! `infer_pattern.rs`, `lower.rs`, and `coerce.rs` as further `impl Checker` blocks;
//! keeping them in separate files is the deliberate anti-monolith split.

use crate::errors::TypeError;
use ecow::EcoString;
use nymph_ast::{NodeId, Span, decl::Declaration, decl::Module};
use nymph_diagnostics::Diagnostic;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::annotate::{Checked, CheckedSemantic};
use crate::def::{DefMap, Signatures, build_def_map};
use crate::ids::{DefId, InferVar, ParamIdx};
use crate::ty::fold::occurs;
use crate::ty::{Interner, Ty, TyKind};
use crate::unify::{TyVarValue, UnifyTable};

/// A local variable binding in a lexical scope.
pub(crate) struct Binding {
	pub ty: Ty,
	pub mutable: bool,
}

/// Which AST node shape a deferred `pending_operators` entry was recorded from,
/// carrying the specific operator itself (Slice 4C-a: a prefix op has no separate
/// `BinaryOperator` to hang off a shared tuple slot, so the operator moved into the
/// variant) — see [`Checker::pending_operators`] for why `finalize_pending_operators`
/// must treat `BinaryOp`/`AssignOp` vs. `PrefixOp` differently (Finding 1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
// The shared `Op` postfix names the AST node shape each variant was recorded
// from (`BinaryOp`/`AssignOp`/`PrefixOp` expression kinds) — deliberate, not an
// accidental naming collision the lint should flag.
#[allow(clippy::enum_variant_names)]
pub(crate) enum PendingOperatorKind {
	BinaryOp(nymph_ast::ops::BinaryOperator),
	AssignOp(nymph_ast::ops::BinaryOperator),
	PrefixOp(nymph_ast::ops::PrefixOperator),
}

pub struct Checker<'m> {
	pub(crate) module: &'m Module,
	pub(crate) interner: Interner,
	pub(crate) defs: DefMap,
	pub(crate) sigs: Signatures,
	/// Collected interface definitions (method signatures), keyed by interface def.
	pub(crate) interfaces: FxHashMap<DefId, crate::iface::InterfaceDef>,
	/// Collected `impl` blocks, indexed for candidate lookup by the solver.
	pub(crate) impls: crate::iface::ImplRegistry,
	/// Inherent methods and statics (methods not attached to an interface), indexed
	/// by the implementing type's head constructor.
	pub(crate) inherent: crate::members::InherentRegistry<'m>,
	pub(crate) table: UnifyTable,
	pub(crate) diags: Vec<Diagnostic>,
	pub(crate) external_value_marshals: FxHashMap<Span, nymph_hir::hir::MarshalKind>,

	// ── Transient per-body state ─────────────────────────────────────────────
	pub(crate) scopes: Vec<FxHashMap<EcoString, Binding>>,
	/// Stack of generic-parameter scopes (name → rigid `ParamIdx`).
	pub(crate) params: Vec<FxHashMap<EcoString, ParamIdx>>,
	/// The interface bounds on the generic parameters of the body currently being
	/// checked (`ParamIdx` → the interfaces it must implement). Rebuilt per body; used
	/// to resolve a namespaced call through a type parameter, e.g. `R.default()` where
	/// `R: Default`.
	pub(crate) param_bounds: FxHashMap<ParamIdx, Vec<DefId>>,
	/// Declared bounds with their named interface arguments preserved.
	pub(crate) param_bound_details: FxHashMap<ParamIdx, Vec<crate::iface::Bound>>,
	/// The type `this`/`self` refers to inside the current method, if any.
	pub(crate) self_ty: Option<Ty>,
	/// The declared/expected return type of the function currently being checked.
	pub(crate) ret_ty: Option<Ty>,
	/// Recursion guard for on-demand type-alias expansion.
	pub(crate) alias_depth: u32,
	/// Counter minting anonymous generic parameters for `impl Interface` used in type
	/// position (an interface reference desugars to a fresh generic bounded by it).
	pub(crate) synthetic_params: u32,
	/// The interface bounds on the anonymous parameters minted for `impl Interface` types,
	/// by `ParamIdx`. Unlike `param_bounds` (rebuilt per body for declared generics), these
	/// are recorded once at mint time and persist, because the parameter is baked into a
	/// stored signature and its bound must still resolve at every call site.
	pub(crate) synthetic_bounds: FxHashMap<ParamIdx, Vec<DefId>>,
	/// Full bounds for synthetic opaque interface types, including their generic
	/// arguments. Used when checking that a concrete return value implements the
	/// declared opaque interface with the exact associated arguments.
	pub(crate) synthetic_bound_details: FxHashMap<ParamIdx, Vec<crate::iface::Bound>>,
	/// Set only while checking an interface's own default-method body
	/// (`check_interface_default_body`, Slice 4C-b): `(interface, this's ParamIdx)`.
	/// `resolve_method` consults this to resolve a call to another method of *this
	/// same interface* on `this` directly against the interface's own signature,
	/// bypassing impl search entirely — see `resolve_method`'s doc comment on why
	/// the ordinary impl/blanket search is wrong for this one case.
	pub(crate) checking_interface_default: Option<(DefId, ParamIdx)>,

	/// The per-expression decisions recorded for the lowering pass (resolved type,
	/// selected operator/method impl). Keyed by [`nymph_ast::NodeId`]. Emitted
	/// alongside diagnostics as part of [`crate::Checked`].
	pub(crate) annotations: crate::annotate::Annotations,

	/// Operator nodes whose LHS operand was still an unresolved inference variable
	/// at the moment `infer_binary`'s fallback arm ran (an `Infer` type var that
	/// hasn't unified with a primitive/ADT yet — see the D3 fallback in
	/// `infer_expr.rs`). Recorded as `(node id, operator, span, operand ty, kind)` and
	/// drained at the end of the *same body* that recorded it (`finalize_pending_operators`,
	/// called from `check_func_body`, `check_let_body`, `check_method_body`, and
	/// `check_interface_impl_members`), while that body's own `param_bounds` and the
	/// unify table are still alive, so an operand resolved later in the same body
	/// (e.g. via a `check`-mode subtype applied *after* the operator node was
	/// recorded) still gets a `Resolution` instead of forcing lowering to panic on a
	/// valid program. Must be drained per body, not once at module end: `param_bounds`
	/// is a single shared map that each body's checking clears and rebuilds, so a
	/// module-end pass would resolve every deferred operator against only the *last*
	/// body's bounds, making a valid program's diagnostics depend on declaration
	/// order. `kind` distinguishes a `BinaryOp` node (whose recorded type is the
	/// operator's own placeholder result and must be unified with the
	/// finally-resolved type) from an `AssignOp` node (whose recorded type is always
	/// `Void` and must be left alone — Finding 1: only the `Resolution` gets attached
	/// there).
	pub(crate) pending_operators: Vec<(nymph_ast::NodeId, Span, Ty, Ty, PendingOperatorKind)>,

	/// Call-site bound obligations deferred until the instantiated variable has
	/// had a chance to unify with a concrete argument (Slice 4G), mirroring
	/// `pending_operators` exactly: `fn_type_of` pushes one entry per bound on
	/// every minted var — declared generics (`FuncSig::bounds`) and `impl Trait`
	/// synthetics (`synthetic_bounds`) alike — and `finalize_pending_bounds`
	/// drains them at the end of the *same body* that recorded them (called
	/// alongside `finalize_pending_operators` from every per-body driver), while
	/// that body's own `param_bounds`/`synthetic_bounds` and the unify table are
	/// still live. Must be drained per body, not once at module end, for the same
	/// reason `pending_operators` is: a module-end pass would check every
	/// obligation against only the last-checked body's `param_bounds`.
	pub(crate) pending_bounds: Vec<PendingBound>,

	/// MT2 OO4: per-`pending_bounds`-variable record of whether the ACTUAL
	/// argument(s) bound to it (at a free-function call site) were `mut` —
	/// `(saw_a_mut_arg, saw_a_plain_arg)`. `subtype`'s one-way `mut T <: T`
	/// cancellation (`coerce.rs`) erases an argument's `mut`-ness the moment it
	/// binds the call's freshly-minted generic-parameter variable, so a bound
	/// obligation like `T: A` — where `A` is implemented only for `mut B`
	/// (`impl A for mut B` / `impl mut A for B`) — would otherwise always see a
	/// plain `B` by the time `finalize_pending_bounds` checks it, regardless of
	/// what the caller actually passed. `check_call_arg` (`infer_expr.rs`) is
	/// the one site that captures this, BEFORE the cancellation, keyed by the
	/// exact (still-unresolved) `Ty` handle `fn_type_of` also used as the
	/// obligation's own `ty` field — the same fresh variable, so the keys agree.
	/// Drained per body alongside `pending_bounds` (same lifecycle, same
	/// reasoning: a module-end pass would check every obligation against only
	/// the last body's recordings).
	pub(crate) pending_bound_arg_mut: FxHashMap<Ty, (bool, bool)>,

	/// Stack of the parameter types of the anonymous (`$N`) closures currently
	/// being FORMED, innermost last — pushed/popped only by
	/// `Checker::form_anon_closure` (`anon_closure.rs`) around lowering the
	/// committed boundary node's own kind. `ExprKind::AnonymousParam(idx)`
	/// reads `idx` (default 0) out of the innermost frame directly, rather
	/// than through the ordinary local-scope lookup every other identifier
	/// uses — a `$N` is never a real binding, just a positional index into
	/// whichever anonymous closure currently encloses it.
	pub(crate) anon_ctx: Vec<Vec<Ty>>,
	/// NodeIds of `$N` occurrences already claimed by an in-progress
	/// `Checker::resolve_anon` scan (Slice: `$N` anonymous closure params).
	/// Guards against a NESTED slot reached while trial- or really-evaluating
	/// an already-committed boundary (e.g. `check_call_arg` on an argument
	/// that turns out to just be a bare `$0`, itself already consumed by an
	/// enclosing boundary one level up) rediscovering and re-binding the same
	/// occurrence as an independent, spurious one-off boundary of its own.
	/// Never cleared: a `NodeId` is assigned once, globally, by the parser,
	/// so a given `$N` occurrence can only ever be discovered by exactly one
	/// top-level `resolve_anon` scan.
	pub(crate) anon_consumed: FxHashSet<NodeId>,
}

/// One deferred call-site bound obligation: the call/reference span, the
/// (possibly still-unresolved) minted variable, the required interface, and
/// its argument bindings (substituted through the same call-site map as the
/// variable itself — empty for a synthetic `impl Trait` param, which carries
/// no argument fidelity). See [`Checker::pending_bounds`].
pub(crate) type PendingBound = (Span, Ty, DefId, Vec<(EcoString, Ty)>);

/// Whether [`check_module_impl`] should additionally validate the module's
/// entry point (`main`) — see [`check_module`] vs [`check_module_entry`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::SalsaValue)]
pub enum EntryMode {
	/// Plain library-mode checking: no `main` requirement. Every existing
	/// caller of `check_module` gets this and is unaffected by entry-point
	/// validation.
	Library,
	/// The module is the program's entry module: a top-level `func main`
	/// taking no parameters, declaring no generics, and declaring no return
	/// type other than `void` is required (see `entry::check_entry_main`).
	Entry,
}

/// Check a whole (single) module and return every diagnostic produced.
///
/// This is the Milestone-A entry point. It runs the three conceptual passes in
/// order: item resolution (`build_def_map`), signature lowering, then body
/// inference. The signature/body split mirrors the incremental query boundary the
/// full salsa driver will formalise later. Shared by [`check_module`] (library
/// mode) and [`check_module_entry`] (entry mode) — see [`EntryMode`].
pub(crate) fn check_module_impl(module: &Module, entry: EntryMode) -> Checked {
	let mut diags = Vec::new();
	let defs = build_def_map(module, &mut diags);
	let mut checker = Checker::new(module, defs, diags);
	checker.lower_signatures();
	checker.collect_interfaces();
	// Inherent methods (struct/enum body `func`s, top-level inherent `impl Type {
	// .. }`) must be collected before interface impls (below) so `finish_interface_impl`
	// (iface.rs) can cross-check an interface-impl method against the owning type's
	// already-known inherent methods (Slice 4K, HH3) — closing the ICE where a
	// same-named inherent method and interface-impl method type-checked clean and
	// only panicked later in `lower_hir.rs`'s `assert_no_duplicate_methods`. Neither
	// pass reads data the other produces otherwise, so this reordering is safe.
	checker.collect_inherent();
	checker.collect_impls();
	checker.collect_inner_impls();
	checker.check_coherence();
	checker.generalize_returns();
	checker.check_bodies();
	checker.check_member_bodies();
	checker.check_external_value_linkage();
	checker.check_external_func_kinds();
	if entry == EntryMode::Entry {
		// Runs after every body has been checked, so its diagnostics append
		// after body-checking diagnostics rather than interleaving with them.
		checker.check_entry_main();
	}
	// Every body-checking path (`check_func_body`, `check_let_body`,
	// `check_method_body`, `check_interface_impl_members`) drains its own
	// `pending_operators` entries before the next body's `param_bounds` are
	// built, while that body's own bounds are still live -- see
	// `finalize_pending_operators`'s doc comment for why per-body draining, not a
	// single module-end pass, is required. Nothing should be left by the time
	// every body has been checked.
	debug_assert!(
		checker.pending_operators.is_empty(),
		"pending_operators should be drained per-body, not left for module end"
	);
	debug_assert!(
		checker.pending_bounds.is_empty(),
		"pending_bounds should be drained per-body, not left for module end"
	);
	debug_assert!(
		checker.pending_bound_arg_mut.is_empty(),
		"pending_bound_arg_mut should be drained per-body, not left for module end"
	);
	// Some annotations are recorded before later expressions constrain their
	// inference variables. Resolve them one final time while the unify table is
	// still available so lowering never sees a stale `TyKind::Infer`.
	let mut annotations = std::mem::take(&mut checker.annotations);
	annotations.map_types(|ty| checker.resolve_deep(ty));
	// Signatures are lowered before body inference, so their slots may still hold
	// inference handles even after checking has solved them.  CheckedSemantic is
	// the immutable completion boundary: publish the resolved forms, just as we
	// already do for expression annotations, rather than making downstream
	// consumers consult the checker's now-discarded unification table.
	let let_ids = checker.sigs.lets.keys().copied().collect::<Vec<_>>();
	for id in let_ids {
		let ty = checker.sigs.lets[&id];
		let resolved = checker.resolve_deep(ty);
		checker.sigs.lets.insert(id, resolved);
	}
	let inherent = checker
		.inherent
		.impls
		.iter()
		.map(|implementation| crate::annotate::CheckedInherentImpl {
			generics: implementation
				.owner_generics
				.iter()
				.map(|generic| generic.0.name.0.clone())
				.collect(),
			self_ty: implementation.self_ty,
			constraints: implementation.constraints.clone(),
			methods: implementation
				.methods
				.iter()
				.map(|(name, method)| {
					(
						name.clone(),
						crate::annotate::CheckedMethod {
							params: method.params.clone(),
							ret: method.ret,
							bounds: method.bounds.clone(),
						},
					)
				})
				.collect(),
		})
		.collect();
	Checked {
		diags: checker.diags,
		annotations,
		external_value_marshals: checker.external_value_marshals,
		interner: checker.interner,
		semantic: CheckedSemantic {
			definitions: checker.defs,
			signatures: checker.sigs,
			interfaces: checker.interfaces,
			implementations: checker.impls,
			inherent,
			anonymous_bounds: checker.synthetic_bound_details,
		},
	}
}

/// Check a whole (single) module and return every diagnostic produced.
///
/// Library mode: does not require a `main` entry point. Use
/// [`check_module_entry`] for the program's entry module.
pub fn check_module(module: &Module) -> Checked {
	check_module_impl(module, EntryMode::Library)
}

/// Check a whole (single) module as the program's *entry module* and return
/// every diagnostic produced.
///
/// Identical to [`check_module`], except it additionally requires a top-level
/// `func main` taking no parameters, declaring no generics, and declaring no
/// return type other than `void` — see `entry::check_entry_main` for the
/// exact rules and [`crate::TypeError`]'s `Main*` variants for the
/// diagnostics it can emit.
pub fn check_module_entry(module: &Module) -> Checked {
	check_module_impl(module, EntryMode::Entry)
}

/// Check several modules together as one program.
///
/// This is the **minimal multi-module driver** (the module-resolution shim the plan
/// anticipated): it flattens every module's top-level declarations into one combined
/// module and runs the single-module checker over the result. `import` statements are
/// dropped — after flattening, every item shares a single global namespace, so an
/// `import @/x with (Y)` needs no separate binding step. This lets the cross-file
/// stdlib typecheck before the full salsa module graph (a later project-layer concern)
/// exists. It deliberately does *not* yet enforce per-module visibility or import
/// aliasing; those arrive with the real module graph.
pub fn check_program(modules: &[Module]) -> Checked {
	let mut members = Vec::new();
	for module in modules {
		for decl in &module.members {
			if matches!(decl, Declaration::Import { .. }) {
				continue;
			}
			members.push(decl.clone());
		}
	}
	let combined = Module {
		members,
		path: "<program>".into(),
	};
	check_module(&combined)
}

impl<'m> Checker<'m> {
	fn check_external_func_kinds(&mut self) {
		fn check_members(
			checker: &mut Checker<'_>,
			members: &[nymph_ast::Spanned<nymph_ast::decl::ImplMember>],
		) {
			for member in members {
				if let nymph_ast::decl::ImplMember::ExternalFunc(_, marker, meta) = &member.0
					&& nymph_hir::linkage::is_value_marker(marker)
				{
					checker.emit(
						meta.name.1,
						TypeError::ExternalFunctionLinkageWrongKind {
							marker: marker.clone(),
						},
					);
				}
			}
		}
		for decl in &self.module.members {
			use nymph_ast::decl::Declaration;
			match decl {
				Declaration::ExternalFunc(_, marker, meta)
					if nymph_hir::linkage::is_value_marker(marker) =>
				{
					self.emit(
						meta.name.1,
						TypeError::ExternalFunctionLinkageWrongKind {
							marker: marker.clone(),
						},
					)
				}
				Declaration::Struct { members, impls, .. } | Declaration::Enum { members, impls, .. } => {
					check_members(self, members);
					for impl_ in impls {
						check_members(self, &impl_.0.members);
					}
				}
				Declaration::Namespace { members, .. }
				| Declaration::Impl { members, .. }
				| Declaration::ImplFor { members, .. } => check_members(self, members),
				Declaration::Interface { members, .. } => {
					for member in members {
						if let nymph_ast::decl::InterfaceMember::Impl { members, .. } = &member.0 {
							check_members(self, members);
						}
					}
				}
				_ => {}
			}
		}
	}

	fn check_external_value_linkage(&mut self) {
		use nymph_ast::decl::Declaration;
		for decl in &self.module.members {
			let Declaration::ExternalLet(_, marker, meta) = decl else {
				continue;
			};
			let span = meta.name.1;
			let linked = match nymph_hir::linkage::lookup_value(marker) {
				Ok(linked) => Some(linked),
				Err(nymph_hir::linkage::LinkageError::Missing { .. }) => {
					self.emit(
						span,
						TypeError::ExternalValueLinkageMissing {
							marker: marker.clone(),
						},
					);
					None
				}
				Err(nymph_hir::linkage::LinkageError::WrongKind { .. }) => {
					self.emit(
						span,
						TypeError::ExternalLinkageWrongKind {
							marker: marker.clone(),
						},
					);
					None
				}
			};
			let marshal = self
				.defs
				.get(&meta.name.0.as_binding().expect("external let binding").0)
				.and_then(|def| self.sigs.lets.get(&def).copied())
				.and_then(|ty| match self.interner.kind(ty) {
					nymph_hir::ty::TyKind::Int => Some(nymph_hir::hir::MarshalKind::Int),
					nymph_hir::ty::TyKind::UInt => Some(nymph_hir::hir::MarshalKind::UInt),
					nymph_hir::ty::TyKind::Float => Some(nymph_hir::hir::MarshalKind::Float),
					nymph_hir::ty::TyKind::Char => Some(nymph_hir::hir::MarshalKind::Char),
					nymph_hir::ty::TyKind::String => Some(nymph_hir::hir::MarshalKind::String),
					nymph_hir::ty::TyKind::Boolean => Some(nymph_hir::hir::MarshalKind::Boolean),
					nymph_hir::ty::TyKind::List(_) => Some(nymph_hir::hir::MarshalKind::List),
					nymph_hir::ty::TyKind::Tuple(_) => Some(nymph_hir::hir::MarshalKind::Tuple),
					nymph_hir::ty::TyKind::Map(_, _) => Some(nymph_hir::hir::MarshalKind::Map),
					_ => None,
				});
			if let Some(marshal) = marshal {
				self.external_value_marshals.insert(span, marshal);
			}
			if marshal.is_none() {
				self.emit(span, TypeError::ExternalValueTypeUnsupported);
			} else if linked.is_some_and(|linked| Some(linked.marshal) != marshal) {
				self.emit(
					span,
					TypeError::ExternalValueTypeMismatch {
						marker: marker.clone(),
					},
				);
			}
			if meta.is_mutable() {
				self.emit(span, TypeError::ExternalValueMutable);
			}
		}
	}

	fn new(module: &'m Module, defs: DefMap, diags: Vec<Diagnostic>) -> Self {
		Self {
			module,
			interner: Interner::new(),
			defs,
			sigs: Signatures::default(),
			interfaces: FxHashMap::default(),
			impls: crate::iface::ImplRegistry::default(),
			inherent: crate::members::InherentRegistry::default(),
			table: UnifyTable::new(),
			diags,
			external_value_marshals: FxHashMap::default(),
			scopes: Vec::new(),
			params: Vec::new(),
			param_bounds: FxHashMap::default(),
			param_bound_details: FxHashMap::default(),
			self_ty: None,
			ret_ty: None,
			alias_depth: 0,
			synthetic_params: 0,
			synthetic_bounds: FxHashMap::default(),
			synthetic_bound_details: FxHashMap::default(),
			checking_interface_default: None,
			annotations: crate::annotate::Annotations::default(),
			pending_operators: Vec::new(),
			pending_bounds: Vec::new(),
			pending_bound_arg_mut: FxHashMap::default(),
			anon_ctx: Vec::new(),
			anon_consumed: FxHashSet::default(),
		}
	}

	// ── Diagnostics ──────────────────────────────────────────────────────────
	/// Emit a typed [`TypeError`](crate::errors::TypeError), anchored at `span`.
	pub(crate) fn emit(&mut self, span: Span, err: TypeError) {
		use nymph_diagnostics::IntoDiagnostic;
		self.diags.push(err.as_diagnostic(span));
	}

	// ── Annotations ──────────────────────────────────────────────────────────
	/// Record the checker's decision about an expression node so the lowering pass
	/// can read it back. `ty` is the node's resolved type; `resolution` is set only
	/// for desugared operator/cast/method nodes (whose selected impl codegen needs).
	pub(crate) fn record(
		&mut self,
		id: nymph_ast::NodeId,
		ty: Ty,
		resolution: Option<crate::annotate::Resolution>,
	) {
		// Zonk before storing: a raw `Ty` can still be an unsolved inference variable,
		// and the unify table is dropped when checking finishes, so it must be resolved
		// to its concrete form *now* while the table is alive. (Interpreting the stored
		// `Ty` also needs the `Interner`, which the lowering slice threads through.)
		let ty = self.resolve_deep(ty);
		self
			.annotations
			.record(id, crate::annotate::ExprInfo { ty, resolution });
	}

	// ── Inference variables ──────────────────────────────────────────────────
	pub(crate) fn fresh(&mut self) -> Ty {
		let var = self.table.new_var();
		self.interner.mk_infer(var)
	}

	// ── Local scopes ─────────────────────────────────────────────────────────
	pub(crate) fn push_scope(&mut self) {
		self.scopes.push(FxHashMap::default());
	}

	pub(crate) fn pop_scope(&mut self) {
		self.scopes.pop();
	}

	pub(crate) fn define_local(&mut self, name: EcoString, ty: Ty, mutable: bool) {
		if let Some(scope) = self.scopes.last_mut() {
			scope.insert(name, Binding { ty, mutable });
		}
	}

	pub(crate) fn lookup_local(&self, name: &str) -> Option<&Binding> {
		self.scopes.iter().rev().find_map(|scope| scope.get(name))
	}

	// ── Generic-parameter scopes ─────────────────────────────────────────────
	pub(crate) fn push_params(&mut self, scope: FxHashMap<EcoString, ParamIdx>) {
		self.params.push(scope);
	}

	pub(crate) fn pop_params(&mut self) {
		self.params.pop();
	}

	pub(crate) fn lookup_param(&self, name: &str) -> Option<ParamIdx> {
		self
			.params
			.iter()
			.rev()
			.find_map(|scope| scope.get(name).copied())
	}

	// ── Type resolution ──────────────────────────────────────────────────────
	/// Peel a chain of bound inference variables from the top of a type. The result
	/// is either a non-variable type or an *unbound* variable (in canonical form).
	pub(crate) fn shallow_resolve(&mut self, ty: Ty) -> Ty {
		let var = match self.interner.kind(ty) {
			TyKind::Infer(v) => *v,
			_ => return ty,
		};
		match self.table.probe(var) {
			TyVarValue::Known(bound) => self.shallow_resolve(bound),
			TyVarValue::Unknown => {
				let root = self.table.root(var);
				self.interner.mk_infer(root)
			}
		}
	}

	/// Peel a top-level `mut` wrapper, if present. `mk_mut` guarantees `Mut` never
	/// nests, so a single peel is always enough. Used at the two mutability
	/// "cancel points": a plain `let x = v` drops `v`'s `mut`, and dispatch/
	/// type-inspection sites that don't care about mutability.
	pub(crate) fn strip_mut(&mut self, ty: Ty) -> Ty {
		let ty = self.shallow_resolve(ty);
		match self.interner.kind(ty) {
			TyKind::Mut(inner) => *inner,
			_ => ty,
		}
	}

	/// If `expected` is (shallow-resolved to) a concrete enum `Adt` whose variants
	/// include `name`, return that enum's def and the variant's index — the
	/// type-directed resolution a bare variant name (pattern or construction) should
	/// try FIRST, before falling back to the global by-name `DefMap::resolve_variant`.
	/// Returns `None` for an unbound inference var, a non-enum Adt, or a name that
	/// isn't one of the enum's variants — in all of which cases the caller falls back
	/// to today's global path unchanged.
	pub(crate) fn expected_enum_variant(
		&mut self,
		expected: Ty,
		name: &str,
	) -> Option<(DefId, usize)> {
		let ty = self.strip_mut(expected);
		let TyKind::Adt(def, _) = self.interner.kind(ty).clone() else {
			return None;
		};
		if !matches!(self.defs.data(def).kind, crate::def::DefKind::Enum { .. }) {
			return None;
		}
		let idx = self
			.sigs
			.enums
			.get(&def)?
			.variants
			.iter()
			.position(|v| v.name == name)?;
		Some((def, idx))
	}

	/// If `expected` is (shallow-resolved to) a concrete `List` type, return its
	/// element type — the type-directed target a list literal's own elements should
	/// check against, so a nested expression (e.g. a bare variant) sees the concrete
	/// element type instead of a still-unbound fresh var that would only unify with
	/// `expected` after the fact. Returns `None` for an unbound inference var or any
	/// non-list type, in which case the caller falls back to a fresh element var
	/// exactly as `infer_kind`'s own `ExprKind::List` arm does.
	pub(crate) fn expected_list_element(&mut self, expected: Ty) -> Option<Ty> {
		let ty = self.strip_mut(expected);
		match self.interner.kind(ty) {
			TyKind::List(elem) => Some(*elem),
			_ => None,
		}
	}

	/// If `expected` is (shallow-resolved to, through `mut`) a concrete `Map` type,
	/// return its `(key, value)` types — the type-directed target a map literal's
	/// own entries should check against. Mirrors [`Self::expected_list_element`]
	/// exactly (see its doc comment); the `Map` counterpart is what lets
	/// `check_dispatch`'s own `ExprKind::Map` arm propagate a concrete, possibly
	/// `mut`, value type (e.g. `#{int: mut #[int]}`'s value) down into a nested
	/// literal, instead of that nested literal only ever seeing an unconstrained
	/// fresh var. Returns `None` for an unbound inference var or any non-map type.
	pub(crate) fn expected_map_entry(&mut self, expected: Ty) -> Option<(Ty, Ty)> {
		let ty = self.strip_mut(expected);
		match self.interner.kind(ty) {
			TyKind::Map(key, value) => Some((*key, *value)),
			_ => None,
		}
	}

	/// Fully resolve a type, replacing every bound variable throughout. Unbound
	/// variables are left as canonical `Infer` handles.
	pub(crate) fn resolve_deep(&mut self, ty: Ty) -> Ty {
		let ty = self.shallow_resolve(ty);
		match self.interner.kind(ty).clone() {
			TyKind::List(elem) => {
				let elem = self.resolve_deep(elem);
				self.interner.mk_list(elem)
			}
			TyKind::Tuple(elems) => {
				let elems = elems.iter().map(|&e| self.resolve_deep(e)).collect();
				self.interner.mk_tuple(elems)
			}
			TyKind::Map(key, value) => {
				let key = self.resolve_deep(key);
				let value = self.resolve_deep(value);
				self.interner.mk_map(key, value)
			}
			TyKind::Fn { params, ret } => {
				let params = params.iter().map(|&p| self.resolve_deep(p)).collect();
				let ret = self.resolve_deep(ret);
				self.interner.mk_fn(params, ret)
			}
			TyKind::Adt(def, args) => {
				let positional = args
					.positional
					.iter()
					.map(|&t| self.resolve_deep(t))
					.collect();
				let named = args
					.named
					.iter()
					.map(|(n, t)| (n.clone(), self.resolve_deep(*t)))
					.collect();
				self
					.interner
					.mk_adt(def, crate::ty::GenericArgs { positional, named })
			}
			TyKind::Intersection(parts) => {
				let parts = parts.iter().map(|&p| self.resolve_deep(p)).collect();
				self.interner.mk_intersection(parts)
			}
			TyKind::Mut(inner) => {
				let inner = self.resolve_deep(inner);
				self.interner.mk_mut(inner)
			}
			_ => ty,
		}
	}

	/// Whether a (deeply resolved) type still contains any inference variable.
	pub(crate) fn has_infer(&self, ty: Ty) -> bool {
		match self.interner.kind(ty) {
			TyKind::Infer(_) => true,
			TyKind::List(elem) => self.has_infer(*elem),
			TyKind::Tuple(elems) => elems.iter().any(|&e| self.has_infer(e)),
			TyKind::Map(key, value) => self.has_infer(*key) || self.has_infer(*value),
			TyKind::Fn { params, ret } => {
				params.iter().any(|&p| self.has_infer(p)) || self.has_infer(*ret)
			}
			TyKind::Adt(_, args) => {
				args.positional.iter().any(|&t| self.has_infer(t))
					|| args.named.iter().any(|(_, t)| self.has_infer(*t))
			}
			TyKind::Intersection(parts) => parts.iter().any(|&p| self.has_infer(p)),
			TyKind::Mut(inner) => self.has_infer(*inner),
			_ => false,
		}
	}

	// ── Substitution (instantiation) ─────────────────────────────────────────
	/// Substitute rigid parameters and `self` throughout a type. Used to instantiate
	/// a stored signature at a use site: each `ParamIdx` is mapped to a fresh
	/// inference variable (or concrete argument), and `SelfTy` to the receiver.
	pub(crate) fn subst(
		&mut self,
		ty: Ty,
		params: &FxHashMap<ParamIdx, Ty>,
		self_ty: Option<Ty>,
	) -> Ty {
		match self.interner.kind(ty).clone() {
			TyKind::Param(p) => params.get(&p).copied().unwrap_or(ty),
			TyKind::SelfTy => self_ty.unwrap_or(ty),
			TyKind::List(elem) => {
				let elem = self.subst(elem, params, self_ty);
				self.interner.mk_list(elem)
			}
			TyKind::Tuple(elems) => {
				let elems = elems
					.iter()
					.map(|&e| self.subst(e, params, self_ty))
					.collect();
				self.interner.mk_tuple(elems)
			}
			TyKind::Map(key, value) => {
				let key = self.subst(key, params, self_ty);
				let value = self.subst(value, params, self_ty);
				self.interner.mk_map(key, value)
			}
			TyKind::Fn { params: ps, ret } => {
				let ps = ps.iter().map(|&p| self.subst(p, params, self_ty)).collect();
				let ret = self.subst(ret, params, self_ty);
				self.interner.mk_fn(ps, ret)
			}
			TyKind::Adt(def, args) => {
				let positional = args
					.positional
					.iter()
					.map(|&t| self.subst(t, params, self_ty))
					.collect();
				let named = args
					.named
					.iter()
					.map(|(n, t)| (n.clone(), self.subst(*t, params, self_ty)))
					.collect();
				self
					.interner
					.mk_adt(def, crate::ty::GenericArgs { positional, named })
			}
			TyKind::Intersection(parts) => {
				let parts = parts
					.iter()
					.map(|&p| self.subst(p, params, self_ty))
					.collect();
				self.interner.mk_intersection(parts)
			}
			TyKind::Mut(inner) => {
				let inner = self.subst(inner, params, self_ty);
				self.interner.mk_mut(inner)
			}
			_ => ty,
		}
	}

	/// Build a substitution mapping a signature's generic parameters `0..n` to fresh
	/// inference variables, to be solved from the use site.
	pub(crate) fn fresh_subst(&mut self, count: usize) -> FxHashMap<ParamIdx, Ty> {
		(0..count)
			.map(|i| (ParamIdx(i as u32), self.fresh()))
			.collect()
	}

	/// The offset above which a `ParamIdx` is *synthetic*: minted by
	/// `mint_synthetic_param` (`lower.rs`) for an `impl Interface` type reference
	/// (Slice 4F sugar) rather than a declared generic parameter. Declared
	/// generics occupy `0..sig.generics.len()`, well below this offset, so the
	/// two never collide within one signature.
	pub(crate) const SYNTHETIC_PARAM_BASE: u32 = 1 << 28;

	/// Collect every synthetic `ParamIdx` (see [`Self::SYNTHETIC_PARAM_BASE`])
	/// occurring in `ty`, mirroring `subst`'s traversal. Callers that instantiate
	/// a stored signature at a use site (a call, a function-value reference) use
	/// this to extend their substitution with a fresh variable per synthetic
	/// param, exactly like a declared generic — otherwise a synthetic param
	/// leaks through `subst` rigid (it is Some `ParamIdx`, just not one `0..n`
	/// covers) and unifying a concrete argument against it fails outright.
	pub(crate) fn synthetic_params_in(&self, ty: Ty, out: &mut FxHashSet<ParamIdx>) {
		match self.interner.kind(ty).clone() {
			TyKind::Param(p) if p.0 >= Self::SYNTHETIC_PARAM_BASE => {
				out.insert(p);
			}
			TyKind::List(elem) => self.synthetic_params_in(elem, out),
			TyKind::Tuple(elems) => {
				for e in elems {
					self.synthetic_params_in(e, out);
				}
			}
			TyKind::Map(key, value) => {
				self.synthetic_params_in(key, out);
				self.synthetic_params_in(value, out);
			}
			TyKind::Fn { params, ret } => {
				for p in params {
					self.synthetic_params_in(p, out);
				}
				self.synthetic_params_in(ret, out);
			}
			TyKind::Adt(_, args) => {
				for &t in &args.positional {
					self.synthetic_params_in(t, out);
				}
				for (_, t) in &args.named {
					self.synthetic_params_in(*t, out);
				}
			}
			TyKind::Intersection(parts) => {
				for p in parts {
					self.synthetic_params_in(p, out);
				}
			}
			TyKind::Mut(inner) => self.synthetic_params_in(inner, out),
			_ => {}
		}
	}

	// ── Unification helpers shared with coerce.rs ────────────────────────────
	/// Bind an unbound variable to a type, guarding against infinite types.
	pub(crate) fn bind_var(&mut self, var: InferVar, ty: Ty, span: Span) {
		if occurs(&self.interner, var, ty) {
			let rendered = self.display(ty);
			self.emit(span, TypeError::InfiniteType { ty: rendered });
			let error = self.interner.error();
			self.table.assign(var, error);
			return;
		}
		self.table.assign(var, ty);
	}

	// ── Display ──────────────────────────────────────────────────────────────
	/// Render a (deeply resolved) type for a diagnostic message.
	pub(crate) fn display(&mut self, ty: Ty) -> String {
		let ty = self.resolve_deep(ty);
		self.display_resolved(ty)
	}

	fn display_resolved(&self, ty: Ty) -> String {
		match self.interner.kind(ty) {
			TyKind::Int => "int".into(),
			TyKind::UInt => "uint".into(),
			TyKind::Float => "float".into(),
			TyKind::Char => "char".into(),
			TyKind::String => "string".into(),
			TyKind::Boolean => "boolean".into(),
			TyKind::Void => "void".into(),
			TyKind::Never => "never".into(),
			TyKind::SelfTy => "self".into(),
			TyKind::Error => "<error>".into(),
			TyKind::Infer(_) => "_".into(),
			TyKind::Param(p) => self
				.params
				.iter()
				.rev()
				.find_map(|scope| {
					scope
						.iter()
						.find(|(_, idx)| **idx == *p)
						.map(|(n, _)| n.to_string())
				})
				.unwrap_or_else(|| format!("T{}", p.0)),
			TyKind::List(elem) => format!("#[{}]", self.display_resolved(*elem)),
			TyKind::Tuple(elems) => {
				let inner: Vec<_> = elems.iter().map(|&e| self.display_resolved(e)).collect();
				format!("#({})", inner.join(", "))
			}
			TyKind::Map(key, value) => format!(
				"#{{{}: {}}}",
				self.display_resolved(*key),
				self.display_resolved(*value)
			),
			TyKind::Fn { params, ret } => {
				let inner: Vec<_> = params.iter().map(|&p| self.display_resolved(p)).collect();
				format!("({}) -> {}", inner.join(", "), self.display_resolved(*ret))
			}
			TyKind::Adt(def, args) => {
				let name = self.defs.data(*def).name.clone();
				if args.is_empty() {
					name.to_string()
				} else {
					let mut inner: Vec<String> = args
						.positional
						.iter()
						.map(|&t| self.display_resolved(t))
						.collect();
					inner.extend(
						args
							.named
							.iter()
							.map(|(n, t)| format!("{n} = {}", self.display_resolved(*t))),
					);
					format!("{name}<{}>", inner.join(", "))
				}
			}
			TyKind::Intersection(parts) => {
				let inner: Vec<_> = parts.iter().map(|&p| self.display_resolved(p)).collect();
				inner.join(" + ")
			}
			TyKind::Mut(inner) => format!("mut {}", self.display_resolved(*inner)),
		}
	}
}
