//! The union-find table backing inference variables.
//!
//! This is deliberately thin: it only tracks the equivalence classes of
//! [`InferVar`]s and, once solved, the concrete [`Ty`] a class resolves to. All
//! *structural* unification (walking two types, recursing, running the occurs-check)
//! lives in `coerce.rs` on the `Checker`, because that needs the interner too. Here
//! we merely provide fresh variables and record bindings.

use ena::unify::{InPlace, InPlaceUnificationTable, NoError, Snapshot, UnifyKey, UnifyValue};

use crate::ids::InferVar;
use crate::ty::Ty;

/// A saved point in the unification table, for trial unification during overload
/// resolution (try a candidate, then keep or discard its bindings).
pub type UnifySnapshot = Snapshot<InPlace<InferVar>>;

/// The value stored for an inference variable's equivalence class: either still
/// unknown, or resolved to a concrete type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TyVarValue {
	Unknown,
	Known(Ty),
}

impl UnifyKey for InferVar {
	type Value = TyVarValue;

	fn index(&self) -> u32 {
		self.0
	}

	fn from_index(index: u32) -> Self {
		InferVar(index)
	}

	fn tag() -> &'static str {
		"InferVar"
	}
}

impl UnifyValue for TyVarValue {
	type Error = NoError;

	/// Merging two classes: a known type wins over an unknown. Two *known* types
	/// meeting here would mean the caller unioned without first resolving both
	/// sides; the structural unifier in `coerce.rs` always resolves first, so it
	/// only ever unions when at least one side is still unknown. If both are known
	/// we keep the first and rely on the structural unifier to have equated them.
	fn unify_values(a: &Self, b: &Self) -> Result<Self, NoError> {
		match (a, b) {
			(TyVarValue::Known(ty), _) => Ok(TyVarValue::Known(*ty)),
			(_, TyVarValue::Known(ty)) => Ok(TyVarValue::Known(*ty)),
			(TyVarValue::Unknown, TyVarValue::Unknown) => Ok(TyVarValue::Unknown),
		}
	}
}

/// Fresh-variable allocator and binding store for inference.
#[derive(Debug, Default)]
pub struct UnifyTable {
	table: InPlaceUnificationTable<InferVar>,
}

impl UnifyTable {
	pub fn new() -> Self {
		Self::default()
	}

	/// Allocate a fresh, unbound inference variable.
	pub fn new_var(&mut self) -> InferVar {
		self.table.new_key(TyVarValue::Unknown)
	}

	/// The current value of a variable's class (following it to its root).
	pub fn probe(&mut self, var: InferVar) -> TyVarValue {
		self.table.probe_value(var)
	}

	/// The canonical representative of a variable's class.
	pub fn root(&mut self, var: InferVar) -> InferVar {
		self.table.find(var)
	}

	/// Merge two still-unbound variables into one class.
	pub fn union_var(&mut self, a: InferVar, b: InferVar) {
		self.table.union(a, b);
	}

	/// Bind a variable's class to a concrete type.
	pub fn assign(&mut self, var: InferVar, ty: Ty) {
		self.table.union_value(var, TyVarValue::Known(ty));
	}

	/// Begin a trial: bindings made after this can be undone with [`Self::rollback_to`].
	pub fn snapshot(&mut self) -> UnifySnapshot {
		self.table.snapshot()
	}

	/// Discard every binding made since `snapshot`.
	pub fn rollback_to(&mut self, snapshot: UnifySnapshot) {
		self.table.rollback_to(snapshot);
	}
}
