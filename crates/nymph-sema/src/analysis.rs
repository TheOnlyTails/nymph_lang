//! Owned, diagnostic-free semantic results for a source module.

use std::{ops::Deref, sync::Arc};

use nymph_ast::decl::Module;

use crate::Annotations;

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
/// Task 7 will add the environment-check result after that payload is separated
/// from [`crate::Checked::diags`]. Storing `Checked` here in the meantime would
/// violate the diagnostic-free boundary this type establishes.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticAnalysis {
	pub source: Arc<Module>,
	pub annotations: ModuleAnnotations,
}
