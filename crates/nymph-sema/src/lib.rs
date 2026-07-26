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

mod analysis;
mod annotate;
mod anon_closure;
mod check;
mod coerce;
mod def;
mod entry;
mod environment;
mod errors;
mod exhaustive;
mod identity;
mod iface;
mod infer_expr;
mod infer_pattern;
mod interface;
mod interface_extract;
mod lower;
mod lower_hir;
mod members;
mod prelude;
pub mod query;
mod solve;
mod unify;

pub use analysis::{
	ModuleAnnotations, SemanticAnalysis, SemanticCheckResult, StableAnnotationView,
	stable_annotation_view,
};
pub use annotate::{
	Annotations, Checked, CheckedFacts, CheckedSemantic, DispatchKind, ExprInfo, IterMode, Resolution,
};
pub use check::{
	EntryMode, check_module, check_module_entry, check_module_with_environment, check_program,
};
pub use def::{
	AliasSig, DefData, DefKind, DefMap, DefOrigin, FieldSigMetadata, NamespaceMemberSig,
	NamespaceSig, OwnedMemberSig, Signatures, ValueSig,
};
pub use environment::*;
pub use errors::TypeError;
pub use identity::{
	BinderId, BinderScope, DeclarationCategory, DeclarationKey, DefinitionId, GenericParameterId,
	HeaderBinder, HeaderConstraint, HeaderParameterId, HeaderType, ImplementationHeader,
	ModuleIdentity, ModuleOrigin, RecoveredHeaderConstraint, RecoveredHeaderType,
	RecoveredImplementationHeader, StableIdBuilder,
};
pub use iface::{ImplRegistry, InterfaceDef};
pub use interface::*;
pub use interface_extract::*;
pub use lower_hir::{
	LoweredHir, RuntimeOwner, lower_hir, lower_hir_with_prelude, lower_hir_with_prelude_and_deps,
	lower_hir_with_prelude_runtime_and_deps, lower_hir_with_prelude_runtime_and_deps_with_owners,
};
pub use members::InherentRegistry;
pub use nymph_hir::ids::{self, DefId, InferVar, ParamIdx};
pub use nymph_hir::ty::{self, GenericArgs, Interner, Ty, TyKind};
pub use prelude::{
	CheckedModule, check_module_entry_with_prelude, check_module_entry_with_prelude_and_module,
	check_module_with_prelude, check_module_with_prelude_and_module,
};
