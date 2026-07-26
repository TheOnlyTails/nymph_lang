//! Canonical, interner-independent semantic module interfaces.

use std::{
	collections::HashMap,
	hash::{Hash, Hasher},
};

use ecow::EcoString;
use nymph_ast::decl::Visibility;
use nymph_hir::{
	hir::MarshalKind,
	ids::{DefId, InferVar, ParamIdx},
	ty::{GenericArgs, Interner, Ty, TyKind},
};

use crate::{DefinitionId, GenericParameterId, ModuleIdentity};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum InterfaceType {
	Int,
	UInt,
	Float,
	Char,
	String,
	Boolean,
	Void,
	Never,
	SelfType,
	List(Box<Self>),
	Tuple(Vec<Self>),
	Map(Box<Self>, Box<Self>),
	Function {
		parameters: Vec<Self>,
		return_type: Box<Self>,
	},
	Named {
		definition: DefinitionId,
		positional: Vec<Self>,
		named: Vec<(EcoString, Self)>,
	},
	Intersection(Vec<Self>),
	Mutable(Box<Self>),
	Generic(GenericParameterId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterfaceConversionError {
	UnsolvedInference(InferVar),
	ErrorType,
	UnknownDefinition(DefId),
	UnknownBinder(ParamIdx),
	UnknownStableDefinition(DefinitionId),
	UnknownGenericParameter(GenericParameterId),
}

#[derive(Default)]
pub struct CanonicalizationContext {
	definitions: HashMap<DefId, DefinitionId>,
	parameters: HashMap<ParamIdx, GenericParameterId>,
}

impl CanonicalizationContext {
	pub fn new(
		definitions: HashMap<DefId, DefinitionId>,
		parameters: HashMap<ParamIdx, GenericParameterId>,
	) -> Self {
		Self {
			definitions,
			parameters,
		}
	}
}

#[derive(Default)]
pub struct InstantiationContext {
	definitions: HashMap<DefinitionId, DefId>,
	parameters: HashMap<GenericParameterId, ParamIdx>,
}

impl InstantiationContext {
	pub fn new(
		definitions: HashMap<DefinitionId, DefId>,
		parameters: HashMap<GenericParameterId, ParamIdx>,
	) -> Self {
		Self {
			definitions,
			parameters,
		}
	}
}

pub fn canonicalize_type(
	interner: &Interner,
	ty: Ty,
	context: &CanonicalizationContext,
) -> Result<InterfaceType, InterfaceConversionError> {
	fn go(
		interner: &Interner,
		ty: Ty,
		context: &CanonicalizationContext,
	) -> Result<InterfaceType, InterfaceConversionError> {
		Ok(match interner.kind(ty) {
			TyKind::Int => InterfaceType::Int,
			TyKind::UInt => InterfaceType::UInt,
			TyKind::Float => InterfaceType::Float,
			TyKind::Char => InterfaceType::Char,
			TyKind::String => InterfaceType::String,
			TyKind::Boolean => InterfaceType::Boolean,
			TyKind::Void => InterfaceType::Void,
			TyKind::Never => InterfaceType::Never,
			TyKind::SelfTy => InterfaceType::SelfType,
			TyKind::List(inner) => InterfaceType::List(Box::new(go(interner, *inner, context)?)),
			TyKind::Tuple(items) => InterfaceType::Tuple(
				items
					.iter()
					.map(|t| go(interner, *t, context))
					.collect::<Result<_, _>>()?,
			),
			TyKind::Map(k, v) => InterfaceType::Map(
				Box::new(go(interner, *k, context)?),
				Box::new(go(interner, *v, context)?),
			),
			TyKind::Fn { params, ret } => InterfaceType::Function {
				parameters: params
					.iter()
					.map(|t| go(interner, *t, context))
					.collect::<Result<_, _>>()?,
				return_type: Box::new(go(interner, *ret, context)?),
			},
			TyKind::Adt(def, args) => InterfaceType::Named {
				definition: context
					.definitions
					.get(def)
					.cloned()
					.ok_or(InterfaceConversionError::UnknownDefinition(*def))?,
				positional: args
					.positional
					.iter()
					.map(|t| go(interner, *t, context))
					.collect::<Result<_, _>>()?,
				named: args
					.named
					.iter()
					.map(|(n, t)| Ok((n.clone(), go(interner, *t, context)?)))
					.collect::<Result<_, _>>()?,
			},
			TyKind::Intersection(items) => {
				let mut items = items
					.iter()
					.map(|t| go(interner, *t, context))
					.collect::<Result<Vec<_>, _>>()?;
				items.sort();
				items.dedup();
				InterfaceType::Intersection(items)
			}
			TyKind::Mut(inner) => InterfaceType::Mutable(Box::new(go(interner, *inner, context)?)),
			TyKind::Param(idx) => InterfaceType::Generic(
				context
					.parameters
					.get(idx)
					.cloned()
					.ok_or(InterfaceConversionError::UnknownBinder(*idx))?,
			),
			TyKind::Infer(var) => return Err(InterfaceConversionError::UnsolvedInference(*var)),
			TyKind::Error => return Err(InterfaceConversionError::ErrorType),
		})
	}
	go(interner, ty, context)
}

pub fn try_instantiate_interface_type(
	interner: &mut Interner,
	ty: &InterfaceType,
	context: &InstantiationContext,
) -> Result<Ty, InterfaceConversionError> {
	Ok(match ty {
		InterfaceType::Int => interner.int(),
		InterfaceType::UInt => interner.uint(),
		InterfaceType::Float => interner.float(),
		InterfaceType::Char => interner.char(),
		InterfaceType::String => interner.string(),
		InterfaceType::Boolean => interner.boolean(),
		InterfaceType::Void => interner.void(),
		InterfaceType::Never => interner.never(),
		InterfaceType::SelfType => interner.self_ty(),
		InterfaceType::List(t) => {
			let t = try_instantiate_interface_type(interner, t, context)?;
			interner.mk_list(t)
		}
		InterfaceType::Tuple(ts) => {
			let ts = ts
				.iter()
				.map(|t| try_instantiate_interface_type(interner, t, context))
				.collect::<Result<_, _>>()?;
			interner.mk_tuple(ts)
		}
		InterfaceType::Map(k, v) => {
			let k = try_instantiate_interface_type(interner, k, context)?;
			let v = try_instantiate_interface_type(interner, v, context)?;
			interner.mk_map(k, v)
		}
		InterfaceType::Function {
			parameters,
			return_type,
		} => {
			let ps = parameters
				.iter()
				.map(|t| try_instantiate_interface_type(interner, t, context))
				.collect::<Result<_, _>>()?;
			let ret = try_instantiate_interface_type(interner, return_type, context)?;
			interner.mk_fn(ps, ret)
		}
		InterfaceType::Named {
			definition,
			positional,
			named,
		} => {
			let def = *context
				.definitions
				.get(definition)
				.ok_or_else(|| InterfaceConversionError::UnknownStableDefinition(definition.clone()))?;
			let positional = positional
				.iter()
				.map(|t| try_instantiate_interface_type(interner, t, context))
				.collect::<Result<_, _>>()?;
			let named = named
				.iter()
				.map(|(n, t)| {
					Ok((
						n.clone(),
						try_instantiate_interface_type(interner, t, context)?,
					))
				})
				.collect::<Result<_, _>>()?;
			interner.mk_adt(def, GenericArgs::new(positional, named))
		}
		InterfaceType::Intersection(ts) => {
			let ts = ts
				.iter()
				.map(|t| try_instantiate_interface_type(interner, t, context))
				.collect::<Result<_, _>>()?;
			interner.mk_intersection(ts)
		}
		InterfaceType::Mutable(t) => {
			let t = try_instantiate_interface_type(interner, t, context)?;
			interner.mk_mut(t)
		}
		InterfaceType::Generic(parameter) => {
			let idx = *context
				.parameters
				.get(parameter)
				.ok_or_else(|| InterfaceConversionError::UnknownGenericParameter(parameter.clone()))?;
			interner.mk_param(idx)
		}
	})
}

/// Instantiates a complete canonical interface type.
///
/// Missing mappings indicate a compiler-internal violation: complete interface
/// data must only be instantiated with its full definition and binder context.
pub fn instantiate_interface_type(
	interner: &mut Interner,
	ty: &InterfaceType,
	context: &InstantiationContext,
) -> Ty {
	try_instantiate_interface_type(interner, ty, context)
		.expect("complete interface type has an incomplete instantiation context")
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct GenericParameter {
	pub id: GenericParameterId,
	pub name: EcoString,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct ConstraintShape<T> {
	pub parameter: GenericParameterId,
	pub interface: DefinitionId,
	pub positional: Vec<T>,
	pub named: Vec<(EcoString, T)>,
}
pub type GenericConstraint = ConstraintShape<InterfaceType>;
pub type RecoveredGenericConstraint = ConstraintShape<RecoveredInterfaceType>;

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ParameterShape<T> {
	pub name: Option<EcoString>,
	pub ty: T,
	pub mutable: bool,
	pub spread: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct FieldShape<T> {
	pub id: DefinitionId,
	pub name: EcoString,
	pub visibility: Option<Visibility>,
	pub ty: T,
	pub mutable: bool,
	pub has_default: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct VariantShape<T> {
	pub id: DefinitionId,
	pub name: EcoString,
	pub fields: Vec<FieldShape<T>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct MemberShape<T> {
	pub id: DefinitionId,
	pub name: EcoString,
	pub visibility: Option<Visibility>,
	pub kind: MemberKind,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<ConstraintShape<T>>,
	pub parameters: Vec<ParameterShape<T>>,
	pub return_type: T,
	pub external: Option<ExternalAbi>,
	pub runtime_owner: Option<DefinitionId>,
	pub has_default: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum MemberKind {
	Value,
	MutableValue,
	Function,
	MutatingFunction,
	StaticValue,
	StaticFunction,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ExternalAbi {
	pub marker: EcoString,
	pub module: EcoString,
	pub symbol: EcoString,
	pub marshal: Option<MarshalKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum DefinitionShapeKind {
	Function,
	Let,
	TypeAlias,
	Struct,
	Enum,
	Interface,
	Namespace,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ExportedDefinition {
	pub id: DefinitionId,
	pub name: EcoString,
	pub visibility: Option<Visibility>,
	pub kind: DefinitionShapeKind,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<GenericConstraint>,
	pub parameters: Vec<ParameterShape<InterfaceType>>,
	pub return_type: Option<InterfaceType>,
	pub ty: Option<InterfaceType>,
	pub fields: Vec<FieldShape<InterfaceType>>,
	pub variants: Vec<VariantShape<InterfaceType>>,
	pub members: Vec<MemberShape<InterfaceType>>,
	pub super_interfaces: Vec<GenericConstraint>,
	pub external: Option<ExternalAbi>,
	pub runtime_owner: Option<DefinitionId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct SupportDefinition {
	pub definition: ExportedDefinition,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ExportedImpl {
	pub id: DefinitionId,
	pub visibility: Option<Visibility>,
	pub interface: Option<DefinitionId>,
	pub interface_arguments: Vec<(EcoString, InterfaceType)>,
	pub self_type: InterfaceType,
	pub mutable: bool,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<GenericConstraint>,
	pub members: Vec<MemberShape<InterfaceType>>,
	pub runtime_owner: Option<DefinitionId>,
}

#[derive(Clone, Debug, salsa::SalsaValue)]
pub struct ModuleInterface {
	pub module: ModuleIdentity,
	pub exports: Vec<ExportedDefinition>,
	pub support_definitions: Vec<SupportDefinition>,
	pub implementations: Vec<ExportedImpl>,
	pub fingerprint: u64,
}
impl PartialEq for ModuleInterface {
	fn eq(&self, other: &Self) -> bool {
		self.module == other.module
			&& self.exports == other.exports
			&& self.support_definitions == other.support_definitions
			&& self.implementations == other.implementations
	}
}
impl Eq for ModuleInterface {}
impl Hash for ModuleInterface {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.module.hash(state);
		self.exports.hash(state);
		self.support_definitions.hash(state);
		self.implementations.hash(state);
	}
}
impl ModuleInterface {
	pub fn structural_fingerprint(&self) -> u64 {
		let mut h = Fnv(0xcbf29ce484222325);
		self.hash(&mut h);
		h.finish()
	}
}
struct Fnv(u64);
impl Hasher for Fnv {
	fn finish(&self) -> u64 {
		self.0
	}
	fn write(&mut self, bytes: &[u8]) {
		for byte in bytes {
			self.0 ^= u64::from(*byte);
			self.0 = self.0.wrapping_mul(0x100000001b3);
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum RecoveredInterfaceType {
	Known(InterfaceType),
	Poison,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum SemanticAvailability {
	Available,
	StructureUnavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct RecoveredExportedDefinition {
	pub id: DefinitionId,
	pub name: EcoString,
	pub visibility: Option<Visibility>,
	pub kind: DefinitionShapeKind,
	pub availability: SemanticAvailability,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<RecoveredGenericConstraint>,
	pub parameters: Vec<ParameterShape<RecoveredInterfaceType>>,
	pub return_type: Option<RecoveredInterfaceType>,
	pub ty: Option<RecoveredInterfaceType>,
	pub fields: Vec<FieldShape<RecoveredInterfaceType>>,
	pub variants: Vec<VariantShape<RecoveredInterfaceType>>,
	pub members: Vec<MemberShape<RecoveredInterfaceType>>,
	pub super_interfaces: Vec<RecoveredGenericConstraint>,
	pub external: Option<ExternalAbi>,
	pub runtime_owner: Option<DefinitionId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct RecoveredSupportDefinition {
	pub definition: RecoveredExportedDefinition,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct RecoveredExportedImpl {
	pub id: DefinitionId,
	pub visibility: Option<Visibility>,
	pub availability: SemanticAvailability,
	pub interface: Option<DefinitionId>,
	pub interface_arguments: Vec<(EcoString, RecoveredInterfaceType)>,
	pub self_type: RecoveredInterfaceType,
	pub mutable: bool,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<RecoveredGenericConstraint>,
	pub members: Vec<MemberShape<RecoveredInterfaceType>>,
	pub runtime_owner: Option<DefinitionId>,
}
#[derive(Clone, Debug, salsa::SalsaValue)]
pub struct RecoveredModuleInterface {
	pub module: ModuleIdentity,
	pub exports: Vec<RecoveredExportedDefinition>,
	pub support_definitions: Vec<RecoveredSupportDefinition>,
	pub implementations: Vec<RecoveredExportedImpl>,
	pub fingerprint: u64,
}
impl PartialEq for RecoveredModuleInterface {
	fn eq(&self, other: &Self) -> bool {
		self.module == other.module
			&& self.exports == other.exports
			&& self.support_definitions == other.support_definitions
			&& self.implementations == other.implementations
	}
}
impl Eq for RecoveredModuleInterface {}
impl Hash for RecoveredModuleInterface {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.module.hash(state);
		self.exports.hash(state);
		self.support_definitions.hash(state);
		self.implementations.hash(state);
	}
}
impl RecoveredModuleInterface {
	pub fn structural_fingerprint(&self) -> u64 {
		let mut h = Fnv(0xcbf29ce484222325);
		self.hash(&mut h);
		h.finish()
	}
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ModuleEnvironment {
	Complete(ModuleInterface),
	Recovered(RecoveredModuleInterface),
}
