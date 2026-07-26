//! Stable, semantic-only contracts for per-definition HIR lowering.
//!
//! This module intentionally contains no lowering implementation. Its inputs are
//! exact stable identities and owned semantic artifacts, so a future lowerer does
//! not need compiler queries, parser identities, source locations, or module ASTs.

use std::{collections::HashSet, sync::Arc};

use ecow::EcoString;
use nymph_hir::hir::{HirClass, HirEnum, HirFunc, HirLet, HirMethod};

use crate::{
	DefinitionId, EnumShell, ExportedDefinition, ExportedImpl, ExternalAbi, InterfaceType,
	MemberShape, ModuleIdentity, RuntimeDefinition, StructShell,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeDefinitionLookupError {
	Missing {
		definition: DefinitionId,
	},
	Recovered {
		definition: DefinitionId,
	},
	Unavailable {
		definition: DefinitionId,
		reason: EcoString,
	},
}

/// Fetches exactly one checked runtime artifact by stable identity.
pub trait RuntimeDefinitionLookup {
	fn runtime_definition(
		&self,
		definition: &DefinitionId,
	) -> Result<Arc<RuntimeDefinition>, RuntimeDefinitionLookupError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StableShapeRequest {
	TypeShell(DefinitionId),
	Member(DefinitionId),
	Implementation(DefinitionId),
	InterfaceShell(DefinitionId),
	ExternalAbi(DefinitionId),
}

impl StableShapeRequest {
	pub fn definition(&self) -> &DefinitionId {
		match self {
			Self::TypeShell(id)
			| Self::Member(id)
			| Self::Implementation(id)
			| Self::InterfaceShell(id)
			| Self::ExternalAbi(id) => id,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StableTypeShell {
	Struct(StructShell),
	Enum(EnumShell),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StableShapeFact {
	TypeShell(StableTypeShell),
	Member(MemberShape<InterfaceType>),
	Implementation(ExportedImpl),
	InterfaceShell(ExportedDefinition),
	ExternalAbi(ExternalAbi),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StableShapeLookupError {
	Missing { request: StableShapeRequest },
	Recovered { definition: DefinitionId },
	WrongFact { request: StableShapeRequest },
}

/// One coherent gateway to all location-free shape and ABI facts used by lowering.
pub trait StableShapeLookup {
	fn stable_shape(
		&self,
		request: &StableShapeRequest,
	) -> Result<StableShapeFact, StableShapeLookupError>;
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EmittedBindingName(EcoString);

impl EmittedBindingName {
	pub fn new(name: impl Into<EcoString>) -> Self {
		Self(name.into())
	}
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EmittedMemberName(EcoString);

impl EmittedMemberName {
	pub fn new(name: impl Into<EcoString>) -> Self {
		Self(name.into())
	}
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalModuleSpecifier {
	Project(EcoString),
	Importable(EcoString),
	CompilerRuntime(EcoString),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StableNameLookupError {
	MissingBinding { definition: DefinitionId },
	MissingMember { definition: DefinitionId },
	MissingModule { module: ModuleIdentity },
}

/// Canonical emitted names. Separate return types prevent using a property name
/// where a module-scope binding is required (or vice versa).
pub trait StableNameLookup {
	fn binding_name(
		&self,
		definition: &DefinitionId,
	) -> Result<EmittedBindingName, StableNameLookupError>;
	fn member_name(
		&self,
		definition: &DefinitionId,
	) -> Result<EmittedMemberName, StableNameLookupError>;
	fn module_specifier(
		&self,
		module: &ModuleIdentity,
	) -> Result<CanonicalModuleSpecifier, StableNameLookupError>;
}

/// Complete semantic context required by stable lowering, independent of Salsa
/// and of `nymph-compiler`.
pub trait StableLoweringContext:
	RuntimeDefinitionLookup + StableShapeLookup + StableNameLookup
{
}
impl<T> StableLoweringContext for T where
	T: RuntimeDefinitionLookup + StableShapeLookup + StableNameLookup
{
}

/// A typed HIR contribution with its only legal placement encoded by variant.
#[derive(Clone, Debug, PartialEq)]
pub enum LoweredHirFragment {
	TopLevelFunction(HirFunc),
	TopLevelValue(HirLet),
	TopLevelExternal(ExternalAbi),
	StructShell(HirClass),
	EnumShell(HirEnum),
	AttachedInstance {
		owner: DefinitionId,
		method: HirMethod,
	},
	AttachedStatic {
		owner: DefinitionId,
		method: HirMethod,
	},
	AttachedMember {
		owner: DefinitionId,
		method: HirMethod,
	},
	MaterializedDefault {
		owner: DefinitionId,
		implementation: DefinitionId,
		interface_member: DefinitionId,
		method: HirMethod,
	},
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StableDemandSet {
	ordered: Vec<DefinitionId>,
	seen: HashSet<DefinitionId>,
}

impl StableDemandSet {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn insert(&mut self, definition: DefinitionId) -> bool {
		if self.seen.insert(definition.clone()) {
			self.ordered.push(definition);
			true
		} else {
			false
		}
	}
	pub fn as_slice(&self) -> &[DefinitionId] {
		&self.ordered
	}
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoweredRuntimeDefinition {
	definition: DefinitionId,
	fragment: LoweredHirFragment,
	demands: StableDemandSet,
}

impl LoweredRuntimeDefinition {
	pub fn new(
		definition: DefinitionId,
		fragment: LoweredHirFragment,
		demands: StableDemandSet,
	) -> Self {
		Self {
			definition,
			fragment,
			demands,
		}
	}
	pub fn definition(&self) -> &DefinitionId {
		&self.definition
	}
	pub fn fragment(&self) -> &LoweredHirFragment {
		&self.fragment
	}
	pub fn demands(&self) -> &[DefinitionId] {
		self.demands.as_slice()
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StableLoweringError {
	Runtime(RuntimeDefinitionLookupError),
	Shape(StableShapeLookupError),
	Name(StableNameLookupError),
	InvalidArtifact {
		definition: DefinitionId,
		reason: EcoString,
	},
}

impl From<RuntimeDefinitionLookupError> for StableLoweringError {
	fn from(error: RuntimeDefinitionLookupError) -> Self {
		Self::Runtime(error)
	}
}
impl From<StableShapeLookupError> for StableLoweringError {
	fn from(error: StableShapeLookupError) -> Self {
		Self::Shape(error)
	}
}
impl From<StableNameLookupError> for StableLoweringError {
	fn from(error: StableNameLookupError) -> Self {
		Self::Name(error)
	}
}
