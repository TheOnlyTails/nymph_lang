//! Canonical, interner-independent semantic module interfaces.

use std::{
	collections::HashMap,
	hash::{Hash, Hasher},
	sync::Arc,
};

use ecow::EcoString;
use nymph_ast::{Span, decl::Visibility};
use nymph_hir::{
	hir::MarshalKind,
	ids::{DefId, InferVar, ParamIdx},
	ty::{GenericArgs, Interner, Ty, TyKind},
};

use crate::{DefinitionId, EffectRow, GenericParameterId, ModuleIdentity, RecoveredEffectRow};

/// Body-independent lexical declarations owned by one semantic module.
///
/// This is deliberately separate from [`ModuleInterface`]: private declarations
/// participate in import/privacy decisions but never become semantic exports.
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct NamespaceSummary {
	pub module: ModuleIdentity,
	pub declarations: Vec<NamespaceDeclaration>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct NamespaceDeclaration {
	pub name: EcoString,
	pub definition: DefinitionId,
	pub visibility: NamespaceVisibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum NamespaceVisibility {
	Importable,
	Internal,
	Private,
}

impl NamespaceVisibility {
	pub fn allows(self, owner: &ModuleIdentity, current: &ModuleIdentity) -> bool {
		match self {
			Self::Importable => true,
			Self::Internal => owner.same_package_as(current),
			Self::Private => owner == current,
		}
	}
}

/// Body-independent source facts for one user-visible top-level declaration.
///
/// These are shared by project tooling that needs semantic declaration identity,
/// category, and visibility while remaining available over malformed bodies.
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct TopLevelDeclaration {
	pub name: EcoString,
	pub definition: DefinitionId,
	pub visibility: NamespaceVisibility,
	pub category: crate::DeclarationCategory,
	pub name_span: Span,
}

impl NamespaceSummary {
	#[must_use]
	pub fn declaration(&self, name: &str) -> Option<&NamespaceDeclaration> {
		self
			.declarations
			.iter()
			.rev()
			.find(|declaration| declaration.name == name)
	}
}

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
		effects: EffectRow,
	},
	Task {
		output: Box<Self>,
		effects: EffectRow,
	},
	Handle(Box<Self>),
	HandleOutcome(Box<Self>),
	Named {
		definition: DefinitionId,
		positional: Vec<Self>,
		named: Vec<(EcoString, Self)>,
	},
	Intersection(Vec<Self>),
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
	UnknownInterfaceArgument {
		interface: DefinitionId,
		name: EcoString,
	},
}

#[derive(Default)]
pub struct CanonicalizationContext {
	definitions: HashMap<DefId, DefinitionId>,
	parameters: HashMap<ParamIdx, GenericParameterId>,
	self_parameter: Option<ParamIdx>,
}

impl CanonicalizationContext {
	pub fn new(
		definitions: HashMap<DefId, DefinitionId>,
		parameters: HashMap<ParamIdx, GenericParameterId>,
	) -> Self {
		Self {
			definitions,
			parameters,
			self_parameter: None,
		}
	}
	pub(crate) fn with_self_parameter(mut self, parameter: ParamIdx) -> Self {
		self.self_parameter = Some(parameter);
		self
	}

	pub(crate) fn definitions(&self) -> HashMap<DefId, DefinitionId> {
		self.definitions.clone()
	}
}

#[derive(Default)]
pub struct InstantiationContext {
	definitions: Arc<HashMap<DefinitionId, DefId>>,
	parameters: HashMap<GenericParameterId, ParamIdx>,
}

impl InstantiationContext {
	pub fn new(
		definitions: HashMap<DefinitionId, DefId>,
		parameters: HashMap<GenericParameterId, ParamIdx>,
	) -> Self {
		Self {
			definitions: Arc::new(definitions),
			parameters,
		}
	}

	pub(crate) fn with_shared_definitions(
		definitions: Arc<HashMap<DefinitionId, DefId>>,
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
		match interner.kind(ty) {
			TyKind::Task { output, effects } => {
				return canonicalize_task(interner, *output, effects, context);
			}
			TyKind::Handle(output) => return canonicalize_handle(interner, *output, false, context),
			TyKind::HandleOutcome(output) => {
				return canonicalize_handle(interner, *output, true, context);
			}
			_ => {}
		}
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
			TyKind::Fn {
				params,
				ret,
				effects,
			} => InterfaceType::Function {
				parameters: params
					.iter()
					.map(|t| go(interner, *t, context))
					.collect::<Result<_, _>>()?,
				return_type: Box::new(go(interner, *ret, context)?),
				effects: EffectRow::new(
					effects
						.atoms()
						.iter()
						.map(|atom| match atom {
							nymph_hir::ty::EffectAtom::Nominal(definition) => context
								.definitions
								.get(definition)
								.cloned()
								.map(crate::EffectAtom::Nominal)
								.ok_or(InterfaceConversionError::UnknownDefinition(*definition)),
							nymph_hir::ty::EffectAtom::Parameter(parameter) => context
								.parameters
								.get(parameter)
								.cloned()
								.map(crate::EffectAtom::Parameter)
								.ok_or(InterfaceConversionError::UnknownBinder(*parameter)),
						})
						.collect::<Result<_, _>>()?,
				),
			},
			TyKind::Task { .. } | TyKind::Handle(_) | TyKind::HandleOutcome(_) => unreachable!(),
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
			TyKind::Param(idx) if context.self_parameter == Some(*idx) => InterfaceType::SelfType,
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

fn canonicalize_task(
	interner: &Interner,
	output: Ty,
	effects: &nymph_hir::ty::EffectRow,
	context: &CanonicalizationContext,
) -> Result<InterfaceType, InterfaceConversionError> {
	Ok(InterfaceType::Task {
		output: Box::new(canonicalize_type(interner, output, context)?),
		effects: canonicalize_effect_row(effects, context)?,
	})
}

fn canonicalize_handle(
	interner: &Interner,
	output: Ty,
	outcome: bool,
	context: &CanonicalizationContext,
) -> Result<InterfaceType, InterfaceConversionError> {
	let output = Box::new(canonicalize_type(interner, output, context)?);
	Ok(if outcome {
		InterfaceType::HandleOutcome(output)
	} else {
		InterfaceType::Handle(output)
	})
}

pub fn canonicalize_effect_row(
	effects: &nymph_hir::ty::EffectRow,
	context: &CanonicalizationContext,
) -> Result<EffectRow, InterfaceConversionError> {
	Ok(EffectRow::new(
		effects
			.atoms()
			.iter()
			.map(|atom| match atom {
				nymph_hir::ty::EffectAtom::Nominal(definition) => context
					.definitions
					.get(definition)
					.cloned()
					.map(crate::EffectAtom::Nominal)
					.ok_or(InterfaceConversionError::UnknownDefinition(*definition)),
				nymph_hir::ty::EffectAtom::Parameter(parameter) => context
					.parameters
					.get(parameter)
					.cloned()
					.map(crate::EffectAtom::Parameter)
					.ok_or(InterfaceConversionError::UnknownBinder(*parameter)),
			})
			.collect::<Result<_, _>>()?,
	))
}

pub fn try_instantiate_interface_type(
	interner: &mut Interner,
	ty: &InterfaceType,
	context: &InstantiationContext,
) -> Result<Ty, InterfaceConversionError> {
	match ty {
		InterfaceType::Task { output, effects } => {
			return instantiate_task(interner, output, effects, context);
		}
		InterfaceType::Handle(output) => return instantiate_handle(interner, output, false, context),
		InterfaceType::HandleOutcome(output) => {
			return instantiate_handle(interner, output, true, context);
		}
		_ => {}
	}
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
			effects,
		} => {
			let ps = parameters
				.iter()
				.map(|t| try_instantiate_interface_type(interner, t, context))
				.collect::<Result<_, _>>()?;
			let ret = try_instantiate_interface_type(interner, return_type, context)?;
			let effects = try_instantiate_effect_row(effects, context)?;
			interner.mk_effectful_fn(ps, ret, effects)
		}
		InterfaceType::Task { .. } | InterfaceType::Handle(_) | InterfaceType::HandleOutcome(_) => {
			unreachable!()
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
		InterfaceType::Generic(parameter) => {
			let idx = *context
				.parameters
				.get(parameter)
				.ok_or_else(|| InterfaceConversionError::UnknownGenericParameter(parameter.clone()))?;
			interner.mk_param(idx)
		}
	})
}

fn instantiate_task(
	interner: &mut Interner,
	output: &InterfaceType,
	effects: &EffectRow,
	context: &InstantiationContext,
) -> Result<Ty, InterfaceConversionError> {
	let output = try_instantiate_interface_type(interner, output, context)?;
	let effects = try_instantiate_effect_row(effects, context)?;
	Ok(interner.mk_task(output, effects))
}

fn instantiate_handle(
	interner: &mut Interner,
	output: &InterfaceType,
	outcome: bool,
	context: &InstantiationContext,
) -> Result<Ty, InterfaceConversionError> {
	let output = try_instantiate_interface_type(interner, output, context)?;
	Ok(if outcome {
		interner.mk_handle_outcome(output)
	} else {
		interner.mk_handle(output)
	})
}

pub fn try_instantiate_effect_row(
	effects: &EffectRow,
	context: &InstantiationContext,
) -> Result<nymph_hir::ty::EffectRow, InterfaceConversionError> {
	Ok(nymph_hir::ty::EffectRow::new(
		effects
			.atoms()
			.iter()
			.map(|atom| match atom {
				crate::EffectAtom::Nominal(definition) => context
					.definitions
					.get(definition)
					.copied()
					.map(nymph_hir::ty::EffectAtom::Nominal)
					.ok_or_else(|| InterfaceConversionError::UnknownStableDefinition(definition.clone())),
				crate::EffectAtom::Parameter(parameter) => context
					.parameters
					.get(parameter)
					.copied()
					.map(nymph_hir::ty::EffectAtom::Parameter)
					.ok_or_else(|| InterfaceConversionError::UnknownGenericParameter(parameter.clone())),
			})
			.collect::<Result<_, _>>()?,
	))
}

#[must_use]
pub fn instantiate_effect_row(
	effects: &EffectRow,
	context: &InstantiationContext,
) -> nymph_hir::ty::EffectRow {
	try_instantiate_effect_row(effects, context)
		.expect("complete effect rows require complete definition and binder mappings")
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
	pub kind: GenericParameterKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum GenericParameterKind {
	Type,
	Effect,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct ConstraintShape<T, R = DefinitionId> {
	pub parameter: GenericParameterId,
	pub interface: R,
	pub positional: Vec<T>,
	pub named: Vec<(EcoString, T)>,
	pub effect_args: Vec<(EcoString, EffectRow)>,
}
pub type GenericConstraint = ConstraintShape<InterfaceType>;
pub type RecoveredGenericConstraint =
	ConstraintShape<RecoveredInterfaceType, RecoveredDefinitionReference>;

/// An instantiated superinterface constraining an interface's implicit `Self`.
/// Unlike a generic constraint, it has no declared generic parameter identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct SuperInterfaceShape<T, R = DefinitionId> {
	pub interface: R,
	pub positional: Vec<T>,
	pub named: Vec<(EcoString, T)>,
}
pub type SuperInterface = SuperInterfaceShape<InterfaceType>;
pub type RecoveredSuperInterface =
	SuperInterfaceShape<RecoveredInterfaceType, RecoveredDefinitionReference>;

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ParameterShape<T> {
	pub name: Option<EcoString>,
	pub ty: T,
	pub spread: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct FieldShape<T> {
	pub id: DefinitionId,
	pub name: EcoString,
	pub visibility: Option<Visibility>,
	pub ty: T,
	pub has_default: bool,
	/// Checked owner-default effects. `None` means no default or a recovered
	/// default whose effect row was unavailable.
	pub default_effects: Option<EffectRow>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct VariantShape<T> {
	pub id: DefinitionId,
	pub name: EcoString,
	pub fields: Vec<FieldShape<T>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct MemberShape<T, R = DefinitionId, E = EffectRow> {
	pub id: DefinitionId,
	pub name: EcoString,
	pub visibility: Option<Visibility>,
	pub kind: MemberKind,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<ConstraintShape<T, R>>,
	pub parameters: Vec<ParameterShape<T>>,
	pub return_type: T,
	pub effects: E,
	pub external: Option<ExternalAbi<E>>,
	pub runtime_owner: Option<DefinitionId>,
	pub has_default: bool,
}
pub type RecoveredMemberShape =
	MemberShape<RecoveredInterfaceType, RecoveredDefinitionReference, RecoveredEffectRow>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum MemberKind {
	Value,
	Function,
	StaticValue,
	StaticFunction,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ExternalCallable {
	Linked { adapter: ExternalAdapterId },
	Native(nymph_hir::linkage::NativeExternal),
	Deferred,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ExternalAdapterId {
	/// Compiler-owned logical module identity. Backends resolve this identity to
	/// their own delivery mechanism; it is not a host import path.
	pub module: EcoString,
	pub symbol: EcoString,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ExternalState {
	#[default]
	None,
	Read,
	Write,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ExternalTransaction {
	#[default]
	Pure,
	Aware,
	Irreversible,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ExternalAudit {
	pub state: ExternalState,
	pub transaction: ExternalTransaction,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ExternalCallMode {
	#[default]
	Ordinary,
	Cancellable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ExternalMarshalPlan {
	pub parameters: Vec<Option<MarshalKind>>,
	pub result: Option<MarshalKind>,
}

/// Mint the stable nominal payload carried by an opaque external marshal step.
/// The source definition, not a backend class or import path, owns this ID.
#[must_use]
pub fn opaque_external_identity(definition: &DefinitionId) -> u64 {
	let mut hash = Fnv(0xcbf29ce484222325);
	"nymph.opaque.external.v1".hash(&mut hash);
	definition.hash(&mut hash);
	hash.finish()
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ExternalAbi<E = EffectRow> {
	pub marker: EcoString,
	pub callable: ExternalCallable,
	pub effects: E,
	pub audit: ExternalAudit,
	pub call_mode: ExternalCallMode,
	pub marshal: ExternalMarshalPlan,
}

impl<E> ExternalAbi<E> {
	#[must_use]
	pub fn adapter(&self) -> Option<&ExternalAdapterId> {
		match &self.callable {
			ExternalCallable::Linked { adapter } => Some(adapter),
			ExternalCallable::Native(_) | ExternalCallable::Deferred => None,
		}
	}

	#[must_use]
	pub fn linked(&self) -> Option<(&EcoString, &EcoString)> {
		self
			.adapter()
			.map(|adapter| (&adapter.module, &adapter.symbol))
	}
}

impl ExternalAbi {
	#[must_use]
	pub fn recovered(self) -> ExternalAbi<RecoveredEffectRow> {
		ExternalAbi {
			marker: self.marker,
			callable: self.callable,
			effects: RecoveredEffectRow::Known(self.effects),
			audit: self.audit,
			call_mode: self.call_mode,
			marshal: self.marshal,
		}
	}
}

impl ExternalAbi<RecoveredEffectRow> {
	#[must_use]
	pub fn complete(&self) -> Option<ExternalAbi> {
		let RecoveredEffectRow::Known(effects) = &self.effects else {
			return None;
		};
		Some(ExternalAbi {
			marker: self.marker.clone(),
			callable: self.callable.clone(),
			effects: effects.clone(),
			audit: self.audit,
			call_mode: self.call_mode,
			marshal: self.marshal.clone(),
		})
	}
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
	Effect,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ExportedDefinition {
	pub id: DefinitionId,
	pub name: EcoString,
	pub visibility: Option<Visibility>,
	pub kind: DefinitionShapeKind,
	/// Callable/value modifier retained for function and let declarations.
	pub declaration_kind: Option<MemberKind>,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<GenericConstraint>,
	pub parameters: Vec<ParameterShape<InterfaceType>>,
	pub return_type: Option<InterfaceType>,
	pub effects: EffectRow,
	pub ty: Option<InterfaceType>,
	pub fields: Vec<FieldShape<InterfaceType>>,
	pub variants: Vec<VariantShape<InterfaceType>>,
	/// Source-owned variants accepted by this enum's fixed-point static view.
	pub enum_view_variants: Vec<DefinitionId>,
	pub members: Vec<MemberShape<InterfaceType>>,
	pub super_interfaces: Vec<SuperInterface>,
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
	pub interface_effect_arguments: Vec<(EcoString, EffectRow)>,
	pub interface_argument_bindings: Vec<(GenericParameterId, InterfaceType)>,
	pub self_type: InterfaceType,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<GenericConstraint>,
	pub members: Vec<MemberShape<InterfaceType>>,
	pub member_slots: ImplementationMemberCatalog,
	pub runtime_owner: Option<DefinitionId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ImplementationMemberSource {
	Override,
	InheritedDefault,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ImplementationMemberSlot {
	pub implementation_id: DefinitionId,
	pub interface_member_id: DefinitionId,
	pub member_id: DefinitionId,
	pub body_definition_id: DefinitionId,
	pub placement_owner: DefinitionId,
	pub kind: MemberKind,
	pub name: EcoString,
	pub source: ImplementationMemberSource,
	pub external: bool,
}

/// Final checker-owned relation from an implementation and exact interface
/// member identity to the one runtime target selected for that pair.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ImplementationMemberCatalog {
	slots: Vec<ImplementationMemberSlot>,
}

impl ImplementationMemberCatalog {
	#[must_use]
	pub fn target(&self, interface_member: &DefinitionId) -> Option<&ImplementationMemberSlot> {
		self
			.slots
			.iter()
			.find(|slot| &slot.interface_member_id == interface_member)
	}

	pub fn retain(&mut self, mut predicate: impl FnMut(&ImplementationMemberSlot) -> bool) {
		self.slots.retain(|slot| predicate(slot));
	}
}

impl std::ops::Deref for ImplementationMemberCatalog {
	type Target = [ImplementationMemberSlot];

	fn deref(&self) -> &Self::Target {
		&self.slots
	}
}

impl<'a> IntoIterator for &'a ImplementationMemberCatalog {
	type Item = &'a ImplementationMemberSlot;
	type IntoIter = std::slice::Iter<'a, ImplementationMemberSlot>;

	fn into_iter(self) -> Self::IntoIter {
		self.slots.iter()
	}
}

impl FromIterator<ImplementationMemberSlot> for ImplementationMemberCatalog {
	fn from_iter<T: IntoIterator<Item = ImplementationMemberSlot>>(iter: T) -> Self {
		Self {
			slots: iter.into_iter().collect(),
		}
	}
}

impl From<Vec<ImplementationMemberSlot>> for ImplementationMemberCatalog {
	fn from(slots: Vec<ImplementationMemberSlot>) -> Self {
		Self { slots }
	}
}

pub(crate) fn project_implementation_member_catalog(
	implementation: &DefinitionId,
	interface_members: impl IntoIterator<Item = (DefinitionId, EcoString, MemberKind, bool)>,
	implementation_members: impl IntoIterator<Item = (DefinitionId, EcoString, MemberKind, bool)>,
) -> ImplementationMemberCatalog {
	let implementation_members = implementation_members.into_iter().collect::<Vec<_>>();
	interface_members
		.into_iter()
		.filter_map(|(interface_member_id, name, kind, has_default)| {
			if let Some((member_id, _, _, external)) =
				implementation_members
					.iter()
					.find(|(_, candidate_name, candidate_kind, _)| {
						*candidate_name == name && *candidate_kind == kind
					}) {
				return Some(ImplementationMemberSlot {
					implementation_id: implementation.clone(),
					interface_member_id,
					member_id: member_id.clone(),
					body_definition_id: member_id.clone(),
					placement_owner: implementation.clone(),
					kind,
					name,
					source: ImplementationMemberSource::Override,
					external: *external,
				});
			}
			has_default.then(|| {
				let member_id = DefinitionId::new(
					implementation.module.clone(),
					crate::DeclarationKey::materialized_interface_member(
						implementation.clone(),
						interface_member_id.clone(),
					),
				);
				ImplementationMemberSlot {
					implementation_id: implementation.clone(),
					interface_member_id: interface_member_id.clone(),
					member_id,
					body_definition_id: interface_member_id,
					placement_owner: implementation.clone(),
					kind,
					name,
					source: ImplementationMemberSource::InheritedDefault,
					external: false,
				}
			})
		})
		.collect()
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

	fn available_to(&self, current: &ModuleIdentity) -> Self {
		let mut interface = self.clone();
		let owner = interface.module.clone();
		let mut hidden = Vec::new();
		interface.exports.retain_mut(|definition| {
			project_definition(definition, &owner, current);
			if visibility_allows(definition.visibility, &owner, current) {
				true
			} else {
				hidden.push(SupportDefinition {
					definition: definition.clone(),
				});
				false
			}
		});
		let mut promoted = Vec::new();
		interface.support_definitions.retain_mut(|support| {
			project_definition(&mut support.definition, &owner, current);
			if visibility_allows(support.definition.visibility, &owner, current) {
				promoted.push(support.definition.clone());
				false
			} else {
				true
			}
		});
		interface.exports.extend(promoted);
		interface.support_definitions.extend(hidden);
		interface.implementations.retain_mut(|implementation| {
			project_members(&mut implementation.members, &owner, current);
			visibility_allows(implementation.visibility, &owner, current)
		});
		interface
	}
}

fn visibility_allows(
	visibility: Option<Visibility>,
	owner: &ModuleIdentity,
	current: &ModuleIdentity,
) -> bool {
	match visibility {
		None | Some(Visibility::Public) => true,
		Some(Visibility::Internal) => owner.same_package_as(current),
		Some(Visibility::Private) => owner == current,
	}
}

fn project_members<T, R, E>(
	members: &mut Vec<MemberShape<T, R, E>>,
	owner: &ModuleIdentity,
	current: &ModuleIdentity,
) {
	members.retain(|member| visibility_allows(member.visibility, owner, current));
}

fn project_definition(
	definition: &mut ExportedDefinition,
	owner: &ModuleIdentity,
	current: &ModuleIdentity,
) {
	project_members(&mut definition.members, owner, current);
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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum RecoveredDefinitionReference {
	Known(DefinitionId),
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
	pub declaration_kind: Option<MemberKind>,
	pub availability: SemanticAvailability,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<RecoveredGenericConstraint>,
	pub parameters: Vec<ParameterShape<RecoveredInterfaceType>>,
	pub return_type: Option<RecoveredInterfaceType>,
	pub effects: RecoveredEffectRow,
	pub ty: Option<RecoveredInterfaceType>,
	pub fields: Vec<FieldShape<RecoveredInterfaceType>>,
	pub variants: Vec<VariantShape<RecoveredInterfaceType>>,
	pub enum_view_variants: Vec<DefinitionId>,
	pub members: Vec<RecoveredMemberShape>,
	pub super_interfaces: Vec<RecoveredSuperInterface>,
	pub external: Option<ExternalAbi<RecoveredEffectRow>>,
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
	pub interface: Option<RecoveredDefinitionReference>,
	pub interface_arguments: Vec<(EcoString, RecoveredInterfaceType)>,
	pub interface_effect_arguments: Vec<(EcoString, RecoveredEffectRow)>,
	pub self_type: RecoveredInterfaceType,
	pub binders: Vec<GenericParameter>,
	pub constraints: Vec<RecoveredGenericConstraint>,
	pub members: Vec<RecoveredMemberShape>,
	pub member_slots: ImplementationMemberCatalog,
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

	fn available_to(&self, current: &ModuleIdentity) -> Self {
		let mut interface = self.clone();
		let owner = interface.module.clone();
		let mut hidden = Vec::new();
		interface.exports.retain_mut(|definition| {
			project_recovered_definition(definition, &owner, current);
			if visibility_allows(definition.visibility, &owner, current) {
				true
			} else {
				hidden.push(RecoveredSupportDefinition {
					definition: definition.clone(),
				});
				false
			}
		});
		let mut promoted = Vec::new();
		interface.support_definitions.retain_mut(|support| {
			project_recovered_definition(&mut support.definition, &owner, current);
			if visibility_allows(support.definition.visibility, &owner, current) {
				promoted.push(support.definition.clone());
				false
			} else {
				true
			}
		});
		interface.exports.extend(promoted);
		interface.support_definitions.extend(hidden);
		interface.implementations.retain_mut(|implementation| {
			project_members(&mut implementation.members, &owner, current);
			visibility_allows(implementation.visibility, &owner, current)
		});
		interface
	}
}

fn project_recovered_definition(
	definition: &mut RecoveredExportedDefinition,
	owner: &ModuleIdentity,
	current: &ModuleIdentity,
) {
	project_members(&mut definition.members, owner, current);
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ModuleEnvironment {
	Complete(ModuleInterface),
	Recovered(RecoveredModuleInterface),
}

impl ModuleEnvironment {
	pub(crate) fn available_to(&self, current: &ModuleIdentity) -> Self {
		match self {
			Self::Complete(interface) => Self::Complete(interface.available_to(current)),
			Self::Recovered(interface) => Self::Recovered(interface.available_to(current)),
		}
	}
}
