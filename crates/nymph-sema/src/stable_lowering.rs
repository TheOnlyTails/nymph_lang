//! Stable, semantic-only contracts for per-definition HIR lowering.
//!
//! Its inputs are exact stable identities and owned semantic artifacts, so lowering
//! does not need compiler queries, parser identities, source locations, or module ASTs.

use std::{
	cell::{Cell, RefCell},
	collections::{HashMap, HashSet},
	sync::Arc,
};

use ecow::EcoString;
use nymph_ast::ops::{AssignOperator, BinaryOperator, PatternOperator, PrefixOperator};
use nymph_hir::hir::{
	BinOp, BuiltinResult, HirArm, HirArrayElem, HirArrayKind, HirBoundDispatchCase,
	HirBoundDispatchTarget, HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirLit, HirMapElem,
	HirMethod, HirModule, HirPat, HirRange, HirStmt, HirVariant, NumKind, ScalarCastKind, UnOp,
};

use crate::{
	DefinitionId, EnumShell, ExportedDefinition, ExportedImpl, ExternalAbi, InterfaceType,
	MemberShape, ModuleIdentity, RuntimeDefinition, StableExpr, StableExprKind, StableListItem,
	StableListPatternEntry, StableMapEntry, StableMapPatternEntry, StablePattern, StablePatternKind,
	StablePatternRange, StableRange, StableStatement, StableStringPart, StableStringPatternPart,
	StableStructPatternField, StructShell,
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
	ImplementationsForInterface(DefinitionId),
	InterfaceShell(DefinitionId),
	ExternalAbi(DefinitionId),
}

impl StableShapeRequest {
	pub fn definition(&self) -> &DefinitionId {
		match self {
			Self::TypeShell(id)
			| Self::Member(id)
			| Self::Implementation(id)
			| Self::ImplementationsForInterface(id)
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
	Implementations(Vec<ExportedImpl>),
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
	TopLevelExternal {
		name: EmittedBindingName,
		abi: ExternalAbi,
	},
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

/// Exact, location-free result of assembling one semantic module. Dependency
/// declarations are never copied into `hir`; their identities remain explicit
/// in `imports`, while compiler-owned runtime fragments remain separately typed.
#[derive(Clone, Debug, PartialEq)]
pub struct StableHirModule {
	pub module: ModuleIdentity,
	pub hir: HirModule,
	pub own_definitions: Vec<DefinitionId>,
	pub fragments: Vec<LoweredRuntimeDefinition>,
	pub imports: Vec<DefinitionId>,
	pub virtual_runtime: Vec<VirtualRuntimeFragment>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VirtualRuntimeFragment {
	pub owner: ModuleIdentity,
	pub definition: DefinitionId,
	pub fragment: LoweredRuntimeDefinition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StableModuleAssemblyError {
	RuntimeExtraction(crate::RuntimeExtractionError),
	RecoveredEnvironment {
		module: ModuleIdentity,
	},
	Lowering(StableLoweringError),
	DuplicateAttachment {
		owner: DefinitionId,
		name: EcoString,
	},
	MissingOwnerShell {
		owner: DefinitionId,
	},
	MismatchedPlacement {
		definition: DefinitionId,
		owner: DefinitionId,
	},
	UnresolvedDemand {
		definition: DefinitionId,
	},
	RecoveredDemand {
		definition: DefinitionId,
	},
	DemandCycle {
		definition: DefinitionId,
	},
	MissingExecutionBody {
		caller: DefinitionId,
		callee: DefinitionId,
	},
	InitializerCycle {
		cycle: Vec<DefinitionId>,
	},
	UnresolvedInitializerCall {
		initializer: DefinitionId,
		body: DefinitionId,
		call: UnresolvedRuntimeCall,
	},
}

impl From<StableLoweringError> for StableModuleAssemblyError {
	fn from(error: StableLoweringError) -> Self {
		Self::Lowering(error)
	}
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeExecutionSummary {
	immediate_reads: StableDemandSet,
	immediate_calls: StableDemandSet,
	unresolved_calls: Vec<UnresolvedRuntimeCall>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnresolvedRuntimeCall {
	DynamicCallee,
	CallableValue(DefinitionId),
	OpaqueExternal(DefinitionId),
	GenericDispatch {
		interface: DefinitionId,
		member: DefinitionId,
	},
	IteratorNext {
		interface: DefinitionId,
		member: DefinitionId,
	},
}

impl RuntimeExecutionSummary {
	pub fn immediate_reads(&self) -> &[DefinitionId] {
		self.immediate_reads.as_slice()
	}
	pub fn immediate_calls(&self) -> &[DefinitionId] {
		self.immediate_calls.as_slice()
	}
	pub fn unresolved_calls(&self) -> &[UnresolvedRuntimeCall] {
		&self.unresolved_calls
	}
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoweredRuntimeDefinition {
	definition: DefinitionId,
	fragment: LoweredHirFragment,
	demands: StableDemandSet,
	direct_demands: StableDemandSet,
	routed_demands: StableDemandSet,
	execution: RuntimeExecutionSummary,
	placement: RuntimeAssemblyPlacement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeAssemblyPlacement {
	Module(ModuleIdentity),
	Shell(DefinitionId),
	Template,
}

impl LoweredRuntimeDefinition {
	pub fn new(
		definition: DefinitionId,
		fragment: LoweredHirFragment,
		demands: StableDemandSet,
		placement: RuntimeAssemblyPlacement,
	) -> Self {
		let direct_demands = demands.clone();
		Self {
			definition,
			fragment,
			demands,
			direct_demands,
			routed_demands: StableDemandSet::new(),
			execution: RuntimeExecutionSummary::default(),
			placement,
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
	pub fn direct_demands(&self) -> &[DefinitionId] {
		self.direct_demands.as_slice()
	}
	pub fn routed_demands(&self) -> &[DefinitionId] {
		self.routed_demands.as_slice()
	}
	pub fn immediate_reads(&self) -> &[DefinitionId] {
		self.execution.immediate_reads()
	}
	pub fn immediate_calls(&self) -> &[DefinitionId] {
		self.execution.immediate_calls()
	}
	pub fn unresolved_calls(&self) -> &[UnresolvedRuntimeCall] {
		self.execution.unresolved_calls()
	}
	pub fn placement(&self) -> &RuntimeAssemblyPlacement {
		&self.placement
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
	Parse {
		definition: DefinitionId,
		reason: EcoString,
	},
	ShapeDrift {
		definition: DefinitionId,
		reason: EcoString,
	},
	MissingAnnotation {
		definition: DefinitionId,
		node: crate::BodyNodeId,
		channel: EcoString,
	},
	MissingExternalModule {
		definition: DefinitionId,
	},
	MissingExternalSymbol {
		definition: DefinitionId,
	},
	MissingExternalMarshal {
		definition: DefinitionId,
	},
	MismatchedExternalAbi {
		definition: DefinitionId,
	},
	MismatchedExternalMarshal {
		definition: DefinitionId,
		expected: nymph_hir::hir::MarshalKind,
		actual: nymph_hir::hir::MarshalKind,
	},
	MismatchedExternalMember {
		member: DefinitionId,
		implementation: DefinitionId,
	},
	MissingImplementationSlot {
		implementation: DefinitionId,
		member: DefinitionId,
	},
	MissingInterfaceMember {
		interface: DefinitionId,
		member: DefinitionId,
	},
	AmbiguousDispatchCase {
		interface: DefinitionId,
		member: DefinitionId,
		receiver_tag: EcoString,
		argument_tag: EcoString,
	},
	MismatchedImplementationPlacement {
		definition: DefinitionId,
		expected: DefinitionId,
		actual: DefinitionId,
	},
	MissingAttachmentShell {
		definition: DefinitionId,
		owner: DefinitionId,
	},
	Unsupported {
		definition: DefinitionId,
		node: Option<crate::BodyNodeId>,
		feature: EcoString,
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

/// Lowers one checked, location-free runtime artifact. This deliberately accepts
/// no module, compatibility annotation table, prelude offset, or symbol map.
pub fn lower_runtime_definition(
	context: &impl StableLoweringContext,
	artifact: Arc<RuntimeDefinition>,
) -> Result<LoweredRuntimeDefinition, StableLoweringError> {
	let definition = artifact.definition.clone();
	let mut demands = StableDemandSet::new();
	let mut direct_demands = StableDemandSet::new();
	let mut routed_demands = StableDemandSet::new();
	let mut execution = RuntimeExecutionSummary::default();
	let fragment = match &artifact.payload {
		crate::RuntimePayload::External(abi) => lower_external(context, &artifact, abi)?,
		crate::RuntimePayload::Struct(shell) => {
			let name = context.binding_name(&definition)?;
			let fields = shell
				.fields
				.iter()
				.map(|field| {
					context
						.member_name(&field.id)
						.map(|name| name.as_str().into())
				})
				.collect::<Result<_, _>>()?;
			LoweredHirFragment::StructShell(HirClass {
				name: name.as_str().into(),
				fields,
				methods: vec![],
				statics: vec![],
			})
		}
		crate::RuntimePayload::Enum(shell) => {
			let name = context.binding_name(&definition)?;
			let variants = shell
				.variants
				.iter()
				.map(|variant| {
					Ok(HirVariant {
						name: context.member_name(&variant.id)?.as_str().into(),
						fields: variant
							.fields
							.iter()
							.map(|field| {
								context
									.member_name(&field.id)
									.map(|name| name.as_str().into())
							})
							.collect::<Result<_, _>>()?,
					})
				})
				.collect::<Result<_, StableLoweringError>>()?;
			LoweredHirFragment::EnumShell(HirEnum {
				name: name.as_str().into(),
				variants,
				methods: vec![],
				statics: vec![],
			})
		}
		crate::RuntimePayload::NymphBody(body) => {
			let implementation_member = attached_implementation_member(context, &artifact)?;
			let implementation = implementation_member
				.as_ref()
				.map(|(implementation, _)| implementation)
				.filter(|implementation| implementation.interface.is_some());
			lower_body(
				context,
				&artifact,
				body,
				&mut demands,
				&mut direct_demands,
				&mut routed_demands,
				&mut execution,
				implementation.map(|implementation| &implementation.self_type),
				implementation.map(|implementation| &implementation.member_slots),
				implementation_member
					.as_ref()
					.map(|(_, member)| member.kind),
			)?
		}
		crate::RuntimePayload::MaterializedInterfaceMember {
			body_definition,
			interface_member,
		} => {
			let implementation = match &definition.key {
				crate::DeclarationKey::MaterializedInterfaceMember {
					implementation,
					interface_member: key_member,
				} if **key_member == *interface_member => (**implementation).clone(),
				_ => {
					return Err(invalid(
						&definition,
						"materialized artifact identity does not match its interface member",
					));
				}
			};
			let owner = attached_owner(&artifact)?;
			if owner != implementation {
				return Err(StableLoweringError::MismatchedImplementationPlacement {
					definition: definition.clone(),
					expected: implementation,
					actual: owner,
				});
			}
			let request = StableShapeRequest::Implementation(implementation.clone());
			let StableShapeFact::Implementation(implementation_shape) = context.stable_shape(&request)?
			else {
				return Err(StableShapeLookupError::WrongFact { request }.into());
			};
			if let crate::InterfaceType::Named { definition, .. } = &implementation_shape.self_type {
				demands.insert(definition.clone());
				direct_demands.insert(definition.clone());
			}
			let slot = implementation_shape
				.member_slots
				.iter()
				.find(|slot| slot.member_id == definition)
				.ok_or_else(|| StableLoweringError::MissingImplementationSlot {
					implementation: implementation.clone(),
					member: definition.clone(),
				})?;
			if slot.implementation_id != implementation
				|| slot.placement_owner != implementation
				|| slot.interface_member_id != *interface_member
				|| slot.body_definition_id != *body_definition
				|| slot.source != crate::ImplementationMemberSource::InheritedDefault
				|| slot.external
			{
				return Err(invalid(
					&definition,
					"materialized artifact disagrees with its persisted implementation slot",
				));
			}
			let interface = implementation_shape
				.interface
				.as_ref()
				.ok_or_else(|| invalid(&definition, "materialized default has no exact interface"))?;
			let request = StableShapeRequest::InterfaceShell(interface.clone());
			let StableShapeFact::InterfaceShell(interface_shape) = context.stable_shape(&request)? else {
				return Err(StableShapeLookupError::WrongFact { request }.into());
			};
			let member_shape = interface_shape
				.members
				.iter()
				.find(|member| member.id == *interface_member)
				.ok_or_else(|| StableLoweringError::MissingInterfaceMember {
					interface: interface.clone(),
					member: interface_member.clone(),
				})?;
			let crate::RuntimePlacement::Attached { name, .. } = &artifact.placement else {
				unreachable!()
			};
			if slot.kind != member_shape.kind || slot.name != member_shape.name || name != &slot.name {
				return Err(invalid(
					&definition,
					"materialized artifact disagrees with its persisted slot shape",
				));
			}
			let body = context.runtime_definition(body_definition)?;
			let crate::RuntimePayload::NymphBody(checked) = &body.payload else {
				return Err(invalid(
					&definition,
					"materialized default body is not a Nymph body",
				));
			};
			let lowered = lower_body(
				context,
				&artifact,
				checked,
				&mut demands,
				&mut direct_demands,
				&mut routed_demands,
				&mut execution,
				Some(&implementation_shape.self_type),
				Some(&implementation_shape.member_slots),
				Some(member_shape.kind),
			)?;
			if matches!(lowered, LoweredHirFragment::TopLevelFunction(_)) {
				lowered
			} else {
				let method = fragment_method(lowered)?;
				LoweredHirFragment::MaterializedDefault {
					owner,
					implementation,
					interface_member: interface_member.clone(),
					method,
				}
			}
		}
	};
	let placement = runtime_assembly_placement(context, &definition, &fragment)?;
	if let RuntimeAssemblyPlacement::Shell(shell) = &placement {
		demands.insert(shell.clone());
		direct_demands.insert(shell.clone());
	}
	let mut lowered = LoweredRuntimeDefinition::new(definition, fragment, demands, placement);
	lowered.direct_demands = direct_demands;
	lowered.routed_demands = routed_demands;
	lowered.execution = execution;
	Ok(lowered)
}

fn lower_external(
	context: &impl StableLoweringContext,
	artifact: &RuntimeDefinition,
	abi: &crate::ExternalAbi,
) -> Result<LoweredHirFragment, StableLoweringError> {
	let definition = &artifact.definition;
	let exact = external_abi(context, definition)?;
	if exact != *abi {
		return Err(StableLoweringError::MismatchedExternalAbi {
			definition: definition.clone(),
		});
	}
	let crate::RuntimePlacement::Attached { owner, .. } = &artifact.placement else {
		return lower_top_level_external(context, definition, abi);
	};
	let Some((_, member)) = attached_implementation_member(context, artifact)? else {
		return Err(invalid(
			definition,
			"external member has no exact implementation shape",
		));
	};
	if owner_has_no_attachment_shell(context, owner)? {
		return lower_shellless_external(context, definition, abi, &member);
	}
	let name: EcoString = context.member_name(definition)?.as_str().into();
	match member.kind {
		crate::MemberKind::Value | crate::MemberKind::MutableValue | crate::MemberKind::StaticValue => {
			let module = external_module(definition, abi)?;
			let symbol = external_symbol(definition, abi)?;
			let marshal = abi
				.marshal
				.ok_or_else(|| StableLoweringError::MissingExternalMarshal {
					definition: definition.clone(),
				})?;
			Ok(LoweredHirFragment::AttachedMember {
				owner: owner.clone(),
				method: HirMethod {
					name,
					params: vec![],
					body: HirExpr::ExternValue {
						module,
						symbol,
						marshal,
					},
				},
			})
		}
		crate::MemberKind::Function
		| crate::MemberKind::MutatingFunction
		| crate::MemberKind::StaticFunction => {
			let params = member
				.parameters
				.iter()
				.enumerate()
				.map(|(index, parameter)| {
					parameter
						.name
						.clone()
						.unwrap_or_else(|| format!("$arg{index}").into())
				})
				.collect::<Vec<_>>();
			let mut args = if member.kind == crate::MemberKind::StaticFunction {
				Vec::new()
			} else {
				vec![HirExpr::This]
			};
			args.extend(params.iter().cloned().map(HirExpr::Local));
			let body = external_call_expr(definition, abi, &args)?;
			let method = HirMethod { name, params, body };
			if member.kind == crate::MemberKind::StaticFunction {
				Ok(LoweredHirFragment::AttachedStatic {
					owner: owner.clone(),
					method,
				})
			} else {
				Ok(LoweredHirFragment::AttachedInstance {
					owner: owner.clone(),
					method,
				})
			}
		}
	}
}

fn lower_shellless_external(
	context: &impl StableLoweringContext,
	definition: &DefinitionId,
	abi: &crate::ExternalAbi,
	member: &crate::MemberShape<InterfaceType>,
) -> Result<LoweredHirFragment, StableLoweringError> {
	match member.kind {
		crate::MemberKind::Function
		| crate::MemberKind::MutatingFunction
		| crate::MemberKind::StaticFunction => lower_top_level_external(context, definition, abi),
		crate::MemberKind::Value | crate::MemberKind::MutableValue | crate::MemberKind::StaticValue => {
			let value = HirExpr::ExternValue {
				module: external_module(definition, abi)?,
				symbol: external_symbol(definition, abi)?,
				marshal: abi
					.marshal
					.ok_or_else(|| StableLoweringError::MissingExternalMarshal {
						definition: definition.clone(),
					})?,
			};
			let name: EcoString = context.binding_name(definition)?.as_str().into();
			if member.kind == crate::MemberKind::StaticValue {
				Ok(LoweredHirFragment::TopLevelValue(HirLet {
					name,
					mutable: false,
					value,
				}))
			} else {
				Ok(LoweredHirFragment::TopLevelFunction(HirFunc {
					name,
					params: vec!["$self".into()],
					body: value,
				}))
			}
		}
	}
}

fn external_call_expr(
	definition: &DefinitionId,
	abi: &crate::ExternalAbi,
	args: &[HirExpr],
) -> Result<HirExpr, StableLoweringError> {
	match &abi.callable {
		crate::ExternalCallable::Linked { module, symbol } => Ok(HirExpr::ExternCall {
			module: Box::leak(module.to_string().into_boxed_str()),
			symbol: Box::leak(symbol.to_string().into_boxed_str()),
			args: args.to_vec(),
		}),
		crate::ExternalCallable::Native(native) => match (native, args) {
			(nymph_hir::linkage::NativeExternal::Binary { op, result }, [lhs, rhs]) => {
				Ok(HirExpr::Binary {
					op: *op,
					result: *result,
					lhs: Box::new(lhs.clone()),
					rhs: Box::new(rhs.clone()),
				})
			}
			(nymph_hir::linkage::NativeExternal::Unary { op, result }, [operand]) => Ok(HirExpr::Unary {
				op: *op,
				result: *result,
				operand: Box::new(operand.clone()),
			}),
			(nymph_hir::linkage::NativeExternal::Index, [receiver, index]) => Ok(HirExpr::Index {
				recv: Box::new(receiver.clone()),
				index: Box::new(index.clone()),
			}),
			_ => Err(invalid(
				definition,
				"native external dispatch arity does not match its exact ABI",
			)),
		},
		crate::ExternalCallable::Deferred => {
			Err(invalid(definition, "external dispatch target is deferred"))
		}
	}
}

fn lower_top_level_external(
	context: &impl StableLoweringContext,
	definition: &DefinitionId,
	abi: &crate::ExternalAbi,
) -> Result<LoweredHirFragment, StableLoweringError> {
	let name = context.binding_name(definition)?;
	if matches!(
		&definition.key,
		crate::DeclarationKey::TopLevel {
			category: crate::DeclarationCategory::Let,
			..
		}
	) {
		let module = external_module(definition, abi)?;
		let symbol = external_symbol(definition, abi)?;
		let marshal = abi
			.marshal
			.ok_or_else(|| StableLoweringError::MissingExternalMarshal {
				definition: definition.clone(),
			})?;
		return Ok(LoweredHirFragment::TopLevelValue(HirLet {
			name: name.as_str().into(),
			mutable: false,
			value: HirExpr::ExternValue {
				module,
				symbol,
				marshal,
			},
		}));
	}
	if let crate::ExternalCallable::Native(native) = abi.callable {
		let params = match native {
			nymph_hir::linkage::NativeExternal::Binary { .. } => {
				vec![EcoString::from("$self"), EcoString::from("other")]
			}
			nymph_hir::linkage::NativeExternal::Unary { .. } => vec![EcoString::from("$self")],
			nymph_hir::linkage::NativeExternal::Index => {
				vec![EcoString::from("$self"), EcoString::from("key")]
			}
		};
		let args = params
			.iter()
			.cloned()
			.map(HirExpr::Local)
			.collect::<Vec<_>>();
		return Ok(LoweredHirFragment::TopLevelFunction(HirFunc {
			name: name.as_str().into(),
			params,
			body: external_call_expr(definition, abi, &args)?,
		}));
	}
	external_module(definition, abi)?;
	external_symbol(definition, abi)?;
	Ok(LoweredHirFragment::TopLevelExternal {
		name,
		abi: abi.clone(),
	})
}

fn runtime_assembly_placement(
	context: &impl StableShapeLookup,
	definition: &DefinitionId,
	fragment: &LoweredHirFragment,
) -> Result<RuntimeAssemblyPlacement, StableLoweringError> {
	use LoweredHirFragment as Fragment;
	match fragment {
		Fragment::TopLevelFunction(_)
		| Fragment::TopLevelValue(_)
		| Fragment::TopLevelExternal { .. }
		| Fragment::StructShell(_)
		| Fragment::EnumShell(_) => Ok(RuntimeAssemblyPlacement::Module(definition.module.clone())),
		Fragment::MaterializedDefault { implementation, .. } => {
			attachment_shell(context, definition, implementation)
		}
		Fragment::AttachedInstance { owner, .. }
		| Fragment::AttachedStatic { owner, .. }
		| Fragment::AttachedMember { owner, .. } => match &owner.key {
			crate::DeclarationKey::TopLevel {
				category: crate::DeclarationCategory::Interface,
				..
			} => Ok(RuntimeAssemblyPlacement::Template),
			_ => attachment_shell(context, definition, owner),
		},
	}
}

fn attachment_shell(
	context: &impl StableShapeLookup,
	definition: &DefinitionId,
	owner: &DefinitionId,
) -> Result<RuntimeAssemblyPlacement, StableLoweringError> {
	let shell = match &owner.key {
		crate::DeclarationKey::TopLevel {
			category: crate::DeclarationCategory::Struct | crate::DeclarationCategory::Enum,
			..
		} => owner.clone(),
		crate::DeclarationKey::Implementation { .. } => {
			let request = StableShapeRequest::Implementation(owner.clone());
			let StableShapeFact::Implementation(shape) = context.stable_shape(&request)? else {
				return Err(StableShapeLookupError::WrongFact { request }.into());
			};
			let Some(definition) = nominal_attachment_shell(&shape.self_type) else {
				return Err(StableLoweringError::MissingAttachmentShell {
					definition: definition.clone(),
					owner: owner.clone(),
				});
			};
			definition.clone()
		}
		_ => {
			return Err(StableLoweringError::MissingAttachmentShell {
				definition: definition.clone(),
				owner: owner.clone(),
			});
		}
	};
	let request = StableShapeRequest::TypeShell(shell.clone());
	if !matches!(
		context.stable_shape(&request)?,
		StableShapeFact::TypeShell(_)
	) {
		return Err(StableShapeLookupError::WrongFact { request }.into());
	}
	Ok(RuntimeAssemblyPlacement::Shell(shell))
}

fn invalid(definition: &DefinitionId, reason: &str) -> StableLoweringError {
	StableLoweringError::InvalidArtifact {
		definition: definition.clone(),
		reason: reason.into(),
	}
}

fn external_abi(
	context: &impl StableShapeLookup,
	definition: &DefinitionId,
) -> Result<ExternalAbi, StableLoweringError> {
	let request = StableShapeRequest::ExternalAbi(definition.clone());
	let StableShapeFact::ExternalAbi(abi) = context.stable_shape(&request)? else {
		return Err(StableShapeLookupError::WrongFact { request }.into());
	};
	Ok(abi)
}

fn exact_external_abi(
	context: &impl StableLoweringContext,
	definition: &DefinitionId,
	persisted_marshal: Option<nymph_hir::hir::MarshalKind>,
) -> Result<ExternalAbi, StableLoweringError> {
	let artifact = context.runtime_definition(definition)?;
	let crate::RuntimePayload::External(payload_abi) = &artifact.payload else {
		return Err(invalid(
			definition,
			"external target has a non-external body",
		));
	};
	let shape_abi = external_abi(context, definition)?;
	if let Some(expected) = persisted_marshal {
		match shape_abi.marshal {
			None => {
				return Err(StableLoweringError::MissingExternalMarshal {
					definition: definition.clone(),
				});
			}
			Some(actual) if actual != expected => {
				return Err(StableLoweringError::MismatchedExternalMarshal {
					definition: definition.clone(),
					expected,
					actual,
				});
			}
			Some(_) => {}
		}
	}
	if shape_abi != *payload_abi {
		return Err(StableLoweringError::MismatchedExternalAbi {
			definition: definition.clone(),
		});
	}
	Ok(shape_abi)
}

fn external_module(
	definition: &DefinitionId,
	abi: &ExternalAbi,
) -> Result<&'static str, StableLoweringError> {
	abi
		.linked()
		.map(|(module, _)| module)
		.map(|module| Box::leak(module.to_string().into_boxed_str()) as &'static str)
		.ok_or_else(|| StableLoweringError::MissingExternalModule {
			definition: definition.clone(),
		})
}

fn external_symbol(
	definition: &DefinitionId,
	abi: &ExternalAbi,
) -> Result<&'static str, StableLoweringError> {
	abi
		.linked()
		.map(|(_, symbol)| symbol)
		.map(|symbol| Box::leak(symbol.to_string().into_boxed_str()) as &'static str)
		.ok_or_else(|| StableLoweringError::MissingExternalSymbol {
			definition: definition.clone(),
		})
}

fn stable_runtime_tag(ty: &InterfaceType) -> Option<EcoString> {
	let tag = match ty {
		InterfaceType::Int => "int",
		InterfaceType::UInt => "uint",
		InterfaceType::Float => "float",
		InterfaceType::Char => "char",
		InterfaceType::String => "string",
		InterfaceType::Boolean => "bool",
		InterfaceType::Void => "void",
		InterfaceType::List(_) | InterfaceType::Tuple(_) => "list",
		InterfaceType::Map(..) => "map",
		InterfaceType::Mutable(inner) => {
			return stable_runtime_tag(inner)
				.map(|tag| EcoString::from(format!("nymph.mut_{}", &tag[6..])));
		}
		_ => return None,
	};
	Some(EcoString::from(format!("nymph.{tag}")))
}

fn substitute_self_type(ty: &InterfaceType, self_type: &InterfaceType) -> InterfaceType {
	match ty {
		InterfaceType::SelfType => self_type.clone(),
		InterfaceType::List(inner) => {
			InterfaceType::List(Box::new(substitute_self_type(inner, self_type)))
		}
		InterfaceType::Tuple(items) => InterfaceType::Tuple(
			items
				.iter()
				.map(|item| substitute_self_type(item, self_type))
				.collect(),
		),
		InterfaceType::Map(key, value) => InterfaceType::Map(
			Box::new(substitute_self_type(key, self_type)),
			Box::new(substitute_self_type(value, self_type)),
		),
		InterfaceType::Function {
			parameters,
			return_type,
		} => InterfaceType::Function {
			parameters: parameters
				.iter()
				.map(|parameter| substitute_self_type(parameter, self_type))
				.collect(),
			return_type: Box::new(substitute_self_type(return_type, self_type)),
		},
		InterfaceType::Named {
			definition,
			positional,
			named,
		} => InterfaceType::Named {
			definition: definition.clone(),
			positional: positional
				.iter()
				.map(|argument| substitute_self_type(argument, self_type))
				.collect(),
			named: named
				.iter()
				.map(|(name, argument)| (name.clone(), substitute_self_type(argument, self_type)))
				.collect(),
		},
		InterfaceType::Intersection(items) => InterfaceType::Intersection(
			items
				.iter()
				.map(|item| substitute_self_type(item, self_type))
				.collect(),
		),
		InterfaceType::Mutable(inner) => {
			InterfaceType::Mutable(Box::new(substitute_self_type(inner, self_type)))
		}
		_ => ty.clone(),
	}
}

fn owner_has_no_attachment_shell(
	context: &impl StableShapeLookup,
	owner: &DefinitionId,
) -> Result<bool, StableLoweringError> {
	if !matches!(owner.key, crate::DeclarationKey::Implementation { .. }) {
		return Ok(false);
	}
	let request = StableShapeRequest::Implementation(owner.clone());
	let StableShapeFact::Implementation(shape) = context.stable_shape(&request)? else {
		return Err(StableShapeLookupError::WrongFact { request }.into());
	};
	Ok(nominal_attachment_shell(&shape.self_type).is_none())
}

fn nominal_attachment_shell(ty: &InterfaceType) -> Option<&DefinitionId> {
	match ty {
		InterfaceType::Named { definition, .. } => Some(definition),
		InterfaceType::Mutable(inner) => nominal_attachment_shell(inner),
		_ => None,
	}
}

fn exact_implementation_slot<'a>(
	implementation: &'a crate::ExportedImpl,
	member: &DefinitionId,
) -> Option<&'a crate::ImplementationMemberSlot> {
	match &member.key {
		crate::DeclarationKey::MaterializedInterfaceMember {
			implementation: owner,
			..
		} if **owner == implementation.id => implementation
			.member_slots
			.iter()
			.find(|slot| slot.member_id == *member),
		crate::DeclarationKey::Member { owner, .. } if **owner == implementation.id => implementation
			.member_slots
			.iter()
			.find(|slot| slot.member_id == *member),
		crate::DeclarationKey::Member { owner, .. }
			if implementation.interface.as_ref() == Some(owner.as_ref()) =>
		{
			implementation
				.member_slots
				.iter()
				.find(|slot| slot.interface_member_id == *member)
		}
		_ => None,
	}
}

fn validate_attached_member(
	context: &impl StableLoweringContext,
	implementation: &crate::ExportedImpl,
	member: &DefinitionId,
) -> Result<Arc<RuntimeDefinition>, StableLoweringError> {
	let shape = implementation
		.members
		.iter()
		.find(|shape| shape.id == *member)
		.ok_or_else(|| {
			invalid(
				member,
				"dispatch member is absent from its exact implementation shape",
			)
		})?;
	let artifact = context.runtime_definition(member)?;
	let crate::RuntimePlacement::Attached { owner, name } = &artifact.placement else {
		return Err(invalid(member, "dispatch member has top-level placement"));
	};
	if owner != &implementation.id {
		return Err(StableLoweringError::MismatchedImplementationPlacement {
			definition: member.clone(),
			expected: implementation.id.clone(),
			actual: owner.clone(),
		});
	}
	if shape.name != *name
		|| shape.external.is_some() != matches!(artifact.payload, crate::RuntimePayload::External(_))
	{
		return Err(invalid(
			member,
			"dispatch member kind, name, or external shape has drifted",
		));
	}
	Ok(artifact)
}

fn validate_direct_member(
	context: &impl StableLoweringContext,
	owner: &DefinitionId,
	member: &DefinitionId,
) -> Result<Arc<RuntimeDefinition>, StableLoweringError> {
	let emitted_name = context.member_name(member)?;
	let request = StableShapeRequest::Member(member.clone());
	let StableShapeFact::Member(shape) = context.stable_shape(&request)? else {
		return Err(StableShapeLookupError::WrongFact { request }.into());
	};
	let artifact = context.runtime_definition(member)?;
	let crate::RuntimePlacement::Attached {
		owner: actual_owner,
		name,
	} = &artifact.placement
	else {
		return Err(invalid(
			member,
			"direct dispatch member has top-level placement",
		));
	};
	if actual_owner != owner {
		return Err(StableLoweringError::MismatchedImplementationPlacement {
			definition: member.clone(),
			expected: owner.clone(),
			actual: actual_owner.clone(),
		});
	}
	if shape.id != *member
		|| shape.name != *name
		|| emitted_name.as_str() != shape.name
		|| shape.runtime_owner.as_ref() != Some(owner)
		|| !matches!(
			shape.kind,
			crate::MemberKind::Function | crate::MemberKind::MutatingFunction
		) || shape.external.is_some() != matches!(artifact.payload, crate::RuntimePayload::External(_))
	{
		return Err(invalid(member, "direct dispatch member shape has drifted"));
	}
	Ok(artifact)
}

fn validate_dispatch_slot<'a>(
	context: &impl StableLoweringContext,
	implementation: &'a crate::ExportedImpl,
	interface: &DefinitionId,
	member: &DefinitionId,
	source: crate::ImplementationMemberSource,
	materialization: crate::DispatchMaterialization,
) -> Result<&'a crate::ImplementationMemberSlot, StableLoweringError> {
	if implementation.interface.as_ref() != Some(interface) {
		return Err(invalid(
			member,
			"dispatch interface does not match its exact implementation",
		));
	}
	let slot = implementation
		.member_slots
		.iter()
		.find(|slot| slot.member_id == *member)
		.ok_or_else(|| StableLoweringError::MissingImplementationSlot {
			implementation: implementation.id.clone(),
			member: member.clone(),
		})?;
	let request = StableShapeRequest::InterfaceShell(interface.clone());
	let StableShapeFact::InterfaceShell(shell) = context.stable_shape(&request)? else {
		return Err(StableShapeLookupError::WrongFact { request }.into());
	};
	let interface_member = shell
		.members
		.iter()
		.find(|shape| shape.id == slot.interface_member_id)
		.ok_or_else(|| StableLoweringError::MissingInterfaceMember {
			interface: interface.clone(),
			member: slot.interface_member_id.clone(),
		})?;
	let artifact = context.runtime_definition(member)?;
	let implementation_member = implementation
		.members
		.iter()
		.find(|shape| shape.id == *member);
	if source == crate::ImplementationMemberSource::Override
		&& implementation_member.is_none_or(|shape| {
			shape.id != slot.member_id
				|| shape.name != slot.name
				|| shape.kind != slot.kind
				|| shape.runtime_owner.as_ref() != Some(&implementation.id)
				|| shape.external.is_some() != slot.external
		}) {
		return Err(invalid(
			member,
			"override dispatch slot disagrees with its exact implementation member shape",
		));
	}
	let expected_body = match &artifact.payload {
		crate::RuntimePayload::MaterializedInterfaceMember {
			body_definition,
			interface_member,
		} => {
			if materialization != crate::DispatchMaterialization::CanonicalBody
				|| interface_member != &slot.interface_member_id
			{
				return Err(invalid(member, "dispatch materialization has drifted"));
			}
			body_definition
		}
		crate::RuntimePayload::External(_)
			if materialization == crate::DispatchMaterialization::ExternalAbi =>
		{
			member
		}
		_ if materialization == crate::DispatchMaterialization::Attached => member,
		_ => return Err(invalid(member, "dispatch materialization has drifted")),
	};
	let crate::RuntimePlacement::Attached { owner, name } = &artifact.placement else {
		return Err(invalid(member, "dispatch target has top-level placement"));
	};
	if slot.implementation_id != implementation.id
		|| slot.placement_owner != implementation.id
		|| owner != &implementation.id
		|| slot.body_definition_id != *expected_body
		|| slot.source != source
		|| slot.kind != interface_member.kind
		|| slot.name != interface_member.name
		|| name != &slot.name
		|| slot.external != matches!(artifact.payload, crate::RuntimePayload::External(_))
	{
		return Err(invalid(
			member,
			"dispatch target disagrees with its complete persisted slot",
		));
	}
	Ok(slot)
}

fn shellless_implementation_member(
	context: &impl StableLoweringContext,
	member: &DefinitionId,
	implementation: &DefinitionId,
) -> Result<bool, StableLoweringError> {
	if !owner_has_no_attachment_shell(context, implementation)? {
		return Ok(false);
	}
	let artifact = context.runtime_definition(member)?;
	let crate::RuntimePlacement::Attached { owner, .. } = &artifact.placement else {
		return Err(invalid(
			member,
			"shell-less implementation member has top-level placement",
		));
	};
	if owner != implementation {
		return Err(StableLoweringError::MismatchedImplementationPlacement {
			definition: member.clone(),
			expected: implementation.clone(),
			actual: owner.clone(),
		});
	}
	Ok(true)
}

fn attached_owner(artifact: &RuntimeDefinition) -> Result<DefinitionId, StableLoweringError> {
	match &artifact.placement {
		crate::RuntimePlacement::Attached { owner, .. } => Ok(owner.clone()),
		_ => Err(invalid(
			&artifact.definition,
			"attached fragment has top-level placement",
		)),
	}
}

fn attached_implementation_member(
	context: &impl StableLoweringContext,
	artifact: &RuntimeDefinition,
) -> Result<Option<(crate::ExportedImpl, crate::MemberShape<InterfaceType>)>, StableLoweringError> {
	let crate::RuntimePlacement::Attached { owner, name } = &artifact.placement else {
		return Ok(None);
	};
	if !matches!(owner.key, crate::DeclarationKey::Implementation { .. }) {
		return Ok(None);
	}
	let request = StableShapeRequest::Implementation(owner.clone());
	let StableShapeFact::Implementation(implementation) = context.stable_shape(&request)? else {
		return Err(StableShapeLookupError::WrongFact { request }.into());
	};
	if implementation.id != *owner {
		return Err(invalid(
			&artifact.definition,
			"attached artifact owner disagrees with its implementation shape",
		));
	}
	let member = implementation
		.members
		.iter()
		.find(|member| member.id == artifact.definition)
		.cloned()
		.ok_or_else(|| {
			invalid(
				&artifact.definition,
				"attached artifact has no exact implementation member shape",
			)
		})?;
	let canonical = validate_attached_member(context, &implementation, &artifact.definition)?;
	if canonical.as_ref() != artifact
		|| member.name != *name
		|| member.runtime_owner.as_ref() != Some(owner)
	{
		return Err(invalid(
			&artifact.definition,
			"attached artifact disagrees with its exact implementation member shape",
		));
	}
	let emitted_name = context.member_name(&artifact.definition)?;
	if let Some(interface) = &implementation.interface {
		let request = StableShapeRequest::InterfaceShell(interface.clone());
		let StableShapeFact::InterfaceShell(shell) = context.stable_shape(&request)? else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		if let Some(interface_member) = shell
			.members
			.iter()
			.find(|interface_member| interface_member.name == member.name)
		{
			if interface_member.kind != member.kind {
				return Err(invalid(
					&artifact.definition,
					"implementation member shadows an interface member with a different kind",
				));
			}
			validate_dispatch_slot(
				context,
				&implementation,
				interface,
				&artifact.definition,
				crate::ImplementationMemberSource::Override,
				if matches!(artifact.payload, crate::RuntimePayload::External(_)) {
					crate::DispatchMaterialization::ExternalAbi
				} else {
					crate::DispatchMaterialization::Attached
				},
			)?;
			if emitted_name != context.member_name(&interface_member.id)? {
				return Err(invalid(
					&artifact.definition,
					"implementation override selector disagrees with its exact interface member",
				));
			}
		} else {
			if implementation
				.member_slots
				.iter()
				.any(|slot| slot.member_id == artifact.definition)
			{
				return Err(invalid(
					&artifact.definition,
					"interface extension member has an extraneous implementation slot",
				));
			}
			if emitted_name.as_str() != member.name {
				return Err(invalid(
					&artifact.definition,
					"interface extension selector disagrees with its exact member shape",
				));
			}
		}
	} else if emitted_name.as_str() != member.name {
		return Err(invalid(
			&artifact.definition,
			"inherent implementation selector disagrees with its exact member shape",
		));
	}
	if let crate::RuntimePayload::NymphBody(body) = &artifact.payload
		&& !matches!(
			(member.kind, &body.kind),
			(
				crate::MemberKind::Function | crate::MemberKind::MutatingFunction,
				crate::RuntimeBodyKind::InstanceFunction
			) | (
				crate::MemberKind::StaticFunction,
				crate::RuntimeBodyKind::StaticFunction
			) | (
				crate::MemberKind::Value | crate::MemberKind::MutableValue | crate::MemberKind::StaticValue,
				crate::RuntimeBodyKind::Value
			)
		) {
		return Err(invalid(
			&artifact.definition,
			"attached artifact body kind disagrees with its exact member kind",
		));
	}
	Ok(Some((implementation, member)))
}

fn fragment_method(fragment: LoweredHirFragment) -> Result<HirMethod, StableLoweringError> {
	match fragment {
		LoweredHirFragment::AttachedInstance { method, .. }
		| LoweredHirFragment::AttachedStatic { method, .. }
		| LoweredHirFragment::AttachedMember { method, .. } => Ok(method),
		_ => Err(StableLoweringError::ShapeDrift {
			definition: DefinitionId::new(
				ModuleIdentity {
					origin: crate::ModuleOrigin::Compiler,
					project: "nymph".into(),
					path: "lowering".into(),
				},
				crate::DeclarationKey::top_level(crate::DeclarationCategory::Function, "default"),
			),
			reason: "default body did not lower to an attached method".into(),
		}),
	}
}

fn lower_body(
	context: &impl StableLoweringContext,
	artifact: &RuntimeDefinition,
	body: &crate::CheckedRuntimeBody,
	demands: &mut StableDemandSet,
	direct_demands: &mut StableDemandSet,
	routed_demands: &mut StableDemandSet,
	execution: &mut RuntimeExecutionSummary,
	self_type: Option<&InterfaceType>,
	implementation_slots: Option<&crate::ImplementationMemberCatalog>,
	member_kind: Option<crate::MemberKind>,
) -> Result<LoweredHirFragment, StableLoweringError> {
	let is_function = body.kind != crate::RuntimeBodyKind::Value;
	let stable = &body.stable;
	let shellless_implementation = match &artifact.placement {
		crate::RuntimePlacement::Attached { owner, .. } => {
			owner_has_no_attachment_shell(context, owner)?
		}
		crate::RuntimePlacement::TopLevel => false,
	};
	let has_receiver = shellless_implementation
		&& matches!(
			member_kind,
			Some(
				crate::MemberKind::Function
					| crate::MemberKind::MutatingFunction
					| crate::MemberKind::Value
					| crate::MemberKind::MutableValue
			)
		);
	let lowerer = StableBodyLowerer {
		context,
		artifact,
		annotations: &body.annotations,
		scopes: RefCell::new(vec![HashMap::new()]),
		counters: RefCell::new(HashMap::new()),
		pattern_declaration_records: RefCell::new(vec![]),
		pattern_declaration_reuse: RefCell::new(vec![]),
		demands: RefCell::new(demands),
		direct_demands: RefCell::new(direct_demands),
		routed_demands: RefCell::new(routed_demands),
		execution: RefCell::new(execution),
		deferred_depth: Cell::new(0),
		loop_depth: Cell::new(0),
		loop_targets: RefCell::new(Vec::new()),
		next_loop_target: Cell::new(0),
		block_targets: RefCell::new(Vec::new()),
		next_block_target: Cell::new(0),
		self_type,
		implementation_slots,
		receiver_binding: has_receiver.then(|| EcoString::from("$self")),
	};
	if has_receiver {
		lowerer.scopes.borrow_mut()[0].insert("this".into(), "$self".into());
	}
	let emitted: EcoString = context.binding_name(&artifact.definition)?.as_str().into();
	if is_function {
		let params = stable
			.params
			.iter()
			.map(|param| pattern_name(&param.pattern).map(|name| lowerer.declare(name)))
			.collect::<Result<Vec<_>, StableLoweringError>>()?;
		let lowered_body = lowerer.lower_function_body(&stable.root)?;
		let function = HirFunc {
			name: emitted.clone(),
			params: if has_receiver {
				std::iter::once(EcoString::from("$self"))
					.chain(params.iter().cloned())
					.collect()
			} else {
				params.clone()
			},
			body: lowered_body.clone(),
		};
		if shellless_implementation {
			return Ok(LoweredHirFragment::TopLevelFunction(function));
		}
		match &artifact.placement {
			crate::RuntimePlacement::TopLevel => Ok(LoweredHirFragment::TopLevelFunction(function)),
			crate::RuntimePlacement::Attached { owner, .. } => {
				let name = context.member_name(&artifact.definition)?.as_str().into();
				let method = HirMethod {
					name,
					params,
					body: lowered_body,
				};
				if body.kind == crate::RuntimeBodyKind::StaticFunction {
					Ok(LoweredHirFragment::AttachedStatic {
						owner: owner.clone(),
						method,
					})
				} else {
					Ok(LoweredHirFragment::AttachedInstance {
						owner: owner.clone(),
						method,
					})
				}
			}
		}
	} else {
		let value = lowerer.lower(&stable.root)?;
		if shellless_implementation {
			return if has_receiver {
				Ok(LoweredHirFragment::TopLevelFunction(HirFunc {
					name: emitted,
					params: vec!["$self".into()],
					body: value,
				}))
			} else {
				Ok(LoweredHirFragment::TopLevelValue(HirLet {
					name: emitted,
					mutable: member_kind == Some(crate::MemberKind::MutableValue),
					value,
				}))
			};
		}
		match &artifact.placement {
			crate::RuntimePlacement::TopLevel => Ok(LoweredHirFragment::TopLevelValue(HirLet {
				name: emitted,
				mutable: false,
				value,
			})),
			crate::RuntimePlacement::Attached { owner, .. } => Ok(LoweredHirFragment::AttachedMember {
				owner: owner.clone(),
				method: HirMethod {
					name: context.member_name(&artifact.definition)?.as_str().into(),
					params: vec![],
					body: value,
				},
			}),
		}
	}
}

fn pattern_name(pattern: &StablePattern) -> Result<&EcoString, StableLoweringError> {
	match &pattern.kind {
		StablePatternKind::Binding { name, .. } => Ok(name),
		StablePatternKind::Placeholder => {
			static PLACEHOLDER: std::sync::LazyLock<EcoString> = std::sync::LazyLock::new(|| "_".into());
			Ok(&PLACEHOLDER)
		}
		_ => Err(StableLoweringError::Unsupported {
			definition: dummy_id(),
			node: None,
			feature: "destructuring binding".into(),
		}),
	}
}
fn dummy_id() -> DefinitionId {
	DefinitionId::new(
		ModuleIdentity {
			origin: crate::ModuleOrigin::Compiler,
			project: "nymph".into(),
			path: "lowering".into(),
		},
		crate::DeclarationKey::top_level(crate::DeclarationCategory::Function, "body"),
	)
}

struct StableBodyLowerer<'a, C> {
	context: &'a C,
	artifact: &'a RuntimeDefinition,
	annotations: &'a crate::RuntimeAnnotations,
	scopes: RefCell<Vec<HashMap<EcoString, EcoString>>>,
	counters: RefCell<HashMap<EcoString, u32>>,
	pattern_declaration_records: RefCell<Vec<HashMap<EcoString, EcoString>>>,
	pattern_declaration_reuse: RefCell<Vec<HashMap<EcoString, EcoString>>>,
	demands: RefCell<&'a mut StableDemandSet>,
	direct_demands: RefCell<&'a mut StableDemandSet>,
	routed_demands: RefCell<&'a mut StableDemandSet>,
	execution: RefCell<&'a mut RuntimeExecutionSummary>,
	deferred_depth: Cell<u32>,
	loop_depth: Cell<u32>,
	loop_targets: RefCell<Vec<(crate::BodyNodeId, u32)>>,
	next_loop_target: Cell<u32>,
	block_targets: RefCell<Vec<(crate::BodyNodeId, nymph_hir::hir::BlockTarget)>>,
	next_block_target: Cell<nymph_hir::hir::BlockTarget>,
	self_type: Option<&'a InterfaceType>,
	implementation_slots: Option<&'a crate::ImplementationMemberCatalog>,
	receiver_binding: Option<EcoString>,
}
fn js_reserved_word(name: &str) -> bool {
	matches!(
		name,
		"await"
			| "break"
			| "case"
			| "catch"
			| "class"
			| "const"
			| "continue"
			| "debugger"
			| "default"
			| "delete"
			| "do"
			| "else"
			| "enum"
			| "export"
			| "extends"
			| "false"
			| "finally"
			| "for"
			| "function"
			| "if"
			| "import"
			| "in"
			| "instanceof"
			| "let"
			| "new"
			| "null"
			| "return"
			| "static"
			| "super"
			| "switch"
			| "this"
			| "throw"
			| "true"
			| "try"
			| "typeof"
			| "var"
			| "void"
			| "while"
			| "with"
			| "yield"
	)
}
impl<C: StableLoweringContext> StableBodyLowerer<'_, C> {
	fn id(&self, expr: &StableExpr) -> crate::BodyNodeId {
		expr.id
	}
	fn unsupported(&self, expr: &StableExpr, feature: &str) -> StableLoweringError {
		StableLoweringError::Unsupported {
			definition: self.artifact.definition.clone(),
			node: Some(self.id(expr)),
			feature: feature.into(),
		}
	}
	fn declare(&self, name: &EcoString) -> EcoString {
		if let Some(emitted) = self
			.pattern_declaration_reuse
			.borrow()
			.last()
			.and_then(|bindings| bindings.get(name))
			.cloned()
		{
			self
				.scopes
				.borrow_mut()
				.last_mut()
				.unwrap()
				.insert(name.clone(), emitted.clone());
			for record in self.pattern_declaration_records.borrow_mut().iter_mut() {
				record.insert(name.clone(), emitted.clone());
			}
			return emitted;
		}
		let collision = self
			.scopes
			.borrow()
			.iter()
			.any(|scope| scope.contains_key(name));
		let emitted = if js_reserved_word(name) {
			format!("${name}").into()
		} else if collision {
			let mut counters = self.counters.borrow_mut();
			let count = counters.entry(name.clone()).or_default();
			*count += 1;
			format!("{name}${count}").into()
		} else {
			name.clone()
		};
		self
			.scopes
			.borrow_mut()
			.last_mut()
			.unwrap()
			.insert(name.clone(), emitted.clone());
		for record in self.pattern_declaration_records.borrow_mut().iter_mut() {
			record.insert(name.clone(), emitted.clone());
		}
		emitted
	}
	fn resolve(&self, name: &EcoString) -> EcoString {
		self
			.scopes
			.borrow()
			.iter()
			.rev()
			.find_map(|scope| scope.get(name).cloned())
			.unwrap_or_else(|| name.clone())
	}
	fn target(&self, expr: &StableExpr) -> Option<&DefinitionId> {
		let id = self.id(expr);
		self
			.annotations
			.definition_targets
			.iter()
			.find(|(found, _)| *found == id)
			.map(|(_, target)| target)
	}
	fn record_read(&self, target: &DefinitionId) {
		if self.deferred_depth.get() == 0 {
			self
				.execution
				.borrow_mut()
				.immediate_reads
				.insert(target.clone());
		}
	}
	fn record_call(&self, target: &DefinitionId) -> Result<bool, StableLoweringError> {
		if self.deferred_depth.get() != 0 {
			return Ok(false);
		}
		let artifact = self.context.runtime_definition(target)?;
		let callable = match &artifact.payload {
			crate::RuntimePayload::NymphBody(body) => body.kind != crate::RuntimeBodyKind::Value,
			crate::RuntimePayload::MaterializedInterfaceMember { .. } => true,
			crate::RuntimePayload::External(_) => !matches!(
				target.key,
				crate::DeclarationKey::TopLevel {
					category: crate::DeclarationCategory::Let,
					..
				}
			),
			crate::RuntimePayload::Struct(_) | crate::RuntimePayload::Enum(_) => false,
		};
		if callable {
			self
				.execution
				.borrow_mut()
				.immediate_calls
				.insert(target.clone());
			if matches!(
				artifact.payload,
				crate::RuntimePayload::External(crate::ExternalAbi {
					callable: crate::ExternalCallable::Linked { .. },
					..
				})
			) {
				self.record_unresolved_call(UnresolvedRuntimeCall::OpaqueExternal(target.clone()));
			}
		}
		Ok(callable)
	}
	fn record_unresolved_call(&self, call: UnresolvedRuntimeCall) {
		if self.deferred_depth.get() == 0 && !self.execution.borrow().unresolved_calls.contains(&call) {
			self.execution.borrow_mut().unresolved_calls.push(call);
		}
	}
	fn lower_deferred<T>(
		&self,
		lower: impl FnOnce() -> Result<T, StableLoweringError>,
	) -> Result<T, StableLoweringError> {
		let depth = self.deferred_depth.get();
		self.deferred_depth.set(depth + 1);
		let result = lower();
		self.deferred_depth.set(depth);
		result
	}
	fn external_marshal(
		&self,
		expr: &StableExpr,
	) -> Result<nymph_hir::hir::MarshalKind, StableLoweringError> {
		let node = self.id(expr);
		self
			.annotations
			.external_marshals
			.iter()
			.find(|(id, _)| *id == node)
			.map(|(_, marshal)| *marshal)
			.ok_or_else(|| StableLoweringError::MissingAnnotation {
				definition: self.artifact.definition.clone(),
				node,
				channel: "external marshal".into(),
			})
	}
	fn ty(&self, expr: &StableExpr) -> Result<InterfaceType, StableLoweringError> {
		let node = self.id(expr);
		let ty = self
			.annotations
			.types
			.iter()
			.find(|(id, _)| *id == node)
			.map(|(_, ty)| ty)
			.ok_or_else(|| StableLoweringError::MissingAnnotation {
				definition: self.artifact.definition.clone(),
				node,
				channel: "type".into(),
			})?;
		Ok(self.self_type.map_or_else(
			|| ty.clone(),
			|self_type| substitute_self_type(ty, self_type),
		))
	}
	/// Whether this checked expression cannot complete normally. An anonymous
	/// closure annotation turns the underlying expression into a callable value,
	/// so its body's `never` type must not erase an enclosing operation.
	fn definitely_transfers(&self, expr: &StableExpr) -> bool {
		let node = self.id(expr);
		if self
			.annotations
			.anonymous_closures
			.iter()
			.any(|(id, _)| *id == node)
		{
			return false;
		}
		if self
			.annotations
			.types
			.iter()
			.find(|(id, _)| *id == node)
			.is_some_and(|(_, ty)| matches!(peel_mut(ty), InterfaceType::Never))
		{
			return true;
		}
		match &expr.kind {
			StableExprKind::Return { .. }
			| StableExprKind::Break { .. }
			| StableExprKind::Continue { .. } => true,
			StableExprKind::Grouped(value) => self.definitely_transfers(value),
			StableExprKind::Block { body, .. } => body.iter().any(|statement| {
				self.definitely_transfers(match statement {
					StableStatement::Let { value, .. } | StableStatement::Expr(value) => value,
				})
			}),
			StableExprKind::If {
				condition,
				then,
				otherwise,
			} => {
				self.definitely_transfers(condition)
					|| otherwise.as_deref().is_some_and(|otherwise| {
						self.definitely_transfers(then) && self.definitely_transfers(otherwise)
					})
			}
			StableExprKind::Match { value, arms } => {
				self.definitely_transfers(value)
					|| (!arms.is_empty() && arms.iter().all(|arm| self.definitely_transfers(&arm.body)))
			}
			_ => false,
		}
	}
	fn builtin_result(&self, expr: &StableExpr) -> Result<BuiltinResult, StableLoweringError> {
		match peel_mut(&self.ty(expr)?) {
			InterfaceType::Int => Ok(BuiltinResult::Int),
			InterfaceType::UInt => Ok(BuiltinResult::UInt),
			InterfaceType::Float => Ok(BuiltinResult::Float),
			InterfaceType::Char => Ok(BuiltinResult::Char),
			InterfaceType::String => Ok(BuiltinResult::String),
			InterfaceType::Boolean => Ok(BuiltinResult::Boolean),
			_ => Err(self.unsupported(expr, "built-in operator result type")),
		}
	}
	fn int_literal_kind(&self, expr: &StableExpr) -> Result<NumKind, StableLoweringError> {
		let Some(ty) = self
			.annotations
			.types
			.iter()
			.find(|(id, _)| *id == self.id(expr))
			.map(|(_, ty)| ty)
		else {
			return Ok(NumKind::Int);
		};
		match peel_mut(ty) {
			InterfaceType::Int => Ok(NumKind::Int),
			InterfaceType::UInt => Ok(NumKind::UInt),
			InterfaceType::Float => Ok(NumKind::Float),
			_ => Err(self.unsupported(expr, "integer literal type")),
		}
	}
	fn dispatch(&self, expr: &StableExpr) -> Result<&crate::StableDispatch, StableLoweringError> {
		let node = self.id(expr);
		self
			.annotations
			.dispatches
			.iter()
			.find(|(id, _)| *id == node)
			.map(|(_, dispatch)| dispatch)
			.ok_or_else(|| StableLoweringError::MissingAnnotation {
				definition: self.artifact.definition.clone(),
				node,
				channel: "dispatch".into(),
			})
	}
	fn variant(&self, expr: &StableExpr) -> Option<&crate::ExpressionVariant> {
		self
			.annotations
			.variants
			.iter()
			.find(|(id, _)| *id == self.id(expr))
			.map(|(_, variant)| variant)
	}
	fn lower_function_body(&self, expr: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		let outer_loops = self.loop_targets.replace(Vec::new());
		let outer_blocks = self.block_targets.replace(Vec::new());
		let lowered = match &expr.kind {
			StableExprKind::Block { body, label: None } => self.lower_block(body, false),
			StableExprKind::Block {
				body,
				label: Some(_),
			} => {
				let target = self.next_block_target.get();
				self.next_block_target.set(target + 1);
				self
					.block_targets
					.borrow_mut()
					.push((self.id(expr), target));
				let body = self.lower_block(body, false);
				self.block_targets.borrow_mut().pop();
				body.map(|body| HirExpr::LabeledBlock {
					target,
					body: Box::new(body),
				})
			}
			_ => self.lower(expr),
		};
		self.block_targets.replace(outer_blocks);
		self.loop_targets.replace(outer_loops);
		lowered
	}
	fn lower_branch(&self, expr: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		self.lower(expr)
	}
	fn lower_loop_branch(
		&self,
		source: crate::BodyNodeId,
		target: u32,
		expr: &StableExpr,
	) -> Result<HirExpr, StableLoweringError> {
		let depth = self.loop_depth.get();
		self.loop_depth.set(depth + 1);
		self.loop_targets.borrow_mut().push((source, target));
		let result = self.lower_branch(expr);
		self.loop_targets.borrow_mut().pop();
		self.loop_depth.set(depth);
		result
	}
	fn next_loop(&self) -> u32 {
		let target = self.next_loop_target.get();
		self.next_loop_target.set(target + 1);
		target
	}
	fn return_target(&self, expr: &StableExpr) -> nymph_hir::hir::HirReturnTarget {
		let jump = self.id(expr);
		let resolved = self
			.annotations
			.control_targets
			.iter()
			.find_map(|(candidate, target)| (*candidate == jump).then_some(*target));
		let Some(crate::runtime::RuntimeControlTarget::Block(resolved)) = resolved else {
			return nymph_hir::hir::HirReturnTarget::Callable;
		};
		self
			.block_targets
			.borrow()
			.iter()
			.rev()
			.find(|(source, _)| *source == resolved)
			.map_or(nymph_hir::hir::HirReturnTarget::Callable, |(_, target)| {
				nymph_hir::hir::HirReturnTarget::Block(*target)
			})
	}
	fn loop_option(
		&self,
		expr: &StableExpr,
	) -> Result<Option<nymph_hir::hir::HirOptionAbi>, StableLoweringError> {
		let Some(role) = &self.annotations.option else {
			return Ok(None);
		};
		let Some((_, ty)) = self
			.annotations
			.types
			.iter()
			.find(|(id, _)| *id == self.id(expr))
		else {
			return Ok(None);
		};
		if !matches!(peel_mut(ty), InterfaceType::Named { definition, .. } if definition == &role.option)
		{
			return Ok(None);
		}
		self.demand_external(&role.option)?;
		Ok(Some(nymph_hir::hir::HirOptionAbi {
			enum_name: self.context.binding_name(&role.option)?.as_str().into(),
			some: self.context.member_name(&role.some)?.as_str().into(),
			some_value: self.context.member_name(&role.some_value)?.as_str().into(),
			none: self.context.member_name(&role.none)?.as_str().into(),
		}))
	}
	fn lower(&self, expr: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		if let Some((_, arity)) = self
			.annotations
			.anonymous_closures
			.iter()
			.find(|(id, _)| *id == self.id(expr))
		{
			self.scopes.borrow_mut().push(HashMap::new());
			let params = (0..*arity)
				.map(|i| self.declare(&crate::anon_closure::anon_param_name(i)))
				.collect();
			let loop_depth = self.loop_depth.replace(0);
			let loop_targets = self.loop_targets.replace(Vec::new());
			let block_targets = self.block_targets.replace(Vec::new());
			let body = self.lower_deferred(|| self.lower_inner(expr));
			self.block_targets.replace(block_targets);
			self.loop_targets.replace(loop_targets);
			self.loop_depth.set(loop_depth);
			let body = body?;
			self.scopes.borrow_mut().pop();
			return Ok(HirExpr::Closure {
				params,
				body: Box::new(body),
			});
		}
		self.lower_inner(expr)
	}
	fn lower_inner(&self, expr: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		Ok(match &expr.kind {
			StableExprKind::Int(value) => HirExpr::Num(*value as f64, self.int_literal_kind(expr)?),
			StableExprKind::UInt(value) => HirExpr::Num(*value as f64, NumKind::UInt),
			StableExprKind::Float(value) => HirExpr::Num(value.into_inner(), NumKind::Float),
			StableExprKind::Boolean(value) => HirExpr::Bool(*value),
			StableExprKind::Char(value) => HirExpr::Char(*value),
			StableExprKind::String(parts)
				if parts
					.iter()
					.all(|part| !matches!(part, StableStringPart::Expr(_))) =>
			{
				HirExpr::Str(
					parts
						.iter()
						.map(|part| match part {
							StableStringPart::Text(text) => text.to_string(),
							StableStringPart::Escape(escape) => cooked_escape(*escape),
							_ => unreachable!(),
						})
						.collect::<String>()
						.into(),
				)
			}
			StableExprKind::Identifier(name) => {
				if let Some(variant) = self.variant(expr) {
					self.demand_external(&variant.enum_definition)?;
					return Ok(HirExpr::VariantRef {
						enum_name: self
							.context
							.binding_name(&variant.enum_definition)?
							.as_str()
							.into(),
						variant: self
							.context
							.member_name(&variant.variant_definition)?
							.as_str()
							.into(),
					});
				}
				if let Some(target) = self.target(expr) {
					self.record_read(target);
					let emitted = self.context.binding_name(target)?;
					let target_artifact = self.context.runtime_definition(target)?;
					if matches!(target_artifact.payload, crate::RuntimePayload::External(_)) {
						let abi = exact_external_abi(self.context, target, None)?;
						if matches!(
							&target.key,
							crate::DeclarationKey::TopLevel {
								category: crate::DeclarationCategory::Let,
								..
							}
						) {
							let expected =
								abi
									.marshal
									.ok_or_else(|| StableLoweringError::MissingExternalMarshal {
										definition: target.clone(),
									})?;
							let actual = self.external_marshal(expr)?;
							if expected != actual {
								return Err(StableLoweringError::MismatchedExternalMarshal {
									definition: target.clone(),
									expected,
									actual,
								});
							}
						}
						self.demand_direct(target);
					} else if target != &self.artifact.definition {
						if target.module != self.artifact.definition.module {
							self.context.module_specifier(&target.module)?;
						}
						self.demand_direct(target);
					}
					HirExpr::Local(emitted.as_str().into())
				} else {
					HirExpr::Local(self.resolve(&name))
				}
			}
			StableExprKind::AnonymousParam(index) => {
				HirExpr::Local(self.resolve(&crate::anon_closure::anon_param_name(index.unwrap_or(0))))
			}
			StableExprKind::This => self
				.receiver_binding
				.clone()
				.map(HirExpr::Local)
				.unwrap_or(HirExpr::This),
			StableExprKind::Grouped(inner) => self.lower(inner)?,
			StableExprKind::List(items) | StableExprKind::Tuple(items) => {
				let kind = if matches!(expr.kind, StableExprKind::List(_)) {
					HirArrayKind::List
				} else {
					HirArrayKind::Tuple
				};
				if items
					.iter()
					.any(|item| matches!(*item, StableListItem::Spread(_)))
				{
					return Ok(HirExpr::ArraySpread {
						kind,
						elems: items
							.iter()
							.map(|item| match item {
								StableListItem::Expr(value) => self.lower(value).map(HirArrayElem::Item),
								StableListItem::Spread(value) => self.lower_spread(value).map(HirArrayElem::Spread),
							})
							.collect::<Result<_, _>>()?,
					});
				}
				HirExpr::Array {
					kind,
					items: items
						.iter()
						.map(|item| match item {
							StableListItem::Expr(item) => self.lower(item),
							_ => unreachable!(),
						})
						.collect::<Result<_, _>>()?,
				}
			}
			StableExprKind::Map(entries) => {
				if entries
					.iter()
					.any(|entry| matches!(*entry, StableMapEntry::Spread(_)))
				{
					return Ok(HirExpr::MapSpread(
						entries
							.iter()
							.map(|entry| match entry {
								StableMapEntry::Entry(key, value) => {
									Ok(HirMapElem::Entry(self.lower(key)?, self.lower(value)?))
								}
								StableMapEntry::Spread(value) => self.lower_spread(value).map(HirMapElem::Spread),
							})
							.collect::<Result<_, StableLoweringError>>()?,
					));
				}
				HirExpr::MapLit(
					entries
						.iter()
						.map(|entry| match entry {
							StableMapEntry::Entry(key, value) => Ok((self.lower(key)?, self.lower(value)?)),
							_ => unreachable!(),
						})
						.collect::<Result<_, StableLoweringError>>()?,
				)
			}
			StableExprKind::MemberAccess {
				parent,
				member,
				optional: _,
			} => {
				if self.definitely_transfers(parent) {
					return self.lower(parent);
				}
				if let Some((_, dispatch)) = self
					.annotations
					.dispatches
					.iter()
					.find(|(id, _)| *id == self.id(expr))
				{
					return self.lower_method_value(expr, dispatch, parent);
				}
				if let Some(variant) = self.variant(expr) {
					self.demand_external(&variant.enum_definition)?;
					return Ok(HirExpr::VariantRef {
						enum_name: self
							.context
							.binding_name(&variant.enum_definition)?
							.as_str()
							.into(),
						variant: self
							.context
							.member_name(&variant.variant_definition)?
							.as_str()
							.into(),
					});
				}
				if self
					.annotations
					.direct_namespace_members
					.contains(&self.id(expr))
					&& let Some(target) = self.target(expr)
				{
					if matches!(target.key, crate::DeclarationKey::TopLevel { .. }) {
						self.demand_external(target)?;
						return Ok(HirExpr::Local(
							self.context.binding_name(target)?.as_str().into(),
						));
					}
					if matches!(target.key, crate::DeclarationKey::Member { .. }) {
						let runtime = self.context.runtime_definition(target)?;
						let crate::RuntimePlacement::Attached { owner, .. } = &runtime.placement else {
							return Err(invalid(
								&self.artifact.definition,
								"static member target is not attached to an owner",
							));
						};
						let shell = match &owner.key {
							crate::DeclarationKey::TopLevel {
								category: crate::DeclarationCategory::Struct | crate::DeclarationCategory::Enum,
								..
							} => Some(owner.clone()),
							crate::DeclarationKey::Implementation { .. } => {
								let request = StableShapeRequest::Implementation(owner.clone());
								let StableShapeFact::Implementation(implementation) =
									self.context.stable_shape(&request)?
								else {
									return Err(StableShapeLookupError::WrongFact { request }.into());
								};
								if implementation.id != *owner {
									return Err(invalid(
										&self.artifact.definition,
										"static member owner disagrees with its exact implementation shape",
									));
								}
								nominal_attachment_shell(&implementation.self_type).cloned()
							}
							_ => {
								return Err(invalid(
									&self.artifact.definition,
									"static member owner has no nominal or implementation shell",
								));
							}
						};
						self.demand_external(target)?;
						let Some(shell) = shell else {
							return Ok(HirExpr::Local(
								self.context.binding_name(target)?.as_str().into(),
							));
						};
						return Ok(HirExpr::Field {
							recv: Box::new(HirExpr::Local(
								self.context.binding_name(&shell)?.as_str().into(),
							)),
							name: self.context.member_name(target)?.as_str().into(),
						});
					}
				}
				HirExpr::Field {
					recv: Box::new(self.lower(parent)?),
					name: if let Some(target) = self.target(expr) {
						self.context.member_name(target)?.as_str().into()
					} else {
						member.clone()
					},
				}
			}
			StableExprKind::Call { func, args } => {
				if self.definitely_transfers(func) {
					return self.lower(func);
				}
				if let Some(variant) = self.variant(expr) {
					self.demand_external(&variant.enum_definition)?;
					let fields = args
						.iter()
						.enumerate()
						.map(|(index, arg)| {
							let field = arg
								.name
								.as_ref()
								.map(|name| name.clone())
								.or_else(|| variant.fields.get(index).map(|field| field.name.clone()))
								.ok_or_else(|| {
									invalid(
										&self.artifact.definition,
										"variant positional argument has no exact field",
									)
								})?;
							Ok((field, self.lower(&arg.value)?))
						})
						.collect::<Result<_, StableLoweringError>>()?;
					return Ok(HirExpr::VariantNew {
						enum_name: self
							.context
							.binding_name(&variant.enum_definition)?
							.as_str()
							.into(),
						variant: self
							.context
							.member_name(&variant.variant_definition)?
							.as_str()
							.into(),
						fields,
					});
				}
				if let Some(target) = self.target(func)
					&& matches!(
						target.key,
						crate::DeclarationKey::TopLevel {
							category: crate::DeclarationCategory::Struct,
							..
						}
					) {
					let request = StableShapeRequest::TypeShell(target.clone());
					if let StableShapeFact::TypeShell(crate::StableTypeShell::Struct(shell)) =
						self.context.stable_shape(&request)?
					{
						self.demand_external(target)?;
						self.scopes.borrow_mut().push(HashMap::new());
						let mut stmts = Vec::with_capacity(args.len());
						let mut values = HashMap::with_capacity(args.len());
						for (index, argument) in args.iter().enumerate() {
							let field = argument
								.name
								.as_ref()
								.and_then(|name| shell.fields.iter().find(|field| field.name == *name))
								.or_else(|| shell.fields.get(index))
								.ok_or_else(|| {
									invalid(
										&self.artifact.definition,
										"struct argument has no exact field",
									)
								})?;
							let temporary = self.declare(&format!("$struct_arg_{index}").into());
							stmts.push(HirStmt::Let {
								name: temporary.clone(),
								mutable: false,
								value: self.lower(&argument.value)?,
							});
							values.insert(field.name.clone(), temporary);
						}
						let fields = shell
							.fields
							.iter()
							.filter_map(|field| {
								values
									.get(&field.name)
									.cloned()
									.map(|value| (field.name.clone(), HirExpr::Local(value)))
							})
							.collect();
						self.scopes.borrow_mut().pop();
						return Ok(HirExpr::Block {
							stmts,
							tail: Some(Box::new(HirExpr::New {
								class: self.context.binding_name(target)?.as_str().into(),
								fields,
							})),
						});
					}
				}
				if let StableExprKind::MemberAccess { parent, .. } = &func.kind
					&& let Some((_, dispatch)) = self
						.annotations
						.dispatches
						.iter()
						.find(|(id, _)| *id == self.id(expr))
				{
					return self.lower_dispatch(
						dispatch,
						parent,
						args.iter().map(|arg| &arg.value).collect(),
					);
				}
				if self
					.annotations
					.generic_namespaced_calls
					.contains(&self.id(expr))
				{
					return Err(self.unsupported(expr, "namespaced call through a generic type parameter"));
				}
				if let Some(target) = self.target(func) {
					let target_artifact = self.context.runtime_definition(target)?;
					if matches!(target_artifact.payload, crate::RuntimePayload::External(_)) {
						let _ = self.record_call(target)?;
						self.demand_direct(target);
						let abi = exact_external_abi(self.context, target, None)?;
						let module = external_module(target, &abi)?;
						let symbol = external_symbol(target, &abi)?;
						return Ok(HirExpr::ExternCall {
							module,
							symbol,
							args: args
								.iter()
								.map(|arg| self.lower(&arg.value))
								.collect::<Result<_, _>>()?,
						});
					}
					if !self.record_call(target)? {
						self.record_unresolved_call(UnresolvedRuntimeCall::CallableValue(target.clone()));
					}
				} else {
					self.record_unresolved_call(UnresolvedRuntimeCall::DynamicCallee);
				}
				HirExpr::Call {
					callee: Box::new(self.lower(func)?),
					args: args
						.iter()
						.map(|arg| self.lower(&arg.value))
						.collect::<Result<_, _>>()?,
				}
			}
			StableExprKind::BinaryOp { lhs, op, rhs } => {
				if self.definitely_transfers(lhs) {
					return self.lower(lhs);
				}
				if !matches!(op, BinaryOperator::BoolAnd | BinaryOperator::BoolOr)
					&& self.definitely_transfers(rhs)
				{
					return Ok(HirExpr::Block {
						stmts: vec![HirStmt::Expr(self.lower(lhs)?)],
						tail: Some(Box::new(self.lower(rhs)?)),
					});
				}
				if *op == BinaryOperator::Pipe {
					if let Some(target) = self.target(rhs) {
						if !self.record_call(target)? {
							self.record_unresolved_call(UnresolvedRuntimeCall::CallableValue(target.clone()));
						}
					} else {
						self.record_unresolved_call(UnresolvedRuntimeCall::DynamicCallee);
					}
					self.scopes.borrow_mut().push(HashMap::new());
					let lhs_name = self.declare(&EcoString::from("$pipe"));
					let lhs = self.lower(lhs)?;
					let rhs = self.lower(rhs)?;
					self.scopes.borrow_mut().pop();
					return Ok(HirExpr::Block {
						stmts: vec![HirStmt::Let {
							name: lhs_name.clone(),
							mutable: false,
							value: lhs,
						}],
						tail: Some(Box::new(HirExpr::Call {
							callee: Box::new(rhs),
							args: vec![HirExpr::Local(lhs_name)],
						})),
					});
				}
				if matches!(op, BinaryOperator::Equals | BinaryOperator::NotEquals)
					&& matches!(
						peel_mut(&self.ty(lhs)?),
						InterfaceType::Named { .. } | InterfaceType::Generic(_)
					) {
					return Ok(HirExpr::Binary {
						op: binop(*op).expect("equality operators have builtin identity HIR"),
						result: BuiltinResult::IdentityBoolean,
						lhs: Box::new(self.lower(lhs)?),
						rhs: Box::new(self.lower(rhs)?),
					});
				}
				let dispatch = self.dispatch(expr)?;
				if matches!(op, BinaryOperator::In | BinaryOperator::NotIn) {
					self.scopes.borrow_mut().push(HashMap::new());
					let lhs_name = self.declare(&EcoString::from("$member"));
					let lhs_value = self.lower(lhs)?;
					let mut operation = self.lower_dispatch(dispatch, rhs, vec![lhs])?;
					Self::replace_dispatch_argument(&mut operation, HirExpr::Local(lhs_name.clone()));
					self.scopes.borrow_mut().pop();
					return Ok(HirExpr::Block {
						stmts: vec![HirStmt::Let {
							name: lhs_name,
							mutable: false,
							value: lhs_value,
						}],
						tail: Some(Box::new(operation)),
					});
				}
				if !matches!(dispatch, crate::StableDispatch::Builtin { .. }) {
					let (receiver, argument) = if matches!(op, BinaryOperator::In | BinaryOperator::NotIn) {
						(rhs.as_ref(), lhs.as_ref())
					} else {
						(lhs.as_ref(), rhs.as_ref())
					};
					return self.lower_dispatch(dispatch, receiver, vec![argument]);
				}
				HirExpr::Binary {
					op: binop(*op).ok_or_else(|| self.unsupported(expr, "protocol binary operator"))?,
					result: self.builtin_result(expr)?,
					lhs: Box::new(self.lower(lhs)?),
					rhs: Box::new(self.lower(rhs)?),
				}
			}
			StableExprKind::PrefixOp { op, value } => {
				if self.definitely_transfers(value) {
					return self.lower(value);
				}
				let dispatch = self.dispatch(expr)?;
				if !matches!(dispatch, crate::StableDispatch::Builtin { .. }) {
					return self.lower_dispatch(dispatch, value, vec![]);
				}
				HirExpr::Unary {
					op: match op {
						PrefixOperator::Negate => UnOp::Neg,
						PrefixOperator::BoolNot => UnOp::Not,
						PrefixOperator::BitNot => UnOp::BitNot,
					},
					result: self.builtin_result(expr)?,
					operand: Box::new(self.lower(value)?),
				}
			}
			StableExprKind::AssignOp {
				lhs,
				op: AssignOperator::Assign,
				rhs,
			} => HirExpr::Assign {
				target: Box::new(self.lower(lhs)?),
				value: Box::new(self.lower(rhs)?),
			},
			StableExprKind::AssignOp { lhs, op, rhs } => {
				if self.definitely_transfers(rhs) {
					return Ok(HirExpr::Assign {
						target: Box::new(self.lower(lhs)?),
						value: Box::new(self.lower(rhs)?),
					});
				}
				let binary =
					assign_binop(*op).ok_or_else(|| self.unsupported(expr, "assignment operator"))?;
				let value = match self.dispatch(expr)? {
					crate::StableDispatch::Builtin { .. } => HirExpr::Binary {
						op: binop(binary).unwrap(),
						result: self.builtin_result(lhs)?,
						lhs: Box::new(self.lower(lhs)?),
						rhs: Box::new(self.lower(rhs)?),
					},
					dispatch => self.lower_dispatch(dispatch, lhs, vec![rhs])?,
				};
				HirExpr::Assign {
					target: Box::new(self.lower(lhs)?),
					value: Box::new(value),
				}
			}
			StableExprKind::Block { body, label } => {
				if label.is_some() {
					let target = self.next_block_target.get();
					self.next_block_target.set(target + 1);
					self
						.block_targets
						.borrow_mut()
						.push((self.id(expr), target));
					let body = self.lower_block(body, true)?;
					self.block_targets.borrow_mut().pop();
					HirExpr::LabeledBlock {
						target,
						body: Box::new(body),
					}
				} else {
					self.lower_block(body, true)?
				}
			}
			StableExprKind::If {
				condition,
				then,
				otherwise,
			} => HirExpr::If {
				cond: Box::new(self.lower(condition)?),
				then: Box::new(self.lower_branch(then)?),
				otherwise: otherwise
					.as_ref()
					.map(|value| self.lower_branch(value).map(Box::new))
					.transpose()?,
			},
			StableExprKind::While {
				condition, body, ..
			} => {
				let target = self.next_loop();
				HirExpr::While {
					target,
					cond: Box::new(self.lower(condition)?),
					body: Box::new(self.lower_loop_branch(self.id(expr), target, body)?),
					continue_epilogue: None,
					option: self.loop_option(expr)?,
				}
			}
			StableExprKind::Closure { params, body, .. } => {
				self.scopes.borrow_mut().push(HashMap::new());
				let params = params
					.iter()
					.map(|param| pattern_name(&param.pattern).map(|name| self.declare(name)))
					.collect::<Result<_, _>>()?;
				let loop_depth = self.loop_depth.replace(0);
				let loop_targets = self.loop_targets.replace(Vec::new());
				let block_targets = self.block_targets.replace(Vec::new());
				let body = self.lower_deferred(|| self.lower_function_body(body));
				self.block_targets.replace(block_targets);
				self.loop_targets.replace(loop_targets);
				self.loop_depth.set(loop_depth);
				let body = body?;
				self.scopes.borrow_mut().pop();
				HirExpr::Closure {
					params,
					body: Box::new(body),
				}
			}
			StableExprKind::Return { value, .. } => HirExpr::Block {
				stmts: vec![HirStmt::Return {
					value: value.as_ref().map(|value| self.lower(value)).transpose()?,
					target: self.return_target(expr),
				}],
				tail: None,
			},
			StableExprKind::Break { value, .. } => HirExpr::Break {
				target: self
					.loop_targets
					.borrow()
					.iter()
					.rev()
					.find(|(source, _)| {
						self
							.annotations
							.control_targets
							.iter()
							.any(|(jump, target)| {
								*jump == self.id(expr)
									&& matches!(target, crate::runtime::RuntimeControlTarget::Loop(id) if id == source)
							})
					})
					.map(|(_, target)| *target)
					.ok_or_else(|| self.unsupported(expr, "break outside a loop"))?,
				value: Box::new(
					value
						.as_ref()
						.map(|value| self.lower(value))
						.transpose()?
						.unwrap_or(HirExpr::Array {
							kind: HirArrayKind::Tuple,
							items: vec![],
						}),
				),
			},
			StableExprKind::Continue { .. } => HirExpr::Continue {
				target: self
					.loop_targets
					.borrow()
					.iter()
					.rev()
					.find(|(source, _)| {
						self
							.annotations
							.control_targets
							.iter()
							.any(|(jump, target)| {
								*jump == self.id(expr)
									&& matches!(target, crate::runtime::RuntimeControlTarget::Loop(id) if id == source)
							})
					})
					.map(|(_, target)| *target)
					.ok_or_else(|| self.unsupported(expr, "continue outside a loop"))?,
			},
			StableExprKind::IndexAccess { parent, index, .. } if self.definitely_transfers(parent) => {
				self.lower(parent)?
			}
			StableExprKind::IndexAccess { parent, index, .. } if self.definitely_transfers(index) => {
				HirExpr::Block {
					stmts: vec![HirStmt::Expr(self.lower(parent)?)],
					tail: Some(Box::new(self.lower(index)?)),
				}
			}
			StableExprKind::IndexAccess { parent, index, .. } => match peel_mut(&self.ty(parent)?) {
				InterfaceType::Map(..) => HirExpr::MapGet {
					recv: Box::new(self.lower(parent)?),
					key: Box::new(self.lower(index)?),
				},
				InterfaceType::List(_) | InterfaceType::Tuple(_) => HirExpr::Index {
					recv: Box::new(self.lower(parent)?),
					index: Box::new(self.lower(index)?),
				},
				_ => self.lower_dispatch(self.dispatch(expr)?, parent, vec![index])?,
			},
			StableExprKind::For {
				variable,
				iterable,
				body,
				..
			} => self.lower_for(
				self.id(expr),
				variable,
				iterable,
				body,
				self.loop_option(expr)?,
			)?,
			StableExprKind::Range(range) => {
				let definition = self.target(expr).ok_or_else(|| {
					invalid(
						&self.artifact.definition,
						"range has no canonical nominal target",
					)
				})?;
				self.demand_external(definition)?;
				let fields = match range {
					StableRange::From(start) => vec![("start".into(), self.lower(start)?)],
					StableRange::To(end) | StableRange::ToInclusive(end) => {
						vec![("end".into(), self.lower(end)?)]
					}
					StableRange::Exclusive { min, max } | StableRange::Inclusive { min, max } => vec![
						("start".into(), self.lower(min)?),
						("end".into(), self.lower(max)?),
					],
				};
				HirExpr::New {
					class: self.context.binding_name(definition)?.as_str().into(),
					fields,
				}
			}
			StableExprKind::Match { value, arms } => HirExpr::Match {
				scrutinee: Box::new(self.lower(value)?),
				arms: arms
					.iter()
					.map(|arm| {
						self.scopes.borrow_mut().push(HashMap::new());
						let result = Ok(HirArm {
							pat: self.lower_pattern(&arm.pattern)?,
							guard: arm
								.guard
								.as_ref()
								.map(|guard| self.lower(guard))
								.transpose()?,
							body: self.lower(&arm.body)?,
						});
						self.scopes.borrow_mut().pop();
						result
					})
					.collect::<Result<_, StableLoweringError>>()?,
			},
			StableExprKind::PatternOp { lhs, op, rhs } => {
				let scrutinee = Box::new(self.lower(lhs)?);
				// Pattern-operator bindings are scoped to the test and must not replace
				// an outer source-name mapping used by following expressions. Lower the
				// source first to preserve source-order name allocation, matching the
				// compatibility lowering path.
				self.scopes.borrow_mut().push(HashMap::new());
				let pat = self.lower_pattern(rhs);
				self.scopes.borrow_mut().pop();
				let pat = pat?;
				let (yes, no) = if *op == PatternOperator::Is {
					(true, false)
				} else {
					(false, true)
				};
				HirExpr::Match {
					scrutinee,
					arms: vec![
						HirArm {
							pat,
							guard: None,
							body: HirExpr::Bool(yes),
						},
						HirArm {
							pat: HirPat::Wildcard,
							guard: None,
							body: HirExpr::Bool(no),
						},
					],
				}
			}
			StableExprKind::PostfixOp { value, .. } if self.definitely_transfers(value) => {
				self.lower(value)?
			}
			StableExprKind::PostfixOp { value, .. } => {
				self.lower_dispatch(self.dispatch(expr)?, value, vec![])?
			}
			StableExprKind::TypeOp { lhs, .. } if self.definitely_transfers(lhs) => self.lower(lhs)?,
			StableExprKind::TypeOp { lhs, .. } => match self.dispatch(expr)? {
				crate::StableDispatch::Builtin { .. } => self.lower_cast(expr, lhs)?,
				dispatch => self.lower_dispatch(dispatch, lhs, vec![])?,
			},
			StableExprKind::String(parts) => self.lower_string(parts)?,
		})
	}
	fn replace_dispatch_argument(expr: &mut HirExpr, argument: HirExpr) {
		match expr {
			HirExpr::Call { args, .. } | HirExpr::ExternCall { args, .. } => {
				*args.last_mut().expect("binary dispatch has an argument") = argument;
			}
			HirExpr::BoundDispatch {
				argument: found, ..
			} => **found = argument,
			HirExpr::Binary { rhs, .. } => **rhs = argument,
			other => panic!("binary dispatch lowered to an unexpected HIR shape: {other:?}"),
		}
	}
	fn lower_dispatch(
		&self,
		dispatch: &crate::StableDispatch,
		receiver: &StableExpr,
		arguments: Vec<&StableExpr>,
	) -> Result<HirExpr, StableLoweringError> {
		if let crate::StableDispatch::GenericBound { interface, member } = dispatch {
			if self
				.implementation_slots
				.is_none_or(|slots| slots.target(member).is_none())
			{
				return self.lower_generic_bound(interface, member, receiver, arguments);
			}
		}
		let (member, demand, external, implementation, persisted_marshal) = match dispatch {
			crate::StableDispatch::Builtin { method, .. } => {
				return Ok(HirExpr::Call {
					callee: Box::new(HirExpr::Field {
						recv: Box::new(self.lower(receiver)?),
						name: method.clone(),
					}),
					args: arguments
						.into_iter()
						.map(|arg| self.lower(arg))
						.collect::<Result<_, _>>()?,
				});
			}
			crate::StableDispatch::Direct {
				member,
				implementation,
				materialization,
			} => {
				if !matches!(
					&implementation.key,
					crate::DeclarationKey::Implementation { header, .. } if header.interface.is_some()
				) {
					if *materialization != crate::DispatchMaterialization::Attached {
						return Err(invalid(
							member,
							"direct dispatch materialization has drifted",
						));
					}
					let artifact = validate_direct_member(self.context, implementation, member)?;
					if matches!(artifact.payload, crate::RuntimePayload::External(_)) {
						return Err(invalid(
							member,
							"direct dispatch targets an external member",
						));
					}
					(
						member.clone(),
						Some(member.clone()),
						false,
						Some(implementation.clone()),
						None,
					)
				} else {
					let request = StableShapeRequest::Implementation(implementation.clone());
					let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
						return Err(StableShapeLookupError::WrongFact { request }.into());
					};
					let exact = exact_implementation_slot(&shape, member)
						.map(|slot| slot.member_id.clone())
						.ok_or_else(|| StableLoweringError::MissingImplementationSlot {
							implementation: implementation.clone(),
							member: member.clone(),
						})?;
					let external = matches!(
						self.context.runtime_definition(&exact)?.payload,
						crate::RuntimePayload::External(_)
					);
					(
						exact.clone(),
						Some(exact),
						external,
						Some(implementation.clone()),
						None,
					)
				}
			}
			crate::StableDispatch::SelectedImplementation {
				interface,
				member,
				implementation,
				materialization,
			} => {
				let request = StableShapeRequest::Implementation(implementation.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					member,
					crate::ImplementationMemberSource::Override,
					*materialization,
				)?;
				(
					member.clone(),
					Some(member.clone()),
					false,
					Some(implementation.clone()),
					None,
				)
			}
			crate::StableDispatch::InterfaceDefault {
				interface,
				member,
				implementation,
				materialization,
			} => {
				let request = StableShapeRequest::Implementation(implementation.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				let slot = validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					member,
					crate::ImplementationMemberSource::InheritedDefault,
					*materialization,
				)?;
				(
					slot.member_id.clone(),
					Some(slot.member_id.clone()),
					false,
					Some(implementation.clone()),
					None,
				)
			}
			crate::StableDispatch::GenericBound { interface, member } => {
				let slot = self
					.implementation_slots
					.and_then(|slots| slots.target(member))
					.ok_or_else(|| {
						invalid(
							&self.artifact.definition,
							"missing exact generic-bound slot",
						)
					})?;
				let request = StableShapeRequest::Implementation(slot.implementation_id.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				let materialization = if slot.external {
					crate::DispatchMaterialization::ExternalAbi
				} else if slot.source == crate::ImplementationMemberSource::InheritedDefault {
					crate::DispatchMaterialization::CanonicalBody
				} else {
					crate::DispatchMaterialization::Attached
				};
				validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					&slot.member_id,
					slot.source,
					materialization,
				)?;
				let external = matches!(
					self.context.runtime_definition(&slot.member_id)?.payload,
					crate::RuntimePayload::External(_)
				);
				(
					slot.member_id.clone(),
					Some(slot.member_id.clone()),
					external,
					Some(slot.implementation_id.clone()),
					None,
				)
			}
			crate::StableDispatch::External {
				member,
				implementation,
				marshal,
			} => {
				let exact = if !matches!(
					implementation.key,
					crate::DeclarationKey::Implementation { .. }
				) {
					let artifact = validate_direct_member(self.context, implementation, member)?;
					if !matches!(artifact.payload, crate::RuntimePayload::External(_)) {
						return Err(invalid(member, "external dispatch target is not external"));
					}
					member.clone()
				} else {
					let request = StableShapeRequest::Implementation(implementation.clone());
					let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
						return Err(StableShapeLookupError::WrongFact { request }.into());
					};
					if shape.interface.is_some() {
						exact_implementation_slot(&shape, member)
							.map(|slot| {
								validate_dispatch_slot(
									self.context,
									&shape,
									shape.interface.as_ref().unwrap(),
									&slot.member_id,
									slot.source,
									crate::DispatchMaterialization::ExternalAbi,
								)?;
								Ok::<_, StableLoweringError>(slot.member_id.clone())
							})
							.transpose()?
							.ok_or_else(|| StableLoweringError::MismatchedExternalMember {
								member: member.clone(),
								implementation: implementation.clone(),
							})?
					} else {
						validate_attached_member(self.context, &shape, member)?;
						member.clone()
					}
				};
				(
					exact.clone(),
					Some(exact),
					true,
					Some(implementation.clone()),
					*marshal,
				)
			}
		};
		let materialized_member = self
			.implementation_slots
			.and_then(|slots| slots.target(&member))
			.map(|slot| &slot.member_id);
		let member = materialized_member.cloned().unwrap_or(member);
		let demand = if materialized_member.is_some() {
			Some(member.clone())
		} else {
			demand
		};
		if let Some(demand) = demand {
			self.demand_direct(&demand);
		}
		let _ = self.record_call(&member)?;
		if external {
			let abi = exact_external_abi(self.context, &member, persisted_marshal)?;
			let mut args = vec![self.lower(receiver)?];
			args.extend(
				arguments
					.into_iter()
					.map(|arg| self.lower(arg))
					.collect::<Result<Vec<_>, _>>()?,
			);
			return match abi.callable {
				crate::ExternalCallable::Linked { module, symbol } => Ok(HirExpr::ExternCall {
					module: Box::leak(module.to_string().into_boxed_str()),
					symbol: Box::leak(symbol.to_string().into_boxed_str()),
					args,
				}),
				crate::ExternalCallable::Native(native) => match (native, args.as_slice()) {
					(nymph_hir::linkage::NativeExternal::Binary { op, result }, [lhs, rhs]) => {
						Ok(HirExpr::Binary {
							op,
							result,
							lhs: Box::new(lhs.clone()),
							rhs: Box::new(rhs.clone()),
						})
					}
					(nymph_hir::linkage::NativeExternal::Unary { op, result }, [operand]) => {
						Ok(HirExpr::Unary {
							op,
							result,
							operand: Box::new(operand.clone()),
						})
					}
					(nymph_hir::linkage::NativeExternal::Index, [receiver, index]) => Ok(HirExpr::Index {
						recv: Box::new(receiver.clone()),
						index: Box::new(index.clone()),
					}),
					_ => Err(invalid(
						&self.artifact.definition,
						"native external dispatch arity does not match its exact ABI",
					)),
				},
				crate::ExternalCallable::Deferred => Err(invalid(
					&self.artifact.definition,
					"external dispatch target is deferred",
				)),
			};
		}
		if let Some(implementation) = implementation
			&& shellless_implementation_member(self.context, &member, &implementation)?
		{
			let mut args = vec![self.lower(receiver)?];
			args.extend(
				arguments
					.into_iter()
					.map(|arg| self.lower(arg))
					.collect::<Result<Vec<_>, _>>()?,
			);
			return Ok(HirExpr::Call {
				callee: Box::new(HirExpr::Local(
					self.context.binding_name(&member)?.as_str().into(),
				)),
				args,
			});
		}
		let name: EcoString = self.context.member_name(&member)?.as_str().into();
		Ok(HirExpr::Call {
			callee: Box::new(HirExpr::Field {
				recv: Box::new(self.lower(receiver)?),
				name,
			}),
			args: arguments
				.into_iter()
				.map(|arg| self.lower(arg))
				.collect::<Result<_, _>>()?,
		})
	}

	fn lower_method_value(
		&self,
		expr: &StableExpr,
		dispatch: &crate::StableDispatch,
		receiver: &StableExpr,
	) -> Result<HirExpr, StableLoweringError> {
		let InterfaceType::Function { parameters, .. } = self.ty(expr)? else {
			return Err(invalid(
				&self.artifact.definition,
				"first-class method has no exact function type",
			));
		};
		let params = (0..parameters.len())
			.map(|index| EcoString::from(format!("$arg{index}")))
			.collect::<Vec<_>>();
		let receiver_name = EcoString::from("$receiver");
		let receiver_value = HirExpr::Local(receiver_name.clone());
		let arguments = params
			.iter()
			.cloned()
			.map(HirExpr::Local)
			.collect::<Vec<_>>();
		if let crate::StableDispatch::GenericBound { interface, member } = dispatch
			&& params.is_empty()
			&& self
				.implementation_slots
				.and_then(|slots| slots.target(member))
				.is_none()
		{
			let mut body = self.lower_generic_bound(interface, member, receiver, vec![])?;
			let HirExpr::UnaryBoundDispatch {
				receiver: dispatched_receiver,
				..
			} = &mut body
			else {
				return Err(invalid(
					&self.artifact.definition,
					"zero-argument generic method value did not lower to unary dispatch",
				));
			};
			*dispatched_receiver = Box::new(receiver_value);
			return Ok(HirExpr::Call {
				callee: Box::new(HirExpr::Closure {
					params: vec![receiver_name],
					body: Box::new(HirExpr::Closure {
						params,
						body: Box::new(body),
					}),
				}),
				args: vec![self.lower(receiver)?],
			});
		}
		let (member, implementation, external) = match dispatch {
			crate::StableDispatch::Direct {
				member,
				implementation,
				materialization,
			} => {
				if *materialization != crate::DispatchMaterialization::Attached {
					return Err(invalid(
						member,
						"direct method-value materialization has drifted",
					));
				}
				let artifact = validate_direct_member(self.context, implementation, member)?;
				if matches!(artifact.payload, crate::RuntimePayload::External(_)) {
					return Err(invalid(
						member,
						"direct method value targets an external member",
					));
				}
				(member.clone(), Some(implementation.clone()), false)
			}
			crate::StableDispatch::SelectedImplementation {
				interface,
				member,
				implementation,
				materialization,
			} => {
				let request = StableShapeRequest::Implementation(implementation.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					member,
					crate::ImplementationMemberSource::Override,
					*materialization,
				)?;
				(member.clone(), Some(implementation.clone()), false)
			}
			crate::StableDispatch::InterfaceDefault {
				interface,
				member,
				implementation,
				materialization,
			} => {
				let request = StableShapeRequest::Implementation(implementation.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				let slot = validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					member,
					crate::ImplementationMemberSource::InheritedDefault,
					*materialization,
				)?;
				(slot.member_id.clone(), Some(implementation.clone()), false)
			}
			crate::StableDispatch::External {
				member,
				implementation,
				..
			} => {
				let exact = if !matches!(
					implementation.key,
					crate::DeclarationKey::Implementation { .. }
				) {
					let artifact = validate_direct_member(self.context, implementation, member)?;
					if !matches!(artifact.payload, crate::RuntimePayload::External(_)) {
						return Err(invalid(
							member,
							"external method-value target is not external",
						));
					}
					member.clone()
				} else {
					let request = StableShapeRequest::Implementation(implementation.clone());
					let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
						return Err(StableShapeLookupError::WrongFact { request }.into());
					};
					if shape.interface.is_some() {
						exact_implementation_slot(&shape, member)
							.map(|slot| {
								validate_dispatch_slot(
									self.context,
									&shape,
									shape.interface.as_ref().unwrap(),
									&slot.member_id,
									slot.source,
									crate::DispatchMaterialization::ExternalAbi,
								)?;
								Ok::<_, StableLoweringError>(slot.member_id.clone())
							})
							.transpose()?
							.ok_or_else(|| StableLoweringError::MismatchedExternalMember {
								member: member.clone(),
								implementation: implementation.clone(),
							})?
					} else {
						validate_attached_member(self.context, &shape, member)?;
						member.clone()
					}
				};
				(exact, Some(implementation.clone()), true)
			}
			crate::StableDispatch::GenericBound { interface, member } => {
				if let Some(slot) = self
					.implementation_slots
					.and_then(|slots| slots.target(member))
				{
					let request = StableShapeRequest::Implementation(slot.implementation_id.clone());
					let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
						return Err(StableShapeLookupError::WrongFact { request }.into());
					};
					let materialization = if slot.external {
						crate::DispatchMaterialization::ExternalAbi
					} else if slot.source == crate::ImplementationMemberSource::InheritedDefault {
						crate::DispatchMaterialization::CanonicalBody
					} else {
						crate::DispatchMaterialization::Attached
					};
					validate_dispatch_slot(
						self.context,
						&shape,
						interface,
						&slot.member_id,
						slot.source,
						materialization,
					)?;
					(
						slot.member_id.clone(),
						Some(slot.implementation_id.clone()),
						matches!(
							self.context.runtime_definition(&slot.member_id)?.payload,
							crate::RuntimePayload::External(_)
						),
					)
				} else {
					(member.clone(), None, false)
				}
			}
			crate::StableDispatch::Builtin { .. } => {
				return Err(invalid(
					&self.artifact.definition,
					"builtin dispatch cannot be used as a first-class declared method",
				));
			}
		};
		if implementation.is_some() {
			self.demand_direct(&member);
		}
		let body = if external {
			let persisted_marshal = match dispatch {
				crate::StableDispatch::External { marshal, .. } => *marshal,
				_ => None,
			};
			let abi = exact_external_abi(self.context, &member, persisted_marshal)?;
			let mut args = vec![receiver_value];
			args.extend(arguments);
			external_call_expr(&member, &abi, &args)?
		} else if let Some(implementation) = &implementation
			&& shellless_implementation_member(self.context, &member, implementation)?
		{
			let mut args = vec![receiver_value];
			args.extend(arguments);
			HirExpr::Call {
				callee: Box::new(HirExpr::Local(
					self.context.binding_name(&member)?.as_str().into(),
				)),
				args,
			}
		} else {
			HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(receiver_value),
					name: self.context.member_name(&member)?.as_str().into(),
				}),
				args: arguments,
			}
		};
		Ok(HirExpr::Call {
			callee: Box::new(HirExpr::Closure {
				params: vec![receiver_name],
				body: Box::new(HirExpr::Closure {
					params,
					body: Box::new(body),
				}),
			}),
			args: vec![self.lower(receiver)?],
		})
	}

	fn lower_generic_bound(
		&self,
		interface: &DefinitionId,
		member: &DefinitionId,
		receiver: &StableExpr,
		arguments: Vec<&StableExpr>,
	) -> Result<HirExpr, StableLoweringError> {
		let interface_request = StableShapeRequest::InterfaceShell(interface.clone());
		let StableShapeFact::InterfaceShell(interface_shape) =
			self.context.stable_shape(&interface_request)?
		else {
			return Err(
				StableShapeLookupError::WrongFact {
					request: interface_request,
				}
				.into(),
			);
		};
		if !interface_shape
			.members
			.iter()
			.any(|shape| shape.id == *member)
		{
			return Err(StableLoweringError::MissingInterfaceMember {
				interface: interface.clone(),
				member: member.clone(),
			});
		}
		let method: EcoString = self.context.member_name(member)?.as_str().into();
		if arguments.len() > 1 {
			self.record_unresolved_call(UnresolvedRuntimeCall::GenericDispatch {
				interface: interface.clone(),
				member: member.clone(),
			});
			return Ok(HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(self.lower(receiver)?),
					name: method,
				}),
				args: arguments
					.into_iter()
					.map(|argument| self.lower(argument))
					.collect::<Result<_, _>>()?,
			});
		}

		let request = StableShapeRequest::ImplementationsForInterface(interface.clone());
		let StableShapeFact::Implementations(implementations) = self.context.stable_shape(&request)?
		else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		let interface_member = interface_shape
			.members
			.iter()
			.find(|shape| shape.id == *member)
			.ok_or_else(|| StableLoweringError::MissingInterfaceMember {
				interface: interface.clone(),
				member: member.clone(),
			})?;
		let mut cases = Vec::new();
		let mut has_nominal_fallback = false;
		for implementation in implementations {
			if implementation.interface.as_ref() != Some(interface) {
				return Err(invalid(
					&self.artifact.definition,
					"generic dispatch implementation belongs to another interface",
				));
			}
			let Some(slot) = implementation.member_slots.target(member) else {
				let declares_override = implementation.members.iter().any(|candidate| {
					candidate.name == interface_member.name && candidate.kind == interface_member.kind
				});
				if interface_member.has_default || declares_override {
					return Err(StableLoweringError::MissingImplementationSlot {
						implementation: implementation.id.clone(),
						member: member.clone(),
					});
				}
				continue;
			};
			let materialization = if slot.external {
				crate::DispatchMaterialization::ExternalAbi
			} else if slot.source == crate::ImplementationMemberSource::InheritedDefault {
				crate::DispatchMaterialization::CanonicalBody
			} else {
				crate::DispatchMaterialization::Attached
			};
			validate_dispatch_slot(
				self.context,
				&implementation,
				interface,
				&slot.member_id,
				slot.source,
				materialization,
			)?;
			if slot.external {
				exact_external_abi(self.context, &slot.member_id, None)?;
			}
			let Some(receiver_tag) = stable_runtime_tag(&implementation.self_type) else {
				has_nominal_fallback = true;
				continue;
			};
			let argument_tag = if arguments.is_empty() {
				receiver_tag.clone()
			} else {
				let parameter_type = interface_member.parameters.first().ok_or_else(|| {
					invalid(
						&self.artifact.definition,
						"single-argument generic dispatch member has no exact parameter shape",
					)
				})?;
				let argument_type = match &parameter_type.ty {
					InterfaceType::Generic(parameter) => implementation
						.interface_argument_bindings
						.iter()
						.find(|(candidate, _)| candidate == parameter)
						.map(|(_, ty)| ty.clone())
						.ok_or_else(|| {
							invalid(
								&implementation.id,
								"generic dispatch implementation is missing its exact interface argument binding",
							)
						})?,
					other => substitute_self_type(other, &implementation.self_type),
				};
				let Some(argument_tag) = stable_runtime_tag(&argument_type) else {
					continue;
				};
				argument_tag
			};
			if receiver_tag != argument_tag {
				continue;
			}
			let _ = self.record_call(&slot.member_id)?;
			let body = self.context.runtime_definition(&slot.member_id)?;
			let target = match &body.payload {
				crate::RuntimePayload::External(abi) => match &abi.callable {
					crate::ExternalCallable::Linked { module, symbol } => HirBoundDispatchTarget::Extern {
						module: Box::leak(module.to_string().into_boxed_str()),
						symbol: Box::leak(symbol.to_string().into_boxed_str()),
					},
					crate::ExternalCallable::Native(_) => {
						let module = self.context.module_specifier(&slot.member_id.module)?;
						let module = match module {
							CanonicalModuleSpecifier::Project(module)
							| CanonicalModuleSpecifier::Importable(module)
							| CanonicalModuleSpecifier::CompilerRuntime(module) => module,
						};
						HirBoundDispatchTarget::TopLevel {
							module,
							name: self.context.binding_name(&slot.member_id)?.as_str().into(),
						}
					}
					crate::ExternalCallable::Deferred => {
						return Err(invalid(
							&body.definition,
							"generic dispatch target is deferred",
						));
					}
				},
				crate::RuntimePayload::NymphBody(_) => {
					let module = self.context.module_specifier(&body.definition.module)?;
					let module = match module {
						CanonicalModuleSpecifier::Project(module)
						| CanonicalModuleSpecifier::Importable(module)
						| CanonicalModuleSpecifier::CompilerRuntime(module) => module,
					};
					HirBoundDispatchTarget::TopLevel {
						module,
						name: self.context.binding_name(&body.definition)?.as_str().into(),
					}
				}
				crate::RuntimePayload::MaterializedInterfaceMember { .. }
					if stable_runtime_tag(&implementation.self_type).is_some() =>
				{
					let module = self.context.module_specifier(&slot.member_id.module)?;
					let module = match module {
						CanonicalModuleSpecifier::Project(module)
						| CanonicalModuleSpecifier::Importable(module)
						| CanonicalModuleSpecifier::CompilerRuntime(module) => module,
					};
					HirBoundDispatchTarget::TopLevel {
						module,
						name: self.context.binding_name(&slot.member_id)?.as_str().into(),
					}
				}
				crate::RuntimePayload::MaterializedInterfaceMember { .. } => {
					return Err(invalid(
						&body.definition,
						"nominal materialized default cannot be a primitive generic dispatch case",
					));
				}
				_ => {
					return Err(invalid(
						&body.definition,
						"generic dispatch body is not callable",
					));
				}
			};
			if cases.iter().any(|case: &HirBoundDispatchCase| {
				case.receiver_tag == receiver_tag && case.argument_tag == argument_tag
			}) {
				return Err(StableLoweringError::AmbiguousDispatchCase {
					interface: interface.clone(),
					member: member.clone(),
					receiver_tag,
					argument_tag,
				});
			}
			self.demands.borrow_mut().insert(slot.member_id.clone());
			self
				.routed_demands
				.borrow_mut()
				.insert(slot.member_id.clone());
			cases.push(HirBoundDispatchCase {
				receiver_tag,
				argument_tag,
				target,
			});
		}
		cases.sort_by(|left, right| {
			(&left.receiver_tag, &left.argument_tag).cmp(&(&right.receiver_tag, &right.argument_tag))
		});
		if has_nominal_fallback {
			self.record_unresolved_call(UnresolvedRuntimeCall::GenericDispatch {
				interface: interface.clone(),
				member: member.clone(),
			});
		}
		if arguments.is_empty() {
			Ok(HirExpr::UnaryBoundDispatch {
				interface: interface_shape.name,
				method,
				receiver: Box::new(self.lower(receiver)?),
				cases,
			})
		} else {
			Ok(HirExpr::BoundDispatch {
				interface: interface_shape.name,
				method,
				receiver: Box::new(self.lower(receiver)?),
				argument: Box::new(self.lower(arguments[0])?),
				cases,
			})
		}
	}
	fn lower_spread(&self, value: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		match self.ty(value)? {
			InterfaceType::List(_) | InterfaceType::Tuple(_) => Ok(HirExpr::Field {
				recv: Box::new(self.lower(value)?),
				name: "v".into(),
			}),
			InterfaceType::Map(..) => self.lower(value),
			_ => {
				let iteration = self
					.annotations
					.iterations
					.iter()
					.find(|(id, _)| *id == self.id(value))
					.map(|(_, value)| value)
					.ok_or_else(|| StableLoweringError::MissingAnnotation {
						definition: self.artifact.definition.clone(),
						node: self.id(value),
						channel: "spread iteration".into(),
					})?;
				let source = self.lower(value)?;
				let (it, next, option) = match iteration {
					crate::RuntimeIteration::Direct {
						iterator_interface,
						next,
						option,
					} => {
						self.record_unresolved_call(UnresolvedRuntimeCall::IteratorNext {
							interface: iterator_interface.clone(),
							member: next.clone(),
						});
						(source, next, option)
					}
					crate::RuntimeIteration::ViaIter {
						iter,
						iterable_interface,
						iter_interface_member,
						iterator_interface,
						next,
						option,
					} => {
						self.demand_concrete_iteration_next(
							iter,
							iterable_interface,
							iter_interface_member,
							iterator_interface,
							next,
						)?;
						(self.lower_dispatch_value(iter, source)?, next, option)
					}
				};
				let next = self.context.member_name(next)?.as_str().into();
				Ok(drain_spread(
					it,
					next,
					self.context.binding_name(&option.option)?.as_str().into(),
					self.context.member_name(&option.some)?.as_str().into(),
					self
						.context
						.member_name(&option.some_value)?
						.as_str()
						.into(),
					self.context.member_name(&option.none)?.as_str().into(),
				))
			}
		}
	}
	fn lower_string(&self, parts: &[StableStringPart]) -> Result<HirExpr, StableLoweringError> {
		let mut result = vec![];
		let mut text = EcoString::new();
		let mut interpolated = false;
		for part in parts {
			match part {
				StableStringPart::Text(value) => text.push_str(value),
				StableStringPart::Escape(value) => text.push_str(&cooked_escape(*value)),
				StableStringPart::Expr(value) => {
					self.record_unresolved_call(UnresolvedRuntimeCall::DynamicCallee);
					interpolated = true;
					if !text.is_empty() {
						result.push(HirExpr::Str(std::mem::take(&mut text)));
					}
					result.push(HirExpr::ExternCall {
						module: "std/display",
						symbol: "display",
						args: vec![self.lower(value)?],
					});
				}
			}
		}
		if !interpolated {
			return Ok(HirExpr::Str(text));
		}
		if !text.is_empty() {
			result.push(HirExpr::Str(text));
		}
		Ok(HirExpr::InterpolatedString(result))
	}
	fn lower_cast(
		&self,
		expr: &StableExpr,
		lhs: &StableExpr,
	) -> Result<HirExpr, StableLoweringError> {
		let source_type = self.ty(lhs)?;
		let target_type = self.ty(expr)?;
		let source = peel_mut(&source_type);
		let target = peel_mut(&target_type);
		let operand = self.lower(lhs)?;
		let kind = match (source, target) {
			(InterfaceType::Int, InterfaceType::Int) => Some(ScalarCastKind::IdentityInt),
			(InterfaceType::UInt, InterfaceType::UInt) => Some(ScalarCastKind::IdentityUInt),
			(InterfaceType::Float, InterfaceType::Float) => Some(ScalarCastKind::IdentityFloat),
			(InterfaceType::Char, InterfaceType::Char) => Some(ScalarCastKind::IdentityChar),
			(InterfaceType::UInt, InterfaceType::Int) => Some(ScalarCastKind::ToInt),
			(InterfaceType::Int | InterfaceType::UInt, InterfaceType::Float) => {
				Some(ScalarCastKind::ToFloat)
			}
			(InterfaceType::Float, InterfaceType::Int) => Some(ScalarCastKind::SaturatingToInt),
			(InterfaceType::Float | InterfaceType::Int, InterfaceType::UInt) => {
				Some(ScalarCastKind::SaturatingToUInt)
			}
			(InterfaceType::Char, InterfaceType::Int) => Some(ScalarCastKind::CharToInt),
			(InterfaceType::Char, InterfaceType::UInt) => Some(ScalarCastKind::CharToUInt),
			(InterfaceType::Char, InterfaceType::Float) => Some(ScalarCastKind::CharToFloat),
			(InterfaceType::Int | InterfaceType::UInt, InterfaceType::Char) => {
				Some(ScalarCastKind::NumToChar)
			}
			(InterfaceType::Float, InterfaceType::Char) => Some(ScalarCastKind::FloatToChar),
			_ => None,
		};
		Ok(kind.map_or(operand.clone(), |kind| HirExpr::ScalarCast {
			kind,
			operand: Box::new(operand),
		}))
	}
	fn lower_pattern(&self, pattern: &StablePattern) -> Result<HirPat, StableLoweringError> {
		let id = Some(pattern.id);
		let variant = id.and_then(|id| {
			self
				.annotations
				.pattern_variants
				.iter()
				.find(|(found, _)| *found == id)
				.map(|(_, value)| value)
		});
		Ok(match &pattern.kind {
			StablePatternKind::Placeholder => HirPat::Wildcard,
			StablePatternKind::Int(v) => HirPat::Lit(HirLit::Num(*v as f64, NumKind::Int)),
			StablePatternKind::UInt(v) => HirPat::Lit(HirLit::Num(*v as f64, NumKind::UInt)),
			StablePatternKind::Float(v) => HirPat::Lit(HirLit::Num(v.into_inner(), NumKind::Float)),
			StablePatternKind::Boolean(v) => HirPat::Lit(HirLit::Bool(*v)),
			StablePatternKind::Char(v) => HirPat::Lit(HirLit::Char(*v)),
			StablePatternKind::String(parts) => HirPat::Lit(HirLit::Str(string_pattern(parts))),
			StablePatternKind::Grouped(inner) => self.lower_pattern(inner)?,
			StablePatternKind::Binding { name, inner } if variant.is_none() => HirPat::Binding {
				name: self.declare(&name),
				sub: (!matches!(inner.kind, StablePatternKind::Placeholder))
					.then(|| self.lower_pattern(inner).map(Box::new))
					.transpose()?,
			},
			StablePatternKind::Binding { .. } => self.variant_pattern(variant.unwrap(), vec![])?,
			StablePatternKind::Struct { fields, .. } => {
				let fields = self.lower_struct_pattern_fields(fields)?;
				if let Some(variant) = variant {
					self.variant_pattern(variant, fields)?
				} else {
					HirPat::Struct { fields }
				}
			}
			StablePatternKind::Tuple(items)
				if items
					.iter()
					.any(|item| matches!(item, StableListPatternEntry::Rest(_))) =>
			{
				self.lower_list_pattern(items, HirArrayKind::Tuple)?
			}
			StablePatternKind::Tuple(items) => HirPat::Tuple(
				items
					.iter()
					.map(|item| match item {
						StableListPatternEntry::Item(value) => self.lower_pattern(value),
						StableListPatternEntry::Rest(_) => unreachable!(),
					})
					.collect::<Result<_, _>>()?,
			),
			StablePatternKind::List(items) => self.lower_list_pattern(items, HirArrayKind::List)?,
			StablePatternKind::Map(items) => {
				let mut entries = vec![];
				let mut rest = None;
				for item in items.iter() {
					match item {
						StableMapPatternEntry::Entry(key, value) => {
							entries.push((literal_pattern(key)?, self.lower_pattern(value)?))
						}
						StableMapPatternEntry::Rest(name) => {
							rest = Some(name.as_ref().map(|name| self.declare(&name)))
						}
					}
				}
				HirPat::Map { entries, rest }
			}
			StablePatternKind::Range(range) => HirPat::Range(range_pattern(range)?),
			StablePatternKind::Union(left, right) => {
				self
					.pattern_declaration_records
					.borrow_mut()
					.push(HashMap::new());
				let left = self.lower_pattern(left);
				let bindings = self.pattern_declaration_records.borrow_mut().pop().unwrap();
				let left = left?;
				self.pattern_declaration_reuse.borrow_mut().push(bindings);
				let right = self.lower_pattern(right);
				self.pattern_declaration_reuse.borrow_mut().pop();
				HirPat::Or(Box::new(left), Box::new(right?))
			}
		})
	}
	fn lower_list_pattern(
		&self,
		items: &[StableListPatternEntry],
		kind: HirArrayKind,
	) -> Result<HirPat, StableLoweringError> {
		let mut prefix = vec![];
		let mut suffix = vec![];
		let mut rest = None;
		for item in items {
			match item {
				StableListPatternEntry::Item(value) => {
					if rest.is_none() {
						prefix.push(self.lower_pattern(value)?)
					} else {
						suffix.push(self.lower_pattern(value)?)
					}
				}
				StableListPatternEntry::Rest(name) => {
					if rest.is_some() {
						return Err(invalid(
							&self.artifact.definition,
							"list-shaped pattern has more than one rest entry",
						));
					}
					rest = Some(name.as_ref().map(|name| self.declare(&name)));
				}
			}
		}
		Ok(HirPat::List {
			kind,
			prefix,
			rest,
			suffix,
		})
	}
	fn lower_struct_pattern_fields(
		&self,
		fields: &[StableStructPatternField],
	) -> Result<Vec<(EcoString, HirPat)>, StableLoweringError> {
		let mut result = vec![];
		for field in fields {
			match field {
				StableStructPatternField::Value { name, value } => {
					result.push((name.clone(), self.lower_pattern(value)?))
				}
				StableStructPatternField::Named { id, name } => {
					let pattern = self
						.annotations
						.pattern_variants
						.iter()
						.find(|(found, _)| found == id)
						.map(|(_, variant)| self.variant_pattern(variant, vec![]))
						.transpose()?
						.unwrap_or_else(|| HirPat::Binding {
							name: self.declare(name),
							sub: None,
						});
					let exact = self
						.annotations
						.positional_fields
						.iter()
						.find(|(found, _)| found == id)
						.map(|(_, field)| field.name.clone())
						.unwrap_or_else(|| name.clone());
					result.push((exact, pattern));
				}
				StableStructPatternField::Positional { id: pid, pattern } => {
					let exact = self
						.annotations
						.positional_fields
						.iter()
						.find(|(id, _)| id == pid)
						.ok_or_else(|| StableLoweringError::MissingAnnotation {
							definition: self.artifact.definition.clone(),
							node: crate::BodyNodeId(pid.0),
							channel: "positional field".into(),
						})?
						.1
						.name
						.clone();
					result.push((exact, self.lower_pattern(pattern)?));
				}
				StableStructPatternField::Rest => {}
			}
		}
		Ok(result)
	}
	fn variant_pattern(
		&self,
		variant: &crate::PatternVariant,
		fields: Vec<(EcoString, HirPat)>,
	) -> Result<HirPat, StableLoweringError> {
		self.demand_external(&variant.enum_definition)?;
		Ok(HirPat::Variant {
			enum_name: self
				.context
				.binding_name(&variant.enum_definition)?
				.as_str()
				.into(),
			variant: self
				.context
				.member_name(&variant.variant_definition)?
				.as_str()
				.into(),
			fields,
		})
	}

	fn demand_external(&self, definition: &DefinitionId) -> Result<(), StableLoweringError> {
		if definition.module != self.artifact.definition.module {
			self.context.module_specifier(&definition.module)?;
		}
		if definition != &self.artifact.definition {
			self.demand_direct(definition);
		}
		Ok(())
	}
	fn demand_direct(&self, definition: &DefinitionId) {
		self.demands.borrow_mut().insert(definition.clone());
		self.direct_demands.borrow_mut().insert(definition.clone());
	}
	fn lower_for(
		&self,
		source_id: crate::BodyNodeId,
		variable: &StablePattern,
		iterable: &StableExpr,
		body: &StableExpr,
		result_option: Option<nymph_hir::hir::HirOptionAbi>,
	) -> Result<HirExpr, StableLoweringError> {
		if let StableExprKind::Range(
			StableRange::Exclusive { min, max } | StableRange::Inclusive { min, max },
		) = &iterable.kind
			&& (matches!(min.kind, StableExprKind::Int(_) | StableExprKind::UInt(_))
				|| matches!(
					self
						.annotations
						.types
						.iter()
						.find(|(id, _)| *id == self.id(min))
						.map(|(_, ty)| ty),
					Some(InterfaceType::Int | InterfaceType::UInt)
				)) {
			let inclusive = matches!(
				iterable.kind,
				StableExprKind::Range(StableRange::Inclusive { .. })
			);
			let min_type = self
				.annotations
				.types
				.iter()
				.find(|(id, _)| *id == self.id(min))
				.map(|(_, ty)| ty);
			let number = if matches!(min.kind, StableExprKind::UInt(_))
				|| matches!(min_type, Some(InterfaceType::UInt))
			{
				BuiltinResult::UInt
			} else {
				BuiltinResult::Int
			};
			self.scopes.borrow_mut().push(HashMap::new());
			let current = self.declare(&"$range_current".into());
			let end = self.declare(&"$range_end".into());
			let done = self.declare(&"$range_done".into());
			let pat = self.lower_pattern(variable)?;
			let target = self.next_loop();
			let body = self.lower_loop_branch(source_id, target, body)?;
			self.scopes.borrow_mut().pop();
			let current_expr = || HirExpr::Local(current.clone());
			let end_expr = || HirExpr::Local(end.clone());
			let advance = HirExpr::Assign {
				target: Box::new(current_expr()),
				value: Box::new(HirExpr::Binary {
					op: BinOp::Add,
					result: number,
					lhs: Box::new(current_expr()),
					rhs: Box::new(HirExpr::Num(
						1.0,
						if number == BuiltinResult::UInt {
							NumKind::UInt
						} else {
							NumKind::Int
						},
					)),
				}),
			};
			let upper_boundary = HirExpr::Binary {
				op: BinOp::Ge,
				result: BuiltinResult::Boolean,
				lhs: Box::new(current_expr()),
				rhs: Box::new(HirExpr::Num(
					9007199254740991.0,
					if number == BuiltinResult::UInt {
						NumKind::UInt
					} else {
						NumKind::Int
					},
				)),
			};
			let step_boundary = if number == BuiltinResult::Int {
				HirExpr::Binary {
					op: BinOp::Or,
					result: BuiltinResult::Boolean,
					lhs: Box::new(upper_boundary),
					rhs: Box::new(HirExpr::Binary {
						op: BinOp::Lt,
						result: BuiltinResult::Boolean,
						lhs: Box::new(current_expr()),
						rhs: Box::new(HirExpr::Num(-9007199254740991.0, NumKind::Int)),
					}),
				}
			} else {
				upper_boundary
			};
			let stop_after_current = if inclusive {
				HirExpr::Binary {
					op: BinOp::Or,
					result: BuiltinResult::Boolean,
					lhs: Box::new(HirExpr::Binary {
						op: BinOp::Eq,
						result: BuiltinResult::Boolean,
						lhs: Box::new(current_expr()),
						rhs: Box::new(end_expr()),
					}),
					rhs: Box::new(step_boundary),
				}
			} else {
				step_boundary
			};
			let update = HirExpr::If {
				cond: Box::new(stop_after_current),
				then: Box::new(HirExpr::Assign {
					target: Box::new(HirExpr::Local(done.clone())),
					value: Box::new(HirExpr::Bool(true)),
				}),
				otherwise: Some(Box::new(advance)),
			};
			let mut stmts = vec![
				HirStmt::Let {
					name: current.clone(),
					mutable: true,
					value: self.lower(min)?,
				},
				HirStmt::Let {
					name: end.clone(),
					mutable: false,
					value: self.lower(max)?,
				},
				HirStmt::Let {
					name: done.clone(),
					mutable: true,
					value: HirExpr::Bool(false),
				},
				HirStmt::Expr(HirExpr::While {
					target,
					cond: Box::new(HirExpr::Binary {
						op: BinOp::And,
						result: BuiltinResult::Boolean,
						lhs: Box::new(HirExpr::Unary {
							op: UnOp::Not,
							result: BuiltinResult::Boolean,
							operand: Box::new(HirExpr::Local(done)),
						}),
						rhs: Box::new(HirExpr::Binary {
							op: if inclusive { BinOp::Le } else { BinOp::Lt },
							result: BuiltinResult::Boolean,
							lhs: Box::new(current_expr()),
							rhs: Box::new(end_expr()),
						}),
					}),
					body: Box::new(HirExpr::Block {
						stmts: vec![
							HirStmt::Expr(HirExpr::Match {
								scrutinee: Box::new(current_expr()),
								arms: vec![HirArm {
									pat,
									guard: None,
									body,
								}],
							}),
							HirStmt::Expr(update.clone()),
						],
						tail: None,
					}),
					continue_epilogue: Some(Box::new(update)),
					option: result_option.clone(),
				}),
			];
			let tail = if result_option.is_some() {
				let HirStmt::Expr(loop_expr) = stmts.pop().expect("range loop expression") else {
					unreachable!()
				};
				Some(Box::new(loop_expr))
			} else {
				None
			};
			return Ok(HirExpr::Block { stmts, tail });
		}
		let native_range = false;
		let source = self.lower(iterable)?;
		let iteration = self
			.annotations
			.iterations
			.iter()
			.find(|(id, _)| *id == self.id(iterable))
			.map(|(_, value)| value)
			.ok_or_else(|| StableLoweringError::MissingAnnotation {
				definition: self.artifact.definition.clone(),
				node: self.id(iterable),
				channel: "iteration".into(),
			})?;
		let (it, next, option) = match iteration {
			crate::RuntimeIteration::Direct {
				iterator_interface,
				next,
				option,
			} => {
				if !native_range {
					self.record_unresolved_call(UnresolvedRuntimeCall::IteratorNext {
						interface: iterator_interface.clone(),
						member: next.clone(),
					});
				}
				let source = if native_range {
					HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(source),
							name: "iter".into(),
						}),
						args: vec![],
					}
				} else {
					source
				};
				(source, next, option)
			}
			crate::RuntimeIteration::ViaIter {
				iter,
				iterable_interface,
				iter_interface_member,
				iterator_interface,
				next,
				option,
				..
			} => {
				self.demand_concrete_iteration_next(
					iter,
					iterable_interface,
					iter_interface_member,
					iterator_interface,
					next,
				)?;
				let lowered = if matches!(peel_mut(&self.ty(iterable)?), InterfaceType::List(_)) {
					let request = StableShapeRequest::ImplementationsForInterface(iterable_interface.clone());
					let StableShapeFact::Implementations(implementations) =
						self.context.stable_shape(&request)?
					else {
						return Err(StableShapeLookupError::WrongFact { request }.into());
					};
					let implementation = implementations
						.into_iter()
						.find(|implementation| matches!(implementation.self_type, InterfaceType::List(_)))
						.ok_or_else(|| {
							invalid(
								&self.artifact.definition,
								"native List has no exact Iterable implementation",
							)
						})?;
					let member = &implementation
						.member_slots
						.target(iter_interface_member)
						.ok_or_else(|| {
							invalid(
								&self.artifact.definition,
								"native List Iterable implementation has no iter slot",
							)
						})?
						.member_id;
					let slot = implementation
						.member_slots
						.target(iter_interface_member)
						.unwrap();
					let materialization = if slot.external {
						crate::DispatchMaterialization::ExternalAbi
					} else if slot.source == crate::ImplementationMemberSource::InheritedDefault {
						crate::DispatchMaterialization::CanonicalBody
					} else {
						crate::DispatchMaterialization::Attached
					};
					validate_dispatch_slot(
						self.context,
						&implementation,
						iterable_interface,
						member,
						slot.source,
						materialization,
					)?;
					self.demand_external(member)?;
					let _ = self.record_call(member)?;
					HirExpr::Call {
						callee: Box::new(HirExpr::Local(
							self.context.binding_name(member)?.as_str().into(),
						)),
						args: vec![source],
					}
				} else {
					self.lower_dispatch_value(iter, source)?
				};
				(lowered, next, option)
			}
		};
		self.scopes.borrow_mut().push(HashMap::new());
		let it_name = self.declare(&"$it".into());
		let go = self.declare(&"$go".into());
		let pat = self.lower_pattern(variable)?;
		let target = self.next_loop();
		let body = self.lower_loop_branch(source_id, target, body)?;
		self.scopes.borrow_mut().pop();
		let call = HirExpr::Call {
			callee: Box::new(HirExpr::Field {
				recv: Box::new(HirExpr::Local(it_name.clone())),
				name: self.context.member_name(next)?.as_str().into(),
			}),
			args: vec![],
		};
		let option_name: EcoString = self.context.binding_name(&option.option)?.as_str().into();
		let some_name: EcoString = self.context.member_name(&option.some)?.as_str().into();
		let value_name: EcoString = self
			.context
			.member_name(&option.some_value)?
			.as_str()
			.into();
		let none_name: EcoString = self.context.member_name(&option.none)?.as_str().into();
		let option_valued = result_option.is_some();
		let mut stmts = vec![
			HirStmt::Let {
				name: it_name,
				mutable: false,
				value: it,
			},
			HirStmt::Let {
				name: go.clone(),
				mutable: true,
				value: HirExpr::Bool(true),
			},
			HirStmt::Expr(HirExpr::While {
				target,
				cond: Box::new(HirExpr::Local(go.clone())),
				body: Box::new(HirExpr::Match {
					scrutinee: Box::new(call),
					arms: vec![
						HirArm {
							pat: HirPat::Variant {
								enum_name: option_name.clone(),
								variant: some_name,
								fields: vec![(value_name, pat)],
							},
							guard: None,
							body,
						},
						HirArm {
							pat: HirPat::Variant {
								enum_name: option_name,
								variant: none_name,
								fields: vec![],
							},
							guard: None,
							body: HirExpr::Assign {
								target: Box::new(HirExpr::Local(go)),
								value: Box::new(HirExpr::Bool(false)),
							},
						},
					],
				}),
				continue_epilogue: None,
				option: result_option,
			}),
		];
		let tail = if option_valued {
			let HirStmt::Expr(loop_expr) = stmts.pop().expect("iterator loop expression") else {
				unreachable!()
			};
			Some(Box::new(loop_expr))
		} else {
			None
		};
		Ok(HirExpr::Block { stmts, tail })
	}

	fn demand_concrete_iteration_next(
		&self,
		iter: &crate::StableDispatch,
		iterable_interface: &DefinitionId,
		iter_interface_member: &DefinitionId,
		iterator_interface: &DefinitionId,
		next: &DefinitionId,
	) -> Result<(), StableLoweringError> {
		if let crate::StableDispatch::GenericBound { interface, member } = iter
			&& (interface != iterable_interface || member != iter_interface_member)
		{
			return Err(invalid(
				&self.artifact.definition,
				"generic-bound iteration identities disagree",
			));
		}
		let selected = match iter {
			crate::StableDispatch::SelectedImplementation { implementation, .. }
			| crate::StableDispatch::InterfaceDefault { implementation, .. }
			| crate::StableDispatch::Direct { implementation, .. } => Some(implementation.clone()),
			crate::StableDispatch::GenericBound { member, .. } => self
				.implementation_slots
				.and_then(|slots| slots.target(member))
				.map(|slot| slot.implementation_id.clone()),
			crate::StableDispatch::Builtin { .. } => {
				let request = StableShapeRequest::ImplementationsForInterface(iterable_interface.clone());
				let StableShapeFact::Implementations(implementations) =
					self.context.stable_shape(&request)?
				else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				implementations
					.into_iter()
					.find(|implementation| matches!(implementation.self_type, InterfaceType::List(_)))
					.map(|implementation| implementation.id)
			}
			_ => None,
		};
		let Some(implementation) = selected else {
			self.record_unresolved_call(UnresolvedRuntimeCall::IteratorNext {
				interface: iterator_interface.clone(),
				member: next.clone(),
			});
			return Ok(());
		};
		let request = StableShapeRequest::Implementation(implementation.clone());
		let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		let slot = shape
			.member_slots
			.target(iter_interface_member)
			.ok_or_else(|| StableLoweringError::MissingImplementationSlot {
				implementation: implementation.clone(),
				member: iter_interface_member.clone(),
			})?;
		match iter {
			crate::StableDispatch::SelectedImplementation {
				interface,
				materialization,
				..
			} => {
				validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					&slot.member_id,
					crate::ImplementationMemberSource::Override,
					*materialization,
				)?;
			}
			crate::StableDispatch::InterfaceDefault {
				interface,
				materialization,
				..
			} => {
				validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					&slot.member_id,
					crate::ImplementationMemberSource::InheritedDefault,
					*materialization,
				)?;
			}
			crate::StableDispatch::Builtin { .. } => {
				let materialization = if slot.external {
					crate::DispatchMaterialization::ExternalAbi
				} else if slot.source == crate::ImplementationMemberSource::InheritedDefault {
					crate::DispatchMaterialization::CanonicalBody
				} else {
					crate::DispatchMaterialization::Attached
				};
				validate_dispatch_slot(
					self.context,
					&shape,
					iterable_interface,
					&slot.member_id,
					slot.source,
					materialization,
				)?;
			}
			crate::StableDispatch::GenericBound { interface, member } => {
				let selected = self
					.implementation_slots
					.and_then(|slots| slots.target(member))
					.ok_or_else(|| {
						invalid(
							&self.artifact.definition,
							"missing exact generic-bound iteration slot",
						)
					})?;
				let materialization = if selected.external {
					crate::DispatchMaterialization::ExternalAbi
				} else if selected.source == crate::ImplementationMemberSource::InheritedDefault {
					crate::DispatchMaterialization::CanonicalBody
				} else {
					crate::DispatchMaterialization::Attached
				};
				validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					&slot.member_id,
					selected.source,
					materialization,
				)?;
			}
			_ => {}
		}
		let request = StableShapeRequest::Member(slot.member_id.clone());
		let StableShapeFact::Member(member) = self.context.stable_shape(&request)? else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		let concrete_iterator = match &member.return_type {
			InterfaceType::Named { definition, .. } if definition != iterator_interface => definition,
			_ => {
				self.record_unresolved_call(UnresolvedRuntimeCall::IteratorNext {
					interface: iterator_interface.clone(),
					member: next.clone(),
				});
				return Ok(());
			}
		};
		let request = StableShapeRequest::ImplementationsForInterface(iterator_interface.clone());
		let StableShapeFact::Implementations(implementations) = self.context.stable_shape(&request)?
		else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		let mut found = false;
		for implementation in &implementations {
			if !matches!(peel_mut(&implementation.self_type), InterfaceType::Named { definition, .. } if definition == concrete_iterator)
			{
				continue;
			}
			let Some(slot) = implementation.member_slots.target(next) else {
				continue;
			};
			let materialization = if slot.external {
				crate::DispatchMaterialization::ExternalAbi
			} else if slot.source == crate::ImplementationMemberSource::InheritedDefault {
				crate::DispatchMaterialization::CanonicalBody
			} else {
				crate::DispatchMaterialization::Attached
			};
			validate_dispatch_slot(
				self.context,
				implementation,
				iterator_interface,
				&slot.member_id,
				slot.source,
				materialization,
			)?;
			self.demand_external(&slot.member_id)?;
			let _ = self.record_call(&slot.member_id)?;
			found = true;
			break;
		}
		if !found {
			return Err(invalid(
				&self.artifact.definition,
				"native List iterator has no exact next implementation",
			));
		}
		Ok(())
	}

	fn lower_dispatch_value(
		&self,
		dispatch: &crate::StableDispatch,
		receiver: HirExpr,
	) -> Result<HirExpr, StableLoweringError> {
		let member = match dispatch {
			crate::StableDispatch::Direct { member, .. }
			| crate::StableDispatch::SelectedImplementation { member, .. }
			| crate::StableDispatch::InterfaceDefault { member, .. }
			| crate::StableDispatch::GenericBound { member, .. }
			| crate::StableDispatch::External { member, .. } => member,
			crate::StableDispatch::Builtin { method, .. } => {
				return Ok(HirExpr::Call {
					callee: Box::new(HirExpr::Field {
						recv: Box::new(receiver),
						name: method.clone(),
					}),
					args: vec![],
				});
			}
		};
		let concrete = match dispatch {
			crate::StableDispatch::Direct {
				implementation,
				materialization,
				..
			} => {
				if *materialization != crate::DispatchMaterialization::Attached {
					return Err(invalid(
						member,
						"direct iteration dispatch materialization has drifted",
					));
				}
				validate_direct_member(self.context, implementation, member)?;
				Some(member.clone())
			}
			crate::StableDispatch::SelectedImplementation {
				interface,
				implementation,
				materialization,
				..
			}
			| crate::StableDispatch::InterfaceDefault {
				interface,
				implementation,
				materialization,
				..
			} => {
				let request = StableShapeRequest::Implementation(implementation.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				let slot = exact_implementation_slot(&shape, member).ok_or_else(|| {
					StableLoweringError::MissingImplementationSlot {
						implementation: implementation.clone(),
						member: member.clone(),
					}
				})?;
				let source = if matches!(
					dispatch,
					crate::StableDispatch::SelectedImplementation { .. }
				) {
					crate::ImplementationMemberSource::Override
				} else {
					crate::ImplementationMemberSource::InheritedDefault
				};
				validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					&slot.member_id,
					source,
					*materialization,
				)?;
				Some(slot.member_id.clone())
			}
			crate::StableDispatch::GenericBound { interface, member } => {
				let Some(slot) = self
					.implementation_slots
					.and_then(|slots| slots.target(member))
				else {
					self.record_unresolved_call(UnresolvedRuntimeCall::GenericDispatch {
						interface: interface.clone(),
						member: member.clone(),
					});
					return Ok(HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(receiver),
							name: self.context.member_name(member)?.as_str().into(),
						}),
						args: vec![],
					});
				};
				let request = StableShapeRequest::Implementation(slot.implementation_id.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				let materialization = if slot.external {
					crate::DispatchMaterialization::ExternalAbi
				} else if slot.source == crate::ImplementationMemberSource::InheritedDefault {
					crate::DispatchMaterialization::CanonicalBody
				} else {
					crate::DispatchMaterialization::Attached
				};
				validate_dispatch_slot(
					self.context,
					&shape,
					interface,
					&slot.member_id,
					slot.source,
					materialization,
				)?;
				Some(slot.member_id.clone())
			}
			_ => None,
		};
		let target = concrete.as_ref().unwrap_or(member);
		if concrete.is_some() || !matches!(dispatch, crate::StableDispatch::GenericBound { .. }) {
			self.demand_external(target)?;
		}
		let _ = self.record_call(target)?;
		let shellless = match dispatch {
			crate::StableDispatch::Direct { implementation, .. }
			| crate::StableDispatch::SelectedImplementation { implementation, .. }
			| crate::StableDispatch::InterfaceDefault { implementation, .. } => {
				shellless_implementation_member(self.context, target, implementation)?
			}
			crate::StableDispatch::GenericBound { member, .. } => {
				if let Some(slot) = self
					.implementation_slots
					.and_then(|slots| slots.target(member))
				{
					shellless_implementation_member(self.context, target, &slot.implementation_id)?
				} else {
					false
				}
			}
			_ => false,
		};
		if shellless {
			return Ok(HirExpr::Call {
				callee: Box::new(HirExpr::Local(
					self.context.binding_name(target)?.as_str().into(),
				)),
				args: vec![receiver],
			});
		}
		Ok(HirExpr::Call {
			callee: Box::new(HirExpr::Field {
				recv: Box::new(receiver),
				name: self.context.member_name(target)?.as_str().into(),
			}),
			args: vec![],
		})
	}
	fn lower_block(
		&self,
		body: &[StableStatement],
		new_scope: bool,
	) -> Result<HirExpr, StableLoweringError> {
		if new_scope {
			self.scopes.borrow_mut().push(HashMap::new());
		}
		let mut stmts = vec![];
		let mut tail = None;
		for (index, statement) in body.iter().enumerate() {
			let last = index + 1 == body.len();
			match statement {
				StableStatement::Let {
					pattern,
					mutable,
					value,
				} => {
					let value = self.lower(value)?;
					let name = self.declare(pattern_name(pattern)?);
					stmts.push(HirStmt::Let {
						name,
						mutable: *mutable,
						value,
					});
				}
				StableStatement::Expr(expr) if matches!(expr.kind, StableExprKind::Return { .. }) => {
					let StableExprKind::Return { value, .. } = &expr.kind else {
						unreachable!()
					};
					stmts.push(HirStmt::Return {
						value: value.as_ref().map(|value| self.lower(value)).transpose()?,
						target: self.return_target(expr),
					});
				}
				StableStatement::Expr(expr) if last => {
					let lowered = self.lower(expr)?;
					tail = Some(Box::new(lowered));
				}
				StableStatement::Expr(expr) => stmts.push(HirStmt::Expr(self.lower(expr)?)),
			}
		}
		if new_scope {
			self.scopes.borrow_mut().pop();
		}
		Ok(HirExpr::Block { stmts, tail })
	}
}

fn cooked_escape(escape: nymph_ast::expr::StringEscape) -> String {
	escape
		.to_char()
		.map_or_else(|| "${".to_string(), |character| character.to_string())
}

fn binop(op: BinaryOperator) -> Option<BinOp> {
	Some(match op {
		BinaryOperator::Plus => BinOp::Add,
		BinaryOperator::Minus => BinOp::Sub,
		BinaryOperator::Times => BinOp::Mul,
		BinaryOperator::Divide => BinOp::Div,
		BinaryOperator::Remainder => BinOp::Rem,
		BinaryOperator::Power => BinOp::Pow,
		BinaryOperator::Equals => BinOp::Eq,
		BinaryOperator::NotEquals => BinOp::Ne,
		BinaryOperator::LessThan => BinOp::Lt,
		BinaryOperator::LessThanEquals => BinOp::Le,
		BinaryOperator::GreaterThan => BinOp::Gt,
		BinaryOperator::GreaterThanEquals => BinOp::Ge,
		BinaryOperator::BoolAnd => BinOp::And,
		BinaryOperator::BoolOr => BinOp::Or,
		BinaryOperator::BitAnd => BinOp::BitAnd,
		BinaryOperator::BitOr => BinOp::BitOr,
		BinaryOperator::BitXor => BinOp::BitXor,
		BinaryOperator::LeftShift => BinOp::Shl,
		BinaryOperator::RightShift => BinOp::Shr,
		_ => return None,
	})
}
fn assign_binop(op: AssignOperator) -> Option<BinaryOperator> {
	Some(match op {
		AssignOperator::PlusAssign => BinaryOperator::Plus,
		AssignOperator::MinusAssign => BinaryOperator::Minus,
		AssignOperator::TimesAssign => BinaryOperator::Times,
		AssignOperator::DivideAssign => BinaryOperator::Divide,
		AssignOperator::RemainderAssign => BinaryOperator::Remainder,
		AssignOperator::PowerAssign => BinaryOperator::Power,
		AssignOperator::BitAndAssign => BinaryOperator::BitAnd,
		AssignOperator::BitOrAssign => BinaryOperator::BitOr,
		AssignOperator::BitXorAssign => BinaryOperator::BitXor,
		AssignOperator::LeftShiftAssign => BinaryOperator::LeftShift,
		AssignOperator::RightShiftAssign => BinaryOperator::RightShift,
		_ => return None,
	})
}

fn drain_spread(
	iterator: HirExpr,
	next: EcoString,
	option: EcoString,
	some_name: EcoString,
	value_name: EcoString,
	none_name: EcoString,
) -> HirExpr {
	let acc: EcoString = "$acc".into();
	let it: EcoString = "$it".into();
	let go: EcoString = "$go".into();
	let value: EcoString = "$x".into();
	let next_call = HirExpr::Call {
		callee: Box::new(HirExpr::Field {
			recv: Box::new(HirExpr::Local(it.clone())),
			name: next,
		}),
		args: vec![],
	};
	let some = HirArm {
		pat: HirPat::Variant {
			enum_name: option.clone(),
			variant: some_name,
			fields: vec![(
				value_name,
				HirPat::Binding {
					name: value.clone(),
					sub: None,
				},
			)],
		},
		guard: None,
		body: HirExpr::Call {
			callee: Box::new(HirExpr::Field {
				recv: Box::new(HirExpr::Local(acc.clone())),
				name: "push".into(),
			}),
			args: vec![HirExpr::Local(value)],
		},
	};
	let none = HirArm {
		pat: HirPat::Variant {
			enum_name: option,
			variant: none_name,
			fields: vec![],
		},
		guard: None,
		body: HirExpr::Assign {
			target: Box::new(HirExpr::Local(go.clone())),
			value: Box::new(HirExpr::Bool(false)),
		},
	};
	HirExpr::Block {
		stmts: vec![
			HirStmt::Let {
				name: acc.clone(),
				mutable: false,
				value: HirExpr::Array {
					kind: HirArrayKind::Raw,
					items: vec![],
				},
			},
			HirStmt::Let {
				name: it,
				mutable: false,
				value: iterator,
			},
			HirStmt::Let {
				name: go.clone(),
				mutable: true,
				value: HirExpr::Bool(true),
			},
			HirStmt::Expr(HirExpr::While {
				target: u32::MAX,
				cond: Box::new(HirExpr::Local(go)),
				body: Box::new(HirExpr::Match {
					scrutinee: Box::new(next_call),
					arms: vec![some, none],
				}),
				continue_epilogue: None,
				option: None,
			}),
		],
		tail: Some(Box::new(HirExpr::Local(acc))),
	}
}
fn peel_mut(ty: &InterfaceType) -> &InterfaceType {
	if let InterfaceType::Mutable(inner) = ty {
		inner
	} else {
		ty
	}
}
fn string_pattern(parts: &[StableStringPatternPart]) -> EcoString {
	let mut result = EcoString::new();
	for part in parts {
		match part {
			StableStringPatternPart::Text(text) => result.push_str(text),
			StableStringPatternPart::Escape(escape) => result.push_str(&cooked_escape(*escape)),
		}
	}
	result
}
fn literal_pattern(pattern: &StablePattern) -> Result<HirLit, StableLoweringError> {
	Ok(match &pattern.kind {
		StablePatternKind::Int(v) => HirLit::Num(*v as f64, NumKind::Int),
		StablePatternKind::UInt(v) => HirLit::Num(*v as f64, NumKind::UInt),
		StablePatternKind::Float(v) => HirLit::Num(v.into_inner(), NumKind::Float),
		StablePatternKind::Boolean(v) => HirLit::Bool(*v),
		StablePatternKind::Char(v) => HirLit::Char(*v),
		StablePatternKind::String(parts) => HirLit::Str(string_pattern(parts)),
		_ => {
			return Err(StableLoweringError::Unsupported {
				definition: dummy_id(),
				node: None,
				feature: "non-literal map/range pattern".into(),
			});
		}
	})
}
fn range_pattern(range: &StablePatternRange) -> Result<HirRange, StableLoweringError> {
	Ok(match range {
		StablePatternRange::From(value) => HirRange::From(literal_pattern(value)?),
		StablePatternRange::To(value) => HirRange::To(literal_pattern(value)?),
		StablePatternRange::ToInclusive(value) => HirRange::ToInclusive(literal_pattern(value)?),
		StablePatternRange::Exclusive { min, max } => HirRange::Exclusive {
			min: literal_pattern(min)?,
			max: literal_pattern(max)?,
		},
		StablePatternRange::Inclusive { min, max } => HirRange::Inclusive {
			min: literal_pattern(min)?,
			max: literal_pattern(max)?,
		},
	})
}
