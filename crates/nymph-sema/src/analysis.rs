//! Owned, diagnostic-free semantic results for a source module.

use std::{ops::Deref, sync::Arc};

use nymph_ast::decl::Module;

use crate::{Annotations, CheckedFacts};

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
}

#[derive(Clone, Debug)]
pub struct SemanticCheckResult {
	pub analysis: Arc<SemanticAnalysis>,
	pub diagnostics: Arc<[nymph_diagnostics::Diagnostic]>,
	pub lowerable: bool,
}
