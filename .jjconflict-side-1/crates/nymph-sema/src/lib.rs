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
//! occurs-check. [`check_module`] is the module-checking entry point.

mod analysis;
mod annotate;
mod anon_closure;
mod check;
mod coerce;
mod def;
mod effects;
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
mod members;
pub mod query;
mod range_analysis;
mod runtime;
mod solve;
mod stable_lowering;
mod unify;

pub use analysis::{
	DeclarationProvenance, ImportReferenceTarget, ModuleAnnotations, SemanticAnalysis,
	SemanticCheckResult,
};
pub use annotate::{
	Annotations, Checked, CheckedFacts, CheckedSemantic, DispatchKind, ExprInfo,
	GenericSymbolIdentity, IterMode, MemberCompletion, MemberCompletionKind, Resolution,
};
pub use check::{
	EntryMode, check_module, check_module_entry, check_module_with_environment,
	check_module_with_owned_environment,
};
pub use def::{
	AliasSig, DefData, DefKind, DefMap, DefOrigin, FieldSigMetadata, NamespaceMemberSig,
	NamespaceSig, OwnedMemberSig, Signatures, ValueSig,
};
pub use effects::*;
pub use entry::EntryRootShape;
pub use environment::*;
pub use errors::TypeError;
pub use identity::{
	BinderId, BinderScope, DeclarationCategory, DeclarationKey, DefinitionId, GenericParameterId,
	HeaderBinder, HeaderConstraint, HeaderParameterId, HeaderType, ImplementationHeader,
	ModuleIdentity, ModuleOrigin, PackageIdentity, RecoveredHeaderConstraint, RecoveredHeaderType,
	RecoveredImplementationHeader, StableIdBuilder,
};
pub use iface::{ImplRegistry, InterfaceDef};
pub use interface::*;
pub use interface_extract::*;
pub use members::InherentRegistry;
pub use nymph_hir::ids::{self, DefId, InferVar, ParamIdx};
pub use nymph_hir::ty::{self, GenericArgs, Interner, Ty, TyKind};
pub use runtime::{
	BodyNodeId, BuiltinDispatch, CheckedRuntimeBody, DispatchMaterialization, EnumShell,
	ExpressionVariant, PatternNodeId, PatternVariant, RangeDecision, RangeEvidence, RangeOperation,
	RangeProof, RuntimeAnnotations, RuntimeBodyKind, RuntimeDefinition, RuntimeExtractionError,
	RuntimeIteration, RuntimePayload, RuntimePlacement, RuntimePropagationKind, StableBody,
	StableCallArg, StableDispatch, StableExpr, StableExprKind, StableListItem,
	StableListPatternEntry, StableMapEntry, StableMapPatternEntry, StableMatchArm, StableParameter,
	StablePattern, StablePatternKind, StablePatternRange, StableRange, StableStatement,
	StableStringPart, StableStringPatternPart, StableStructPatternField, StableVariantField,
	StructShell, VariantExpressionMode, VariantPatternMode, runtime_definitions,
};
pub use stable_lowering::*;
