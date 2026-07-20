//! The side-table of per-expression decisions the checker records for the lowering
//! pass. Keyed by [`NodeId`] so the lowering can look up, for each AST expression,
//! its resolved type and (for desugared operators/casts/calls) which impl was
//! selected and how it must be dispatched in codegen.

use ecow::EcoString;
use nymph_ast::{NodeId, Span};
use nymph_diagnostics::Diagnostic;
use rustc_hash::FxHashMap;

use crate::Ty;
use crate::ty::Interner;

/// How a resolved binary operator must be emitted by codegen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DispatchKind {
	/// A built-in primitive operation emitted as a native JS operator, eagerly
	/// evaluated (`+`, `-`, `===`, …).
	BuiltinEager,
	/// A built-in default whose semantics short-circuit (`&&`, `||`), lowered to
	/// lazy control flow rather than an eager call.
	BuiltinShortCircuit,
	/// A method defined directly in a user impl: compile to `lhs.method(rhs)`.
	UserImpl,
	/// Resolved to an interface *default* method body (e.g. `Comparable`'s
	/// `less_than`, which calls `compare_to` under the hood). Codegen cannot
	/// materialize interface default methods yet, so lowering panics on this
	/// rather than emitting a call to a method that doesn't exist on the class.
	UserImplDefaultMethod,
}

/// How a binary operator at a specific node must be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
	/// Interface method name the operator resolved to (e.g. `plus`). Codegen
	/// only needs the JS method name — impl methods are not `DefId`'d
	/// (`ImplDef.methods` is a name-keyed map), so a name is all lowering needs
	/// to build a `Call { callee: Field { .. }, .. }`.
	pub method: EcoString,
	pub dispatch: DispatchKind,
	/// The defining span of whatever provided `method` — an impl's own `impl
	/// Interface … for …`/nested `impl Interface { .. }` header (the `Ident`
	/// naming the interface, `solve::ImplDef::span`), or an interface's own
	/// span when resolved through a still-generic bound. `None` for a
	/// `BuiltinEager`/`BuiltinShortCircuit` dispatch, which never goes through
	/// the impl index at all. Lowering (Slice: stdlib body materialization)
	/// reads this back to locate, inside the reconstructed offset prelude AST,
	/// exactly which prelude impl/interface-default body a
	/// `UserImplDefaultMethod` dispatch is unmaterialized *from* — the same
	/// span `crate::infer_expr::impl_is_unmaterialized` already compares
	/// against `SPAN_BASE` to classify `dispatch` in the first place, just
	/// carried forward instead of discarded.
	pub impl_span: Option<Span>,
}

/// What the checker learned about one expression node.
#[derive(Clone, Debug)]
pub struct ExprInfo {
	pub ty: Ty,
	pub resolution: Option<Resolution>,
}

/// The resolved `(enum, variant)` names behind a variant construction or
/// reference. Recorded so lowering can emit the Symbol-tag ABI without
/// re-resolving ambiguous bare variant names (`None`, `Some`).
#[derive(Clone, Debug)]
pub struct VariantResolution {
	pub enum_name: ecow::EcoString,
	pub variant: ecow::EcoString,
}

/// How a `for` loop's source was proven iterable, once the syntactic-range and
/// native-list fast paths are ruled out (see `infer_iterable_element`). Recorded
/// on the iterable expression's `NodeId` so lowering (`lower_for`), which has no
/// solver access of its own, can tell a source that IS the iterator (call
/// `.next()` directly) apart from one that must first be turned into one (call
/// `.iter()`). Both desugar to the same while/match protocol; only the first
/// statement of the desugared block differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterMode {
	/// The source itself implements `Iterator<Item>` — use it as-is.
	Direct,
	/// The source implements `Iterable<T>` — call `.iter()` to get the iterator.
	ViaIter,
}

/// A [`NodeId`]-keyed map of [`ExprInfo`] plus variant resolutions, produced by
/// checking and consumed by lowering.
#[derive(Clone, Debug, Default)]
pub struct Annotations {
	infos: FxHashMap<NodeId, ExprInfo>,
	variants: FxHashMap<NodeId, VariantResolution>,
	/// Variant *patterns*, keyed by span — patterns carry no `NodeId`, but each
	/// written pattern has a unique source span.
	pattern_variants: FxHashMap<Span, VariantResolution>,
	/// A `for` loop's iterable, keyed by its own `NodeId` — see [`IterMode`].
	iter_modes: FxHashMap<NodeId, IterMode>,
	/// NodeId of a committed anonymous-closure-parameter (`$N`) boundary → its
	/// arity (Slice: `$N` anonymous closure params). Populated by the
	/// checker's type-directed boundary search (`anon_closure.rs`) as each
	/// boundary is finally committed — this is the one channel from that
	/// search back to lowering, which re-walks the original AST and has no
	/// solver/type access of its own. Consumed by `lower_anon_closure` to know
	/// which nodes must be wrapped as a synthesized `HirExpr::Closure` rather
	/// than lowered as their own expression kind.
	anon_boundaries: FxHashMap<NodeId, u8>,
}

impl Annotations {
	pub fn get(&self, id: NodeId) -> Option<ExprInfo> {
		self.infos.get(&id).cloned()
	}

	pub fn len(&self) -> usize {
		self.infos.len()
	}

	pub fn is_empty(&self) -> bool {
		self.infos.is_empty()
	}

	/// Record the checker's decision about an expression node. Nodes built outside
	/// the parser carry [`NodeId::DUMMY`] and are never annotated.
	pub(crate) fn record(&mut self, id: NodeId, info: ExprInfo) {
		if id != NodeId::DUMMY {
			self.infos.insert(id, info);
		}
	}

	/// Record which `(enum, variant)` a variant construction/reference resolved to.
	pub(crate) fn record_variant(&mut self, id: NodeId, res: VariantResolution) {
		if id != NodeId::DUMMY {
			self.variants.insert(id, res);
		}
	}

	/// The variant a construction/reference node resolved to, if any.
	pub fn variant_of(&self, id: NodeId) -> Option<&VariantResolution> {
		self.variants.get(&id)
	}

	/// Record which `(enum, variant)` a variant *pattern* resolved to, keyed by the
	/// pattern's source span.
	pub(crate) fn record_pattern_variant(&mut self, span: Span, res: VariantResolution) {
		self.pattern_variants.insert(span, res);
	}

	/// The variant a pattern (by span) resolved to, if any.
	pub fn pattern_variant_of(&self, span: Span) -> Option<&VariantResolution> {
		self.pattern_variants.get(&span)
	}

	/// Record how a `for` loop's iterable (by its own `NodeId`) was proven
	/// iterable. Nodes built outside the parser carry [`NodeId::DUMMY`] and are
	/// never annotated (mirrors [`Annotations::record`]).
	pub(crate) fn record_iter_mode(&mut self, id: NodeId, mode: IterMode) {
		if id != NodeId::DUMMY {
			self.iter_modes.insert(id, mode);
		}
	}

	/// The [`IterMode`] recorded for a `for` loop's iterable, if any.
	pub fn iter_mode_of(&self, id: NodeId) -> Option<IterMode> {
		self.iter_modes.get(&id).copied()
	}

	/// Record `id` as a committed anonymous-closure-parameter boundary with
	/// the given arity. Called only from the checker's trial-search commit
	/// points (`anon_closure.rs`) — both the winning hypothesis of a
	/// successful trial round and the final, widest hypothesis on search
	/// exhaustion (so the subsequent real check/infer still forms a closure
	/// and surfaces its natural type error loudly, rather than silently
	/// falling through to `AnonymousParamUnsupported`).
	pub(crate) fn record_anon_boundary(&mut self, id: NodeId, arity: u8) {
		self.anon_boundaries.insert(id, arity);
	}

	/// Undo a trial [`Self::record_anon_boundary`] whose round's diagnostics
	/// were discarded (the hypothesis didn't check) — see
	/// `Checker::resolve_anon`.
	pub(crate) fn remove_anon_boundary(&mut self, id: NodeId) {
		self.anon_boundaries.remove(&id);
	}

	/// The arity of the anonymous-closure boundary committed at `id`, if any.
	/// `Checker::check`/`Checker::infer` consult this at the top of every
	/// dispatch (during both trial and real evaluation) to intercept a
	/// boundary node before its ordinary expression-kind handling runs;
	/// lowering (`lower_anon_closure`) consults the same map afterward to
	/// rebuild the synthesized closure.
	pub fn anon_boundary_arity(&self, id: NodeId) -> Option<u8> {
		self.anon_boundaries.get(&id).copied()
	}

	/// Attach a `Resolution` to a node, preserving its already-recorded type. Used
	/// by operator dispatch (Slice 4B) without clobbering the type recorded by the
	/// uniform `infer` wrapper.
	///
	/// A resolved node is always also `infer`'d first (the wrapper records its type
	/// before any resolution is attached), so the entry is expected to already
	/// exist; this only updates it in place, and never inserts a bare
	/// resolution-only entry (there is no placeholder `Ty` to insert with).
	pub(crate) fn record_resolution(&mut self, id: NodeId, resolution: Resolution) {
		if id == NodeId::DUMMY {
			return;
		}
		match self.infos.get_mut(&id) {
			Some(info) => info.resolution = Some(resolution),
			None => debug_assert!(
				false,
				"record_resolution({id:?}) called before the node was infer'd"
			),
		}
	}

	/// The operator `Resolution` recorded for a `BinaryOp` node, if any. Mirrors
	/// [`Annotations::variant_of`]'s access pattern: lowering reads this back to
	/// decide how to compile the operator (native JS vs. a dispatched method call).
	pub fn resolution_of(&self, id: NodeId) -> Option<&Resolution> {
		self
			.infos
			.get(&id)
			.and_then(|info| info.resolution.as_ref())
	}
}

/// The full result of checking: diagnostics plus the annotation side-table. When
/// `diags` contains errors, `annotations` may be incomplete and lowering is skipped.
#[derive(Debug)]
pub struct Checked {
	pub diags: Vec<Diagnostic>,
	pub annotations: Annotations,
	/// The interner that minted the types in `annotations`. A `Ty` is meaningless
	/// without it, so it travels with the result for the lowering pass to consult.
	pub interner: Interner,
}
