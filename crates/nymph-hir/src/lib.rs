//! The interned semantic type model and stable identity handles shared between the
//! type checker (`nymph-sema`, which produces types) and later passes (lowering and
//! code generation, which consume them). Kept in its own crate so neither side
//! depends on the other's logic.

pub mod ids;
pub mod ty;

pub mod hir;

pub use ids::{DefId, InferVar, ParamIdx};
pub use ty::{GenericArgs, Interner, Ty, TyKind};
