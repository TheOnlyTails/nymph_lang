//! Stable, semantic-only contracts for per-definition HIR lowering.
//!
//! Its inputs are exact stable identities and owned semantic artifacts, so lowering
//! not need compiler queries, parser identities, source locations, or module ASTs.

use std::{
	cell::RefCell,
	collections::{HashMap, HashSet},
	sync::Arc,
};

use ecow::EcoString;
use nymph_ast::{
	decl::{Declaration, LetKind},
	expr::{
		Expr, ExprKind, ListItem, ListPatternEntry, MapEntry, MapPatternEntry, Pattern, RangeKind,
		RangePatternKind, Statement, StringPart, StructPatternField,
	},
	ops::{AssignOperator, BinaryOperator, PatternOperator, PrefixOperator},
};
use nymph_hir::hir::{
	BinOp, BuiltinResult, HirArm, HirArrayElem, HirArrayKind, HirBoundDispatchCase,
	HirBoundDispatchTarget, HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirLit, HirMapElem,
	HirMethod, HirModule, HirPat, HirRange, HirStmt, HirVariant, NumKind, ScalarCastKind, UnOp,
};

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
	let fragment = match &artifact.payload {
		crate::RuntimePayload::External(abi) => {
			let name = context.binding_name(&definition)?;
			let exact = external_abi(context, &definition)?;
			if exact != *abi {
				return Err(StableLoweringError::MismatchedExternalAbi { definition });
			}
			let module = external_module(&definition, abi)?;
			let symbol = external_symbol(&definition, abi)?;
			if matches!(
				&definition.key,
				crate::DeclarationKey::TopLevel {
					category: crate::DeclarationCategory::Let,
					..
				}
			) {
				let marshal = abi
					.marshal
					.ok_or_else(|| StableLoweringError::MissingExternalMarshal {
						definition: definition.clone(),
					})?;
				LoweredHirFragment::TopLevelValue(HirLet {
					name: name.as_str().into(),
					mutable: false,
					value: HirExpr::ExternValue {
						module,
						symbol,
						marshal,
					},
				})
			} else {
				LoweredHirFragment::TopLevelExternal {
					name,
					abi: abi.clone(),
				}
			}
		}
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
			lower_body(context, &artifact, body, &mut demands, None)?
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
			{
				return Err(invalid(
					&definition,
					"materialized artifact disagrees with its persisted implementation slot",
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
				Some(&implementation_shape.member_slots),
			)?;
			let method = fragment_method(lowered)?;
			LoweredHirFragment::MaterializedDefault {
				owner,
				implementation,
				interface_member: interface_member.clone(),
				method,
			}
		}
	};
	Ok(LoweredRuntimeDefinition::new(definition, fragment, demands))
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

fn external_module(
	definition: &DefinitionId,
	abi: &ExternalAbi,
) -> Result<&'static str, StableLoweringError> {
	abi
		.module
		.as_ref()
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
		.symbol
		.as_ref()
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

fn primitive_extension_owner(
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
	Ok(stable_runtime_tag(&shape.self_type).is_some())
}

fn primitive_extension_member(
	context: &impl StableLoweringContext,
	member: &DefinitionId,
	implementation: &DefinitionId,
) -> Result<bool, StableLoweringError> {
	if !primitive_extension_owner(context, implementation)? {
		return Ok(false);
	}
	let artifact = context.runtime_definition(member)?;
	let crate::RuntimePlacement::Attached { owner, .. } = &artifact.placement else {
		return Err(invalid(
			member,
			"primitive extension member has top-level placement",
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
	implementation_slots: Option<&[crate::ImplementationMemberSlot]>,
) -> Result<LoweredHirFragment, StableLoweringError> {
	let is_function = body.signature.contains('(');
	let source = if is_function {
		format!("func {} = {}", body.signature, body.expression)
	} else {
		format!("let {} = {}", body.signature, body.expression)
	};
	let parsed = nymph_syntax::parse_module(&source, "$stable-lowering.nym");
	if !parsed.diagnostics.is_empty() {
		return Err(StableLoweringError::Parse {
			definition: artifact.definition.clone(),
			reason: format!("{:?}", parsed.diagnostics).into(),
		});
	}
	let (meta, expression) = match parsed.tree.members.as_slice() {
		[Declaration::Func { meta, body, .. }] if is_function => (Some(meta), body),
		[Declaration::Let { meta: _, value, .. }] if !is_function => (None, value),
		_ => {
			return Err(StableLoweringError::ShapeDrift {
				definition: artifact.definition.clone(),
				reason: "canonical source reconstructed a different declaration shape".into(),
			});
		}
	};
	let mut nodes = vec![];
	walk(expression, &mut nodes);
	let node_ids: HashMap<_, _> = nodes
		.iter()
		.enumerate()
		.map(|(index, expr)| (expr.id, crate::BodyNodeId(index as u32)))
		.collect();
	let mut patterns = vec![];
	walk_body_patterns(expression, &mut patterns);
	let pattern_ids = patterns
		.into_iter()
		.enumerate()
		.map(|(index, pattern)| (pattern.1, crate::PatternNodeId(index as u32)))
		.collect();
	let primitive_extension = match &artifact.placement {
		crate::RuntimePlacement::Attached { owner, .. } => primitive_extension_owner(context, owner)?,
		crate::RuntimePlacement::TopLevel => false,
	};
	for id in body
		.annotations
		.types
		.iter()
		.map(|(id, _)| id)
		.chain(body.annotations.definition_targets.iter().map(|(id, _)| id))
	{
		if id.0 as usize >= nodes.len() {
			return Err(StableLoweringError::ShapeDrift {
				definition: artifact.definition.clone(),
				reason: format!("annotation node {} exceeds reconstructed preorder", id.0).into(),
			});
		}
	}
	let lowerer = StableBodyLowerer {
		context,
		artifact,
		annotations: &body.annotations,
		node_ids,
		pattern_ids,
		scopes: RefCell::new(vec![HashMap::new()]),
		counters: RefCell::new(HashMap::new()),
		demands: RefCell::new(demands),
		implementation_slots,
		receiver_binding: primitive_extension.then(|| EcoString::from("$self")),
	};
	if primitive_extension {
		lowerer.scopes.borrow_mut()[0].insert("this".into(), "$self".into());
	}
	let emitted: EcoString = context.binding_name(&artifact.definition)?.as_str().into();
	if let Some(meta) = meta {
		let params = meta
			.params
			.iter()
			.map(|param| pattern_name(&param.0.name).map(|name| lowerer.declare(name)))
			.collect::<Result<Vec<_>, StableLoweringError>>()?;
		let lowered_body = lowerer.lower_function_body(expression)?;
		let function = HirFunc {
			name: emitted.clone(),
			params: if primitive_extension {
				std::iter::once(EcoString::from("$self"))
					.chain(params.iter().cloned())
					.collect()
			} else {
				params.clone()
			},
			body: lowered_body.clone(),
		};
		if primitive_extension {
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
				if meta.kind == nymph_ast::decl::FuncKind::Namespace {
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
		let value = lowerer.lower(expression)?;
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

fn pattern_name(pattern: &nymph_ast::Spanned<Pattern>) -> Result<&EcoString, StableLoweringError> {
	match &pattern.0 {
		Pattern::Binding { name, .. } => Ok(&name.0),
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
	node_ids: HashMap<nymph_ast::NodeId, crate::BodyNodeId>,
	pattern_ids: HashMap<nymph_ast::Span, crate::PatternNodeId>,
	scopes: RefCell<Vec<HashMap<EcoString, EcoString>>>,
	counters: RefCell<HashMap<EcoString, u32>>,
	demands: RefCell<&'a mut StableDemandSet>,
	implementation_slots: Option<&'a [crate::ImplementationMemberSlot]>,
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
	fn id(&self, expr: &Expr) -> crate::BodyNodeId {
		self.node_ids[&expr.id]
	}
	fn unsupported(&self, expr: &Expr, feature: &str) -> StableLoweringError {
		StableLoweringError::Unsupported {
			definition: self.artifact.definition.clone(),
			node: Some(self.id(expr)),
			feature: feature.into(),
		}
	}
	fn declare(&self, name: &EcoString) -> EcoString {
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
	fn target(&self, expr: &Expr) -> Option<&DefinitionId> {
		let id = self.id(expr);
		self
			.annotations
			.definition_targets
			.iter()
			.find(|(found, _)| *found == id)
			.map(|(_, target)| target)
	}
	fn external_marshal(
		&self,
		expr: &Expr,
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
	fn ty(&self, expr: &Expr) -> Result<&InterfaceType, StableLoweringError> {
		let node = self.id(expr);
		self
			.annotations
			.types
			.iter()
			.find(|(id, _)| *id == node)
			.map(|(_, ty)| ty)
			.ok_or_else(|| StableLoweringError::MissingAnnotation {
				definition: self.artifact.definition.clone(),
				node,
				channel: "type".into(),
			})
	}
	fn dispatch(&self, expr: &Expr) -> Result<&crate::StableDispatch, StableLoweringError> {
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
	fn variant(&self, expr: &Expr) -> Option<&crate::ExpressionVariant> {
		self
			.annotations
			.variants
			.iter()
			.find(|(id, _)| *id == self.id(expr))
			.map(|(_, variant)| variant)
	}
	fn lower_function_body(&self, expr: &Expr) -> Result<HirExpr, StableLoweringError> {
		if let ExprKind::Block { body, .. } = &expr.kind {
			self.lower_block(body, false)
		} else {
			self.lower(expr)
		}
	}
	fn lower(&self, expr: &Expr) -> Result<HirExpr, StableLoweringError> {
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
			let body = self.lower_inner(expr)?;
			self.scopes.borrow_mut().pop();
			return Ok(HirExpr::Closure {
				params,
				body: Box::new(body),
			});
		}
		self.lower_inner(expr)
	}
	fn lower_inner(&self, expr: &Expr) -> Result<HirExpr, StableLoweringError> {
		Ok(match &expr.kind {
			ExprKind::Int(value) => HirExpr::Num(value.0 as f64, NumKind::Int),
			ExprKind::UInt(value) => HirExpr::Num(value.0 as f64, NumKind::UInt),
			ExprKind::Float(value) => HirExpr::Num(value.0.into_inner(), NumKind::Float),
			ExprKind::Boolean(value) => HirExpr::Bool(value.0),
			ExprKind::Char(value) => HirExpr::Char(value.0),
			ExprKind::String(parts)
				if parts
					.iter()
					.all(|part| !matches!(part.0, StringPart::InterpolatedExpr(_))) =>
			{
				HirExpr::Str(
					parts
						.iter()
						.map(|part| match &part.0 {
							StringPart::Text(text) => text.to_string(),
							StringPart::EscapeSequence(escape) => escape.to_string(),
							_ => unreachable!(),
						})
						.collect::<String>()
						.into(),
				)
			}
			ExprKind::Identifier(name) => {
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
					let emitted = self.context.binding_name(target)?;
					let target_artifact = self.context.runtime_definition(target)?;
					if let crate::RuntimePayload::External(payload_abi) = &target_artifact.payload {
						let abi = external_abi(self.context, target)?;
						if abi != *payload_abi {
							return Err(StableLoweringError::MismatchedExternalAbi {
								definition: target.clone(),
							});
						}
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
						self.demands.borrow_mut().insert(target.clone());
					} else if target.module != self.artifact.definition.module {
						self.context.module_specifier(&target.module)?;
						self.demands.borrow_mut().insert(target.clone());
					}
					HirExpr::Local(emitted.as_str().into())
				} else {
					HirExpr::Local(self.resolve(&name.0))
				}
			}
			ExprKind::AnonymousParam(index) => {
				HirExpr::Local(self.resolve(&crate::anon_closure::anon_param_name(index.unwrap_or(0))))
			}
			ExprKind::This => self
				.receiver_binding
				.clone()
				.map(HirExpr::Local)
				.unwrap_or(HirExpr::This),
			ExprKind::Grouped(inner) => self.lower(inner)?,
			ExprKind::List(items) | ExprKind::Tuple(items) => {
				let kind = if matches!(expr.kind, ExprKind::List(_)) {
					HirArrayKind::List
				} else {
					HirArrayKind::Tuple
				};
				if items
					.iter()
					.any(|item| matches!(item.0, ListItem::Spread(_)))
				{
					return Ok(HirExpr::ArraySpread {
						kind,
						elems: items
							.iter()
							.map(|item| match &item.0 {
								ListItem::Expr(value) => self.lower(value).map(HirArrayElem::Item),
								ListItem::Spread(value) => self.lower_spread(value).map(HirArrayElem::Spread),
							})
							.collect::<Result<_, _>>()?,
					});
				}
				HirExpr::Array {
					kind,
					items: items
						.iter()
						.map(|item| match &item.0 {
							ListItem::Expr(item) => self.lower(item),
							_ => unreachable!(),
						})
						.collect::<Result<_, _>>()?,
				}
			}
			ExprKind::Map(entries) => {
				if entries
					.iter()
					.any(|entry| matches!(entry.0, MapEntry::Spread(_)))
				{
					return Ok(HirExpr::MapSpread(
						entries
							.iter()
							.map(|entry| match &entry.0 {
								MapEntry::Entry(key, value) => {
									Ok(HirMapElem::Entry(self.lower(key)?, self.lower(value)?))
								}
								MapEntry::Spread(value) => self.lower_spread(value).map(HirMapElem::Spread),
							})
							.collect::<Result<_, StableLoweringError>>()?,
					));
				}
				HirExpr::MapLit(
					entries
						.iter()
						.map(|entry| match &entry.0 {
							MapEntry::Entry(key, value) => Ok((self.lower(key)?, self.lower(value)?)),
							_ => unreachable!(),
						})
						.collect::<Result<_, StableLoweringError>>()?,
				)
			}
			ExprKind::MemberAccess {
				parent,
				member,
				optional: _,
			} => {
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
				HirExpr::Field {
					recv: Box::new(self.lower(parent)?),
					name: if let Some(target) = self.target(expr) {
						self.context.member_name(target)?.as_str().into()
					} else {
						member.0.clone()
					},
				}
			}
			ExprKind::Call {
				func,
				generics,
				args,
			} => {
				let _ = generics;
				if let Some(variant) = self.variant(expr) {
					self.demand_external(&variant.enum_definition)?;
					let fields = args
						.iter()
						.enumerate()
						.map(|(index, arg)| {
							let field = arg
								.0
								.name
								.as_ref()
								.map(|name| name.0.clone())
								.or_else(|| variant.fields.get(index).map(|field| field.name.clone()))
								.ok_or_else(|| {
									invalid(
										&self.artifact.definition,
										"variant positional argument has no exact field",
									)
								})?;
							Ok((field, self.lower(&arg.0.value)?))
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
						let fields = args
							.iter()
							.enumerate()
							.map(|(index, argument)| {
								let name = argument
									.0
									.name
									.as_ref()
									.map(|name| name.0.clone())
									.or_else(|| shell.fields.get(index).map(|field| field.name.clone()))
									.ok_or_else(|| {
										invalid(
											&self.artifact.definition,
											"struct argument has no exact field",
										)
									})?;
								Ok((name, self.lower(&argument.0.value)?))
							})
							.collect::<Result<_, StableLoweringError>>()?;
						return Ok(HirExpr::New {
							class: self.context.binding_name(target)?.as_str().into(),
							fields,
						});
					}
				}
				if let ExprKind::MemberAccess { parent, .. } = &func.kind
					&& let Some((_, dispatch)) = self
						.annotations
						.dispatches
						.iter()
						.find(|(id, _)| *id == self.id(expr))
				{
					return self.lower_dispatch(
						dispatch,
						parent,
						args.iter().map(|arg| &arg.0.value).collect(),
					);
				}
				HirExpr::Call {
					callee: Box::new(self.lower(func)?),
					args: args
						.iter()
						.map(|arg| self.lower(&arg.0.value))
						.collect::<Result<_, _>>()?,
				}
			}
			ExprKind::BinaryOp { lhs, op, rhs } => {
				if *op == BinaryOperator::Pipe {
					return Ok(HirExpr::Call {
						callee: Box::new(self.lower(rhs)?),
						args: vec![self.lower(lhs)?],
					});
				}
				let dispatch = self.dispatch(expr)?;
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
					result: result_for(*op),
					lhs: Box::new(self.lower(lhs)?),
					rhs: Box::new(self.lower(rhs)?),
				}
			}
			ExprKind::PrefixOp { op, value } => {
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
					result: if *op == PrefixOperator::BoolNot {
						BuiltinResult::Boolean
					} else {
						BuiltinResult::Int
					},
					operand: Box::new(self.lower(value)?),
				}
			}
			ExprKind::AssignOp {
				lhs,
				op: AssignOperator::Assign,
				rhs,
			} => HirExpr::Assign {
				target: Box::new(self.lower(lhs)?),
				value: Box::new(self.lower(rhs)?),
			},
			ExprKind::AssignOp { lhs, op, rhs } => {
				let binary =
					assign_binop(*op).ok_or_else(|| self.unsupported(expr, "assignment operator"))?;
				let value = match self.dispatch(expr)? {
					crate::StableDispatch::Builtin { .. } => HirExpr::Binary {
						op: binop(binary).unwrap(),
						result: result_for(binary),
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
			ExprKind::Block { body, .. } => self.lower_block(body, true)?,
			ExprKind::If {
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
			ExprKind::While {
				condition,
				body,
				label: None,
			} => HirExpr::While {
				cond: Box::new(self.lower(condition)?),
				body: Box::new(self.lower(body)?),
			},
			ExprKind::Closure { params, body, .. } => {
				self.scopes.borrow_mut().push(HashMap::new());
				let params = params
					.iter()
					.map(|param| pattern_name(&param.0.name).map(|name| self.declare(name)))
					.collect::<Result<_, _>>()?;
				let body = self.lower_function_body(body)?;
				self.scopes.borrow_mut().pop();
				HirExpr::Closure {
					params,
					body: Box::new(body),
				}
			}
			ExprKind::Return { .. } => {
				return Err(self.unsupported(expr, "return outside block statement"));
			}
			ExprKind::Break { .. } => return Err(self.unsupported(expr, "break (HIR has no jump node)")),
			ExprKind::Continue { .. } => {
				return Err(self.unsupported(expr, "continue (HIR has no jump node)"));
			}
			ExprKind::IndexAccess { parent, index, .. } => match peel_mut(self.ty(parent)?) {
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
			ExprKind::For {
				variable,
				iterable,
				body,
				label: None,
			} => self.lower_for(variable, iterable, body)?,
			ExprKind::Range(_) => return Err(self.unsupported(expr, "range/protocol")),
			ExprKind::Match { value, arms } => HirExpr::Match {
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
			ExprKind::PatternOp { lhs, op, rhs } => {
				let pat = self.lower_pattern(rhs)?;
				let (yes, no) = if *op == PatternOperator::Is {
					(true, false)
				} else {
					(false, true)
				};
				HirExpr::Match {
					scrutinee: Box::new(self.lower(lhs)?),
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
			ExprKind::PostfixOp { value, .. } => {
				self.lower_dispatch(self.dispatch(expr)?, value, vec![])?
			}
			ExprKind::TypeOp { lhs, .. } => match self.dispatch(expr)? {
				crate::StableDispatch::Builtin { .. } => self.lower_cast(expr, lhs)?,
				dispatch => self.lower_dispatch(dispatch, lhs, vec![])?,
			},
			ExprKind::String(parts) => self.lower_string(parts)?,
			ExprKind::While { .. } => return Err(self.unsupported(expr, "labeled while")),
			ExprKind::For { .. } => return Err(self.unsupported(expr, "labeled for")),
		})
	}
	fn lower_dispatch(
		&self,
		dispatch: &crate::StableDispatch,
		receiver: &Expr,
		arguments: Vec<&Expr>,
	) -> Result<HirExpr, StableLoweringError> {
		if let crate::StableDispatch::GenericBound { interface, member } = dispatch {
			if self
				.implementation_slots
				.is_none_or(|slots| !slots.iter().any(|slot| slot.interface_member_id == *member))
			{
				return self.lower_generic_bound(interface, member, receiver, arguments);
			}
		}
		let (member, demand, external, implementation) = match dispatch {
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
				..
			} => (
				member.clone(),
				Some(member.clone()),
				false,
				Some(implementation.clone()),
			),
			crate::StableDispatch::SelectedImplementation {
				member,
				implementation,
				..
			}
			| crate::StableDispatch::InterfaceDefault {
				member,
				implementation,
				..
			} => {
				let request = StableShapeRequest::Implementation(implementation.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				let slot = shape
					.member_slots
					.iter()
					.find(|slot| slot.interface_member_id == *member || slot.member_id == *member)
					.ok_or_else(|| StableLoweringError::MissingImplementationSlot {
						implementation: implementation.clone(),
						member: member.clone(),
					})?;
				(
					slot.member_id.clone(),
					Some(slot.member_id.clone()),
					false,
					Some(implementation.clone()),
				)
			}
			crate::StableDispatch::GenericBound { member, .. } => (member.clone(), None, false, None),
			crate::StableDispatch::External {
				member,
				implementation,
				..
			} => {
				let request = StableShapeRequest::Implementation(implementation.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				let slot = shape
					.member_slots
					.iter()
					.find(|slot| slot.interface_member_id == *member || slot.member_id == *member)
					.ok_or_else(|| StableLoweringError::MismatchedExternalMember {
						member: member.clone(),
						implementation: implementation.clone(),
					})?;
				(
					slot.member_id.clone(),
					Some(slot.member_id.clone()),
					true,
					Some(implementation.clone()),
				)
			}
		};
		let materialized_member = self.implementation_slots.and_then(|slots| {
			slots
				.iter()
				.find(|slot| slot.interface_member_id == member)
				.map(|slot| &slot.member_id)
		});
		let member = materialized_member.cloned().unwrap_or(member);
		let demand = if materialized_member.is_some() {
			Some(member.clone())
		} else {
			demand
		};
		if let Some(demand) = demand {
			self.demands.borrow_mut().insert(demand);
		}
		if external {
			let fact = self
				.context
				.stable_shape(&crate::StableShapeRequest::ExternalAbi(member.clone()))?;
			let crate::StableShapeFact::ExternalAbi(abi) = fact else {
				return Err(
					StableShapeLookupError::WrongFact {
						request: crate::StableShapeRequest::ExternalAbi(member.clone()),
					}
					.into(),
				);
			};
			let module = abi
				.module
				.ok_or_else(|| invalid(&self.artifact.definition, "external dispatch has no module"))?;
			let symbol = abi
				.symbol
				.ok_or_else(|| invalid(&self.artifact.definition, "external dispatch has no symbol"))?;
			let module: &'static str = Box::leak(module.to_string().into_boxed_str());
			let symbol: &'static str = Box::leak(symbol.to_string().into_boxed_str());
			let mut args = vec![self.lower(receiver)?];
			args.extend(
				arguments
					.into_iter()
					.map(|arg| self.lower(arg))
					.collect::<Result<Vec<_>, _>>()?,
			);
			return Ok(HirExpr::ExternCall {
				module,
				symbol,
				args,
			});
		}
		if let Some(implementation) = implementation
			&& primitive_extension_member(self.context, &member, &implementation)?
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

	fn lower_generic_bound(
		&self,
		interface: &DefinitionId,
		member: &DefinitionId,
		receiver: &Expr,
		arguments: Vec<&Expr>,
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
		if arguments.len() != 1 {
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
		let mut cases = Vec::new();
		for implementation in implementations {
			if implementation.interface.as_ref() != Some(interface) {
				return Err(invalid(
					&self.artifact.definition,
					"generic dispatch implementation belongs to another interface",
				));
			}
			let Some(receiver_tag) = stable_runtime_tag(&implementation.self_type) else {
				continue;
			};
			let argument_type = implementation
				.interface_arguments
				.iter()
				.find(|(name, _)| name == "Other")
				.map(|(_, ty)| ty)
				.unwrap_or(&implementation.self_type);
			let Some(argument_tag) = stable_runtime_tag(argument_type) else {
				continue;
			};
			if receiver_tag != argument_tag {
				continue;
			}
			let slot = implementation
				.member_slots
				.iter()
				.find(|slot| slot.interface_member_id == *member)
				.ok_or_else(|| StableLoweringError::MissingImplementationSlot {
					implementation: implementation.id.clone(),
					member: member.clone(),
				})?;
			let body = self.context.runtime_definition(&slot.body_definition_id)?;
			let target =
				match &body.payload {
					crate::RuntimePayload::External(abi) => {
						let module = abi.module.as_ref().ok_or_else(|| {
							invalid(&body.definition, "generic dispatch external has no module")
						})?;
						let symbol = abi.symbol.as_ref().ok_or_else(|| {
							invalid(&body.definition, "generic dispatch external has no symbol")
						})?;
						HirBoundDispatchTarget::Extern {
							module: Box::leak(module.to_string().into_boxed_str()),
							symbol: Box::leak(symbol.to_string().into_boxed_str()),
						}
					}
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
			self
				.demands
				.borrow_mut()
				.insert(slot.body_definition_id.clone());
			cases.push(HirBoundDispatchCase {
				receiver_tag,
				argument_tag,
				target,
			});
		}
		cases.sort_by(|left, right| {
			(&left.receiver_tag, &left.argument_tag).cmp(&(&right.receiver_tag, &right.argument_tag))
		});
		Ok(HirExpr::BoundDispatch {
			interface: interface_shape.name,
			method,
			receiver: Box::new(self.lower(receiver)?),
			argument: Box::new(self.lower(arguments[0])?),
			cases,
		})
	}
	fn lower_spread(&self, value: &Expr) -> Result<HirExpr, StableLoweringError> {
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
				let (it, next) = match iteration {
					crate::RuntimeIteration::Direct { next, .. } => (source, next),
					crate::RuntimeIteration::ViaIter { iter, next, .. } => {
						(self.lower_dispatch_value(iter, source)?, next)
					}
				};
				let next = self.context.member_name(next)?.as_str().into();
				Ok(drain_spread(it, next))
			}
		}
	}
	fn lower_string(
		&self,
		parts: &[nymph_ast::Spanned<StringPart>],
	) -> Result<HirExpr, StableLoweringError> {
		let mut result = vec![];
		let mut text = EcoString::new();
		let mut interpolated = false;
		for part in parts {
			match &part.0 {
				StringPart::Text(value) => text.push_str(value),
				StringPart::EscapeSequence(value) => text.push_str(&value.to_string()),
				StringPart::InterpolatedExpr(value) => {
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
	fn lower_cast(&self, expr: &Expr, lhs: &Expr) -> Result<HirExpr, StableLoweringError> {
		let source = peel_mut(self.ty(lhs)?);
		let target = peel_mut(self.ty(expr)?);
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
	fn lower_pattern(
		&self,
		pattern: &nymph_ast::Spanned<Pattern>,
	) -> Result<HirPat, StableLoweringError> {
		let id = self.pattern_ids.get(&pattern.1).copied();
		let variant = id.and_then(|id| {
			self
				.annotations
				.pattern_variants
				.iter()
				.find(|(found, _)| *found == id)
				.map(|(_, value)| value)
		});
		Ok(match &pattern.0 {
			Pattern::Placeholder => HirPat::Wildcard,
			Pattern::Int(v) => HirPat::Lit(HirLit::Num(v.0 as f64, NumKind::Int)),
			Pattern::UInt(v) => HirPat::Lit(HirLit::Num(v.0 as f64, NumKind::UInt)),
			Pattern::Float(v) => HirPat::Lit(HirLit::Num(v.0.into_inner(), NumKind::Float)),
			Pattern::Boolean(v) => HirPat::Lit(HirLit::Bool(v.0)),
			Pattern::Char(v) => HirPat::Lit(HirLit::Char(v.0)),
			Pattern::String(parts) => HirPat::Lit(HirLit::Str(string_pattern(parts))),
			Pattern::Grouped(inner) => self.lower_pattern(inner)?,
			Pattern::Binding { name, inner } if variant.is_none() => HirPat::Binding {
				name: self.declare(&name.0),
				sub: (!matches!(inner.0, Pattern::Placeholder))
					.then(|| self.lower_pattern(inner).map(Box::new))
					.transpose()?,
			},
			Pattern::Binding { .. } => self.variant_pattern(variant.unwrap(), vec![])?,
			Pattern::Struct { fields, .. } => {
				let fields = self.lower_struct_pattern_fields(fields)?;
				if let Some(variant) = variant {
					self.variant_pattern(variant, fields)?
				} else {
					HirPat::Struct { fields }
				}
			}
			Pattern::Tuple(items) => HirPat::Tuple(
				items
					.iter()
					.filter_map(|item| {
						if let ListPatternEntry::Item(value) = &item.0 {
							Some(self.lower_pattern(value))
						} else {
							None
						}
					})
					.collect::<Result<_, _>>()?,
			),
			Pattern::List(items) => {
				let mut prefix = vec![];
				let mut suffix = vec![];
				let mut rest = None;
				for item in items {
					match &item.0 {
						ListPatternEntry::Item(value) => {
							if rest.is_none() {
								prefix.push(self.lower_pattern(value)?)
							} else {
								suffix.push(self.lower_pattern(value)?)
							}
						}
						ListPatternEntry::Rest(name) => {
							rest = Some(name.as_ref().map(|name| self.declare(&name.0)))
						}
					}
				}
				HirPat::List {
					kind: HirArrayKind::List,
					prefix,
					rest,
					suffix,
				}
			}
			Pattern::Map(items) => {
				let mut entries = vec![];
				let mut rest = None;
				for item in items {
					match &item.0 {
						MapPatternEntry::Entry(key, value) => {
							entries.push((literal_pattern(key)?, self.lower_pattern(value)?))
						}
						MapPatternEntry::Rest(name) => {
							rest = Some(name.as_ref().map(|name| self.declare(&name.0)))
						}
					}
				}
				HirPat::Map { entries, rest }
			}
			Pattern::Range(range) => HirPat::Range(range_pattern(range)?),
			Pattern::Union(left, right) => HirPat::Or(
				Box::new(self.lower_pattern(left)?),
				Box::new(self.lower_pattern(right)?),
			),
		})
	}
	fn lower_struct_pattern_fields(
		&self,
		fields: &[nymph_ast::Spanned<StructPatternField>],
	) -> Result<Vec<(EcoString, HirPat)>, StableLoweringError> {
		let mut result = vec![];
		for field in fields {
			match &field.0 {
				StructPatternField::Value { name, value } => {
					result.push((name.0.clone(), self.lower_pattern(value)?))
				}
				StructPatternField::Named(name) => result.push((
					name.0.clone(),
					HirPat::Binding {
						name: self.declare(&name.0),
						sub: None,
					},
				)),
				StructPatternField::Positional(value) => {
					let pid = self.pattern_ids.get(&field.1).ok_or_else(|| {
						invalid(
							&self.artifact.definition,
							"missing positional pattern identity",
						)
					})?;
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
					result.push((exact, self.lower_pattern(value)?));
				}
				StructPatternField::Rest => {}
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
			self.demands.borrow_mut().insert(definition.clone());
		}
		Ok(())
	}
	fn lower_for(
		&self,
		variable: &nymph_ast::Spanned<Pattern>,
		iterable: &Expr,
		body: &Expr,
	) -> Result<HirExpr, StableLoweringError> {
		let native_range = matches!(
			iterable.kind,
			ExprKind::Range(RangeKind::Exclusive { .. } | RangeKind::Inclusive { .. })
		);
		let source = if let ExprKind::Range(
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max },
		) = &iterable.kind
		{
			HirExpr::New {
				class: "NymphRange".into(),
				fields: vec![
					("start".into(), self.lower(min)?),
					("end".into(), self.lower(max)?),
					(
						"inclusive".into(),
						HirExpr::Bool(matches!(
							iterable.kind,
							ExprKind::Range(RangeKind::Inclusive { .. })
						)),
					),
				],
			}
		} else {
			self.lower(iterable)?
		};
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
			crate::RuntimeIteration::Direct { next, option, .. } => {
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
				iterator_interface,
				next,
				option,
				..
			} => {
				self.demand_concrete_iteration_next(iter, iterable_interface, iterator_interface, next)?;
				let lowered = if matches!(peel_mut(self.ty(iterable)?), InterfaceType::List(_)) {
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
						.first()
						.ok_or_else(|| {
							invalid(
								&self.artifact.definition,
								"native List Iterable implementation has no iter slot",
							)
						})?
						.member_id;
					self.demand_external(member)?;
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
		let body = self.lower(body)?;
		self.scopes.borrow_mut().pop();
		let call = HirExpr::Call {
			callee: Box::new(HirExpr::Field {
				recv: Box::new(HirExpr::Local(it_name.clone())),
				name: self.context.member_name(next)?.as_str().into(),
			}),
			args: vec![],
		};
		let option_name: EcoString = self.context.binding_name(option)?.as_str().into();
		Ok(HirExpr::Block {
			stmts: vec![
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
					cond: Box::new(HirExpr::Local(go.clone())),
					body: Box::new(HirExpr::Match {
						scrutinee: Box::new(call),
						arms: vec![
							HirArm {
								pat: HirPat::Variant {
									enum_name: option_name.clone(),
									variant: "Some".into(),
									fields: vec![("value".into(), pat)],
								},
								guard: None,
								body,
							},
							HirArm {
								pat: HirPat::Variant {
									enum_name: option_name,
									variant: "None".into(),
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
				}),
			],
			tail: None,
		})
	}

	fn demand_concrete_iteration_next(
		&self,
		iter: &crate::StableDispatch,
		iterable_interface: &DefinitionId,
		iterator_interface: &DefinitionId,
		next: &DefinitionId,
	) -> Result<(), StableLoweringError> {
		let selected = match iter {
			crate::StableDispatch::SelectedImplementation { implementation, .. }
			| crate::StableDispatch::InterfaceDefault { implementation, .. }
			| crate::StableDispatch::Direct { implementation, .. } => Some(implementation.clone()),
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
			return Ok(());
		};
		let request = StableShapeRequest::Implementation(implementation.clone());
		let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		let iter_member = match iter {
			crate::StableDispatch::SelectedImplementation { member, .. }
			| crate::StableDispatch::InterfaceDefault { member, .. }
			| crate::StableDispatch::Direct { member, .. } => member,
			crate::StableDispatch::Builtin { .. } => shape
				.member_slots
				.first()
				.map(|slot| &slot.interface_member_id)
				.ok_or_else(|| StableLoweringError::MissingImplementationSlot {
					implementation: implementation.clone(),
					member: next.clone(),
				})?,
			_ => return Ok(()),
		};
		let slot = shape
			.member_slots
			.iter()
			.find(|slot| slot.interface_member_id == *iter_member || slot.member_id == *iter_member)
			.ok_or_else(|| StableLoweringError::MissingImplementationSlot {
				implementation: implementation.clone(),
				member: iter_member.clone(),
			})?;
		let request = StableShapeRequest::Member(slot.member_id.clone());
		let StableShapeFact::Member(member) = self.context.stable_shape(&request)? else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		let InterfaceType::Named {
			definition: iterator,
			..
		} = &member.return_type
		else {
			return Ok(());
		};
		let request = StableShapeRequest::ImplementationsForInterface(iterator_interface.clone());
		let StableShapeFact::Implementations(implementations) = self.context.stable_shape(&request)?
		else {
			return Err(StableShapeLookupError::WrongFact { request }.into());
		};
		let mut found = false;
		for implementation in &implementations {
			if matches!(peel_mut(&implementation.self_type), InterfaceType::Named { definition, .. } if definition == iterator)
				&& let Some(slot) = implementation
					.member_slots
					.iter()
					.find(|slot| slot.interface_member_id == *next)
			{
				self.demand_external(&slot.member_id)?;
				found = true;
				break;
			}
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
			crate::StableDispatch::Direct { implementation, .. }
			| crate::StableDispatch::SelectedImplementation { implementation, .. }
			| crate::StableDispatch::InterfaceDefault { implementation, .. } => {
				let request = StableShapeRequest::Implementation(implementation.clone());
				let StableShapeFact::Implementation(shape) = self.context.stable_shape(&request)? else {
					return Err(StableShapeLookupError::WrongFact { request }.into());
				};
				shape
					.member_slots
					.iter()
					.find(|slot| slot.interface_member_id == *member)
					.map(|slot| slot.member_id.clone())
			}
			_ => None,
		};
		self.demand_external(concrete.as_ref().unwrap_or(member))?;
		if let Some(concrete) = concrete {
			return Ok(HirExpr::Call {
				callee: Box::new(HirExpr::Local(
					self.context.binding_name(&concrete)?.as_str().into(),
				)),
				args: vec![receiver],
			});
		}
		Ok(HirExpr::Call {
			callee: Box::new(HirExpr::Field {
				recv: Box::new(receiver),
				name: self.context.member_name(member)?.as_str().into(),
			}),
			args: vec![],
		})
	}
	fn lower_block(
		&self,
		body: &[nymph_ast::Spanned<Statement>],
		new_scope: bool,
	) -> Result<HirExpr, StableLoweringError> {
		if new_scope {
			self.scopes.borrow_mut().push(HashMap::new());
		}
		let mut stmts = vec![];
		let mut tail = None;
		for (index, statement) in body.iter().enumerate() {
			let last = index + 1 == body.len();
			match &statement.0 {
				Statement::Let { meta, value } => {
					let value = self.lower(value)?;
					let name = self.declare(pattern_name(&meta.name)?);
					stmts.push(HirStmt::Let {
						name,
						mutable: meta.kind == LetKind::Mut,
						value,
					});
				}
				Statement::Expr(expr) if matches!(expr.kind, ExprKind::Return { .. }) => {
					let ExprKind::Return { value, label: None } = &expr.kind else {
						return Err(self.unsupported(expr, "labeled return"));
					};
					stmts.push(HirStmt::Return(
						value.as_ref().map(|value| self.lower(value)).transpose()?,
					));
				}
				Statement::Expr(expr) if last => tail = Some(Box::new(self.lower(expr)?)),
				Statement::Expr(expr) => stmts.push(HirStmt::Expr(self.lower(expr)?)),
			}
		}
		if new_scope {
			self.scopes.borrow_mut().pop();
		}
		Ok(HirExpr::Block { stmts, tail })
	}
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
fn result_for(op: BinaryOperator) -> BuiltinResult {
	match op {
		BinaryOperator::Equals
		| BinaryOperator::NotEquals
		| BinaryOperator::LessThan
		| BinaryOperator::LessThanEquals
		| BinaryOperator::GreaterThan
		| BinaryOperator::GreaterThanEquals
		| BinaryOperator::BoolAnd
		| BinaryOperator::BoolOr => BuiltinResult::Boolean,
		_ => BuiltinResult::Int,
	}
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

fn drain_spread(iterator: HirExpr, next: EcoString) -> HirExpr {
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
			enum_name: "Option".into(),
			variant: "Some".into(),
			fields: vec![(
				"value".into(),
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
			enum_name: "Option".into(),
			variant: "None".into(),
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
				cond: Box::new(HirExpr::Local(go)),
				body: Box::new(HirExpr::Match {
					scrutinee: Box::new(next_call),
					arms: vec![some, none],
				}),
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
fn string_pattern(parts: &[nymph_ast::Spanned<nymph_ast::expr::StringPatternPart>]) -> EcoString {
	let mut result = EcoString::new();
	for part in parts {
		match &part.0 {
			nymph_ast::expr::StringPatternPart::Text(text) => result.push_str(text),
			nymph_ast::expr::StringPatternPart::EscapeSequence(escape) => {
				result.push_str(&escape.to_string())
			}
		}
	}
	result
}
fn literal_pattern(pattern: &nymph_ast::Spanned<Pattern>) -> Result<HirLit, StableLoweringError> {
	Ok(match &pattern.0 {
		Pattern::Int(v) => HirLit::Num(v.0 as f64, NumKind::Int),
		Pattern::UInt(v) => HirLit::Num(v.0 as f64, NumKind::UInt),
		Pattern::Float(v) => HirLit::Num(v.0.into_inner(), NumKind::Float),
		Pattern::Boolean(v) => HirLit::Bool(v.0),
		Pattern::Char(v) => HirLit::Char(v.0),
		Pattern::String(parts) => HirLit::Str(string_pattern(parts)),
		_ => {
			return Err(StableLoweringError::Unsupported {
				definition: dummy_id(),
				node: None,
				feature: "non-literal map/range pattern".into(),
			});
		}
	})
}
fn range_pattern(range: &RangePatternKind) -> Result<HirRange, StableLoweringError> {
	Ok(match range {
		RangePatternKind::From(value) => HirRange::From(literal_pattern(value)?),
		RangePatternKind::To(value) => HirRange::To(literal_pattern(value)?),
		RangePatternKind::ToInclusive(value) => HirRange::ToInclusive(literal_pattern(value)?),
		RangePatternKind::Exclusive { min, max } => HirRange::Exclusive {
			min: literal_pattern(min)?,
			max: literal_pattern(max)?,
		},
		RangePatternKind::Inclusive { min, max } => HirRange::Inclusive {
			min: literal_pattern(min)?,
			max: literal_pattern(max)?,
		},
	})
}

fn walk_body_patterns<'a>(expr: &'a Expr, out: &mut Vec<&'a nymph_ast::Spanned<Pattern>>) {
	match &expr.kind {
		ExprKind::Closure { params, .. } => {
			for param in params {
				walk_pattern(&param.0.name, out)
			}
		}
		ExprKind::For { variable, .. } => walk_pattern(variable, out),
		ExprKind::Match { arms, .. } => {
			for arm in arms {
				walk_pattern(&arm.pattern, out)
			}
		}
		ExprKind::PatternOp { rhs, .. } => walk_pattern(rhs, out),
		ExprKind::Block { body, .. } => {
			for statement in body {
				if let Statement::Let { meta, .. } = &statement.0 {
					walk_pattern(&meta.name, out)
				}
			}
		}
		_ => {}
	}
	let mut expressions = vec![];
	walk(expr, &mut expressions);
	for child in expressions.into_iter().skip(1) {
		match &child.kind {
			ExprKind::Closure { params, .. } => {
				for param in params {
					walk_pattern(&param.0.name, out)
				}
			}
			ExprKind::For { variable, .. } => walk_pattern(variable, out),
			ExprKind::Match { arms, .. } => {
				for arm in arms {
					walk_pattern(&arm.pattern, out)
				}
			}
			ExprKind::PatternOp { rhs, .. } => walk_pattern(rhs, out),
			ExprKind::Block { body, .. } => {
				for statement in body {
					if let Statement::Let { meta, .. } = &statement.0 {
						walk_pattern(&meta.name, out)
					}
				}
			}
			_ => {}
		}
	}
}
fn walk_pattern<'a>(
	pattern: &'a nymph_ast::Spanned<Pattern>,
	out: &mut Vec<&'a nymph_ast::Spanned<Pattern>>,
) {
	out.push(pattern);
	match &pattern.0 {
		Pattern::Binding { inner, .. } | Pattern::Grouped(inner) => walk_pattern(inner, out),
		Pattern::List(items) | Pattern::Tuple(items) => {
			for item in items {
				if let ListPatternEntry::Item(value) = &item.0 {
					walk_pattern(value, out)
				}
			}
		}
		Pattern::Map(items) => {
			for item in items {
				if let MapPatternEntry::Entry(key, value) = &item.0 {
					walk_pattern(key, out);
					walk_pattern(value, out)
				}
			}
		}
		Pattern::Range(range) => match range {
			RangePatternKind::From(v) | RangePatternKind::To(v) | RangePatternKind::ToInclusive(v) => {
				walk_pattern(v, out)
			}
			RangePatternKind::Exclusive { min, max } | RangePatternKind::Inclusive { min, max } => {
				walk_pattern(min, out);
				walk_pattern(max, out)
			}
		},
		Pattern::Struct { fields, .. } => {
			for field in fields {
				match &field.0 {
					StructPatternField::Value { value, .. } | StructPatternField::Positional(value) => {
						walk_pattern(value, out)
					}
					_ => {}
				}
			}
		}
		Pattern::Union(a, b) => {
			walk_pattern(a, out);
			walk_pattern(b, out)
		}
		_ => {}
	}
}

fn walk<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
	out.push(expr);
	let mut child = |value| walk(value, out);
	match &expr.kind {
		ExprKind::String(parts) => {
			for part in parts {
				if let StringPart::InterpolatedExpr(value) = &part.0 {
					child(value);
				}
			}
		}
		ExprKind::List(items) | ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(value) | ListItem::Spread(value) => child(value),
				}
			}
		}
		ExprKind::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapEntry::Entry(key, value) => {
						child(key);
						child(value);
					}
					MapEntry::Spread(value) => child(value),
				}
			}
		}
		ExprKind::Call { func, args, .. } => {
			child(func);
			for arg in args {
				child(&arg.0.value);
			}
		}
		ExprKind::MemberAccess { parent, .. } => child(parent),
		ExprKind::IndexAccess { parent, index, .. }
		| ExprKind::BinaryOp {
			lhs: parent,
			rhs: index,
			..
		}
		| ExprKind::AssignOp {
			lhs: parent,
			rhs: index,
			..
		} => {
			child(parent);
			child(index);
		}
		ExprKind::Closure { body, .. }
		| ExprKind::PrefixOp { value: body, .. }
		| ExprKind::PostfixOp { value: body, .. }
		| ExprKind::Grouped(body)
		| ExprKind::TypeOp { lhs: body, .. }
		| ExprKind::PatternOp { lhs: body, .. } => child(body),
		ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
			if let Some(value) = value {
				child(value);
			}
		}
		ExprKind::While {
			condition, body, ..
		} => {
			child(condition);
			child(body);
		}
		ExprKind::For { iterable, body, .. } => {
			child(iterable);
			child(body);
		}
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			child(condition);
			child(then);
			if let Some(value) = otherwise {
				child(value);
			}
		}
		ExprKind::Match { value, arms } => {
			child(value);
			for arm in arms {
				if let Some(guard) = &arm.guard {
					child(guard);
				}
				child(&arm.body);
			}
		}
		ExprKind::Block { body, .. } => {
			for statement in body {
				match &statement.0 {
					Statement::Expr(value) | Statement::Let { value, .. } => child(value),
				}
			}
		}
		ExprKind::Range(range) => match range {
			RangeKind::From(value) | RangeKind::To(value) | RangeKind::ToInclusive(value) => child(value),
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				child(min);
				child(max);
			}
		},
		ExprKind::Int(_)
		| ExprKind::UInt(_)
		| ExprKind::Float(_)
		| ExprKind::Char(_)
		| ExprKind::Boolean(_)
		| ExprKind::Identifier(_)
		| ExprKind::AnonymousParam(_)
		| ExprKind::This
		| ExprKind::Continue { .. } => {}
	}
}
