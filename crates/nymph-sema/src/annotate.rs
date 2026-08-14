//! The side-table of per-expression decisions the checker records for the lowering
//! pass. Keyed by [`NodeId`] so the lowering can look up, for each AST expression,
//! its resolved type and (for desugared operators/casts/calls) which impl was
//! selected and how it must be dispatched in codegen.

use ecow::EcoString;
use nymph_ast::{NodeId, Span};
use nymph_diagnostics::Diagnostic;
use rustc_hash::{FxHashMap, FxHashSet};
use std::ops::Deref;

use crate::Ty;
use crate::identity::{DefinitionId, GenericParameterId, ModuleIdentity};
use crate::ty::Interner;
use nymph_hir::hir::MarshalKind;

/// Immutable, owned declaration-level checker output used for interface extraction.
/// Diagnostics and transient inference state are deliberately not included.
#[derive(Debug, Clone)]
pub struct CheckedSemantic {
	pub(crate) definitions: crate::def::DefMap,
	pub(crate) signatures: crate::def::Signatures,
	pub(crate) interfaces: FxHashMap<crate::DefId, crate::iface::InterfaceDef>,
	pub(crate) external_abis: FxHashMap<crate::DefId, crate::ExternalAbi>,
	pub(crate) implementations: crate::iface::ImplRegistry,
	pub(crate) inherent: Vec<CheckedInherentImpl>,
	pub(crate) anonymous_bounds: FxHashMap<crate::ParamIdx, Vec<crate::iface::Bound>>,
	pub local_definitions: std::ops::Range<usize>,
	pub local_implementations: std::ops::Range<usize>,
	pub local_inherent: std::ops::Range<usize>,
	pub(crate) has_explicit_local_ranges: bool,
	pub compiler_runtime_roles: crate::CompilerRuntimeRoles,
}

/// Owned, AST-independent facts for one checked inherent implementation.
#[derive(Debug, Clone)]
pub(crate) struct CheckedInherentImpl {
	pub definition: Option<DefinitionId>,
	pub owner: Option<DefinitionId>,
	pub source_span: Option<Span>,
	pub generics: Vec<EcoString>,
	pub self_ty: Ty,
	pub constraints: Vec<crate::iface::Bound>,
	pub methods: FxHashMap<EcoString, CheckedMethod>,
}

/// Owned method facts after return inference and constraint checking.
#[derive(Debug, Clone)]
pub(crate) struct CheckedMethod {
	pub definition: Option<DefinitionId>,
	pub generic_count: usize,
	pub params: Vec<Ty>,
	pub ret: Ty,
	pub bounds: Vec<crate::iface::Bound>,
	pub external: bool,
}

impl CheckedSemantic {
	#[must_use]
	pub fn definition_count(&self) -> usize {
		self.definitions.defs.len()
	}
	#[must_use]
	pub fn interface_count(&self) -> usize {
		self.interfaces.len()
	}
	#[must_use]
	pub fn implementation_count(&self) -> usize {
		self.implementations.impls.len()
	}
	#[must_use]
	pub fn inherent_implementation_count(&self) -> usize {
		self.inherent.len()
	}
	#[must_use]
	pub fn stable_definition(&self, id: crate::DefId) -> Option<&DefinitionId> {
		self.definitions.stable(id)
	}
}

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

/// Exact semantic target selected for a resolved method use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedMethodTarget {
	Inherent {
		member: DefinitionId,
		implementation: DefinitionId,
	},
	InterfaceImplementation {
		interface: DefinitionId,
		slot: crate::ImplementationMemberSlot,
		implementation_arguments: Vec<Ty>,
		method_arguments: Vec<Ty>,
	},
	GenericBound {
		interface: DefinitionId,
		interface_member: DefinitionId,
	},
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
	/// Stable semantic identity of the selected method declaration/member.
	pub target: Option<DefinitionId>,
	/// Stable semantic identity of the selected concrete impl, when known.
	pub implementation: Option<DefinitionId>,
	/// Exact checker-owned dispatch result. This is the authoritative channel
	/// for stable runtime projection; the optional identities above support
	/// definition annotations and dispatch classification.
	pub resolved_target: Option<ResolvedMethodTarget>,
}

fn map_resolution_types(resolution: &mut Resolution, map: &mut impl FnMut(Ty) -> Ty) {
	if let Some(ResolvedMethodTarget::InterfaceImplementation {
		implementation_arguments,
		method_arguments,
		..
	}) = &mut resolution.resolved_target
	{
		for argument in implementation_arguments
			.iter_mut()
			.chain(method_arguments.iter_mut())
		{
			*argument = map(*argument);
		}
	}
}

/// What the checker learned about one expression node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExprInfo {
	pub ty: Ty,
	pub resolution: Option<Resolution>,
}

/// One checker-approved member offered to editor tooling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberCompletion {
	pub name: EcoString,
	pub kind: MemberCompletionKind,
	/// Fully instantiated field type or callable signature.
	pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemberCompletionKind {
	Field,
	Method,
	Function,
	Value,
	Variable,
	Variant,
}

/// The resolved `(enum, variant)` names behind a variant construction or
/// reference. Recorded so lowering can emit the Symbol-tag ABI without
/// re-resolving ambiguous bare variant names (`None`, `Some`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantResolution {
	pub enum_name: ecow::EcoString,
	pub variant: ecow::EcoString,
	/// Stable identity of the enum declaration, independent of checker allocation.
	pub enum_target: Option<DefinitionId>,
	/// Stable identity of the selected variant declaration.
	pub variant_target: Option<DefinitionId>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnresolvedQualifiedAccess {
	pub module: ModuleIdentity,
	pub member: EcoString,
	pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericNamespacedCall {
	pub parameter: crate::ParamIdx,
	pub interface: DefinitionId,
	pub member: DefinitionId,
}

/// Exact identity of a user-written generic parameter. Parameters owned by a
/// stable declaration use that owner's binder and ordinal; transient owners
/// fall back to their declaration token span, which still preserves lexical
/// shadowing within one immutable module analysis.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GenericSymbolIdentity {
	Stable(GenericParameterId),
	Local(Span),
}

/// A [`NodeId`]-keyed map of [`ExprInfo`] plus variant resolutions, produced by
/// checking and consumed by lowering.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Annotations {
	infos: FxHashMap<NodeId, ExprInfo>,
	/// Expressions whose `uint` value is implicitly widened to `int` at this use site.
	/// Lowering reboxes these nodes as `NInt`, preserving the declared runtime type.
	implicit_uint_to_int: FxHashSet<NodeId>,
	member_completions: FxHashMap<NodeId, Vec<MemberCompletion>>,
	unresolved_qualified_accesses: Vec<UnresolvedQualifiedAccess>,
	direct_namespace_members: FxHashSet<NodeId>,
	module_targets: FxHashMap<NodeId, ModuleIdentity>,
	definition_targets: FxHashMap<NodeId, DefinitionId>,
	/// Checker-resolved local identifier uses, keyed by expression node, with
	/// the accepted binding's canonical declaration span as immutable identity.
	local_definition_targets: FxHashMap<NodeId, Span>,
	/// Every written local declaration span mapped to its canonical identity.
	/// Union-pattern alternatives intentionally share one identity.
	local_declarations: FxHashMap<Span, Span>,
	type_definition_targets: FxHashMap<Span, DefinitionId>,
	generic_symbols: FxHashMap<Span, (GenericSymbolIdentity, bool)>,
	conflicting_generic_symbols: FxHashSet<Span>,
	stable_generic_declarations: FxHashMap<Span, GenericParameterId>,
	conflicting_stable_generic_declarations: FxHashSet<Span>,
	source_definition_targets: FxHashMap<Span, DefinitionId>,
	variants: FxHashMap<NodeId, VariantResolution>,
	/// Variant *patterns*, keyed by span — patterns carry no `NodeId`, but each
	/// written pattern has a unique source span.
	pattern_variants: FxHashMap<Span, VariantResolution>,
	/// The field name a positional (unnamed) constructor sub-pattern binds, keyed by
	/// that sub-pattern's span. A positional field carries no name in the AST, but the
	/// checker resolves it to the constructor's sole field; lowering reads this back to
	/// emit the field access, having no type access of its own.
	positional_fields: FxHashMap<Span, PositionalFieldResolution>,
	/// A `for` loop's iterable, keyed by its own `NodeId` — see [`IterMode`].
	iter_modes: FxHashMap<NodeId, IterMode>,
	/// Resolution of the implicit `.iter()` call inserted for an iterable source.
	iter_resolutions: FxHashMap<NodeId, Resolution>,
	/// Resolution of the implicit `.next()` call used to drain the selected iterator.
	iteration_next_resolutions: FxHashMap<NodeId, Resolution>,
	/// NodeId of a committed anonymous-closure-parameter (`$N`) boundary → its
	/// arity (Slice: `$N` anonymous closure params). Populated by the
	/// checker's type-directed boundary search (`anon_closure.rs`) as each
	/// boundary is finally committed — this is the one channel from that
	/// search back to lowering, which re-walks the original AST and has no
	/// solver/type access of its own. Consumed by `lower_anon_closure` to know
	/// which nodes must be wrapped as a synthesized `HirExpr::Closure` rather
	/// than lowered as their own expression kind.
	anon_boundaries: FxHashMap<NodeId, u8>,
	/// Calls proven to be namespaced dispatch through a generic type parameter.
	/// The parameter identifier has no runtime binding, so stable lowering must
	/// reject these rather than emitting an undefined local.
	generic_namespaced_calls: FxHashMap<NodeId, GenericNamespacedCall>,
	/// Instantiated declared generic arguments for a callable reference. The
	/// solver variables are deliberately retained until runtime extraction,
	/// after call argument unification has fixed their concrete types.
	generic_call_arguments: FxHashMap<NodeId, Vec<Ty>>,
	/// Checker-resolved lexical control target for each jump expression. Both
	/// endpoints are source identities; the kind disambiguates constructs which
	/// share a source node (notably a callable and its directly labeled body).
	/// Lowering must never repeat name lookup.
	control_targets: FxHashMap<NodeId, ResolvedControlTarget>,
	propagations: FxHashMap<NodeId, PropagationKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PropagationKind {
	Option,
	Result,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedControlTarget {
	pub source: NodeId,
	pub kind: ResolvedControlTargetKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedControlTargetKind {
	Loop,
	Block,
	Callable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionalFieldResolution {
	pub name: EcoString,
	pub definition: Option<DefinitionId>,
}

impl Annotations {
	pub(crate) fn record_implicit_uint_to_int(&mut self, id: NodeId) {
		if id != NodeId::DUMMY {
			self.implicit_uint_to_int.insert(id);
		}
	}

	pub(crate) fn implicit_uint_to_int(&self) -> impl Iterator<Item = NodeId> + '_ {
		self.implicit_uint_to_int.iter().copied()
	}

	pub(crate) fn record_member_completions(
		&mut self,
		receiver: NodeId,
		mut candidates: Vec<MemberCompletion>,
	) {
		let precedence = |kind| match kind {
			MemberCompletionKind::Field | MemberCompletionKind::Variant => 0,
			_ => 1,
		};
		candidates.sort_by(|a, b| {
			a.name
				.cmp(&b.name)
				.then(precedence(a.kind).cmp(&precedence(b.kind)))
				.then(a.kind.cmp(&b.kind))
		});
		candidates.dedup_by(|a, b| a.name == b.name);
		self.member_completions.insert(receiver, candidates);
	}

	#[must_use]
	pub fn member_completions(&self, receiver: NodeId) -> &[MemberCompletion] {
		self
			.member_completions
			.get(&receiver)
			.map(Vec::as_slice)
			.unwrap_or_default()
	}
	pub(crate) fn record_control_target(&mut self, jump: NodeId, target: ResolvedControlTarget) {
		self.control_targets.insert(jump, target);
	}

	pub(crate) fn control_targets(
		&self,
	) -> impl Iterator<Item = (NodeId, ResolvedControlTarget)> + '_ {
		self
			.control_targets
			.iter()
			.map(|(&jump, &target)| (jump, target))
	}
	pub(crate) fn record_propagation(&mut self, node: NodeId, kind: PropagationKind) {
		self.propagations.insert(node, kind);
	}

	pub(crate) fn propagations(&self) -> impl Iterator<Item = (NodeId, PropagationKind)> + '_ {
		self.propagations.iter().map(|(&node, &kind)| (node, kind))
	}
	pub fn record_unresolved_qualified_access(
		&mut self,
		module: ModuleIdentity,
		member: EcoString,
		span: Span,
	) {
		self
			.unresolved_qualified_accesses
			.push(UnresolvedQualifiedAccess {
				module,
				member,
				span,
			});
	}

	pub fn unresolved_qualified_accesses(&self) -> &[UnresolvedQualifiedAccess] {
		&self.unresolved_qualified_accesses
	}

	pub(crate) fn record_direct_namespace_member(&mut self, id: NodeId) {
		self.direct_namespace_members.insert(id);
	}

	pub(crate) fn record_module_target(&mut self, id: NodeId, module: Option<&ModuleIdentity>) {
		if id != NodeId::DUMMY
			&& let Some(module) = module
		{
			self.module_targets.insert(id, module.clone());
		}
	}

	pub fn module_targets(&self) -> impl Iterator<Item = (NodeId, &ModuleIdentity)> {
		self.module_targets.iter().map(|(&id, module)| (id, module))
	}

	pub(crate) fn direct_namespace_members(&self) -> impl Iterator<Item = NodeId> + '_ {
		self.direct_namespace_members.iter().copied()
	}
}
impl Annotations {
	/// Iterate expression annotations in stable source-node order.
	///
	/// Returned type handles must be interpreted with the [`CheckedFacts::interner`]
	/// that owns this annotation set.
	pub fn infos(&self) -> impl Iterator<Item = (NodeId, &ExprInfo)> {
		let mut entries = self
			.infos
			.iter()
			.map(|(id, info)| (*id, info))
			.collect::<Vec<_>>();
		entries.sort_unstable_by_key(|(id, _)| *id);
		entries.into_iter()
	}

	/// Iterate stable declaration targets in source-node order.
	pub fn definition_targets(&self) -> impl Iterator<Item = (NodeId, &DefinitionId)> {
		let mut entries = self
			.definition_targets
			.iter()
			.map(|(id, target)| (*id, target))
			.collect::<Vec<_>>();
		entries.sort_unstable_by_key(|(id, _)| *id);
		entries.into_iter()
	}

	/// Iterate variant-expression resolutions in source-node order.
	pub fn variants(&self) -> impl Iterator<Item = (NodeId, &VariantResolution)> {
		let mut entries = self
			.variants
			.iter()
			.map(|(id, item)| (*id, item))
			.collect::<Vec<_>>();
		entries.sort_unstable_by_key(|(id, _)| *id);
		entries.into_iter()
	}

	/// Iterate variant-pattern resolutions in source-span order.
	pub fn pattern_variants(&self) -> impl Iterator<Item = (Span, &VariantResolution)> {
		let mut entries = self
			.pattern_variants
			.iter()
			.map(|(span, item)| (*span, item))
			.collect::<Vec<_>>();
		entries.sort_unstable_by_key(|(span, _)| (span.start, span.end));
		entries.into_iter()
	}

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

	pub(crate) fn map_types(&mut self, mut map: impl FnMut(Ty) -> Ty) {
		for info in self.infos.values_mut() {
			info.ty = map(info.ty);
			if let Some(resolution) = &mut info.resolution {
				map_resolution_types(resolution, &mut map);
			}
		}
		for resolution in self
			.iter_resolutions
			.values_mut()
			.chain(self.iteration_next_resolutions.values_mut())
		{
			map_resolution_types(resolution, &mut map);
		}
		for arguments in self.generic_call_arguments.values_mut() {
			for argument in arguments {
				*argument = map(*argument);
			}
		}
	}

	/// Record the stable declaration referenced by a source node.
	pub(crate) fn record_definition_target(&mut self, id: NodeId, target: Option<&DefinitionId>) {
		if id != NodeId::DUMMY
			&& let Some(target) = target
		{
			self.definition_targets.insert(id, target.clone());
		}
	}

	/// The stable declaration referenced by `id`, if this node denotes one.
	pub fn definition_target_of(&self, id: NodeId) -> Option<&DefinitionId> {
		self.definition_targets.get(&id)
	}

	pub(crate) fn record_local_definition_target(&mut self, id: NodeId, declaration: Span) {
		if id != NodeId::DUMMY {
			self.local_definition_targets.insert(id, declaration);
		}
	}

	pub fn local_definition_target_of(&self, id: NodeId) -> Option<Span> {
		self.local_definition_targets.get(&id).copied()
	}

	pub(crate) fn record_local_declaration(&mut self, span: Span, identity: Span) {
		self.local_declarations.insert(span, identity);
	}

	pub fn local_declarations(&self) -> impl Iterator<Item = (Span, Span)> + '_ {
		self
			.local_declarations
			.iter()
			.map(|(&span, &identity)| (span, identity))
	}

	pub(crate) fn record_type_definition_target(
		&mut self,
		span: Span,
		target: Option<&DefinitionId>,
	) {
		if let Some(target) = target {
			self.type_definition_targets.insert(span, target.clone());
		}
	}

	/// The stable declaration referenced by an exactly checked type identifier.
	pub fn type_definition_target_at(&self, span: Span) -> Option<&DefinitionId> {
		self.type_definition_targets.get(&span)
	}

	pub fn type_definition_targets(&self) -> impl Iterator<Item = (Span, &DefinitionId)> {
		self
			.type_definition_targets
			.iter()
			.map(|(&span, target)| (span, target))
	}

	pub(crate) fn record_generic_symbol(
		&mut self,
		span: Span,
		identity: GenericSymbolIdentity,
		is_declaration: bool,
	) {
		if self.conflicting_generic_symbols.contains(&span) {
			return;
		}
		match self.generic_symbols.entry(span) {
			std::collections::hash_map::Entry::Vacant(entry) => {
				entry.insert((identity, is_declaration));
			}
			std::collections::hash_map::Entry::Occupied(entry)
				if entry.get() == &(identity, is_declaration) => {}
			std::collections::hash_map::Entry::Occupied(entry) => {
				entry.remove();
				self.conflicting_generic_symbols.insert(span);
			}
		}
	}

	pub(crate) fn stabilize_generic_declaration(&mut self, span: Span, identity: GenericParameterId) {
		if self.conflicting_stable_generic_declarations.contains(&span) {
			return;
		}
		match self.stable_generic_declarations.entry(span) {
			std::collections::hash_map::Entry::Vacant(entry) => {
				entry.insert(identity);
			}
			std::collections::hash_map::Entry::Occupied(entry) if entry.get() == &identity => {}
			std::collections::hash_map::Entry::Occupied(entry) => {
				entry.remove();
				self.conflicting_stable_generic_declarations.insert(span);
			}
		}
	}

	pub(crate) fn suppress_generic_declaration(&mut self, span: Span) {
		self.generic_symbols.remove(&span);
		self.conflicting_generic_symbols.insert(span);
		self.stable_generic_declarations.remove(&span);
		self.conflicting_stable_generic_declarations.insert(span);
	}

	pub fn stable_generic_declaration(&self, span: Span) -> Option<&GenericParameterId> {
		self.stable_generic_declarations.get(&span)
	}

	pub fn generic_symbols(&self) -> impl Iterator<Item = (Span, &GenericSymbolIdentity, bool)> {
		self
			.generic_symbols
			.iter()
			.map(|(&span, (identity, declaration))| (span, identity, *declaration))
	}

	pub(crate) fn record_source_definition_target(
		&mut self,
		span: Span,
		target: Option<&DefinitionId>,
	) {
		if let Some(target) = target {
			self.source_definition_targets.insert(span, target.clone());
		}
	}

	pub fn source_definition_targets(&self) -> impl Iterator<Item = (Span, &DefinitionId)> {
		self
			.source_definition_targets
			.iter()
			.map(|(&span, target)| (span, target))
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

	pub(crate) fn record_positional_field(&mut self, span: Span, field: PositionalFieldResolution) {
		self.positional_fields.insert(span, field);
	}

	pub fn positional_field_of(&self, span: Span) -> Option<&PositionalFieldResolution> {
		self.positional_fields.get(&span)
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

	pub(crate) fn record_iter_resolution(&mut self, id: NodeId, resolution: Resolution) {
		if id != NodeId::DUMMY {
			self.iter_resolutions.insert(id, resolution);
		}
	}

	pub fn iter_resolution_of(&self, id: NodeId) -> Option<&Resolution> {
		self.iter_resolutions.get(&id)
	}

	pub(crate) fn record_iteration_next_resolution(&mut self, id: NodeId, resolution: Resolution) {
		if id != NodeId::DUMMY {
			self.iteration_next_resolutions.insert(id, resolution);
		}
	}

	pub fn iteration_next_resolution_of(&self, id: NodeId) -> Option<&Resolution> {
		self.iteration_next_resolutions.get(&id)
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

	pub(crate) fn record_generic_namespaced_call(&mut self, id: NodeId, call: GenericNamespacedCall) {
		if id != NodeId::DUMMY {
			self.generic_namespaced_calls.insert(id, call);
		}
	}

	pub fn generic_namespaced_call(&self, id: NodeId) -> Option<&GenericNamespacedCall> {
		self.generic_namespaced_calls.get(&id)
	}

	pub(crate) fn record_generic_call_arguments(&mut self, id: NodeId, arguments: Vec<Ty>) {
		if id != NodeId::DUMMY && !arguments.is_empty() {
			self.generic_call_arguments.insert(id, arguments);
		}
	}

	pub(crate) fn move_generic_call_arguments(&mut self, from: NodeId, to: NodeId) {
		if let Some(arguments) = self.generic_call_arguments.remove(&from) {
			self.record_generic_call_arguments(to, arguments);
		}
	}

	pub fn generic_call_arguments(&self) -> impl Iterator<Item = (NodeId, &[Ty])> {
		self
			.generic_call_arguments
			.iter()
			.map(|(id, arguments)| (*id, arguments.as_slice()))
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

/// Diagnostic-free facts produced by checking and consumed by extraction/lowering.
#[derive(Clone, Debug)]
pub struct CheckedFacts {
	pub annotations: Annotations,
	pub runtime_roles: crate::CompilerRuntimeRoles,
	/// Resolved host marshalling ABI for each checked external-let declaration,
	/// keyed by its binding span for consumption during HIR lowering.
	pub external_value_marshals: FxHashMap<Span, MarshalKind>,
	/// The interner that minted the types in `annotations`. A `Ty` is meaningless
	/// without it, so it travels with the result for the lowering pass to consult.
	pub interner: Interner,
	/// Owned declaration-level facts. This is an immutable extraction boundary;
	/// the stateful checker itself never escapes checking.
	pub semantic: CheckedSemantic,
	/// Exact source-local declaration paths to canonical runtime identities.
	pub source_identities: SourceIdentities,
}

impl CheckedFacts {
	/// Exact resolved ADT owner of each source inherent implementation.
	pub fn local_inherent_owners(&self) -> impl Iterator<Item = (&DefinitionId, Span)> {
		self.semantic.inherent[self.semantic.local_inherent.clone()]
			.iter()
			.filter_map(|implementation| {
				Some((implementation.owner.as_ref()?, implementation.source_span?))
			})
	}
}

#[derive(Clone, Debug, Default)]
pub struct SourceIdentities {
	pub implementations: std::collections::BTreeMap<ImplementationSourcePath, DefinitionId>,
	pub members: std::collections::BTreeMap<ImplementationMemberSourcePath, DefinitionId>,
	/// Exact source token which declares each stable identity assigned while
	/// checking this module. Unlike the path maps above, this also covers
	/// namespace, interface, inherent, variant, and field declarations.
	pub declarations: FxHashMap<DefinitionId, Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImplementationSourcePath {
	pub declaration: u32,
	pub nested: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImplementationMemberSourcePath {
	pub implementation: ImplementationSourcePath,
	pub member: u32,
}

/// Diagnostic-bearing result returned by direct semantic checks.
#[derive(Clone, Debug)]
pub struct Checked {
	pub diags: Vec<Diagnostic>,
	pub facts: CheckedFacts,
}

impl Deref for Checked {
	type Target = CheckedFacts;

	fn deref(&self) -> &Self::Target {
		&self.facts
	}
}
