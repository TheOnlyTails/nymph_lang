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

/// The ena union-find key. A crate-local newtype isomorphic to [`InferVar`]:
/// [`InferVar`] itself lives in `nymph-hir` (the pure type model), so the orphan
/// rule forbids implementing `ena`'s [`UnifyKey`] for it here. We convert at the
/// table boundary (`InferVar(k.0)` ↔ `Key(v.0)`), which is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key(u32);

/// A saved point in the unification table, for trial unification during overload
/// resolution (try a candidate, then keep or discard its bindings).
pub type UnifySnapshot = Snapshot<InPlace<Key>>;

/// The value stored for an inference variable's equivalence class: either still
/// unknown, or resolved to a concrete type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TyVarValue {
	Unknown,
	Known(Ty),
}

impl UnifyKey for Key {
	type Value = TyVarValue;

	fn index(&self) -> u32 {
		self.0
	}

	fn from_index(index: u32) -> Self {
		Key(index)
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
	table: InPlaceUnificationTable<Key>,
}

impl UnifyTable {
	pub fn new() -> Self {
		Self::default()
	}

	/// Allocate a fresh, unbound inference variable.
	pub fn new_var(&mut self) -> InferVar {
		InferVar(self.table.new_key(TyVarValue::Unknown).0)
	}

	/// The current value of a variable's class (following it to its root).
	pub fn probe(&mut self, var: InferVar) -> TyVarValue {
		self.table.probe_value(Key(var.0))
	}

	/// The canonical representative of a variable's class.
	pub fn root(&mut self, var: InferVar) -> InferVar {
		InferVar(self.table.find(Key(var.0)).0)
	}

	/// Merge two still-unbound variables into one class.
	pub fn union_var(&mut self, a: InferVar, b: InferVar) {
		self.table.union(Key(a.0), Key(b.0));
	}

	/// Bind a variable's class to a concrete type.
	pub fn assign(&mut self, var: InferVar, ty: Ty) {
		self.table.union_value(Key(var.0), TyVarValue::Known(ty));
	}

	/// Begin a trial: bindings made after this can be undone with [`Self::rollback_to`].
	pub fn snapshot(&mut self) -> UnifySnapshot {
		self.table.snapshot()
	}

	/// Keep every binding made since `snapshot` and close the trial.
	pub fn commit(&mut self, snapshot: UnifySnapshot) {
		self.table.commit(snapshot);
	}

	/// Discard every binding made since `snapshot`.
	pub fn rollback_to(&mut self, snapshot: UnifySnapshot) {
		self.table.rollback_to(snapshot);
	}
}
