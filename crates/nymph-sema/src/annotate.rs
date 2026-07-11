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

/// A [`NodeId`]-keyed map of [`ExprInfo`], produced by checking and consumed by
/// lowering.
#[derive(Clone, Debug, Default)]
pub struct Annotations(FxHashMap<NodeId, ExprInfo>);

impl Annotations {
	pub fn get(&self, id: NodeId) -> Option<ExprInfo> {
		self.0.get(&id).copied()
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Record the checker's decision about an expression node. Nodes built outside
	/// the parser carry [`NodeId::DUMMY`] and are never annotated.
	pub(crate) fn record(&mut self, id: NodeId, info: ExprInfo) {
		if id != NodeId::DUMMY {
			self.0.insert(id, info);
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
