//! Owned, diagnostic-free semantic results for a source module.

use std::{ops::Deref, sync::Arc};

use nymph_ast::{Span, decl::Module};
use rustc_hash::FxHashMap;

use crate::{Annotations, CheckedFacts, DefinitionId, ModuleIdentity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportReferenceTarget {
	Definition(DefinitionId),
	Module(ModuleIdentity),
}

/// Authoritative source occurrence of a stable declaration in its owner module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeclarationProvenance {
	pub name_span: Span,
}

/// Owned semantic annotations for one module.
///
/// This transparent wrapper gives incremental queries a sema-owned payload
/// boundary while preserving the existing annotation API. It deliberately has
/// no diagnostic storage; diagnostics remain a separate compiler result.
#[repr(transparent)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleAnnotations(Annotations);

impl From<Annotations> for ModuleAnnotations {
	fn from(annotations: Annotations) -> Self {
		Self(annotations)
	}
}

impl Deref for ModuleAnnotations {
	type Target = Annotations;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

/// Owned semantic analysis of a source module, excluding diagnostics.
///
/// Checked facts and annotations are owned independently from the diagnostics in
/// [`SemanticCheckResult`].
#[derive(Clone, Debug)]
pub struct SemanticAnalysis {
	pub module: Arc<Module>,
	pub checked: Arc<CheckedFacts>,
	pub annotations: Arc<ModuleAnnotations>,
	/// Local declarations keyed by their stable semantic identity.
	pub declarations: Arc<FxHashMap<DefinitionId, DeclarationProvenance>>,
	/// User-written import names paired with compiler-resolved semantic targets.
	///
	/// These are installed by the project compiler after import resolution. A
	/// standalone module has no project import universe and therefore leaves
	/// this list empty.
	pub import_references: Arc<[(Span, ImportReferenceTarget)]>,
}

impl SemanticAnalysis {
	#[must_use]
	pub fn declaration(&self, id: &DefinitionId) -> Option<DeclarationProvenance> {
		self.declarations.get(id).copied()
	}
}

#[derive(Clone, Debug)]
pub struct SemanticCheckResult {
	pub analysis: Arc<SemanticAnalysis>,
	pub diagnostics: Arc<[nymph_diagnostics::Diagnostic]>,
	pub lowerable: bool,
}
