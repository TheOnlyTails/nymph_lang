//! Semantic analysis for the Nymph language: name resolution, type checking, and
//! interface (trait) solving.
//!
//! The crate is organised as a pipeline of small, independently testable passes,
//! deliberately *not* the single monolithic checker of the previous implementation:
//!
//! - [`ty`] — the interned semantic type representation the checker reasons about.
//! - [`ids`] — opaque identity handles ([`DefId`](ids::DefId), rigid
//!   [`ParamIdx`](ids::ParamIdx) vs. flexible [`InferVar`](ids::InferVar)).
//! - [`def`] — the item-level resolution pass ([`DefMap`](def::DefMap)) and lowered
//!   signatures.
//!
//! The inference engine is bidirectional (`check`/`infer` modes, in `infer_expr`
//! and `infer_pattern`) backed by union-find unification (`unify`) with an
//! occurs-check. [`check_module`] is the Milestone-A entry point.
//!
//! Milestone B (interface solving, operator overloading, associated generics, and
//! match exhaustiveness) is layered on top later.

mod annotate;
mod check;
mod coerce;
mod def;
mod exhaustive;
mod iface;
mod infer_expr;
mod infer_pattern;
mod lower;
mod lower_hir;
mod members;
mod solve;
mod unify;

pub use annotate::{Annotations, Checked, DispatchKind, ExprInfo, Resolution};
pub use check::{check_module, check_program};
pub use lower_hir::lower_hir;
pub use nymph_hir::ids::{self, DefId, InferVar, ParamIdx};
pub use nymph_hir::ty::{self, GenericArgs, Interner, Ty, TyKind};
