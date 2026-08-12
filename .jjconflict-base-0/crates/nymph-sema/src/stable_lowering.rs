//! Stable, semantic-only contracts for per-definition HIR lowering.
//!
//! Its inputs are exact stable identities and owned semantic artifacts, so lowering
//! does not need compiler queries, parser identities, source locations, or module ASTs.

// Stable lowering errors intentionally retain full stable identities by value.
// Boxing them would either alter the public error API or add pervasive allocation.
#![allow(clippy::result_large_err)]

use std::{
	cell::{Cell, RefCell},
	collections::{HashMap, HashSet},
	sync::Arc,
};

use ecow::EcoString;
use num_bigint::BigInt;
use nymph_ast::ops::{BinaryOperator, PatternOperator, PrefixOperator};
use nymph_hir::hir::{
	BinOp, BuiltinResult, HirArm, HirArrayElem, HirArrayKind, HirBoundDispatchCase,
	HirBoundDispatchTarget, HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirLit, HirMapElem,
	HirMethod, HirModule, HirPat, HirRange, HirStmt, HirVariant, NumKind, OperationMode,
	ScalarCastKind, UnOp,
};

use crate::{
	DefinitionId, EnumShell, ExportedDefinition, ExportedImpl, ExternalAbi, InterfaceType,
	MemberShape, ModuleIdentity, RuntimeDefinition, StableExpr, StableExprKind, StableListItem,
	StableListPatternEntry, StableMapEntry, StableMapPatternEntry, StableMatchArm, StablePattern,
	StablePatternKind, StablePatternRange, StableRange, StableStatement, StableStringPart,
	StableStringPatternPart, StableStructPatternField, StructShell,
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
	Definition(DefinitionId),
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
			Self::Definition(id)
			| Self::TypeShell(id)
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
	Definition(ExportedDefinition),
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
// Runtime type attachments directly own the HIR pieces consumed by codegen;
// boxing only this arm would complicate the public lowering contract.
#[allow(clippy::large_enum_variant)]
pub enum LoweredHirFragment {
	TopLevelFunction(HirFunc),
	TopLevelValue(HirLet),
	TopLevelExternal {
		name: EmittedBindingName,
		abi: ExternalAbi,
		function: Option<HirFunc>,
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
	RuntimeTypeAttachment {
		object: HirExpr,
		function: Option<HirFunc>,
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
	DuplicateRuntimeTypeAttachment {
		object: EcoString,
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
	invocation: Option<Box<RuntimeExecutionSummary>>,
	closures: Vec<RuntimeExecutionSummary>,
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
	pub fn invocation(&self) -> Option<&RuntimeExecutionSummary> {
		self.invocation.as_deref()
	}
	pub fn closures(&self) -> &[RuntimeExecutionSummary] {
		&self.closures
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
	pub fn invocation(&self) -> Option<&RuntimeExecutionSummary> {
		self.execution.invocation()
	}
	pub fn execution_summary(&self) -> &RuntimeExecutionSummary {
		&self.execution
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

/// Lowers one checked, location-free runtime artifact from stable semantic IDs;
/// source modules and location-based annotation or symbol tables are not inputs.
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
			let defaults = shell
				.defaults
				.iter()
				.map(|default| {
					let name = context.member_name(&default.field)?.as_str().into();
					let lowered = lower_body(
						context,
						&artifact,
						&default.body,
						&mut demands,
						&mut direct_demands,
						&mut routed_demands,
						&mut execution,
						None,
						None,
						None,
						&[],
					)?;
					let LoweredHirFragment::TopLevelValue(value) = lowered else {
						return Err(invalid(
							&definition,
							"struct field default did not lower to a value",
						));
					};
					Ok((name, value.value))
				})
				.collect::<Result<_, StableLoweringError>>()?;
			LoweredHirFragment::StructShell(HirClass {
				name: name.as_str().into(),
				fields,
				defaults,
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
				&[],
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
				&implementation_shape.interface_argument_bindings,
			)?;
			if matches!(
				lowered,
				LoweredHirFragment::TopLevelFunction(_) | LoweredHirFragment::RuntimeTypeAttachment { .. }
			) {
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
		return lower_top_level_external(context, definition, abi, None);
	};
	let (member, receiver_type) =
		if let Some((implementation, member)) = attached_implementation_member(context, artifact)? {
			(member, Some(implementation.self_type))
		} else if let Some(member) = attached_nominal_member(context, artifact, abi)? {
			(member, None)
		} else {
			return Err(invalid(
				definition,
				"external member has no exact checked member shape",
			));
		};
	if owner_has_no_attachment_shell(context, owner)? {
		return lower_shellless_external(context, definition, abi, &member, receiver_type.as_ref());
	}
	let name: EcoString = context.member_name(definition)?.as_str().into();
	match member.kind {
		crate::MemberKind::Value | crate::MemberKind::StaticValue => {
			let module = external_module(definition, abi)?;
			let symbol = external_symbol(definition, abi)?;
			let marshal =
				abi
					.marshal
					.result
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
		crate::MemberKind::Function | crate::MemberKind::StaticFunction => {
			if abi.marshal.parameters.len() != member.parameters.len() {
				return Err(invalid(
					definition,
					"external parameter marshalling plan does not match its exact ABI",
				));
			}
			let mut params = member
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
			let (_, hidden_arity, _) = external_callable_shape(context, definition, abi)?;
			params.extend((0..hidden_arity).map(|index| EcoString::from(format!("$type${index}"))));
			let mut args = if member.kind == crate::MemberKind::StaticFunction {
				Vec::new()
			} else {
				vec![HirExpr::This]
			};
			args.extend(params.iter().cloned().map(HirExpr::Local));
			let argument_marshals = external_argument_marshals(
				&abi.marshal.parameters,
				member.kind != crate::MemberKind::StaticFunction,
				receiver_type.as_ref(),
				hidden_arity,
			);
			let body = external_call_expr(
				definition,
				abi,
				&args,
				argument_marshals,
				abi.marshal.result,
			)?;
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
	receiver_type: Option<&InterfaceType>,
) -> Result<LoweredHirFragment, StableLoweringError> {
	match member.kind {
		crate::MemberKind::Function | crate::MemberKind::StaticFunction => {
			lower_top_level_external(context, definition, abi, receiver_type)
		}
		crate::MemberKind::Value | crate::MemberKind::StaticValue => {
			let value = HirExpr::ExternValue {
				module: external_module(definition, abi)?,
				symbol: external_symbol(definition, abi)?,
				marshal: abi
					.marshal
					.result
					.ok_or_else(|| StableLoweringError::MissingExternalMarshal {
						definition: definition.clone(),
					})?,
			};
			let name: EcoString = context.binding_name(definition)?.as_str().into();
			if member.kind == crate::MemberKind::StaticValue {
				Ok(LoweredHirFragment::TopLevelValue(HirLet { name, value }))
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
	argument_marshals: Vec<Option<nymph_hir::hir::MarshalKind>>,
	return_marshal: Option<nymph_hir::hir::MarshalKind>,
) -> Result<HirExpr, StableLoweringError> {
	match &abi.callable {
		crate::ExternalCallable::Linked { adapter } => {
			if argument_marshals.len() != args.len() {
				return Err(invalid(
					definition,
					"external argument marshalling plan does not match its exact ABI",
				));
			}
			if adapter.module == "std/collections/list" {
				let list = match (adapter.symbol.as_str(), args) {
					("appended", [recv, value]) => Some(HirExpr::ListAppend {
						recv: Box::new(recv.clone()),
						value: Box::new(value.clone()),
					}),
					("replaced", [recv, index, value]) => Some(HirExpr::ListReplace {
						recv: Box::new(recv.clone()),
						index: Box::new(index.clone()),
						value: Box::new(value.clone()),
					}),
					("slice", [recv, start, end]) => Some(HirExpr::ListSlice {
						recv: Box::new(recv.clone()),
						start: Box::new(start.clone()),
						end: Box::new(end.clone()),
					}),
					_ => None,
				};
				if let Some(list) = list {
					return Ok(list);
				}
			}
			Ok(HirExpr::ExternCall {
				module: Box::leak(adapter.module.to_string().into_boxed_str()),
				symbol: Box::leak(adapter.symbol.to_string().into_boxed_str()),
				args: args.to_vec(),
				call_mode: match abi.call_mode {
					crate::ExternalCallMode::Ordinary => nymph_hir::hir::ExternalCallMode::Ordinary,
					crate::ExternalCallMode::Cancellable => nymph_hir::hir::ExternalCallMode::Cancellable,
				},
				argument_marshals,
				return_marshal,
			})
		}
		crate::ExternalCallable::Native(native) => match (native, args) {
			(nymph_hir::linkage::NativeExternal::Binary { op, result }, [lhs, rhs]) => {
				Ok(HirExpr::Binary {
					op: *op,
					result: *result,
					mode: OperationMode::Checked,
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
				mode: OperationMode::Checked,
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

fn integer_marshal(ty: &InterfaceType) -> Option<nymph_hir::hir::MarshalKind> {
	match peel_mut(ty) {
		InterfaceType::Int => Some(nymph_hir::hir::MarshalKind::Int),
		InterfaceType::UInt => Some(nymph_hir::hir::MarshalKind::UInt),
		_ => None,
	}
}

fn external_argument_marshals(
	parameters: &[Option<nymph_hir::hir::MarshalKind>],
	has_receiver: bool,
	receiver: Option<&InterfaceType>,
	hidden_arity: usize,
) -> Vec<Option<nymph_hir::hir::MarshalKind>> {
	let mut marshals = Vec::new();
	if has_receiver {
		marshals.push(receiver.and_then(integer_marshal));
	}
	marshals.extend_from_slice(parameters);
	marshals.extend((0..hidden_arity).map(|_| None));
	marshals
}

fn lower_top_level_external(
	context: &impl StableLoweringContext,
	definition: &DefinitionId,
	abi: &crate::ExternalAbi,
	receiver_type: Option<&InterfaceType>,
) -> Result<LoweredHirFragment, StableLoweringError> {
	let name = context.binding_name(definition)?;
	if matches!(abi.callable, crate::ExternalCallable::Linked { .. }) {
		external_module(definition, abi)?;
		external_symbol(definition, abi)?;
	}
	let (is_value, has_receiver, parameters, _return_type) = match &definition.key {
		crate::DeclarationKey::TopLevel { category, .. } => match category {
			crate::DeclarationCategory::Let => (true, false, Vec::new(), None),
			crate::DeclarationCategory::Function => {
				let request = StableShapeRequest::Definition(definition.clone());
				let StableShapeFact::Definition(shape) = context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				if shape.id != *definition || shape.external.as_ref() != Some(abi) {
					return Err(invalid(
						definition,
						"top-level external disagrees with its exact checked definition shape",
					));
				}
				(false, false, shape.parameters, shape.return_type)
			}
			_ => {
				return Err(invalid(
					definition,
					"top-level external has a non-callable, non-value identity",
				));
			}
		},
		crate::DeclarationKey::Member { .. } => {
			let request = StableShapeRequest::Member(definition.clone());
			let StableShapeFact::Member(shape) = context.stable_shape(&request)? else {
				return Err(StableShapeLookupError::WrongFact { request }.into());
			};
			if shape.id != *definition || shape.external.as_ref() != Some(abi) {
				return Err(invalid(
					definition,
					"top-level external disagrees with its exact checked member shape",
				));
			}
			let (is_value, has_receiver) = match shape.kind {
				crate::MemberKind::Value | crate::MemberKind::StaticValue => (true, false),
				crate::MemberKind::Function => (false, true),
				crate::MemberKind::StaticFunction => (false, false),
			};
			(
				is_value,
				has_receiver,
				shape.parameters,
				Some(shape.return_type),
			)
		}
		_ => {
			return Err(invalid(
				definition,
				"top-level external has no exact checked definition or member shape",
			));
		}
	};
	if is_value {
		let module = external_module(definition, abi)?;
		let symbol = external_symbol(definition, abi)?;
		let marshal =
			abi
				.marshal
				.result
				.ok_or_else(|| StableLoweringError::MissingExternalMarshal {
					definition: definition.clone(),
				})?;
		return Ok(LoweredHirFragment::TopLevelValue(HirLet {
			name: name.as_str().into(),
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
			body: external_call_expr(definition, abi, &args, vec![None; args.len()], None)?,
		}));
	}
	if abi.marshal.parameters.len() != parameters.len() {
		return Err(invalid(
			definition,
			"external parameter marshalling plan does not match its exact ABI",
		));
	}
	let (_, hidden_arity, _) = external_callable_shape(context, definition, abi)?;
	let argument_marshals = external_argument_marshals(
		&abi.marshal.parameters,
		has_receiver,
		receiver_type,
		hidden_arity,
	);
	let return_marshal = abi.marshal.result;
	let activation_protocol_adapter = matches!(
		&abi.callable,
		crate::ExternalCallable::Linked { adapter }
			if matches!((adapter.module.as_str(), adapter.symbol.as_str()), ("std/io", "print" | "println"))
	);
	if activation_protocol_adapter
		|| argument_marshals.iter().any(Option::is_some)
		|| return_marshal.is_some()
	{
		let mut params = if has_receiver {
			vec![EcoString::from("$self")]
		} else {
			Vec::new()
		};
		params.extend(parameters.iter().enumerate().map(|(index, parameter)| {
			parameter
				.name
				.clone()
				.unwrap_or_else(|| format!("$arg{index}").into())
		}));
		params.extend((0..hidden_arity).map(|index| EcoString::from(format!("$type${index}"))));
		let args = params
			.iter()
			.cloned()
			.map(HirExpr::Local)
			.collect::<Vec<_>>();
		return Ok(LoweredHirFragment::TopLevelExternal {
			name: name.clone(),
			abi: abi.clone(),
			function: Some(HirFunc {
				name: name.as_str().into(),
				params,
				body: external_call_expr(definition, abi, &args, argument_marshals, return_marshal)?,
			}),
		});
	}
	external_module(definition, abi)?;
	external_symbol(definition, abi)?;
	Ok(LoweredHirFragment::TopLevelExternal {
		name,
		abi: abi.clone(),
		function: None,
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
		| Fragment::EnumShell(_)
		| Fragment::RuntimeTypeAttachment { .. } => {
			Ok(RuntimeAssemblyPlacement::Module(definition.module.clone()))
		}
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
		match shape_abi.marshal.result {
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

fn external_callable_shape(
	context: &impl StableLoweringContext,
	definition: &DefinitionId,
	abi: &ExternalAbi,
) -> Result<(usize, usize, usize), StableLoweringError> {
	match &definition.key {
		crate::DeclarationKey::TopLevel {
			category: crate::DeclarationCategory::Function,
			..
		} => {
			let request = StableShapeRequest::Definition(definition.clone());
			let StableShapeFact::Definition(shape) = context.stable_shape(&request)? else {
				return Err(StableShapeLookupError::WrongFact { request }.into());
			};
			if shape.id != *definition
				|| shape.kind != crate::DefinitionShapeKind::Function
				|| shape.external.as_ref() != Some(abi)
			{
				return Err(invalid(
					definition,
					"generic external callable disagrees with its exact definition shape",
				));
			}
			Ok((
				shape.parameters.len(),
				shape
					.binders
					.iter()
					.filter(|binder| binder.kind == crate::GenericParameterKind::Type)
					.count(),
				0,
			))
		}
		crate::DeclarationKey::Member { .. } => {
			let request = StableShapeRequest::Member(definition.clone());
			let StableShapeFact::Member(shape) = context.stable_shape(&request)? else {
				return Err(StableShapeLookupError::WrongFact { request }.into());
			};
			if shape.id != *definition
				|| !matches!(
					shape.kind,
					crate::MemberKind::Function | crate::MemberKind::StaticFunction
				) || shape.external.as_ref() != Some(abi)
			{
				return Err(invalid(
					definition,
					"generic external callable disagrees with its exact member shape",
				));
			}
			let owner_binders = if shape.kind == crate::MemberKind::StaticFunction {
				let owner = shape.runtime_owner.as_ref().ok_or_else(|| {
					invalid(
						definition,
						"generic static external callable has no exact runtime owner",
					)
				})?;
				match &owner.key {
					crate::DeclarationKey::Implementation { .. } => {
						let request = StableShapeRequest::Implementation(owner.clone());
						let StableShapeFact::Implementation(owner_shape) = context.stable_shape(&request)?
						else {
							return Err(StableShapeLookupError::WrongFact { request }.into());
						};
						if owner_shape.id != *owner {
							return Err(invalid(
								definition,
								"generic static external owner disagrees with its exact implementation shape",
							));
						}
						owner_shape
							.binders
							.iter()
							.filter(|binder| binder.kind == crate::GenericParameterKind::Type)
							.count()
					}
					_ => {
						let request = StableShapeRequest::Definition(owner.clone());
						let StableShapeFact::Definition(owner_shape) = context.stable_shape(&request)? else {
							return Err(StableShapeLookupError::WrongFact { request }.into());
						};
						if owner_shape.id != *owner {
							return Err(invalid(
								definition,
								"generic static external owner disagrees with its exact definition shape",
							));
						}
						owner_shape
							.binders
							.iter()
							.filter(|binder| binder.kind == crate::GenericParameterKind::Type)
							.count()
					}
				}
			} else {
				0
			};
			let receiver_arity = usize::from(matches!(shape.kind, crate::MemberKind::Function));
			Ok((
				shape.parameters.len(),
				owner_binders
					+ shape
						.binders
						.iter()
						.filter(|binder| binder.kind == crate::GenericParameterKind::Type)
						.count(),
				receiver_arity,
			))
		}
		_ => Err(invalid(
			definition,
			"generic external callable has no callable definition shape",
		)),
	}
}

fn external_module(
	definition: &DefinitionId,
	abi: &ExternalAbi,
) -> Result<&'static str, StableLoweringError> {
	abi
		.adapter()
		.map(|adapter| &adapter.module)
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
		.adapter()
		.map(|adapter| &adapter.symbol)
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
		InterfaceType::List(_) => "list",
		InterfaceType::Tuple(_) => "tuple",
		InterfaceType::Map(..) => "map",
		_ => return None,
	};
	Some(EcoString::from(format!("nymph.{tag}")))
}

fn primitive_box_binding(tag: EcoString) -> Option<&'static str> {
	match tag.as_str().strip_prefix("nymph.").unwrap_or(tag.as_str()) {
		"int" => Some("NInt"),
		"uint" => Some("NUint"),
		"float" => Some("NFloat"),
		"char" => Some("NChar"),
		"bool" | "boolean" => Some("NBool"),
		"string" => Some("NString"),
		"list" => Some("NList"),
		"tuple" => Some("NTuple"),
		"map" => Some("NMap"),
		_ => None,
	}
}

fn is_concrete_runtime_type(ty: &InterfaceType) -> bool {
	match ty {
		InterfaceType::Generic(_) => false,
		InterfaceType::List(argument) => is_concrete_runtime_type(argument),
		InterfaceType::Tuple(arguments) => arguments.iter().all(is_concrete_runtime_type),
		InterfaceType::Map(key, value) => {
			is_concrete_runtime_type(key) && is_concrete_runtime_type(value)
		}
		InterfaceType::Named {
			positional, named, ..
		} => {
			positional.iter().all(is_concrete_runtime_type)
				&& named
					.iter()
					.all(|(_, argument)| is_concrete_runtime_type(argument))
		}
		_ => true,
	}
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
			effects,
		} => InterfaceType::Function {
			parameters: parameters
				.iter()
				.map(|parameter| substitute_self_type(parameter, self_type))
				.collect(),
			return_type: Box::new(substitute_self_type(return_type, self_type)),
			effects: effects.clone(),
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
		_ => ty.clone(),
	}
}

fn owner_parameter_path(
	type_: &InterfaceType,
	parameter: &crate::GenericParameterId,
) -> Option<Vec<usize>> {
	fn visit(
		type_: &InterfaceType,
		parameter: &crate::GenericParameterId,
		path: &mut Vec<usize>,
	) -> bool {
		match type_ {
			InterfaceType::Generic(candidate) => candidate == parameter,
			InterfaceType::List(argument) => {
				path.push(0);
				if visit(argument, parameter, path) {
					true
				} else {
					path.pop();
					false
				}
			}
			InterfaceType::Tuple(arguments) => arguments.iter().enumerate().any(|(index, argument)| {
				path.push(index);
				if visit(argument, parameter, path) {
					true
				} else {
					path.pop();
					false
				}
			}),
			InterfaceType::Map(key, value) => {
				[key.as_ref(), value.as_ref()]
					.into_iter()
					.enumerate()
					.any(|(index, argument)| {
						path.push(index);
						if visit(argument, parameter, path) {
							true
						} else {
							path.pop();
							false
						}
					})
			}
			InterfaceType::Named {
				positional, named, ..
			} => positional
				.iter()
				.chain(named.iter().map(|(_, argument)| argument))
				.enumerate()
				.any(|(index, argument)| {
					path.push(index);
					if visit(argument, parameter, path) {
						true
					} else {
						path.pop();
						false
					}
				}),
			_ => false,
		}
	}
	let mut path = Vec::new();
	visit(type_, parameter, &mut path).then_some(path)
}

fn substitute_type_parameters(
	ty: &InterfaceType,
	substitutions: &[(crate::GenericParameterId, InterfaceType)],
) -> InterfaceType {
	match ty {
		InterfaceType::Generic(parameter) => substitutions
			.iter()
			.find_map(|(candidate, ty)| (candidate == parameter).then(|| ty.clone()))
			.unwrap_or_else(|| ty.clone()),
		InterfaceType::List(inner) => {
			InterfaceType::List(Box::new(substitute_type_parameters(inner, substitutions)))
		}
		InterfaceType::Tuple(items) => InterfaceType::Tuple(
			items
				.iter()
				.map(|item| substitute_type_parameters(item, substitutions))
				.collect(),
		),
		InterfaceType::Map(key, value) => InterfaceType::Map(
			Box::new(substitute_type_parameters(key, substitutions)),
			Box::new(substitute_type_parameters(value, substitutions)),
		),
		InterfaceType::Function {
			parameters,
			return_type,
			effects,
		} => InterfaceType::Function {
			parameters: parameters
				.iter()
				.map(|parameter| substitute_type_parameters(parameter, substitutions))
				.collect(),
			return_type: Box::new(substitute_type_parameters(return_type, substitutions)),
			effects: effects.clone(),
		},
		InterfaceType::Named {
			definition,
			positional,
			named,
		} => InterfaceType::Named {
			definition: definition.clone(),
			positional: positional
				.iter()
				.map(|argument| substitute_type_parameters(argument, substitutions))
				.collect(),
			named: named
				.iter()
				.map(|(name, argument)| {
					(
						name.clone(),
						substitute_type_parameters(argument, substitutions),
					)
				})
				.collect(),
		},
		InterfaceType::Intersection(items) => InterfaceType::Intersection(
			items
				.iter()
				.map(|item| substitute_type_parameters(item, substitutions))
				.collect(),
		),
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
		|| !matches!(shape.kind, crate::MemberKind::Function)
		|| shape.external.is_some() != matches!(artifact.payload, crate::RuntimePayload::External(_))
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
				crate::MemberKind::Function,
				crate::RuntimeBodyKind::InstanceFunction
			) | (
				crate::MemberKind::StaticFunction,
				crate::RuntimeBodyKind::StaticFunction
			) | (
				crate::MemberKind::Value | crate::MemberKind::StaticValue,
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

fn attached_nominal_member(
	context: &impl StableLoweringContext,
	artifact: &RuntimeDefinition,
	abi: &ExternalAbi,
) -> Result<Option<crate::MemberShape<InterfaceType>>, StableLoweringError> {
	let crate::RuntimePlacement::Attached { owner, name } = &artifact.placement else {
		return Ok(None);
	};
	if matches!(owner.key, crate::DeclarationKey::Implementation { .. }) {
		return Ok(None);
	}
	let owner_request = StableShapeRequest::Definition(owner.clone());
	let StableShapeFact::Definition(owner_shape) = context.stable_shape(&owner_request)? else {
		return Err(
			StableShapeLookupError::WrongFact {
				request: owner_request,
			}
			.into(),
		);
	};
	if owner_shape.id != *owner
		|| !matches!(
			owner_shape.kind,
			crate::DefinitionShapeKind::Struct | crate::DefinitionShapeKind::Enum
		) {
		return Err(invalid(
			&artifact.definition,
			"attached external owner disagrees with its exact nominal definition shape",
		));
	}
	let owner_member = owner_shape
		.members
		.iter()
		.find(|member| member.id == artifact.definition)
		.cloned()
		.ok_or_else(|| {
			invalid(
				&artifact.definition,
				"attached external has no exact nominal member shape",
			)
		})?;
	let member_request = StableShapeRequest::Member(artifact.definition.clone());
	let StableShapeFact::Member(member) = context.stable_shape(&member_request)? else {
		return Err(
			StableShapeLookupError::WrongFact {
				request: member_request,
			}
			.into(),
		);
	};
	if member != owner_member
		|| member.name != *name
		|| member.runtime_owner.as_ref() != Some(owner)
		|| member.external.as_ref() != Some(abi)
	{
		return Err(invalid(
			&artifact.definition,
			"attached external disagrees with its exact nominal member shape",
		));
	}
	Ok(Some(member))
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

#[allow(clippy::too_many_arguments)]
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
	type_substitutions: &[(crate::GenericParameterId, InterfaceType)],
) -> Result<LoweredHirFragment, StableLoweringError> {
	let is_function = body.kind != crate::RuntimeBodyKind::Value;
	let stable = &body.stable;
	let shellless_implementation = match &artifact.placement {
		crate::RuntimePlacement::Attached { owner, .. } => {
			owner_has_no_attachment_shell(context, owner)?
		}
		crate::RuntimePlacement::TopLevel => false,
	};
	let exact_shellless_implementation = if shellless_implementation {
		let crate::RuntimePlacement::Attached { owner, .. } = &artifact.placement else {
			unreachable!()
		};
		let request = StableShapeRequest::Implementation(owner.clone());
		let StableShapeFact::Implementation(implementation) = context.stable_shape(&request)? else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		implementation.binders.is_empty()
	} else {
		false
	};
	let blanket_implementation_shape = if shellless_implementation {
		let crate::RuntimePlacement::Attached { owner, .. } = &artifact.placement else {
			unreachable!()
		};
		let request = StableShapeRequest::Implementation(owner.clone());
		let StableShapeFact::Implementation(implementation) = context.stable_shape(&request)? else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		matches!(implementation.self_type, InterfaceType::Generic(_)).then_some(implementation)
	} else {
		None
	};
	let blanket_implementation = blanket_implementation_shape.is_some();
	let implementation_type_parameters = blanket_implementation_shape
		.as_ref()
		.map(|implementation| {
			implementation
				.binders
				.iter()
				.map(|binder| binder.id.clone())
				.collect::<Vec<_>>()
		})
		.unwrap_or_default();
	let implementation_hidden = implementation_type_parameters.len();
	let exact_parameterized_implementation = if let (
		Some(InterfaceType::Named {
			positional, named, ..
		}),
		crate::RuntimePlacement::Attached { owner, .. },
	) = (self_type, &artifact.placement)
	{
		if (!positional.is_empty() || !named.is_empty())
			&& is_concrete_runtime_type(self_type.expect("checked self type"))
			&& matches!(owner.key, crate::DeclarationKey::Implementation { .. })
		{
			let request = StableShapeRequest::Implementation(owner.clone());
			let StableShapeFact::Implementation(implementation) = context.stable_shape(&request)? else {
				return Err(StableShapeLookupError::WrongFact { request }.into());
			};
			implementation.binders.is_empty()
		} else {
			false
		}
	} else {
		false
	};
	let instance_member = body.kind == crate::RuntimeBodyKind::InstanceFunction
		&& matches!(
			artifact.definition.key,
			crate::DeclarationKey::Member { .. }
		);
	let has_receiver = (shellless_implementation
		&& matches!(
			member_kind,
			Some(crate::MemberKind::Function | crate::MemberKind::Value)
		)) || (exact_parameterized_implementation && instance_member);
	let mut type_parameters = implementation_type_parameters.clone();
	type_parameters.extend(
		body
			.type_parameters
			.iter()
			.filter(|parameter| {
				(!instance_member
					|| blanket_implementation
					|| parameter.binder.scope == crate::BinderScope::Member)
					&& !type_substitutions
						.iter()
						.any(|(candidate, _)| candidate == *parameter)
			})
			.filter(|&parameter| !type_parameters.contains(parameter))
			.cloned()
			.collect::<Vec<_>>(),
	);
	let lowerer = StableBodyLowerer {
		context,
		artifact,
		annotations: &body.annotations,
		type_parameters: &type_parameters,
		// Annotation slot indexes are defined against the checked body's own
		// binder order. Keep that identity map unchanged; `type_parameters`
		// above separately defines the canonical runtime ABI order.
		all_type_parameters: &body.type_parameters,
		implementation_hidden,
		instance_member,
		type_substitutions,
		scopes: RefCell::new(vec![HashMap::new()]),
		counters: RefCell::new(HashMap::new()),
		pattern_declaration_records: RefCell::new(vec![]),
		pattern_declaration_reuse: RefCell::new(vec![]),
		demands: RefCell::new(demands),
		direct_demands: RefCell::new(direct_demands),
		routed_demands: RefCell::new(routed_demands),
		execution: RefCell::new(execution),
		deferred_execution: RefCell::new(Vec::new()),
		deferred_depth: Cell::new(0),
		capture_root_invocation: !is_function
			&& body.immutable
			&& (matches!(stable.root.kind, StableExprKind::Closure { .. })
				|| body
					.annotations
					.anonymous_closures
					.iter()
					.any(|(node, _)| *node == stable.root.id)),
		loop_depth: Cell::new(0),
		loop_targets: RefCell::new(Vec::new()),
		state_loop_targets: RefCell::new(std::collections::HashSet::new()),
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
		let mut lowered_body = lowerer.lower_function_body(&stable.root)?;
		if stable.is_async {
			lowered_body = HirExpr::TaskRecipe {
				body: Box::new(lowered_body),
				context: nymph_hir::hir::HirTaskContext::Inherited,
			};
		}
		let hidden = (0..type_parameters.len()).map(|index| EcoString::from(format!("$type${index}")));
		let function = HirFunc {
			name: emitted.clone(),
			params: if has_receiver {
				std::iter::once(EcoString::from("$self"))
					.chain(params.iter().cloned())
					.chain(hidden)
					.collect()
			} else {
				params.iter().cloned().chain(hidden).collect()
			},
			body: lowered_body.clone(),
		};
		if shellless_implementation {
			// Primitive `Power` has an intentionally overloaded scalar matrix. A
			// canonical primitive prototype can only own one property named `power`,
			// while every checked call already carries its exact implementation slot
			// and lowers directly to this top-level function. Do not manufacture an
			// ambiguous prototype facade (or collide during runtime assembly); generic
			// bound dispatch likewise routes to exact top-level cases.
			if context.member_name(&artifact.definition)?.as_str() == "power" {
				return Ok(LoweredHirFragment::TopLevelFunction(function));
			}
			if self_type
				.filter(|ty| is_concrete_runtime_type(ty))
				.and_then(stable_runtime_tag)
				.and_then(primitive_box_binding)
				.is_some()
				&& exact_shellless_implementation
			{
				let hidden_params = (0..type_parameters.len())
					.map(|index| EcoString::from(format!("$type${index}")))
					.collect::<Vec<_>>();
				let method_args = params
					.iter()
					.chain(&hidden_params)
					.cloned()
					.map(HirExpr::Local)
					.collect::<Vec<_>>();
				let forwarded_args = has_receiver
					.then_some(HirExpr::Undefined)
					.into_iter()
					.chain(method_args)
					.collect();
				return Ok(LoweredHirFragment::RuntimeTypeAttachment {
					object: lowerer.runtime_type_object(self_type.expect("primitive self type"))?,
					function: Some(function.clone()),
					method: HirMethod {
						name: context.member_name(&artifact.definition)?.as_str().into(),
						params: params.into_iter().chain(hidden_params).collect(),
						body: HirExpr::ActivationCall {
							callee: Box::new(HirExpr::Local(emitted)),
							args: forwarded_args,
							mode: nymph_hir::hir::HirCallMode::Tail,
							source: 0,
						},
					},
				});
			}
			return Ok(LoweredHirFragment::TopLevelFunction(function));
		}
		match &artifact.placement {
			crate::RuntimePlacement::TopLevel => Ok(LoweredHirFragment::TopLevelFunction(function)),
			crate::RuntimePlacement::Attached { owner, .. } => {
				let name = context.member_name(&artifact.definition)?.as_str().into();
				let method = HirMethod {
					name,
					params: params
						.iter()
						.cloned()
						.chain(
							(0..type_parameters.len()).map(|index| EcoString::from(format!("$type${index}"))),
						)
						.collect(),
					body: lowered_body,
				};
				if let Some(InterfaceType::Named {
					positional, named, ..
				}) = self_type
					&& (!positional.is_empty() || !named.is_empty())
					&& is_concrete_runtime_type(self_type.expect("checked self type"))
					&& matches!(owner.key, crate::DeclarationKey::Implementation { .. })
				{
					let request = StableShapeRequest::Implementation(owner.clone());
					let StableShapeFact::Implementation(implementation) = context.stable_shape(&request)?
					else {
						return Err(StableShapeLookupError::WrongFact { request }.into());
					};
					if implementation.binders.is_empty() {
						let hidden_params = (0..type_parameters.len())
							.map(|index| EcoString::from(format!("$type${index}")))
							.collect::<Vec<_>>();
						let method_params = method.params.clone();
						let forwarded = has_receiver
							.then_some(HirExpr::This)
							.into_iter()
							.chain(method_params.iter().cloned().map(HirExpr::Local))
							.collect();
						return Ok(LoweredHirFragment::RuntimeTypeAttachment {
							object: lowerer.runtime_type_object(self_type.expect("checked self type"))?,
							function: Some(function),
							method: HirMethod {
								name: method.name,
								params: params.into_iter().chain(hidden_params).collect(),
								body: HirExpr::ActivationCall {
									callee: Box::new(HirExpr::Local(emitted)),
									args: forwarded,
									mode: nymph_hir::hir::HirCallMode::Tail,
									source: 0,
								},
							},
						});
					}
				}
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
					value,
				}))
			};
		}
		match &artifact.placement {
			crate::RuntimePlacement::TopLevel => Ok(LoweredHirFragment::TopLevelValue(HirLet {
				name: emitted,
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
	type_parameters: &'a [crate::GenericParameterId],
	all_type_parameters: &'a [crate::GenericParameterId],
	implementation_hidden: usize,
	instance_member: bool,
	type_substitutions: &'a [(crate::GenericParameterId, InterfaceType)],
	scopes: RefCell<Vec<HashMap<EcoString, EcoString>>>,
	counters: RefCell<HashMap<EcoString, u32>>,
	pattern_declaration_records: RefCell<Vec<HashMap<EcoString, EcoString>>>,
	pattern_declaration_reuse: RefCell<Vec<HashMap<EcoString, EcoString>>>,
	demands: RefCell<&'a mut StableDemandSet>,
	direct_demands: RefCell<&'a mut StableDemandSet>,
	routed_demands: RefCell<&'a mut StableDemandSet>,
	execution: RefCell<&'a mut RuntimeExecutionSummary>,
	deferred_execution: RefCell<Vec<RuntimeExecutionSummary>>,
	deferred_depth: Cell<u32>,
	capture_root_invocation: bool,
	loop_depth: Cell<u32>,
	loop_targets: RefCell<Vec<(crate::BodyNodeId, u32)>>,
	state_loop_targets: RefCell<std::collections::HashSet<u32>>,
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
	fn range_mode(&self, expr: &StableExpr) -> OperationMode {
		if self
			.annotations
			.range_proof(self.id(expr))
			.is_some_and(|proof| proof.decision == crate::RangeDecision::Safe && proof.audit())
		{
			OperationMode::Direct
		} else {
			OperationMode::Checked
		}
	}
	fn lower_slice(
		&self,
		expr: &StableExpr,
		index: &StableExpr,
		recv: HirExpr,
		string: bool,
	) -> Result<HirExpr, StableLoweringError> {
		let StableExprKind::Range(range) = &index.kind else {
			return Err(self.unsupported(index, "slice index is not a range"));
		};
		let (start, end, inclusive) = match range {
			StableRange::From(start) => (Some(start.as_ref()), None, false),
			StableRange::To(end) => (None, Some(end.as_ref()), false),
			StableRange::Exclusive { min, max } => (Some(min.as_ref()), Some(max.as_ref()), false),
			StableRange::ToInclusive(end) => (None, Some(end.as_ref()), true),
			StableRange::Inclusive { min, max } => (Some(min.as_ref()), Some(max.as_ref()), true),
		};
		Ok(HirExpr::Slice {
			recv: Box::new(recv),
			start: start
				.map(|value| self.lower(value).map(Box::new))
				.transpose()?,
			end: end
				.map(|value| self.lower(value).map(Box::new))
				.transpose()?,
			inclusive,
			string,
			mode: self.range_mode(expr),
		})
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
	fn with_scope<T>(
		&self,
		lower: impl FnOnce() -> Result<T, StableLoweringError>,
	) -> Result<T, StableLoweringError> {
		self.scopes.borrow_mut().push(HashMap::new());
		let result = lower();
		self.scopes.borrow_mut().pop();
		result
	}
	fn with_loop_target<T>(
		&self,
		source: crate::BodyNodeId,
		target: u32,
		lower: impl FnOnce() -> Result<T, StableLoweringError>,
	) -> Result<T, StableLoweringError> {
		let depth = self.loop_depth.get();
		self.loop_depth.set(depth + 1);
		self.loop_targets.borrow_mut().push((source, target));
		let result = lower();
		self.loop_targets.borrow_mut().pop();
		self.loop_depth.set(depth);
		result
	}
	fn with_block_target<T>(
		&self,
		source: crate::BodyNodeId,
		target: nymph_hir::hir::BlockTarget,
		lower: impl FnOnce() -> Result<T, StableLoweringError>,
	) -> Result<T, StableLoweringError> {
		self.block_targets.borrow_mut().push((source, target));
		let result = lower();
		self.block_targets.borrow_mut().pop();
		result
	}
	fn with_callable_frame<T>(
		&self,
		lower: impl FnOnce() -> Result<T, StableLoweringError>,
	) -> Result<T, StableLoweringError> {
		let loop_depth = self.loop_depth.replace(0);
		let loop_targets = self.loop_targets.replace(Vec::new());
		let block_targets = self.block_targets.replace(Vec::new());
		let result = lower();
		self.block_targets.replace(block_targets);
		self.loop_targets.replace(loop_targets);
		self.loop_depth.set(loop_depth);
		result
	}
	fn missing_annotation(&self, node: crate::BodyNodeId, channel: &str) -> StableLoweringError {
		StableLoweringError::MissingAnnotation {
			definition: self.artifact.definition.clone(),
			node,
			channel: channel.into(),
		}
	}
	fn target(&self, expr: &StableExpr) -> Option<&DefinitionId> {
		self.annotations.definition_target(self.id(expr))
	}
	fn record_read(&self, target: &DefinitionId) {
		if let Some(mut execution) = self.active_execution() {
			execution.immediate_reads.insert(target.clone());
		}
	}
	fn record_call(&self, target: &DefinitionId) -> Result<bool, StableLoweringError> {
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
				.active_execution()
				.expect("active execution summary")
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
		if let Some(mut execution) = self.active_execution()
			&& !execution.unresolved_calls.contains(&call)
		{
			execution.unresolved_calls.push(call);
		}
	}
	fn active_execution(&self) -> Option<std::cell::RefMut<'_, RuntimeExecutionSummary>> {
		if !self.deferred_execution.borrow().is_empty() {
			return Some(std::cell::RefMut::map(
				self.deferred_execution.borrow_mut(),
				|stack| stack.last_mut().expect("checked non-empty closure stack"),
			));
		}
		if self.deferred_depth.get() == 0 {
			return Some(std::cell::RefMut::map(
				self.execution.borrow_mut(),
				|execution| &mut **execution,
			));
		}
		None
	}
	fn with_deferred<T>(
		&self,
		lower: impl FnOnce() -> Result<T, StableLoweringError>,
	) -> Result<T, StableLoweringError> {
		let depth = self.deferred_depth.get();
		self.deferred_depth.set(depth + 1);
		self
			.deferred_execution
			.borrow_mut()
			.push(RuntimeExecutionSummary::default());
		let result = lower();
		let execution = self
			.deferred_execution
			.borrow_mut()
			.pop()
			.expect("closure execution summary");
		self.deferred_depth.set(depth);
		if depth == 0 && self.capture_root_invocation {
			self.execution.borrow_mut().invocation = Some(Box::new(execution));
		} else if let Some(mut parent) = self.active_execution() {
			parent.closures.push(execution);
		}
		result
	}
	fn external_marshal(
		&self,
		expr: &StableExpr,
	) -> Result<nymph_hir::hir::MarshalKind, StableLoweringError> {
		let node = self.id(expr);
		self
			.annotations
			.external_marshal(node)
			.ok_or_else(|| self.missing_annotation(node, "external marshal"))
	}
	fn ty(&self, expr: &StableExpr) -> Result<InterfaceType, StableLoweringError> {
		let node = self.id(expr);
		let ty = self
			.annotations
			.type_of(node)
			.ok_or_else(|| self.missing_annotation(node, "type"))?;
		let ty = self.self_type.map_or_else(
			|| ty.clone(),
			|self_type| substitute_self_type(ty, self_type),
		);
		Ok(substitute_type_parameters(&ty, self.type_substitutions))
	}

	fn optional_chain_payload_type(
		&self,
		parent: &StableExpr,
	) -> Result<InterfaceType, StableLoweringError> {
		match peel_mut(&self.ty(parent)?) {
			InterfaceType::Named {
				definition,
				positional,
				..
			} if self
				.annotations
				.option
				.as_ref()
				.is_some_and(|role| &role.option == definition)
				|| self
					.annotations
					.result
					.as_ref()
					.is_some_and(|role| &role.result == definition) =>
			{
				positional.first().cloned().ok_or_else(|| {
					invalid(
						&self.artifact.definition,
						"optional chain has no payload type",
					)
				})
			}
			_ => Err(invalid(
				&self.artifact.definition,
				"optional chain receiver lost its canonical container type",
			)),
		}
	}

	fn lower_optional_chain(
		&self,
		expr: &StableExpr,
		parent: &StableExpr,
		mapped: impl FnOnce(&Self, HirExpr) -> Result<HirExpr, StableLoweringError>,
	) -> Result<HirExpr, StableLoweringError> {
		let parent_type = self.ty(parent)?;
		let definition = match peel_mut(&parent_type) {
			InterfaceType::Named { definition, .. } => definition,
			_ => {
				return Err(invalid(
					&self.artifact.definition,
					"optional chain receiver is not nominal",
				));
			}
		};
		let (enum_definition, success, success_field, failure, failure_field) = if let Some(role) = self
			.annotations
			.option
			.as_ref()
			.filter(|role| &role.option == definition)
		{
			(&role.option, &role.some, &role.some_value, &role.none, None)
		} else if let Some(role) = self
			.annotations
			.result
			.as_ref()
			.filter(|role| &role.result == definition)
		{
			(
				&role.result,
				&role.ok,
				&role.ok_value,
				&role.error,
				Some(&role.error_value),
			)
		} else {
			return Err(invalid(
				&self.artifact.definition,
				"optional chain has no canonical runtime role",
			));
		};
		self.demand_external(enum_definition)?;
		let enum_name: EcoString = self.context.binding_name(enum_definition)?.as_str().into();
		let success_name: EcoString = self.context.member_name(success)?.as_str().into();
		let success_field_name: EcoString = self.context.member_name(success_field)?.as_str().into();
		let failure_name: EcoString = self.context.member_name(failure)?.as_str().into();
		let payload = self.declare(&"$optional_value".into());
		let mapped = mapped(self, HirExpr::Local(payload.clone()))?;
		let prototype = self.construction_prototype(expr)?;
		let success_body = HirExpr::VariantNew {
			enum_name: enum_name.clone(),
			variant: success_name.clone(),
			fields: vec![(success_field_name.clone(), mapped)],
			prototype: prototype.clone(),
		};
		let (failure_pattern_fields, failure_body) = if let Some(failure_field) = failure_field {
			let field_name: EcoString = self.context.member_name(failure_field)?.as_str().into();
			let error = self.declare(&"$optional_error".into());
			(
				vec![(
					field_name.clone(),
					HirPat::Binding {
						name: error.clone(),
						sub: None,
					},
				)],
				HirExpr::VariantNew {
					enum_name: enum_name.clone(),
					variant: failure_name.clone(),
					fields: vec![(field_name, HirExpr::Local(error))],
					prototype: prototype.clone(),
				},
			)
		} else {
			(
				vec![],
				HirExpr::VariantRef {
					enum_name: enum_name.clone(),
					variant: failure_name.clone(),
					prototype: prototype.clone(),
				},
			)
		};
		Ok(HirExpr::Match {
			scrutinee: Box::new(self.lower(parent)?),
			arms: vec![
				HirArm {
					pat: HirPat::Variant {
						enum_name: enum_name.clone(),
						variant: success_name,
						fields: vec![(
							success_field_name,
							HirPat::Binding {
								name: payload,
								sub: None,
							},
						)],
					},
					guard: None,
					body: success_body,
				},
				HirArm {
					pat: HirPat::Variant {
						enum_name: enum_name.clone(),
						variant: failure_name.clone(),
						fields: failure_pattern_fields,
					},
					guard: None,
					body: failure_body,
				},
			],
		})
	}
	/// Whether this checked expression cannot complete normally. An anonymous
	/// closure annotation turns the underlying expression into a callable value,
	/// so its body's `never` type must not erase an enclosing operation.
	fn definitely_transfers(&self, expr: &StableExpr) -> bool {
		let node = self.id(expr);
		if self.annotations.anonymous_closure_arity(node).is_some() {
			return false;
		}
		if self
			.annotations
			.type_of(node)
			.is_some_and(|ty| matches!(peel_mut(ty), InterfaceType::Never))
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
	fn int_literal_result(&self, expr: &StableExpr) -> Result<BuiltinResult, StableLoweringError> {
		let Some(ty) = self.annotations.type_of(self.id(expr)) else {
			return Ok(BuiltinResult::Int);
		};
		match peel_mut(ty) {
			InterfaceType::Int => Ok(BuiltinResult::Int),
			InterfaceType::UInt => Ok(BuiltinResult::UInt),
			InterfaceType::Float => Ok(BuiltinResult::Float),
			_ => Err(self.unsupported(expr, "integer literal type")),
		}
	}
	fn dispatch(&self, expr: &StableExpr) -> Result<&crate::StableDispatch, StableLoweringError> {
		let node = self.id(expr);
		self
			.annotations
			.dispatch(node)
			.ok_or_else(|| self.missing_annotation(node, "dispatch"))
	}
	fn variant(&self, expr: &StableExpr) -> Option<&crate::ExpressionVariant> {
		self.annotations.variant(self.id(expr))
	}
	fn lower_function_body(&self, expr: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		let mut body = self.with_callable_frame(|| match &expr.kind {
			StableExprKind::Block { body, label: None } => self.lower_block(body, false),
			StableExprKind::Block {
				body,
				label: Some(_),
			} => {
				let target = self.next_block_target.get();
				self.next_block_target.set(target + 1);
				self.with_block_target(self.id(expr), target, || {
					self
						.lower_block(body, false)
						.map(|body| HirExpr::LabeledBlock {
							target,
							body: Box::new(body),
						})
				})
			}
			_ => self.lower(expr),
		})?;
		Self::mark_tail_call(&mut body);
		Ok(body)
	}

	fn activation_call(source: crate::BodyNodeId, call: HirExpr) -> HirExpr {
		match call {
			HirExpr::Call { callee, args } => HirExpr::ActivationCall {
				callee,
				args,
				mode: nymph_hir::hir::HirCallMode::Push,
				source: source.0,
			},
			HirExpr::StaticEnumDispatch {
				owner,
				method,
				receiver,
				args,
				..
			} => HirExpr::StaticEnumDispatch {
				owner,
				method,
				receiver,
				args,
				mode: nymph_hir::hir::HirCallMode::Push,
				source: source.0,
			},
			other => other,
		}
	}

	fn activation_dispatch_call(
		source: crate::BodyNodeId,
		dispatch: &crate::StableDispatch,
		call: HirExpr,
	) -> HirExpr {
		let generated = match dispatch {
			crate::StableDispatch::Direct { .. }
			| crate::StableDispatch::SelectedImplementation { .. }
			| crate::StableDispatch::InterfaceDefault { .. }
			| crate::StableDispatch::External { .. } => matches!(call, HirExpr::Call { .. }),
			crate::StableDispatch::GenericBound { .. } => true,
			crate::StableDispatch::Builtin { .. } => false,
		};
		if generated {
			Self::activation_call(source, call)
		} else {
			call
		}
	}

	/// Mark only calls whose result is the callable's result. Blocks, branches,
	/// matches, and explicit callable returns preserve tail position; argument,
	/// condition, and lexical-block-return expressions do not.
	fn mark_tail_call(expr: &mut HirExpr) {
		match expr {
			HirExpr::ActivationCall { mode, .. }
			| HirExpr::StaticEnumDispatch { mode, .. }
			| HirExpr::BoundDispatch { mode, .. }
			| HirExpr::UnaryBoundDispatch { mode, .. } => {
				*mode = nymph_hir::hir::HirCallMode::Tail;
			}
			HirExpr::Block { stmts, tail } => {
				for stmt in stmts {
					if let HirStmt::Return {
						value: Some(value),
						target: nymph_hir::hir::HirReturnTarget::Callable,
					} = stmt
					{
						Self::mark_tail_call(value);
					}
				}
				if let Some(tail) = tail {
					Self::mark_tail_call(tail);
				}
			}
			HirExpr::LabeledBlock { body, .. } => Self::mark_tail_call(body),
			HirExpr::If {
				then, otherwise, ..
			} => {
				Self::mark_tail_call(then);
				if let Some(otherwise) = otherwise {
					Self::mark_tail_call(otherwise);
				}
			}
			HirExpr::Match { arms, .. } => {
				for arm in arms {
					Self::mark_tail_call(&mut arm.body);
				}
			}
			_ => {}
		}
	}
	fn lower_loop_branch(
		&self,
		source: crate::BodyNodeId,
		target: u32,
		expr: &StableExpr,
	) -> Result<HirExpr, StableLoweringError> {
		self.with_loop_target(source, target, || self.lower(expr))
	}
	fn next_loop(&self) -> u32 {
		let target = self.next_loop_target.get();
		self.next_loop_target.set(target + 1);
		target
	}
	fn return_target(&self, expr: &StableExpr) -> nymph_hir::hir::HirReturnTarget {
		let jump = self.id(expr);
		let resolved = self.annotations.control_target(jump);
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
	fn lower_propagation(
		&self,
		expr: &StableExpr,
		value: &StableExpr,
	) -> Result<HirExpr, StableLoweringError> {
		let propagation = self
			.annotations
			.propagations
			.iter()
			.find_map(|(node, propagation)| (*node == self.id(expr)).then_some(propagation))
			.ok_or_else(|| StableLoweringError::MissingAnnotation {
				definition: self.artifact.definition.clone(),
				node: self.id(expr),
				channel: "propagation".into(),
			})?;
		let (definition, success, success_field, failure, failure_field) = match propagation.kind {
			crate::RuntimePropagationKind::Option => {
				let role = self
					.annotations
					.option
					.as_ref()
					.ok_or_else(|| self.unsupported(expr, "canonical Option propagation ABI"))?;
				(&role.option, &role.some, &role.some_value, &role.none, None)
			}
			crate::RuntimePropagationKind::Result => {
				let role = self
					.annotations
					.result
					.as_ref()
					.ok_or_else(|| self.unsupported(expr, "canonical Result propagation ABI"))?;
				(
					&role.result,
					&role.ok,
					&role.ok_value,
					&role.error,
					Some(&role.error_value),
				)
			}
		};
		self.demand_external(definition)?;
		let enum_name: EcoString = self.context.binding_name(definition)?.as_str().into();
		let success: EcoString = self.context.member_name(success)?.as_str().into();
		let success_field: EcoString = self.context.member_name(success_field)?.as_str().into();
		let failure: EcoString = self.context.member_name(failure)?.as_str().into();
		let failure_field = failure_field
			.map(|field| {
				self
					.context
					.member_name(field)
					.map(|name| name.as_str().into())
			})
			.transpose()?;
		let temporary = self.declare(&"$propagation".into());
		let payload = self.declare(&"$value".into());
		let failure_payload = propagation
			.conversion
			.as_ref()
			.map(|_| self.declare(&"$error".into()));
		let failure_fields = failure_field
			.clone()
			.into_iter()
			.map(|field| {
				(
					field,
					failure_payload
						.clone()
						.map_or(HirPat::Wildcard, |name| HirPat::Binding { name, sub: None }),
				)
			})
			.collect();
		let failure_return = if let (Some(dispatch), Some(error), Some(field)) = (
			propagation.conversion.as_ref(),
			failure_payload,
			failure_field,
		) {
			let mut converted = self.lower_dispatch(dispatch, value, vec![])?;
			Self::replace_dispatch_receiver(&mut converted, HirExpr::Local(error));
			HirExpr::VariantNew {
				enum_name: enum_name.clone(),
				variant: failure.clone(),
				fields: vec![(field, converted)],
				prototype: None,
			}
		} else {
			HirExpr::Local(temporary.clone())
		};
		Ok(HirExpr::Block {
			stmts: vec![HirStmt::Let {
				name: temporary.clone(),
				value: self.lower(value)?,
				cleanup: None,
			}],
			tail: Some(Box::new(HirExpr::Match {
				scrutinee: Box::new(HirExpr::Local(temporary.clone())),
				arms: vec![
					HirArm {
						pat: HirPat::Variant {
							enum_name: enum_name.clone(),
							variant: success,
							fields: vec![(
								success_field,
								HirPat::Binding {
									name: payload.clone(),
									sub: None,
								},
							)],
						},
						guard: None,
						body: HirExpr::Local(payload),
					},
					HirArm {
						pat: HirPat::Variant {
							enum_name,
							variant: failure,
							fields: failure_fields,
						},
						guard: None,
						body: HirExpr::Block {
							stmts: vec![HirStmt::Return {
								value: Some(failure_return),
								target: self.return_target(expr),
							}],
							tail: None,
						},
					},
				],
			})),
		})
	}
	fn loop_option(
		&self,
		expr: &StableExpr,
	) -> Result<Option<nymph_hir::hir::HirOptionAbi>, StableLoweringError> {
		let Some(role) = &self.annotations.option else {
			return Ok(None);
		};
		let Some(ty) = self.annotations.type_of(self.id(expr)) else {
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
	fn resolve_loop_target(
		&self,
		expr: &StableExpr,
		unsupported: &str,
	) -> Result<u32, StableLoweringError> {
		let Some(crate::runtime::RuntimeControlTarget::Loop(source)) =
			self.annotations.control_target(self.id(expr))
		else {
			return Err(self.unsupported(expr, unsupported));
		};
		self
			.loop_targets
			.borrow()
			.iter()
			.rev()
			.find_map(|(candidate, target)| (*candidate == source).then_some(*target))
			.ok_or_else(|| self.unsupported(expr, unsupported))
	}
	fn lower(&self, expr: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		if self.is_task_special(expr) {
			return self.lower_task_special(expr);
		}
		if let Some(arity) = self.annotations.anonymous_closure_arity(self.id(expr)) {
			return self.with_scope(|| {
				let params = (0..arity)
					.map(|i| self.declare(&crate::anon_closure::anon_param_name(i)))
					.collect();
				let body = self.with_callable_frame(|| self.with_deferred(|| self.lower_inner(expr)))?;
				Ok(HirExpr::Closure {
					params,
					body: Box::new(body),
				})
			});
		}
		// Keep recursively lowered special forms out of the large `lower_inner`
		// debug frame so ordinary test-thread stacks remain sufficient.
		let lowered = match &expr.kind {
			StableExprKind::Match { value, arms } => self.lower_match(value, arms)?,
			_ => self.lower_inner(expr)?,
		};
		Ok(lowered)
	}

	fn lower_match(
		&self,
		value: &StableExpr,
		arms: &[StableMatchArm],
	) -> Result<HirExpr, StableLoweringError> {
		Ok(HirExpr::Match {
			scrutinee: Box::new(self.lower(value)?),
			arms: arms
				.iter()
				.map(|arm| {
					self.with_scope(|| {
						Ok(HirArm {
							pat: self.lower_pattern(&arm.pattern)?,
							guard: arm
								.guard
								.as_ref()
								.map(|guard| self.lower(guard))
								.transpose()?,
							body: self.lower(&arm.body)?,
						})
					})
				})
				.collect::<Result<_, StableLoweringError>>()?,
		})
	}

	fn lower_inner(&self, expr: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		if !matches!(expr.kind, StableExprKind::Int(_) | StableExprKind::UInt(_))
			&& let Some(value) = stable_integer_constant(expr)
		{
			match self.annotations.type_of(self.id(expr)).map(peel_mut) {
				None | Some(InterfaceType::Int) => {
					return Ok(HirExpr::Int(i64::try_from(value).map_err(|_| {
						invalid(
							&self.artifact.definition,
							"checked int constant is out of range",
						)
					})?));
				}
				Some(InterfaceType::UInt) => {
					return Ok(HirExpr::UInt(u64::try_from(value).map_err(|_| {
						invalid(
							&self.artifact.definition,
							"checked uint constant is out of range",
						)
					})?));
				}
				_ => {}
			}
		}
		Ok(match &expr.kind {
			StableExprKind::Int(value) => match self.int_literal_result(expr)? {
				BuiltinResult::Int => HirExpr::Int(i64::try_from(*value).map_err(|_| {
					invalid(
						&self.artifact.definition,
						"positive int literal exceeds i64::MAX",
					)
				})?),
				BuiltinResult::UInt => HirExpr::UInt(*value),
				BuiltinResult::Float => HirExpr::Num(*value as f64, NumKind::Float),
				_ => unreachable!("integer literals only infer numeric result types"),
			},
			StableExprKind::UInt(value) => HirExpr::UInt(*value),
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
						prototype: self.construction_prototype(expr)?,
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
									.result
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
					self.generic_callable_adapter(self.id(expr), HirExpr::Local(emitted.as_str().into()))?
				} else {
					self.generic_callable_adapter(self.id(expr), HirExpr::Local(self.resolve(name)))?
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
			StableExprKind::Grouped(inner) => {
				let lowered = self.lower(inner)?;
				if matches!(&lowered, HirExpr::Local(_) | HirExpr::Field { .. }) {
					self.generic_callable_adapter(self.id(expr), lowered)?
				} else {
					self.append_hidden_arguments(self.id(expr), lowered)?
				}
			}
			StableExprKind::List(items) | StableExprKind::Tuple(items) => {
				let is_list = matches!(expr.kind, StableExprKind::List(_));
				let kind = if is_list {
					HirArrayKind::List
				} else {
					HirArrayKind::Tuple
				};
				let value = if items
					.iter()
					.any(|item| matches!(*item, StableListItem::Spread(_)))
				{
					let elems = items
						.iter()
						.map(|item| match item {
							StableListItem::Expr(value) => self.lower(value).map(HirArrayElem::Item),
							StableListItem::Spread(value) => self.lower_spread(value).map(HirArrayElem::Spread),
						})
						.collect::<Result<_, _>>()?;
					if is_list {
						HirExpr::ListConstruct(elems)
					} else {
						HirExpr::ArraySpread { kind, elems }
					}
				} else {
					let items: Vec<HirExpr> = items
						.iter()
						.map(|item| match item {
							StableListItem::Expr(item) => self.lower(item),
							_ => unreachable!(),
						})
						.collect::<Result<_, _>>()?;
					if is_list {
						HirExpr::ListConstruct(items.into_iter().map(HirArrayElem::Item).collect())
					} else {
						HirExpr::Array { kind, items }
					}
				};
				match self.construction_prototype(expr)? {
					Some(prototype) => HirExpr::WithPrototype {
						value: Box::new(value),
						prototype,
					},
					None => value,
				}
			}
			StableExprKind::Map(entries) => {
				let value = if entries
					.iter()
					.any(|entry| matches!(*entry, StableMapEntry::Spread(_)))
				{
					HirExpr::MapSpread(
						entries
							.iter()
							.map(|entry| match entry {
								StableMapEntry::Entry(key, value) => {
									Ok(HirMapElem::Entry(self.lower(key)?, self.lower(value)?))
								}
								StableMapEntry::Spread(value) => self.lower_spread(value).map(HirMapElem::Spread),
							})
							.collect::<Result<_, StableLoweringError>>()?,
					)
				} else {
					HirExpr::MapLit(
						entries
							.iter()
							.map(|entry| match entry {
								StableMapEntry::Entry(key, value) => Ok((self.lower(key)?, self.lower(value)?)),
								_ => unreachable!(),
							})
							.collect::<Result<_, StableLoweringError>>()?,
					)
				};
				match self.construction_prototype(expr)? {
					Some(prototype) => HirExpr::WithPrototype {
						value: Box::new(value),
						prototype,
					},
					None => value,
				}
			}
			StableExprKind::MemberAccess {
				parent,
				member,
				optional: true,
			} => {
				let name = if let Some(target) = self.target(expr) {
					self.context.member_name(target)?.as_str().into()
				} else {
					member.clone()
				};
				return self.lower_optional_chain(expr, parent, |_, payload| {
					Ok(HirExpr::Field {
						recv: Box::new(payload),
						name,
					})
				});
			}
			StableExprKind::MemberAccess {
				parent,
				member,
				optional: false,
			} => {
				if self.definitely_transfers(parent) {
					return self.lower(parent);
				}
				if let Some(dispatch) = self.annotations.dispatch(self.id(expr)) {
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
						prototype: self.construction_prototype(expr)?,
					});
				}
				if self.annotations.is_direct_namespace_member(self.id(expr))
					&& let Some(target) = self.target(expr)
				{
					if matches!(target.key, crate::DeclarationKey::TopLevel { .. }) {
						self.demand_external(target)?;
						return self.generic_callable_adapter(
							self.id(expr),
							HirExpr::Local(self.context.binding_name(target)?.as_str().into()),
						);
					}
					if matches!(target.key, crate::DeclarationKey::Member { .. }) {
						let runtime = self.context.runtime_definition(target)?;
						let owner = match &runtime.placement {
							crate::RuntimePlacement::TopLevel => {
								self.demand_external(target)?;
								return self.generic_callable_adapter(
									self.id(expr),
									HirExpr::Local(self.context.binding_name(target)?.as_str().into()),
								);
							}
							crate::RuntimePlacement::Attached { owner, .. } => owner,
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
								if implementation.binders.is_empty()
									&& is_concrete_runtime_type(&implementation.self_type)
									&& matches!(&implementation.self_type, InterfaceType::Named { positional, named, .. } if !positional.is_empty() || !named.is_empty())
								{
									self.demand_external(target)?;
									return self.generic_callable_adapter(
										self.id(expr),
										HirExpr::Field {
											recv: Box::new(self.runtime_type_object(&implementation.self_type)?),
											name: self.context.member_name(target)?.as_str().into(),
										},
									);
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
							return self.generic_callable_adapter(
								self.id(expr),
								HirExpr::Local(self.context.binding_name(target)?.as_str().into()),
							);
						};
						return self.generic_callable_adapter(
							self.id(expr),
							HirExpr::Field {
								recv: Box::new(HirExpr::Local(
									self.context.binding_name(&shell)?.as_str().into(),
								)),
								name: self.context.member_name(target)?.as_str().into(),
							},
						);
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
				if let StableExprKind::MemberAccess {
					parent,
					optional: true,
					..
				} = &func.kind
				{
					let dispatch = self.dispatch(expr)?.clone();
					return self.lower_optional_chain(expr, parent, |lowerer, payload| {
						let mut operation = lowerer.lower_dispatch(
							&dispatch,
							parent,
							args.iter().map(|argument| &argument.value).collect(),
						)?;
						Self::replace_dispatch_receiver(&mut operation, payload);
						Ok(Self::activation_dispatch_call(
							lowerer.id(expr),
							&dispatch,
							operation,
						))
					});
				}
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
								.clone()
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
						prototype: self.construction_prototype(expr)?,
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
						let plan = self
							.annotations
							.struct_construction(self.id(expr))
							.ok_or_else(|| self.missing_annotation(self.id(expr), "struct construction plan"))?;
						if plan.definition != *target {
							return Err(invalid(
								&self.artifact.definition,
								"struct construction plan has the wrong exact owner",
							));
						}
						let (stmts, values, source) = self.with_scope(|| {
							let mut stmts = Vec::with_capacity(args.len());
							let mut values = HashMap::with_capacity(args.len());
							let mut source = None;
							for (index, argument) in args.iter().enumerate() {
								let temporary = self.declare(&format!("$struct_arg_{index}").into());
								stmts.push(HirStmt::Let {
									name: temporary.clone(),
									value: self.lower(&argument.value)?,
									cleanup: None,
								});
								if argument.spread {
									source = Some(temporary);
								} else if let Some((field, _)) = plan
									.explicit_fields
									.iter()
									.find(|(_, value)| *value == argument.value.id)
								{
									values.insert(field.clone(), temporary);
								}
							}
							Ok((stmts, values, source))
						})?;
						let fields = shell
							.fields
							.iter()
							.filter_map(|field| {
								values
									.get(&field.id)
									.map(|value| (field.name.clone(), HirExpr::Local(value.clone())))
							})
							.collect::<Vec<_>>();
						let class = self.context.binding_name(target)?.as_str().into();
						let prototype = self.construction_prototype(expr)?;
						let construction = match &plan.mode {
							crate::runtime::StableStructConstructionMode::Fresh => HirExpr::StructFresh {
								class,
								fields,
								prototype,
							},
							crate::runtime::StableStructConstructionMode::CloneUpdate { .. } => {
								let source = source.ok_or_else(|| {
									invalid(
										&self.artifact.definition,
										"struct clone has no evaluated source",
									)
								})?;
								HirExpr::StructCloneUpdate {
									class,
									source: Box::new(HirExpr::Local(source)),
									replacements: fields,
									prototype,
								}
							}
						};
						return Ok(HirExpr::Block {
							stmts,
							tail: Some(Box::new(construction)),
						});
					}
				}
				if let StableExprKind::MemberAccess { parent, .. } = &func.kind
					&& let Some(dispatch) = self.annotations.dispatch(self.id(expr))
				{
					let lowered = self.lower_dispatch(
						dispatch,
						parent,
						args.iter().map(|arg| &arg.value).collect(),
					)?;
					let lowered = if matches!(
						dispatch,
						crate::StableDispatch::SelectedImplementation { .. }
							| crate::StableDispatch::InterfaceDefault { .. }
					) {
						lowered
					} else {
						self.append_hidden_arguments(self.id(expr), lowered)?
					};
					return Ok(Self::activation_dispatch_call(
						self.id(expr),
						dispatch,
						lowered,
					));
				}
				if let Some((parameter, interface, member_definition)) =
					self.annotations.generic_namespaced_call(self.id(expr))
					&& let StableExprKind::MemberAccess { member, .. } = &func.kind
				{
					self.demand_receiverless_implementations(interface, member_definition)?;
					let mut lowered_args = args
						.iter()
						.map(|arg| self.lower(&arg.value))
						.collect::<Result<Vec<_>, _>>()?;
					if let Some(hidden) = self.annotations.generic_call_arguments(self.id(expr)) {
						for argument in hidden.iter() {
							lowered_args.push(match argument {
								crate::runtime::RuntimeTypeArgument::Canonical(type_) => {
									self.runtime_type_object(type_)?
								}
								crate::runtime::RuntimeTypeArgument::Erased => {
									return Err(StableLoweringError::Unsupported {
										definition: self.artifact.definition.clone(),
										node: Some(self.id(expr)),
										feature: "erased runtime type argument required by receiverless dispatch"
											.into(),
									});
								}
							});
						}
					}
					let parameter = self
						.all_type_parameters
						.get(parameter as usize)
						.ok_or_else(|| {
							invalid(
								&self.artifact.definition,
								"receiverless generic slot is out of bounds",
							)
						})?;
					let receiver = self.runtime_type_object(&InterfaceType::Generic(parameter.clone()))?;
					return Ok(Self::activation_call(
						self.id(expr),
						HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(receiver),
								name: member.clone(),
							}),
							args: lowered_args,
						},
					));
				}
				let mut generated_call = true;
				if let Some(target) = self.target(func) {
					let target_artifact = self.context.runtime_definition(target)?;
					if matches!(target_artifact.payload, crate::RuntimePayload::External(_)) {
						let abi = exact_external_abi(self.context, target, None)?;
						if matches!(&abi.callable, crate::ExternalCallable::Deferred) {
							return Err(invalid(
								target,
								"direct external call targets a deferred external",
							));
						}
						generated_call = matches!(&abi.callable, crate::ExternalCallable::Linked { .. });
						let (source_arity, binder_arity, receiver_arity) =
							external_callable_shape(self.context, target, &abi)?;
						if receiver_arity != 0 || args.len() != source_arity {
							return Err(StableLoweringError::ShapeDrift {
								definition: target.clone(),
								reason:
									"direct external call source arity disagrees with its exact definition shape"
										.into(),
							});
						}
						let hidden_arity = self
							.annotations
							.generic_call_arguments(self.id(expr))
							.map(<[crate::runtime::RuntimeTypeArgument]>::len)
							.unwrap_or_default();
						if hidden_arity != binder_arity {
							return Err(StableLoweringError::ShapeDrift {
								definition: target.clone(),
								reason:
									"direct external call binder arity disagrees with its canonical type arguments"
										.into(),
							});
						}
					}
					if !self.record_call(target)? {
						self.record_unresolved_call(UnresolvedRuntimeCall::CallableValue(target.clone()));
					}
				} else {
					self.record_unresolved_call(UnresolvedRuntimeCall::DynamicCallee);
				}
				let lowered_args = args
					.iter()
					.map(|arg| self.lower(&arg.value))
					.collect::<Result<Vec<_>, _>>()?;
				let lowered = self.append_hidden_arguments(
					self.id(expr),
					HirExpr::Call {
						callee: Box::new(self.lower(func)?),
						args: lowered_args,
					},
				)?;
				if generated_call {
					Self::activation_call(self.id(expr), lowered)
				} else {
					lowered
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
					let (lhs_name, lhs, lowered_rhs) = self.with_scope(|| {
						let lhs_name = self.declare(&EcoString::from("$pipe"));
						Ok((lhs_name, self.lower(lhs)?, self.lower(rhs)?))
					})?;
					let call_args = vec![HirExpr::Local(lhs_name.clone())];
					let mut metadata_source = rhs;
					while let StableExprKind::Grouped(inner) = &metadata_source.kind {
						metadata_source = inner;
					}
					let call = HirExpr::Call {
						callee: Box::new(lowered_rhs),
						args: call_args,
					};
					let call = if matches!(metadata_source.kind, StableExprKind::Call { .. }) {
						call
					} else {
						self.append_hidden_arguments(self.id(rhs), call)?
					};
					let call = Self::activation_call(self.id(expr), call);
					return Ok(HirExpr::Block {
						stmts: vec![HirStmt::Let {
							name: lhs_name.clone(),
							value: lhs,
							cleanup: None,
						}],
						tail: Some(Box::new(call)),
					});
				}
				let dispatch = self.dispatch(expr)?;
				if matches!(op, BinaryOperator::In | BinaryOperator::NotIn) {
					let (lhs_name, lhs_value, operation) = self.with_scope(|| {
						let lhs_name = self.declare(&EcoString::from("$member"));
						let lhs_value = self.lower(lhs)?;
						let mut operation = self.lower_dispatch(dispatch, rhs, vec![lhs])?;
						Self::replace_dispatch_argument(&mut operation, HirExpr::Local(lhs_name.clone()));
						Ok((lhs_name, lhs_value, operation))
					})?;
					return Ok(HirExpr::Block {
						stmts: vec![HirStmt::Let {
							name: lhs_name,
							value: lhs_value,
							cleanup: None,
						}],
						tail: Some(Box::new(operation)),
					});
				}
				if let crate::StableDispatch::Builtin {
					method,
					category: crate::BuiltinDispatch::StructuralEquality,
				} = dispatch
				{
					return Ok(HirExpr::Call {
						callee: Box::new(HirExpr::Field {
							recv: Box::new(self.lower(lhs)?),
							name: method.clone(),
						}),
						args: vec![self.lower(rhs)?],
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
					mode: self.range_mode(expr),
					lhs: Box::new(self.lower(lhs)?),
					rhs: Box::new(self.lower(rhs)?),
				}
			}
			StableExprKind::PrefixOp { op, value } => {
				if self.definitely_transfers(value) {
					return self.lower(value);
				}
				if *op == PrefixOperator::Negate
					&& matches!(&value.kind, StableExprKind::Int(magnitude) if *magnitude == 1_u64 << 63)
					&& matches!(self.builtin_result(expr)?, BuiltinResult::Int)
				{
					return Ok(HirExpr::Int(i64::MIN));
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
			StableExprKind::Block { body, label } => {
				if label.is_some() {
					let target = self.next_block_target.get();
					self.next_block_target.set(target + 1);
					self.with_block_target(self.id(expr), target, || {
						self
							.lower_block(body, true)
							.map(|body| HirExpr::LabeledBlock {
								target,
								body: Box::new(body),
							})
					})?
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
				then: Box::new(self.lower(then)?),
				otherwise: otherwise
					.as_ref()
					.map(|value| self.lower(value).map(Box::new))
					.transpose()?,
			},
			StableExprKind::Closure { params, body, .. } => self.with_scope(|| {
				let params = params
					.iter()
					.map(|param| pattern_name(&param.pattern).map(|name| self.declare(name)))
					.collect::<Result<_, _>>()?;
				let body = self.with_deferred(|| self.lower_function_body(body))?;
				Ok(HirExpr::Closure {
					params,
					body: Box::new(body),
				})
			})?,
			StableExprKind::AsyncBlock(_) | StableExprKind::Await(_) => unreachable!(),
			StableExprKind::Return { value, .. } => HirExpr::Block {
				stmts: vec![HirStmt::Return {
					value: value.as_ref().map(|value| self.lower(value)).transpose()?,
					target: self.return_target(expr),
				}],
				tail: None,
			},
			StableExprKind::Break { value, .. } => HirExpr::Break {
				target: self.resolve_loop_target(expr, "break outside a loop")?,
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
			StableExprKind::Continue { replacements, .. } => {
				let target = self.resolve_loop_target(expr, "continue outside a loop")?;
				if self.state_loop_targets.borrow().contains(&target) {
					HirExpr::ContinueTransition {
						target,
						replacements: replacements
							.iter()
							.map(|replacement| Ok((replacement.name.clone(), self.lower(&replacement.value)?)))
							.collect::<Result<_, StableLoweringError>>()?,
					}
				} else {
					HirExpr::Continue { target }
				}
			}
			StableExprKind::Echo { operand, keyword } => HirExpr::Echo {
				operand: Box::new(self.lower(operand)?),
				site: nymph_hir::hir::EchoSite {
					module: self.artifact.definition.module.path.clone(),
					start: u32::try_from(keyword.start).unwrap_or(u32::MAX),
					end: u32::try_from(keyword.end).unwrap_or(u32::MAX),
				},
			},
			StableExprKind::IndexAccess {
				parent,
				index,
				optional: false,
			} if matches!(index.kind, StableExprKind::Range(_)) => self.lower_slice(
				expr,
				index,
				self.lower(parent)?,
				matches!(peel_mut(&self.ty(parent)?), InterfaceType::String),
			)?,
			StableExprKind::IndexAccess {
				parent,
				index,
				optional: true,
			} => {
				let payload_type = self.optional_chain_payload_type(parent)?;
				return self.lower_optional_chain(expr, parent, |lowerer, payload| {
					if matches!(index.kind, StableExprKind::Range(_)) {
						return lowerer.lower_slice(
							expr,
							index,
							payload,
							matches!(peel_mut(&payload_type), InterfaceType::String),
						);
					}
					Ok(match peel_mut(&payload_type) {
						InterfaceType::Map(..) => HirExpr::MapGet {
							recv: Box::new(payload),
							key: Box::new(lowerer.lower(index)?),
						},
						InterfaceType::List(_) if lowerer.annotations.dispatch(lowerer.id(expr)).is_some() => {
							let mut operation =
								lowerer.lower_dispatch(lowerer.dispatch(expr)?, parent, vec![index])?;
							Self::replace_dispatch_receiver(&mut operation, payload);
							operation
						}
						InterfaceType::List(_) => HirExpr::ListRead {
							recv: Box::new(payload),
							index: Box::new(lowerer.lower(index)?),
							mode: lowerer.range_mode(expr),
						},
						InterfaceType::Tuple(_) | InterfaceType::String => HirExpr::Index {
							recv: Box::new(payload),
							index: Box::new(lowerer.lower(index)?),
							mode: lowerer.range_mode(expr),
						},
						_ => {
							let mut operation =
								lowerer.lower_dispatch(lowerer.dispatch(expr)?, parent, vec![index])?;
							Self::replace_dispatch_receiver(&mut operation, payload);
							operation
						}
					})
				});
			}
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
				InterfaceType::List(_) if self.annotations.dispatch(self.id(expr)).is_some() => {
					self.lower_dispatch(self.dispatch(expr)?, parent, vec![index])?
				}
				InterfaceType::List(_) => HirExpr::ListRead {
					recv: Box::new(self.lower(parent)?),
					index: Box::new(self.lower(index)?),
					mode: self.range_mode(expr),
				},
				InterfaceType::Tuple(_) | InterfaceType::String => HirExpr::Index {
					recv: Box::new(self.lower(parent)?),
					index: Box::new(self.lower(index)?),
					mode: self.range_mode(expr),
				},
				_ => self.lower_dispatch(self.dispatch(expr)?, parent, vec![index])?,
			},
			StableExprKind::For {
				variable,
				iterable,
				body,
				..
			} => self.lower_iteration(
				self.id(expr),
				variable,
				iterable,
				body,
				self.loop_option(expr)?,
			)?,
			StableExprKind::StateLoop { bindings, body, .. } => self.with_scope(|| {
				let target = self.next_loop();
				let mut lowered_bindings = Vec::with_capacity(bindings.len());
				for binding in bindings.iter() {
					let source = binding.value.id;
					let value = self.lower(&binding.value)?;
					let name = self.declare(&binding.name);
					let cleanup = if binding.managed {
						let dispatch = self.annotations.managed_cleanup(source).ok_or_else(|| {
							invalid(
								&self.artifact.definition,
								"managed state binding has no stable cleanup fact",
							)
						})?;
						Some(self.lower_dispatch_value(source, dispatch, HirExpr::Local(name.clone()))?)
					} else {
						None
					};
					lowered_bindings.push(nymph_hir::hir::HirStateBinding {
						name,
						value,
						cleanup,
					});
				}
				self.state_loop_targets.borrow_mut().insert(target);
				let body = self.lower_loop_branch(self.id(expr), target, body)?;
				self.state_loop_targets.borrow_mut().remove(&target);
				Ok(HirExpr::StateLoop {
					target,
					bindings: lowered_bindings,
					body: Box::new(body),
				})
			})?,
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
					prototype: self.construction_prototype(expr)?,
				}
			}
			StableExprKind::Match { value, arms } => return self.lower_match(value, arms),
			StableExprKind::PatternOp { lhs, op, rhs } => {
				let scrutinee = Box::new(self.lower(lhs)?);
				// Pattern-operator bindings are scoped to the test and must not replace
				// an outer source-name mapping used by following expressions. Lower the
				// source first to preserve source-order name allocation in stable lowering.
				let pat = self.with_scope(|| self.lower_pattern(rhs))?;
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
			StableExprKind::PostfixOp { value, .. } => self.lower_propagation(expr, value)?,
			StableExprKind::TypeOp { lhs, .. } if self.definitely_transfers(lhs) => self.lower(lhs)?,
			StableExprKind::TypeOp { lhs, .. } => match self.dispatch(expr)? {
				crate::StableDispatch::Builtin { .. } => self.lower_cast(expr, lhs)?,
				dispatch => self.lower_dispatch(dispatch, lhs, vec![])?,
			},
			StableExprKind::String(parts) => self.lower_string(parts)?,
		})
	}

	fn lower_task_spawn(
		&self,
		expr: &StableExpr,
		func: &StableExpr,
		args: &[crate::StableCallArg],
	) -> Result<Option<HirExpr>, StableLoweringError> {
		let StableExprKind::MemberAccess { parent, member, .. } = &func.kind else {
			return Ok(None);
		};
		if member != "spawn"
			|| !args.is_empty()
			|| !matches!(
				self.annotations.type_of(expr.id),
				Some(InterfaceType::Handle(_))
			) {
			return Ok(None);
		}
		Ok(Some(HirExpr::TaskOperation {
			operation: nymph_hir::hir::HirTaskOperation::Spawn,
			operands: vec![self.lower(parent)?],
		}))
	}

	fn is_task_special(&self, expr: &StableExpr) -> bool {
		match &expr.kind {
			StableExprKind::AsyncBlock(_) | StableExprKind::Await(_) => true,
			StableExprKind::Call { func, args } => {
				matches!(&func.kind, StableExprKind::MemberAccess { member, .. } if member == "spawn")
					&& args.is_empty()
					&& matches!(
						self.annotations.type_of(expr.id),
						Some(InterfaceType::Handle(_))
					)
			}
			_ => false,
		}
	}

	fn lower_task_special(&self, expr: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		let lowered = match &expr.kind {
			StableExprKind::AsyncBlock(body) => self.lower_async_block(body)?,
			StableExprKind::Await(value) => self.lower_await(value)?,
			StableExprKind::Call { func, args } => self
				.lower_task_spawn(expr, func, args)?
				.expect("task-special call is a spawn operation"),
			_ => unreachable!(),
		};
		Ok(lowered)
	}

	fn lower_async_block(&self, body: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		Ok(HirExpr::TaskRecipe {
			body: Box::new(self.with_deferred(|| self.lower_function_body(body))?),
			context: nymph_hir::hir::HirTaskContext::Nested,
		})
	}

	fn lower_await(&self, value: &StableExpr) -> Result<HirExpr, StableLoweringError> {
		let operation = if matches!(
			self.annotations.type_of(value.id),
			Some(InterfaceType::Handle(_))
		) {
			nymph_hir::hir::HirTaskOperation::Observe
		} else {
			nymph_hir::hir::HirTaskOperation::Drive
		};
		Ok(HirExpr::TaskOperation {
			operation,
			operands: vec![self.lower(value)?],
		})
	}

	fn replace_dispatch_argument(expr: &mut HirExpr, argument: HirExpr) {
		match expr {
			HirExpr::Call { callee, args } | HirExpr::ActivationCall { callee, args, .. } => {
				let index = usize::from(!matches!(callee.as_ref(), HirExpr::Field { .. }));
				args[index] = argument;
			}
			HirExpr::StaticEnumDispatch { args, .. } => args[0] = argument,
			HirExpr::ExternCall { args, .. } => {
				args[1] = argument;
			}
			HirExpr::BoundDispatch {
				argument: found, ..
			} => **found = argument,
			HirExpr::Binary { rhs, .. } => **rhs = argument,
			other => panic!("binary dispatch lowered to an unexpected HIR shape: {other:?}"),
		}
	}

	fn replace_dispatch_receiver(expr: &mut HirExpr, receiver: HirExpr) {
		match expr {
			HirExpr::Call { callee, .. } | HirExpr::ActivationCall { callee, .. }
				if matches!(callee.as_ref(), HirExpr::Field { .. }) =>
			{
				let HirExpr::Field { recv, .. } = callee.as_mut() else {
					unreachable!()
				};
				**recv = receiver;
			}
			HirExpr::Call { args, .. }
			| HirExpr::ActivationCall { args, .. }
			| HirExpr::ExternCall { args, .. } => args[0] = receiver,
			HirExpr::StaticEnumDispatch {
				receiver: found, ..
			} => **found = receiver,
			HirExpr::BoundDispatch {
				receiver: found, ..
			} => **found = receiver,
			HirExpr::ListRead { recv, .. } | HirExpr::Index { recv, .. } => **recv = receiver,
			HirExpr::Binary { lhs, .. } => **lhs = receiver,
			other => panic!("binary dispatch lowered to an unexpected HIR shape: {other:?}"),
		}
	}

	fn lower_dispatch(
		&self,
		dispatch: &crate::StableDispatch,
		receiver: &StableExpr,
		arguments: Vec<&StableExpr>,
	) -> Result<HirExpr, StableLoweringError> {
		if let crate::StableDispatch::GenericBound { interface, member } = dispatch
			&& (!matches!(receiver.kind, StableExprKind::This)
				|| self
					.implementation_slots
					.is_none_or(|slots| slots.target(member).is_none()))
		{
			return self.lower_generic_bound(interface, member, receiver, arguments);
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
				..
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
				..
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
			let receiver = self.lower(receiver)?;
			let arguments = arguments
				.into_iter()
				.map(|arg| self.lower(arg))
				.collect::<Result<Vec<_>, _>>()?;
			return match &abi.callable {
				crate::ExternalCallable::Linked { adapter }
					if adapter.module == "std/collections/list"
						&& matches!(
							adapter.symbol.as_str(),
							"appended" | "replaced" | "slice" | "insert" | "remove" | "clear" | "push" | "splice"
						) =>
				{
					let mut args = vec![receiver];
					args.extend(arguments);
					external_call_expr(&member, &abi, &args, vec![None; args.len()], None)
				}
				crate::ExternalCallable::Linked { .. } => {
					if let Some(implementation) = &implementation
						&& shellless_implementation_member(self.context, &member, implementation)?
					{
						let mut args = vec![receiver];
						args.extend(arguments);
						Ok(HirExpr::Call {
							callee: Box::new(HirExpr::Local(
								self.context.binding_name(&member)?.as_str().into(),
							)),
							args,
						})
					} else {
						Ok(HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(receiver),
								name: self.context.member_name(&member)?.as_str().into(),
							}),
							args: arguments,
						})
					}
				}
				crate::ExternalCallable::Native(_) => {
					let mut args = vec![receiver];
					args.extend(arguments);
					external_call_expr(&member, &abi, &args, vec![None; args.len()], None)
				}
				crate::ExternalCallable::Deferred => Err(invalid(
					&self.artifact.definition,
					"external dispatch target is deferred",
				)),
			};
		}
		if let Some(ref implementation) = implementation
			&& shellless_implementation_member(self.context, &member, implementation)?
		{
			let mut args = vec![self.lower(receiver)?];
			args.extend(
				arguments
					.into_iter()
					.map(|arg| self.lower(arg))
					.collect::<Result<Vec<_>, _>>()?,
			);
			self.append_selected_call_arguments(
				dispatch,
				&member,
				Some(self.id(receiver)),
				true,
				&mut args,
			)?;
			if matches!(dispatch, crate::StableDispatch::GenericBound { .. }) {
				args.extend(
					self
						.type_parameters
						.iter()
						.take(self.implementation_hidden)
						.enumerate()
						.map(|(index, _)| HirExpr::Local(format!("$type${index}").into())),
				);
			}
			return Ok(Self::activation_dispatch_call(
				self.id(receiver),
				dispatch,
				HirExpr::Call {
					callee: Box::new(HirExpr::Local(
						self.context.binding_name(&member)?.as_str().into(),
					)),
					args,
				},
			));
		}
		let name: EcoString = self.context.member_name(&member)?.as_str().into();
		let mut args = arguments
			.into_iter()
			.map(|arg| self.lower(arg))
			.collect::<Result<Vec<_>, _>>()?;
		self.append_selected_call_arguments(
			dispatch,
			&member,
			Some(self.id(receiver)),
			false,
			&mut args,
		)?;
		if let Some(implementation) = implementation
			&& matches!(
				implementation.key,
				crate::DeclarationKey::TopLevel {
					category: crate::DeclarationCategory::Enum,
					..
				}
			) {
			let source = self.id(receiver);
			let mut call_args = vec![self.lower(receiver)?];
			let receiver = call_args.remove(0);
			return Ok(Self::activation_dispatch_call(
				source,
				dispatch,
				HirExpr::StaticEnumDispatch {
					owner: self.context.binding_name(&implementation)?.as_str().into(),
					method: name,
					receiver: Box::new(receiver),
					args,
					mode: nymph_hir::hir::HirCallMode::Push,
					source: 0,
				},
			));
		}
		Ok(Self::activation_dispatch_call(
			self.id(receiver),
			dispatch,
			HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(self.lower(receiver)?),
					name,
				}),
				args,
			},
		))
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
			**dispatched_receiver = receiver_value;
			return Ok(Self::activation_call(
				self.id(receiver),
				HirExpr::Call {
					callee: Box::new(HirExpr::Closure {
						params: vec![receiver_name],
						body: Box::new(HirExpr::Closure {
							params,
							body: Box::new(body),
						}),
					}),
					args: vec![self.lower(receiver)?],
				},
			));
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
				..
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
				..
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
			match &abi.callable {
				crate::ExternalCallable::Linked { .. } => {
					if let Some(implementation) = &implementation
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
					}
				}
				crate::ExternalCallable::Native(_) => {
					let mut args = vec![receiver_value];
					args.extend(arguments);
					external_call_expr(&member, &abi, &args, vec![None; args.len()], None)?
				}
				crate::ExternalCallable::Deferred => {
					return Err(invalid(
						&member,
						"generic callable value targets a deferred external",
					));
				}
			}
		} else if let Some(implementation) = &implementation
			&& shellless_implementation_member(self.context, &member, implementation)?
		{
			let mut args = vec![receiver_value];
			args.extend(arguments);
			self.append_selected_call_arguments(
				dispatch,
				&member,
				Some(self.id(expr)),
				true,
				&mut args,
			)?;
			HirExpr::Call {
				callee: Box::new(HirExpr::Local(
					self.context.binding_name(&member)?.as_str().into(),
				)),
				args,
			}
		} else {
			let mut arguments = arguments;
			self.append_selected_call_arguments(
				dispatch,
				&member,
				Some(self.id(expr)),
				false,
				&mut arguments,
			)?;
			HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(receiver_value),
					name: self.context.member_name(&member)?.as_str().into(),
				}),
				args: arguments,
			}
		};
		let body = if matches!(
			dispatch,
			crate::StableDispatch::SelectedImplementation { .. }
				| crate::StableDispatch::InterfaceDefault { .. }
		) {
			body
		} else {
			self.append_hidden_arguments(self.id(expr), body)?
		};
		let body = Self::activation_dispatch_call(self.id(expr), dispatch, body);
		Ok(Self::activation_call(
			self.id(receiver),
			HirExpr::Call {
				callee: Box::new(HirExpr::Closure {
					params: vec![receiver_name],
					body: Box::new(HirExpr::Closure {
						params,
						body: Box::new(body),
					}),
				}),
				args: vec![self.lower(receiver)?],
			},
		))
	}

	fn append_selected_call_arguments(
		&self,
		dispatch: &crate::StableDispatch,
		member: &DefinitionId,
		node: Option<crate::BodyNodeId>,
		include_implementation: bool,
		args: &mut Vec<HirExpr>,
	) -> Result<(), StableLoweringError> {
		let (implementation_arguments, method_arguments) = match dispatch {
			crate::StableDispatch::SelectedImplementation {
				implementation_arguments,
				method_arguments,
				..
			}
			| crate::StableDispatch::InterfaceDefault {
				implementation_arguments,
				method_arguments,
				..
			} => (implementation_arguments.as_ref(), method_arguments.as_ref()),
			_ => return Ok(()),
		};
		let required = self.required_receiverless_slots(member)?;
		let selected_implementation_arguments = if include_implementation {
			implementation_arguments
		} else {
			&[]
		};
		if selected_implementation_arguments
			.iter()
			.enumerate()
			.any(|(index, argument)| {
				required.contains(&index) && matches!(argument, crate::runtime::RuntimeTypeArgument::Erased)
			}) {
			return Err(StableLoweringError::Unsupported {
				definition: self.artifact.definition.clone(),
				node,
				feature: "erased implementation type argument required by receiverless dispatch".into(),
			});
		}
		if method_arguments
			.iter()
			.enumerate()
			.any(|(index, argument)| {
				required.contains(&index) && matches!(argument, crate::runtime::RuntimeTypeArgument::Erased)
			}) {
			return Err(StableLoweringError::Unsupported {
				definition: self.artifact.definition.clone(),
				node,
				feature: "erased runtime type argument required by receiverless dispatch".into(),
			});
		}
		args.extend(
			selected_implementation_arguments
				.iter()
				.chain(method_arguments)
				.map(|argument| match argument {
					crate::runtime::RuntimeTypeArgument::Canonical(type_) => self.runtime_type_object(type_),
					crate::runtime::RuntimeTypeArgument::Erased => Ok(HirExpr::Undefined),
				})
				.collect::<Result<Vec<_>, _>>()?,
		);
		Ok(())
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
			return Ok(Self::activation_call(
				self.id(receiver),
				HirExpr::Call {
					callee: Box::new(HirExpr::Field {
						recv: Box::new(self.lower(receiver)?),
						name: method,
					}),
					args: arguments
						.into_iter()
						.map(|argument| self.lower(argument))
						.collect::<Result<_, _>>()?,
				},
			));
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
		let receiver_type = (!arguments.is_empty())
			.then(|| self.ty(receiver))
			.transpose()?;
		let argument_type = arguments
			.first()
			.map(|argument| self.ty(argument))
			.transpose()?;
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
			if receiver_type
				.as_ref()
				.and_then(stable_runtime_tag)
				.is_some_and(|tag| tag != receiver_tag)
				|| argument_type
					.as_ref()
					.and_then(stable_runtime_tag)
					.is_some_and(|tag| tag != argument_tag)
				|| matches!(
					(&receiver_type, &argument_type),
					(Some(InterfaceType::Generic(receiver)), Some(InterfaceType::Generic(argument)))
						if receiver == argument && receiver_tag != argument_tag
				) {
				continue;
			}
			let _ = self.record_call(&slot.member_id)?;
			let body = self.context.runtime_definition(&slot.member_id)?;
			let target = match &body.payload {
				crate::RuntimePayload::External(abi) => match &abi.callable {
					crate::ExternalCallable::Linked { adapter } => HirBoundDispatchTarget::Extern {
						module: Box::leak(adapter.module.to_string().into_boxed_str()),
						symbol: Box::leak(adapter.symbol.to_string().into_boxed_str()),
						call_mode: match abi.call_mode {
							crate::ExternalCallMode::Ordinary => nymph_hir::hir::ExternalCallMode::Ordinary,
							crate::ExternalCallMode::Cancellable => nymph_hir::hir::ExternalCallMode::Cancellable,
						},
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
				hidden_arguments: vec![],
				cases,
				mode: nymph_hir::hir::HirCallMode::Push,
				source: self.id(receiver).0,
			})
		} else {
			Ok(HirExpr::BoundDispatch {
				interface: interface_shape.name,
				method,
				receiver: Box::new(self.lower(receiver)?),
				argument: Box::new(self.lower(arguments[0])?),
				hidden_arguments: vec![],
				cases,
				mode: nymph_hir::hir::HirCallMode::Push,
				source: self.id(receiver).0,
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
					.iteration(self.id(value))
					.ok_or_else(|| self.missing_annotation(self.id(value), "spread iteration"))?;
				let source = self.lower(value)?;
				let (it, next, next_dispatch, iteration) = match iteration {
					crate::RuntimeIteration::Direct {
						iterator_interface,
						next,
						next_dispatch,
						iteration,
					} => {
						if next_dispatch.is_none() {
							self.record_unresolved_call(UnresolvedRuntimeCall::IteratorNext {
								interface: iterator_interface.clone(),
								member: next.clone(),
							});
						}
						(source, next, next_dispatch, iteration)
					}
					crate::RuntimeIteration::ViaIter {
						iter,
						iterable_interface,
						iter_interface_member,
						iterator_interface,
						next,
						next_dispatch,
						iteration,
					} => {
						let next_is_shellless = next_dispatch
							.as_ref()
							.map(|dispatch| self.iteration_dispatch_is_shellless(dispatch))
							.transpose()?
							.unwrap_or(false);
						if !next_is_shellless {
							self.demand_concrete_iteration_next(
								iter,
								iterable_interface,
								iter_interface_member,
								iterator_interface,
								next,
							)?;
						}
						let source = self.lower_iter_dispatch(
							value,
							iter,
							iterable_interface,
							iter_interface_member,
							source,
						)?;
						(source, next, next_dispatch, iteration)
					}
				};
				let next_call = if let Some(dispatch) = next_dispatch {
					self.lower_iteration_next(self.id(value), dispatch, next, HirExpr::Local("$it".into()))?
				} else {
					Self::activation_call(
						self.id(value),
						HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(HirExpr::Local("$it".into())),
								name: self.context.member_name(next)?.as_str().into(),
							}),
							args: vec![],
						},
					)
				};
				Ok(drain_spread(
					it,
					next_call,
					nymph_hir::hir::HirIterationAbi {
						enum_name: self
							.context
							.binding_name(&iteration.iteration)?
							.as_str()
							.into(),
						done: self.context.member_name(&iteration.done)?.as_str().into(),
						yield_: self.context.member_name(&iteration.yield_)?.as_str().into(),
						item: self
							.context
							.member_name(&iteration.yield_item)?
							.as_str()
							.into(),
						next: self
							.context
							.member_name(&iteration.yield_next)?
							.as_str()
							.into(),
					},
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
					result.push(HirExpr::ProtocolDisplay(Box::new(self.lower(value)?)));
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
			(InterfaceType::Float, InterfaceType::Int) => Some(ScalarCastKind::CheckedToInt),
			(InterfaceType::Int, InterfaceType::UInt) => Some(ScalarCastKind::IntToUInt),
			(InterfaceType::Float, InterfaceType::UInt) => Some(ScalarCastKind::CheckedToUInt),
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
			mode: self.range_mode(expr),
		}))
	}
	fn lower_pattern(&self, pattern: &StablePattern) -> Result<HirPat, StableLoweringError> {
		let variant = self.annotations.pattern_variant(pattern.id);
		Ok(match &pattern.kind {
			StablePatternKind::Placeholder => HirPat::Wildcard,
			StablePatternKind::Int(v) => HirPat::Lit(HirLit::Int(*v)),
			StablePatternKind::UInt(v) => HirPat::Lit(HirLit::UInt(*v)),
			StablePatternKind::Float(v) => HirPat::Lit(HirLit::Num(v.into_inner(), NumKind::Float)),
			StablePatternKind::Boolean(v) => HirPat::Lit(HirLit::Bool(*v)),
			StablePatternKind::Char(v) => HirPat::Lit(HirLit::Char(*v)),
			StablePatternKind::String(parts) => HirPat::Lit(HirLit::Str(string_pattern(parts))),
			StablePatternKind::Grouped(inner) => self.lower_pattern(inner)?,
			StablePatternKind::Binding { name, inner } if variant.is_none() => HirPat::Binding {
				name: self.declare(name),
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
							rest = Some(name.as_ref().map(|name| self.declare(name)))
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
					rest = Some(name.as_ref().map(|name| self.declare(name)));
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
						.pattern_variant(*id)
						.map(|variant| self.variant_pattern(variant, vec![]))
						.transpose()?
						.unwrap_or_else(|| HirPat::Binding {
							name: self.declare(name),
							sub: None,
						});
					let exact = self
						.annotations
						.positional_field(*id)
						.map(|field| field.name.clone())
						.unwrap_or_else(|| name.clone());
					result.push((exact, pattern));
				}
				StableStructPatternField::Positional { id: pid, pattern } => {
					let exact = self
						.annotations
						.positional_field(*pid)
						.ok_or_else(|| self.missing_annotation(crate::BodyNodeId(pid.0), "positional field"))?
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
	fn runtime_type_object(&self, type_: &InterfaceType) -> Result<HirExpr, StableLoweringError> {
		let resolved = substitute_type_parameters(type_, self.type_substitutions);
		let type_ = &resolved;
		let (binding, box_runtime, is_enum, argument_types): (
			EcoString,
			bool,
			bool,
			Vec<&InterfaceType>,
		) = match type_ {
			InterfaceType::Int => ("NInt".into(), true, false, vec![]),
			InterfaceType::UInt => ("NUint".into(), true, false, vec![]),
			InterfaceType::Float => ("NFloat".into(), true, false, vec![]),
			InterfaceType::Char => ("NChar".into(), true, false, vec![]),
			InterfaceType::String => ("NString".into(), true, false, vec![]),
			InterfaceType::Boolean => ("NBool".into(), true, false, vec![]),
			InterfaceType::List(argument) => ("NList".into(), true, false, vec![argument.as_ref()]),
			InterfaceType::Tuple(arguments) => ("NTuple".into(), true, false, arguments.iter().collect()),
			InterfaceType::Map(key, value) => ("NMap".into(), true, false, vec![key, value]),
			InterfaceType::SelfType => {
				let receiver = self
					.receiver_binding
					.clone()
					.map(HirExpr::Local)
					.unwrap_or(HirExpr::This);
				return Ok(HirExpr::RuntimeTypeProjection {
					receiver: Box::new(receiver),
					path: vec![],
				});
			}
			InterfaceType::Generic(parameter) => {
				if let Some(index) = self
					.type_parameters
					.iter()
					.position(|candidate| candidate == parameter)
				{
					return Ok(HirExpr::Local(EcoString::from(format!("$type${index}"))));
				}
				if let Some(path) = self
					.self_type
					.and_then(|type_| owner_parameter_path(type_, parameter))
				{
					let receiver = self
						.receiver_binding
						.clone()
						.map(HirExpr::Local)
						.unwrap_or(HirExpr::This);
					return Ok(HirExpr::RuntimeTypeProjection {
						receiver: Box::new(receiver),
						path,
					});
				}
				if self.instance_member
					&& let Some(index) = self
						.all_type_parameters
						.iter()
						.filter(|parameter| parameter.binder.scope != crate::BinderScope::Member)
						.position(|candidate| candidate == parameter)
				{
					let receiver = self
						.receiver_binding
						.clone()
						.map(HirExpr::Local)
						.unwrap_or(HirExpr::This);
					return Ok(HirExpr::RuntimeTypeProjection {
						receiver: Box::new(receiver),
						path: vec![index],
					});
				}
				return Err(StableLoweringError::Unsupported {
					definition: self.artifact.definition.clone(),
					node: None,
					feature: "unbound hidden generic type object".into(),
				});
			}
			InterfaceType::Named {
				definition,
				positional,
				named,
			} => {
				self.demand_direct(definition);
				let artifact = self.context.runtime_definition(definition)?;
				let arguments = positional
					.iter()
					.chain(named.iter().map(|(_, argument)| argument))
					.collect();
				(
					self.context.binding_name(definition)?.as_str().into(),
					false,
					matches!(artifact.payload, crate::RuntimePayload::Enum(_)),
					arguments,
				)
			}
			_ => {
				return Err(StableLoweringError::Unsupported {
					definition: self.artifact.definition.clone(),
					node: None,
					feature: format!("runtime type object for non-runtime type {type_:?}").into(),
				});
			}
		};
		let arguments = argument_types
			.into_iter()
			.map(|argument| self.runtime_type_object(argument))
			.collect::<Result<_, _>>()?;
		Ok(HirExpr::RuntimeTypeObject {
			binding,
			box_runtime,
			is_enum,
			arguments,
		})
	}

	fn construction_prototype(
		&self,
		expr: &StableExpr,
	) -> Result<Option<Box<HirExpr>>, StableLoweringError> {
		let arguments = self.annotations.generic_call_arguments(self.id(expr));
		let target = self.annotations.generic_call_target(self.id(expr));
		if let (Some(arguments), Some(target)) = (arguments, target)
			&& matches!(
				self.context.runtime_definition(target)?.payload,
				crate::RuntimePayload::Struct(_)
			) {
			let arguments = arguments
				.iter()
				.filter_map(|argument| match argument {
					crate::runtime::RuntimeTypeArgument::Canonical(type_) => {
						Some(self.runtime_type_object(type_))
					}
					crate::runtime::RuntimeTypeArgument::Erased => None,
				})
				.collect::<Result<Vec<_>, _>>()?;
			if !arguments.is_empty() {
				return Ok(Some(Box::new(HirExpr::RuntimeTypeObject {
					binding: self.context.binding_name(target)?.as_str().into(),
					box_runtime: false,
					is_enum: false,
					arguments,
				})));
			}
		}
		let annotated = self.annotations.type_of(self.id(expr));
		let Some(type_) = annotated else {
			return Ok(None);
		};
		let type_ = substitute_type_parameters(type_, self.type_substitutions);
		if matches!(&type_, InterfaceType::Named { definition, .. } if self.annotations.option.as_ref().is_some_and(|option| &option.option == definition))
		{
			// Host/runtime Option producers share the canonical enum prototype.
			// Keep source constructors on that ABI rather than creating a child
			// identity which external Option values cannot carry.
			return Ok(None);
		}
		let parameterized = match &type_ {
			InterfaceType::Named {
				positional, named, ..
			} => !positional.is_empty() || !named.is_empty(),
			InterfaceType::List(_) | InterfaceType::Map(_, _) => true,
			InterfaceType::Tuple(arguments) => !arguments.is_empty(),
			_ => false,
		};
		parameterized
			.then(|| self.runtime_type_object(&type_).map(Box::new))
			.transpose()
	}

	fn demand_receiverless_implementations(
		&self,
		interface: &DefinitionId,
		member: &DefinitionId,
	) -> Result<(), StableLoweringError> {
		let request = StableShapeRequest::ImplementationsForInterface(interface.clone());
		let StableShapeFact::Implementations(implementations) = self.context.stable_shape(&request)?
		else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		for implementation in implementations {
			if let Some(slot) = implementation.member_slots.target(member) {
				self.demand_direct(&slot.member_id);
			}
		}
		Ok(())
	}

	fn demand_concrete_receiverless_implementation(
		&self,
		type_: &InterfaceType,
		interface: &DefinitionId,
		member: &DefinitionId,
	) -> Result<(), StableLoweringError> {
		let request = StableShapeRequest::ImplementationsForInterface(interface.clone());
		let StableShapeFact::Implementations(implementations) = self.context.stable_shape(&request)?
		else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		if let Some(slot) = implementations
			.iter()
			.filter(|implementation| implementation.binders.is_empty())
			.filter(|implementation| peel_mut(&implementation.self_type) == peel_mut(type_))
			.find_map(|implementation| implementation.member_slots.target(member))
		{
			self.demand_direct(&slot.member_id);
			return Ok(());
		}
		if let Some(slot) = implementations
			.iter()
			.filter(|implementation| {
				!implementation.binders.is_empty()
					|| matches!(implementation.self_type, InterfaceType::Generic(_))
			})
			.find_map(|implementation| implementation.member_slots.target(member))
		{
			self.demand_direct(&slot.member_id);
		}
		Ok(())
	}

	fn required_receiverless_slots(
		&self,
		definition: &DefinitionId,
	) -> Result<std::collections::HashSet<usize>, StableLoweringError> {
		fn visit<C: StableLoweringContext>(
			context: &C,
			definition: &DefinitionId,
			visiting: &mut std::collections::HashSet<DefinitionId>,
		) -> Result<std::collections::HashSet<usize>, StableLoweringError> {
			if !visiting.insert(definition.clone()) {
				return Ok(std::collections::HashSet::new());
			}
			if matches!(
				&definition.key,
				crate::DeclarationKey::Member { owner, .. }
					if matches!(
						owner.key,
						crate::DeclarationKey::TopLevel {
							category: crate::DeclarationCategory::Interface,
							..
						}
					)
			) {
				let request = StableShapeRequest::Member(definition.clone());
				let StableShapeFact::Member(member) = context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				if member.id != *definition {
					return Err(invalid(
						definition,
						"generic interface member disagrees with its exact checked shape",
					));
				}
				if !member.has_default {
					visiting.remove(definition);
					return Ok(std::collections::HashSet::new());
				}
			}
			let artifact = context.runtime_definition(definition)?;
			let body = match &artifact.payload {
				crate::RuntimePayload::NymphBody(body) => body,
				crate::RuntimePayload::MaterializedInterfaceMember {
					body_definition, ..
				} => {
					let required = visit(context, body_definition, visiting)?;
					let body_artifact = context.runtime_definition(body_definition)?;
					let crate::RuntimePayload::NymphBody(body) = &body_artifact.payload else {
						return Err(invalid(
							definition,
							"materialized default body is not a Nymph body",
						));
					};
					let crate::DeclarationKey::MaterializedInterfaceMember { implementation, .. } =
						&definition.key
					else {
						return Err(invalid(
							definition,
							"materialized default has inconsistent identity",
						));
					};
					let request = StableShapeRequest::Implementation((**implementation).clone());
					let StableShapeFact::Implementation(implementation) = context.stable_shape(&request)?
					else {
						return Err(StableShapeLookupError::WrongFact { request }.into());
					};
					let required = body
						.type_parameters
						.iter()
						.enumerate()
						.filter(|(index, _)| required.contains(index))
						.filter(|(_, parameter)| {
							!implementation
								.interface_argument_bindings
								.iter()
								.any(|(bound, _)| bound == *parameter)
						})
						.map(|(index, _)| {
							body.type_parameters[..index]
								.iter()
								.filter(|parameter| {
									!implementation
										.interface_argument_bindings
										.iter()
										.any(|(bound, _)| bound == *parameter)
								})
								.count()
						})
						.collect();
					visiting.remove(definition);
					return Ok(required);
				}
				_ => {
					visiting.remove(definition);
					return Ok(std::collections::HashSet::new());
				}
			};
			let mut required = body
				.annotations
				.generic_namespaced_calls
				.iter()
				.map(|(_, parameter, ..)| *parameter as usize)
				.collect::<std::collections::HashSet<_>>();
			fn collect_type_parameters(
				type_: &InterfaceType,
				parameters: &mut std::collections::HashSet<crate::GenericParameterId>,
			) {
				match type_ {
					InterfaceType::Generic(parameter) => {
						parameters.insert(parameter.clone());
					}
					InterfaceType::Named {
						positional, named, ..
					} => {
						for argument in positional
							.iter()
							.chain(named.iter().map(|(_, value)| value))
						{
							collect_type_parameters(argument, parameters);
						}
					}
					InterfaceType::List(argument) => {
						collect_type_parameters(argument, parameters);
					}
					InterfaceType::Tuple(arguments) | InterfaceType::Intersection(arguments) => {
						for argument in arguments {
							collect_type_parameters(argument, parameters);
						}
					}
					InterfaceType::Map(key, value) => {
						collect_type_parameters(key, parameters);
						collect_type_parameters(value, parameters);
					}
					_ => {}
				}
			}
			for (_, type_) in body.annotations.types.iter() {
				if matches!(type_, InterfaceType::Named { positional, named, .. } if !positional.is_empty() || !named.is_empty())
				{
					let mut parameters = std::collections::HashSet::new();
					collect_type_parameters(type_, &mut parameters);
					for parameter in parameters {
						if let Some(position) = body
							.type_parameters
							.iter()
							.position(|candidate| candidate == &parameter)
						{
							required.insert(position);
						}
					}
				}
			}
			for (node, arguments) in body.annotations.generic_call_arguments.iter() {
				let Some(target) = body.annotations.generic_call_target(*node) else {
					continue;
				};
				let nested = visit(context, target, visiting)?;
				for index in nested {
					let Some(crate::runtime::RuntimeTypeArgument::Canonical(argument)) = arguments.get(index)
					else {
						continue;
					};
					let mut parameters = std::collections::HashSet::new();
					collect_type_parameters(argument, &mut parameters);
					for parameter in parameters {
						if let Some(position) = body
							.type_parameters
							.iter()
							.position(|candidate| candidate == &parameter)
						{
							required.insert(position);
						}
					}
				}
			}
			if body.kind != crate::RuntimeBodyKind::StaticFunction
				&& matches!(definition.key, crate::DeclarationKey::Member { .. })
			{
				required = body
					.type_parameters
					.iter()
					.enumerate()
					.filter(|(index, parameter)| {
						required.contains(index) && parameter.binder.scope == crate::BinderScope::Member
					})
					.map(|(index, _)| {
						body.type_parameters[..index]
							.iter()
							.filter(|parameter| parameter.binder.scope == crate::BinderScope::Member)
							.count()
					})
					.collect();
			}
			visiting.remove(definition);
			Ok(required)
		}

		visit(
			self.context,
			definition,
			&mut std::collections::HashSet::new(),
		)
	}

	fn append_hidden_arguments(
		&self,
		node: crate::BodyNodeId,
		mut lowered: HirExpr,
	) -> Result<HirExpr, StableLoweringError> {
		let Some(hidden) = self.annotations.generic_call_arguments(node) else {
			return Ok(lowered);
		};
		let call_target = self.annotations.generic_call_target(node);
		let required = call_target
			.map(|target| self.required_receiverless_slots(target))
			.transpose()?
			.unwrap_or_default();
		let explicit_receiver = usize::from(
			matches!(&lowered, HirExpr::ExternCall { .. })
				|| matches!(
					&lowered,
					HirExpr::Call { callee, .. } | HirExpr::ActivationCall { callee, .. }
						if matches!(&**callee, HirExpr::Local(_))
				),
		);
		let arguments = match &mut lowered {
			HirExpr::Call { args, .. } | HirExpr::ActivationCall { args, .. } => args,
			HirExpr::StaticEnumDispatch { args, .. } => args,
			HirExpr::ExternCall { args, .. } => args,
			HirExpr::BoundDispatch {
				hidden_arguments, ..
			}
			| HirExpr::UnaryBoundDispatch {
				hidden_arguments, ..
			} => hidden_arguments,
			_ => return Ok(lowered),
		};
		let external_target = if let Some(call_target) = call_target {
			match self.context.runtime_definition(call_target) {
				Ok(artifact) => matches!(&artifact.payload, crate::RuntimePayload::External(_)),
				Err(RuntimeDefinitionLookupError::Missing { .. })
					if matches!(
						&call_target.key,
						crate::DeclarationKey::Member { owner, .. }
							if matches!(
								owner.key,
								crate::DeclarationKey::TopLevel {
									category: crate::DeclarationCategory::Interface,
									..
								}
							)
					) =>
				{
					false
				}
				Err(error) => return Err(error.into()),
			}
		} else {
			false
		};
		if external_target && let Some(call_target) = call_target {
			let abi = exact_external_abi(self.context, call_target, None)?;
			let (source_arity, binder_arity, receiver_arity) =
				external_callable_shape(self.context, call_target, &abi)?;
			if arguments.len() != source_arity + receiver_arity * explicit_receiver {
				return Err(StableLoweringError::ShapeDrift {
					definition: call_target.clone(),
					reason:
						"generic external callable source arity disagrees with its exact definition shape"
							.into(),
				});
			}
			if hidden.len() != binder_arity {
				return Err(StableLoweringError::ShapeDrift {
					definition: call_target.clone(),
					reason:
						"generic external callable binder arity disagrees with its canonical type arguments"
							.into(),
				});
			}
		}
		if required.iter().any(|index| *index >= hidden.len()) {
			return Err(StableLoweringError::Unsupported {
				definition: self.artifact.definition.clone(),
				node: Some(node),
				feature: "missing runtime type argument required by receiverless dispatch".into(),
			});
		}
		for (index, argument) in hidden.iter().enumerate() {
			if matches!(argument, crate::runtime::RuntimeTypeArgument::Erased)
				&& required.contains(&index)
			{
				return Err(StableLoweringError::Unsupported {
					definition: self.artifact.definition.clone(),
					node: Some(node),
					feature: "erased runtime type argument required by receiverless dispatch".into(),
				});
			}
			if required.contains(&index)
				&& let crate::runtime::RuntimeTypeArgument::Canonical(type_) = argument
				&& let Some(target) = call_target
			{
				let artifact = self.context.runtime_definition(target)?;
				if let crate::RuntimePayload::NymphBody(body) = &artifact.payload {
					for (_, _parameter, interface, member) in body
						.annotations
						.generic_namespaced_calls
						.iter()
						.filter(|(_, parameter, ..)| *parameter as usize == index)
					{
						self.demand_concrete_receiverless_implementation(type_, interface, member)?;
					}
				}
			}
			arguments.push(match argument {
				crate::runtime::RuntimeTypeArgument::Canonical(type_) => self.runtime_type_object(type_)?,
				crate::runtime::RuntimeTypeArgument::Erased => HirExpr::Undefined,
			});
		}
		Ok(lowered)
	}

	fn generic_callable_adapter(
		&self,
		node: crate::BodyNodeId,
		callee: HirExpr,
	) -> Result<HirExpr, StableLoweringError> {
		let Some(hidden) = self.annotations.generic_call_arguments(node) else {
			return Ok(callee);
		};
		let Some(target) = self.annotations.generic_call_target(node) else {
			return Err(StableLoweringError::Unsupported {
				definition: self.artifact.definition.clone(),
				node: Some(node),
				feature: "generic callable value without an exact runtime target".into(),
			});
		};
		let target_artifact = self.context.runtime_definition(target)?;
		let generated = matches!(
			&target_artifact.payload,
			crate::RuntimePayload::NymphBody(_)
		);
		let arity = match &target_artifact.payload {
			crate::RuntimePayload::NymphBody(target_body) => target_body.stable.params.len(),
			crate::RuntimePayload::External(_) => {
				let abi = exact_external_abi(self.context, target, None)?;
				if matches!(abi.callable, crate::ExternalCallable::Deferred) {
					return Err(StableLoweringError::Unsupported {
						definition: self.artifact.definition.clone(),
						node: Some(node),
						feature: "generic callable value targets a deferred external".into(),
					});
				}
				let (source_arity, binder_arity, _) = external_callable_shape(self.context, target, &abi)?;
				if hidden.len() != binder_arity {
					return Err(StableLoweringError::ShapeDrift {
						definition: target.clone(),
						reason:
							"generic external callable binder arity disagrees with its canonical type arguments"
								.into(),
					});
				}
				source_arity
			}
			_ => {
				return Err(StableLoweringError::Unsupported {
					definition: self.artifact.definition.clone(),
					node: Some(node),
					feature: "generic callable value targets a non-callable runtime artifact".into(),
				});
			}
		};
		let params = (0..arity)
			.map(|index| EcoString::from(format!("$arg${index}")))
			.collect::<Vec<_>>();
		let call = HirExpr::Call {
			callee: Box::new(callee),
			args: params.iter().cloned().map(HirExpr::Local).collect(),
		};
		let call = self.append_hidden_arguments(node, call)?;
		let call = if generated {
			Self::activation_call(node, call)
		} else {
			call
		};
		Ok(HirExpr::Closure {
			params,
			body: Box::new(call),
		})
	}

	fn lower_iteration(
		&self,
		source_id: crate::BodyNodeId,
		variable: &StablePattern,
		iterable: &StableExpr,
		body: &StableExpr,
		result_option: Option<nymph_hir::hir::HirOptionAbi>,
	) -> Result<HirExpr, StableLoweringError> {
		let source = self.lower(iterable)?;
		let iteration = self
			.annotations
			.iteration(self.id(iterable))
			.ok_or_else(|| self.missing_annotation(self.id(iterable), "iteration"))?;
		let (it, next, next_dispatch, iteration) = match iteration {
			crate::RuntimeIteration::Direct {
				iterator_interface,
				next,
				next_dispatch,
				iteration,
			} => {
				if next_dispatch.is_none() {
					self.record_unresolved_call(UnresolvedRuntimeCall::IteratorNext {
						interface: iterator_interface.clone(),
						member: next.clone(),
					});
				}
				(source, next, next_dispatch, iteration)
			}
			crate::RuntimeIteration::ViaIter {
				iter,
				iterable_interface,
				iter_interface_member,
				iterator_interface,
				next,
				next_dispatch,
				iteration,
				..
			} => {
				let next_is_shellless = next_dispatch
					.as_ref()
					.map(|dispatch| self.iteration_dispatch_is_shellless(dispatch))
					.transpose()?
					.unwrap_or(false);
				if !next_is_shellless {
					self.demand_concrete_iteration_next(
						iter,
						iterable_interface,
						iter_interface_member,
						iterator_interface,
						next,
					)?;
				}
				let lowered = self.lower_iter_dispatch(
					iterable,
					iter,
					iterable_interface,
					iter_interface_member,
					source,
				)?;
				(lowered, next, next_dispatch, iteration)
			}
		};
		let (it_name, successor_name, pat, target, body) = self.with_scope(|| {
			let it_name = self.declare(&"$it".into());
			let successor_name = self.declare(&"$next".into());
			let pat = self.lower_pattern(variable)?;
			let target = self.next_loop();
			let body = self.lower_loop_branch(source_id, target, body)?;
			Ok((it_name, successor_name, pat, target, body))
		})?;
		let call = if let Some(dispatch) = next_dispatch {
			self.lower_iteration_next(source_id, dispatch, next, HirExpr::Local(it_name.clone()))?
		} else {
			Self::activation_call(
				source_id,
				HirExpr::Call {
					callee: Box::new(HirExpr::Field {
						recv: Box::new(HirExpr::Local(it_name.clone())),
						name: self.context.member_name(next)?.as_str().into(),
					}),
					args: vec![],
				},
			)
		};
		Ok(HirExpr::For {
			target,
			source: source_id.0,
			iterator_name: it_name,
			successor_name,
			iterator: Box::new(it),
			next: Box::new(call),
			pat,
			body: Box::new(body),
			iteration: nymph_hir::hir::HirIterationAbi {
				enum_name: self
					.context
					.binding_name(&iteration.iteration)?
					.as_str()
					.into(),
				done: self.context.member_name(&iteration.done)?.as_str().into(),
				yield_: self.context.member_name(&iteration.yield_)?.as_str().into(),
				item: self
					.context
					.member_name(&iteration.yield_item)?
					.as_str()
					.into(),
				next: self
					.context
					.member_name(&iteration.yield_next)?
					.as_str()
					.into(),
			},
			option: result_option,
		})
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

	fn lower_iter_dispatch(
		&self,
		value: &StableExpr,
		dispatch: &crate::StableDispatch,
		interface: &DefinitionId,
		member: &DefinitionId,
		fallback: HirExpr,
	) -> Result<HirExpr, StableLoweringError> {
		match dispatch {
			crate::StableDispatch::Builtin { .. } => {
				self.lower_generic_bound(interface, member, value, vec![])
			}
			_ => self.lower_dispatch_value(self.id(value), dispatch, fallback),
		}
	}

	fn lower_dispatch_value(
		&self,
		source: crate::BodyNodeId,
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
				self.record_unresolved_call(UnresolvedRuntimeCall::GenericDispatch {
					interface: interface.clone(),
					member: member.clone(),
				});
				let Some(slot) = self
					.implementation_slots
					.and_then(|slots| slots.target(member))
				else {
					return Ok(Self::activation_dispatch_call(
						source,
						dispatch,
						HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(receiver),
								name: self.context.member_name(member)?.as_str().into(),
							}),
							args: vec![],
						},
					));
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
			let mut args = vec![receiver];
			self.append_selected_call_arguments(dispatch, target, None, true, &mut args)?;
			return Ok(Self::activation_dispatch_call(
				source,
				dispatch,
				HirExpr::Call {
					callee: Box::new(HirExpr::Local(
						self.context.binding_name(target)?.as_str().into(),
					)),
					args,
				},
			));
		}
		Ok(Self::activation_dispatch_call(
			source,
			dispatch,
			HirExpr::Call {
				callee: Box::new(HirExpr::Field {
					recv: Box::new(receiver),
					name: self.context.member_name(target)?.as_str().into(),
				}),
				args: vec![],
			},
		))
	}

	fn iteration_dispatch_is_shellless(
		&self,
		dispatch: &crate::StableDispatch,
	) -> Result<bool, StableLoweringError> {
		let (member, implementation) = match dispatch {
			crate::StableDispatch::Direct {
				member,
				implementation,
				..
			}
			| crate::StableDispatch::SelectedImplementation {
				member,
				implementation,
				..
			}
			| crate::StableDispatch::InterfaceDefault {
				member,
				implementation,
				..
			} => (member, implementation),
			_ => return Ok(false),
		};
		let request = StableShapeRequest::Implementation(implementation.clone());
		let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		let target = exact_implementation_slot(&shape, member)
			.map(|slot| &slot.member_id)
			.unwrap_or(member);
		shellless_implementation_member(self.context, target, implementation)
	}

	fn lower_iteration_next(
		&self,
		source: crate::BodyNodeId,
		dispatch: &crate::StableDispatch,
		member: &DefinitionId,
		receiver: HirExpr,
	) -> Result<HirExpr, StableLoweringError> {
		if self.iteration_dispatch_is_shellless(dispatch)? {
			self.lower_dispatch_value(source, dispatch, receiver)
		} else {
			Ok(Self::activation_dispatch_call(
				source,
				dispatch,
				HirExpr::Call {
					callee: Box::new(HirExpr::Field {
						recv: Box::new(receiver),
						name: self.context.member_name(member)?.as_str().into(),
					}),
					args: vec![],
				},
			))
		}
	}
	fn lower_block(
		&self,
		body: &[StableStatement],
		new_scope: bool,
	) -> Result<HirExpr, StableLoweringError> {
		if new_scope {
			return self.with_scope(|| self.lower_block(body, false));
		}
		let mut stmts = vec![];
		let mut tail = None;
		for (index, statement) in body.iter().enumerate() {
			let last = index + 1 == body.len();
			match statement {
				StableStatement::Let {
					pattern,
					managed,
					value,
				} => {
					let source = value.id;
					let value = self.lower(value)?;
					let name = self.declare(pattern_name(pattern)?);
					let cleanup = if *managed {
						let dispatch = self.annotations.managed_cleanup(source).ok_or_else(|| {
							invalid(
								&self.artifact.definition,
								"managed binding has no stable cleanup fact",
							)
						})?;
						Some(self.lower_dispatch_value(source, dispatch, HirExpr::Local(name.clone()))?)
					} else {
						None
					};
					stmts.push(HirStmt::Let {
						name,
						value,
						cleanup,
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
fn drain_spread(
	iterator: HirExpr,
	next_call: HirExpr,
	iteration: nymph_hir::hir::HirIterationAbi,
) -> HirExpr {
	let acc: EcoString = "$acc".into();
	let it: EcoString = "$it".into();
	let value: EcoString = "$x".into();
	let append = HirExpr::Call {
		callee: Box::new(HirExpr::Field {
			recv: Box::new(HirExpr::Local(acc.clone())),
			name: "push".into(),
		}),
		args: vec![HirExpr::Local(value)],
	};
	HirExpr::Block {
		stmts: vec![
			HirStmt::Let {
				name: acc.clone(),
				value: HirExpr::Array {
					kind: HirArrayKind::Raw,
					items: vec![],
				},
				cleanup: None,
			},
			HirStmt::Expr(HirExpr::For {
				target: u32::MAX,
				source: u32::MAX,
				iterator_name: it,
				successor_name: "$next".into(),
				iterator: Box::new(iterator),
				next: Box::new(next_call),
				pat: HirPat::Binding {
					name: "$x".into(),
					sub: None,
				},
				body: Box::new(append),
				iteration,
				option: None,
			}),
		],
		tail: Some(Box::new(HirExpr::Local(acc))),
	}
}
fn peel_mut(ty: &InterfaceType) -> &InterfaceType {
	ty
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

fn stable_integer_constant(expr: &StableExpr) -> Option<BigInt> {
	match &expr.kind {
		StableExprKind::Int(value) | StableExprKind::UInt(value) => Some(BigInt::from(*value)),
		StableExprKind::Grouped(inner) => stable_integer_constant(inner),
		StableExprKind::PrefixOp {
			op: PrefixOperator::Negate,
			value,
		} => Some(-stable_integer_constant(value)?),
		StableExprKind::PrefixOp {
			op: PrefixOperator::BitNot,
			value,
		} => Some(!stable_integer_constant(value)?),
		StableExprKind::BinaryOp { lhs, op, rhs } => {
			let lhs = stable_integer_constant(lhs)?;
			let rhs = stable_integer_constant(rhs)?;
			Some(match op {
				BinaryOperator::Plus => lhs + rhs,
				BinaryOperator::Minus => lhs - rhs,
				BinaryOperator::Times => lhs * rhs,
				BinaryOperator::Remainder => lhs % rhs,
				BinaryOperator::BitAnd => lhs & rhs,
				BinaryOperator::BitOr => lhs | rhs,
				BinaryOperator::BitXor => lhs ^ rhs,
				BinaryOperator::LeftShift => lhs << u32::try_from(rhs).ok()?,
				BinaryOperator::RightShift => lhs >> u32::try_from(rhs).ok()?,
				_ => return None,
			})
		}
		_ => None,
	}
}

fn literal_pattern(pattern: &StablePattern) -> Result<HirLit, StableLoweringError> {
	Ok(match &pattern.kind {
		StablePatternKind::Int(v) => HirLit::Int(*v),
		StablePatternKind::UInt(v) => HirLit::UInt(*v),
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
