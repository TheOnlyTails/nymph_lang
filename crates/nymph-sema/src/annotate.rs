//! The side-table of per-expression decisions the checker records for the lowering
//! pass. Keyed by [`NodeId`] so the lowering can look up, for each AST expression,
//! its resolved type and (for desugared operators/casts/calls) which impl was
//! selected and how it must be dispatched in codegen.

use nymph_ast::NodeId;
use nymph_diagnostics::Diagnostic;
use rustc_hash::FxHashMap;

use crate::ty::Interner;
use crate::{DefId, Ty};

/// How a resolved operator/method call must be emitted by codegen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DispatchKind {
	/// A built-in primitive operation emitted as a native JS operator.
	BuiltinEager,
	/// A built-in default whose semantics short-circuit (`&&`, `||`, `??`),
	/// lowered to lazy control flow rather than an eager call.
	BuiltinShortCircuit,
	/// A user-provided interface impl: an ordinary eager method/function call.
	UserImpl,
}

/// The resolved callee behind a desugared operator, cast, index, or method call.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Resolution {
	pub method: DefId,
	pub dispatch: DispatchKind,
}

/// What the checker learned about one expression node.
#[derive(Clone, Copy, Debug)]
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

/// A [`NodeId`]-keyed map of [`ExprInfo`] plus variant resolutions, produced by
/// checking and consumed by lowering.
#[derive(Clone, Debug, Default)]
pub struct Annotations {
	infos: FxHashMap<NodeId, ExprInfo>,
	variants: FxHashMap<NodeId, VariantResolution>,
}

impl Annotations {
	pub fn get(&self, id: NodeId) -> Option<ExprInfo> {
		self.infos.get(&id).copied()
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

	/// Attach a `Resolution` to a node, preserving its already-recorded type. Used
	/// by later slices for operator/method dispatch without clobbering the type
	/// recorded by the uniform `infer` wrapper.
	///
	/// A resolved node is always also `infer`'d first (the wrapper records its type
	/// before any resolution is attached), so the entry is expected to already
	/// exist; this only updates it in place, and never inserts a bare
	/// resolution-only entry (there is no placeholder `Ty` to insert with). Slice 2A
	/// does not yet call this method — it is future-proofing for operator dispatch.
	#[allow(dead_code)]
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
