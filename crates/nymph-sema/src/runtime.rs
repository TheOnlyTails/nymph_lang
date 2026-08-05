//! Per-definition checked runtime artifacts.
//!
//! These values are deliberately upstream of HIR. Each artifact owns one source
//! definition and the exact stable checker decisions required to lower that
//! definition; it never retains a module or dependency AST.

use std::sync::Arc;

use ecow::EcoString;
use nymph_ast::{
	decl::{
		FuncDeclaration, FuncParam, ImplMember, InterfaceElement, InterfaceMember, LetDeclaration,
	},
	expr::{
		CallArg, ClosureParam, Expr, ExprKind, ListItem, ListPatternEntry, MapEntry, MapPatternEntry,
		Pattern, RangeKind, RangePatternKind, Statement, StringEscape, StringPart, StringPatternPart,
		StructPatternField,
	},
	ops::{
		AssignOperator, BinaryOperator, PatternOperator, PostfixOperator, PrefixOperator, TypeOperator,
	},
};

use crate::{
	CanonicalizationContext, DefinitionId, DispatchKind, InterfaceType, ModuleIdentity,
	canonicalize_type,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct BodyNodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct PatternNodeId(pub u32);

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct StableBody {
	pub params: Arc<[StableParameter]>,
	pub root: StableExpr,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct StableParameter {
	pub pattern: StablePattern,
	pub mutable: bool,
	pub spread: bool,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct StableExpr {
	pub id: BodyNodeId,
	pub kind: StableExprKind,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableExprKind {
	Int(u64),
	UInt(u64),
	Float(ordered_float::OrderedFloat<f64>),
	Char(char),
	String(Arc<[StableStringPart]>),
	Boolean(bool),
	Identifier(EcoString),
	AnonymousParam(Option<u8>),
	List(Arc<[StableListItem]>),
	Tuple(Arc<[StableListItem]>),
	Map(Arc<[StableMapEntry]>),
	Range(StableRange),
	Call {
		func: Box<StableExpr>,
		args: Arc<[StableCallArg]>,
	},
	MemberAccess {
		parent: Box<StableExpr>,
		member: EcoString,
		optional: bool,
	},
	IndexAccess {
		parent: Box<StableExpr>,
		index: Box<StableExpr>,
		optional: bool,
	},
	Closure {
		params: Arc<[StableParameter]>,
		body: Box<StableExpr>,
	},
	PrefixOp {
		op: PrefixOperator,
		value: Box<StableExpr>,
	},
	PostfixOp {
		op: PostfixOperator,
		value: Box<StableExpr>,
	},
	BinaryOp {
		lhs: Box<StableExpr>,
		op: BinaryOperator,
		rhs: Box<StableExpr>,
	},
	TypeOp {
		lhs: Box<StableExpr>,
		op: TypeOperator,
	},
	PatternOp {
		lhs: Box<StableExpr>,
		op: PatternOperator,
		rhs: StablePattern,
	},
	AssignOp {
		lhs: Box<StableExpr>,
		op: AssignOperator,
		rhs: Box<StableExpr>,
	},
	Return {
		value: Option<Box<StableExpr>>,
		label: Option<EcoString>,
	},
	Break {
		value: Option<Box<StableExpr>>,
		label: Option<EcoString>,
	},
	Continue {
		label: Option<EcoString>,
	},
	While {
		condition: Box<StableExpr>,
		body: Box<StableExpr>,
		label: Option<EcoString>,
	},
	For {
		variable: StablePattern,
		iterable: Box<StableExpr>,
		body: Box<StableExpr>,
		label: Option<EcoString>,
	},
	If {
		condition: Box<StableExpr>,
		then: Box<StableExpr>,
		otherwise: Option<Box<StableExpr>>,
	},
	Match {
		value: Box<StableExpr>,
		arms: Arc<[StableMatchArm]>,
	},
	This,
	Block {
		body: Arc<[StableStatement]>,
		label: Option<EcoString>,
	},
	Grouped(Box<StableExpr>),
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableStringPart {
	Text(EcoString),
	Escape(StringEscape),
	Expr(StableExpr),
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableListItem {
	Expr(StableExpr),
	Spread(StableExpr),
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableMapEntry {
	Entry(StableExpr, StableExpr),
	Spread(StableExpr),
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct StableCallArg {
	pub value: StableExpr,
	pub name: Option<EcoString>,
	pub spread: bool,
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableRange {
	From(Box<StableExpr>),
	To(Box<StableExpr>),
	Exclusive {
		min: Box<StableExpr>,
		max: Box<StableExpr>,
	},
	ToInclusive(Box<StableExpr>),
	Inclusive {
		min: Box<StableExpr>,
		max: Box<StableExpr>,
	},
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableStatement {
	Expr(StableExpr),
	Let {
		pattern: StablePattern,
		mutable: bool,
		value: StableExpr,
	},
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct StableMatchArm {
	pub pattern: StablePattern,
	pub guard: Option<StableExpr>,
	pub body: StableExpr,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct StablePattern {
	pub id: PatternNodeId,
	pub kind: StablePatternKind,
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StablePatternKind {
	Int(i64),
	UInt(u64),
	Float(ordered_float::OrderedFloat<f64>),
	Char(char),
	String(Arc<[StableStringPatternPart]>),
	Boolean(bool),
	Binding {
		name: EcoString,
		inner: Box<StablePattern>,
	},
	List(Arc<[StableListPatternEntry]>),
	Tuple(Arc<[StableListPatternEntry]>),
	Map(Arc<[StableMapPatternEntry]>),
	Range(StablePatternRange),
	Struct {
		path: Arc<[EcoString]>,
		fields: Arc<[StableStructPatternField]>,
	},
	Placeholder,
	Union(Box<StablePattern>, Box<StablePattern>),
	Grouped(Box<StablePattern>),
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableStringPatternPart {
	Text(EcoString),
	Escape(StringEscape),
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableListPatternEntry {
	Item(StablePattern),
	Rest(Option<EcoString>),
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableMapPatternEntry {
	Entry(StablePattern, StablePattern),
	Rest(Option<EcoString>),
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StableStructPatternField {
	Value {
		name: EcoString,
		value: StablePattern,
	},
	Named {
		id: PatternNodeId,
		name: EcoString,
	},
	Positional {
		id: PatternNodeId,
		pattern: StablePattern,
	},
	Rest,
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub enum StablePatternRange {
	From(Box<StablePattern>),
	To(Box<StablePattern>),
	Exclusive {
		min: Box<StablePattern>,
		max: Box<StablePattern>,
	},
	ToInclusive(Box<StablePattern>),
	Inclusive {
		min: Box<StablePattern>,
		max: Box<StablePattern>,
	},
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum BuiltinDispatch {
	Eager,
	ShortCircuit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum DispatchMaterialization {
	Attached,
	CanonicalBody,
	ExternalAbi,
}

/// Complete, location-free dispatch selected by the checker. Variants encode
/// which provenance is mandatory, so an incomplete selected target cannot be
/// represented.
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum StableDispatch {
	Builtin {
		method: EcoString,
		category: BuiltinDispatch,
	},
	Direct {
		member: DefinitionId,
		implementation: DefinitionId,
		materialization: DispatchMaterialization,
	},
	SelectedImplementation {
		interface: DefinitionId,
		member: DefinitionId,
		implementation: DefinitionId,
		materialization: DispatchMaterialization,
	},
	InterfaceDefault {
		interface: DefinitionId,
		member: DefinitionId,
		implementation: DefinitionId,
		materialization: DispatchMaterialization,
	},
	GenericBound {
		interface: DefinitionId,
		member: DefinitionId,
	},
	External {
		member: DefinitionId,
		implementation: DefinitionId,
		marshal: Option<nymph_hir::hir::MarshalKind>,
	},
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum VariantExpressionMode {
	Value,
	Constructor,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum VariantPatternMode {
	Unit,
	Destructure,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct StableVariantField {
	pub name: EcoString,
	pub definition: DefinitionId,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ExpressionVariant {
	pub enum_definition: DefinitionId,
	pub variant_definition: DefinitionId,
	pub variant_name: EcoString,
	pub fields: Vec<StableVariantField>,
	pub mode: VariantExpressionMode,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct PatternVariant {
	pub enum_definition: DefinitionId,
	pub variant_definition: DefinitionId,
	pub variant_name: EcoString,
	pub fields: Vec<StableVariantField>,
	pub mode: VariantPatternMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum RuntimeIteration {
	Direct {
		iterator_interface: DefinitionId,
		next: DefinitionId,
		option: crate::OptionRuntimeRole,
	},
	ViaIter {
		iterable_interface: DefinitionId,
		iter_interface_member: DefinitionId,
		iter: StableDispatch,
		iterator_interface: DefinitionId,
		next: DefinitionId,
		option: crate::OptionRuntimeRole,
	},
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum RuntimePlacement {
	TopLevel,
	Attached {
		owner: DefinitionId,
		name: EcoString,
	},
}

/// Stable, body-local lowering channels. New lowering side tables must be added
/// here rather than recovered later through names or spans.
#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct RuntimeAnnotations {
	pub option: Option<crate::OptionRuntimeRole>,
	pub types: Arc<[(BodyNodeId, InterfaceType)]>,
	pub definition_targets: Arc<[(BodyNodeId, DefinitionId)]>,
	pub direct_namespace_members: Arc<[BodyNodeId]>,
	pub dispatches: Arc<[(BodyNodeId, StableDispatch)]>,
	pub variants: Arc<[(BodyNodeId, ExpressionVariant)]>,
	pub pattern_variants: Arc<[(PatternNodeId, PatternVariant)]>,
	pub positional_fields: Arc<[(PatternNodeId, StableVariantField)]>,
	pub iterations: Arc<[(BodyNodeId, RuntimeIteration)]>,
	pub anonymous_closures: Arc<[(BodyNodeId, u8)]>,
	pub generic_namespaced_calls: Arc<[BodyNodeId]>,
	pub external_marshals: Arc<[(BodyNodeId, nymph_hir::hir::MarshalKind)]>,
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub enum RuntimeBodyKind {
	Value,
	InstanceFunction,
	StaticFunction,
}

#[derive(Clone, Debug, salsa::SalsaValue)]
pub struct CheckedRuntimeBody {
	pub kind: RuntimeBodyKind,
	pub stable: StableBody,
	pub annotations: RuntimeAnnotations,
}

impl PartialEq for CheckedRuntimeBody {
	fn eq(&self, other: &Self) -> bool {
		self.kind == other.kind && self.stable == other.stable && self.annotations == other.annotations
	}
}
impl Eq for CheckedRuntimeBody {}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct StructShell {
	pub binders: Vec<crate::GenericParameter>,
	pub constraints: Vec<crate::GenericConstraint>,
	pub fields: Vec<crate::FieldShape<InterfaceType>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct EnumShell {
	pub binders: Vec<crate::GenericParameter>,
	pub constraints: Vec<crate::GenericConstraint>,
	pub variants: Vec<crate::VariantShape<InterfaceType>>,
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub enum RuntimePayload {
	NymphBody(CheckedRuntimeBody),
	MaterializedInterfaceMember {
		body_definition: DefinitionId,
		interface_member: DefinitionId,
	},
	External(crate::ExternalAbi),
	Struct(StructShell),
	Enum(EnumShell),
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct RuntimeDefinition {
	pub definition: DefinitionId,
	pub source_owner: ModuleIdentity,
	pub placement: RuntimePlacement,
	pub payload: RuntimePayload,
}

/// Project top-level runtime artifacts directly from checker facts. Member and
/// aggregate channels are represented by the schema and will be connected to
/// production lowering in the next #79 unit; no compatibility lookup is used.
pub fn runtime_definitions(
	module: &nymph_ast::decl::Module,
	checked: &crate::CheckedFacts,
	interface: &crate::ModuleInterface,
) -> Result<Vec<RuntimeDefinition>, RuntimeExtractionError> {
	let mut result = Vec::new();
	let shapes = interface
		.exports
		.iter()
		.chain(interface.support_definitions.iter().map(|s| &s.definition))
		.collect::<Vec<_>>();
	let shape = |category, name: &str| {
		shapes.iter().copied().find(|shape| matches!(&shape.id.key, crate::DeclarationKey::TopLevel { category: found, .. } if *found == category) && shape.name == name)
	};
	for (declaration_index, declaration) in module.members.iter().enumerate() {
		match declaration {
			nymph_ast::decl::Declaration::Func { meta, body, .. } => {
				let definition = required_top_level(checked, &meta.name.0)?;
				push_body(
					&mut result,
					definition,
					RuntimePlacement::TopLevel,
					meta,
					body,
					checked,
				)?;
			}
			nymph_ast::decl::Declaration::Let { meta, value, .. } => {
				let name = binding_name(meta)?;
				push_value(
					&mut result,
					required_top_level(checked, name)?,
					RuntimePlacement::TopLevel,
					value,
					checked,
				)?;
			}
			nymph_ast::decl::Declaration::ExternalFunc(_, marker, meta) => {
				let definition = required_top_level(checked, &meta.name.0)?;
				let abi = shape(crate::DeclarationCategory::Function, &meta.name.0)
					.and_then(|item| item.external.clone())
					.unwrap_or_else(|| crate::interface_extract::external_function_abi(marker));
				push_external(
					&mut result,
					definition,
					RuntimePlacement::TopLevel,
					Some(abi),
				)?;
			}
			nymph_ast::decl::Declaration::ExternalLet(_, marker, meta) => {
				let name = binding_name(meta)?;
				let definition = required_top_level(checked, name)?;
				let abi = shape(crate::DeclarationCategory::Let, name)
					.and_then(|item| item.external.clone())
					.unwrap_or_else(|| {
						crate::interface_extract::external_value_abi(
							marker,
							checked.external_value_marshals.get(&meta.name.1).copied(),
						)
					});
				push_external(
					&mut result,
					definition,
					RuntimePlacement::TopLevel,
					Some(abi),
				)?;
			}
			nymph_ast::decl::Declaration::Struct {
				name,
				members,
				impls,
				..
			}
			| nymph_ast::decl::Declaration::Enum {
				name,
				members,
				impls,
				..
			} => {
				let item = shapes
					.iter()
					.copied()
					.find(|s| {
						s.name == name.0
							&& matches!(
								s.kind,
								crate::DefinitionShapeKind::Struct | crate::DefinitionShapeKind::Enum
							)
					})
					.ok_or_else(|| RuntimeExtractionError::MissingStableId(name.0.clone()))?;
				let payload = match item.kind {
					crate::DefinitionShapeKind::Struct => RuntimePayload::Struct(StructShell {
						binders: item.binders.clone(),
						constraints: item.constraints.clone(),
						fields: item.fields.clone(),
					}),
					crate::DefinitionShapeKind::Enum => RuntimePayload::Enum(EnumShell {
						binders: item.binders.clone(),
						constraints: item.constraints.clone(),
						variants: item.variants.clone(),
					}),
					_ => unreachable!(),
				};
				result.push(RuntimeDefinition {
					definition: item.id.clone(),
					source_owner: item.id.module.clone(),
					placement: RuntimePlacement::TopLevel,
					payload,
				});
				extract_members(&mut result, members, &item.members, false, checked)?;
				for (nested_index, nested) in impls.iter().enumerate() {
					let path = crate::annotate::ImplementationSourcePath {
						declaration: declaration_index as u32,
						nested: Some(nested_index as u32),
					};
					let implementation = required_implementation(interface, checked, path)?;
					extract_implementation_members(
						&mut result,
						&nested.0.members,
						implementation,
						checked,
						path,
					)?;
				}
			}
			nymph_ast::decl::Declaration::Namespace { name, members, .. } => {
				let item = shape(crate::DeclarationCategory::Namespace, &name.0)
					.ok_or_else(|| RuntimeExtractionError::MissingStableId(name.0.clone()))?;
				extract_members(&mut result, members, &item.members, true, checked)?;
			}
			nymph_ast::decl::Declaration::Impl { members, .. }
			| nymph_ast::decl::Declaration::ImplFor { members, .. } => {
				let path = crate::annotate::ImplementationSourcePath {
					declaration: declaration_index as u32,
					nested: None,
				};
				let implementation = required_implementation(interface, checked, path)?;
				extract_implementation_members(&mut result, members, implementation, checked, path)?;
			}
			nymph_ast::decl::Declaration::Interface { name, members, .. } => {
				let item = shape(crate::DeclarationCategory::Interface, &name.0)
					.ok_or_else(|| RuntimeExtractionError::MissingStableId(name.0.clone()))?;
				let mut defaults = item.members.iter();
				for member in members {
					match &member.0 {
						InterfaceMember::Element(element) => {
							let member = defaults
								.next()
								.ok_or(RuntimeExtractionError::MissingImplementation)?;
							match &element.0 {
								InterfaceElement::Func {
									meta,
									body: Some(body),
								} => push_body(
									&mut result,
									member.id.clone(),
									attached(member),
									meta,
									body,
									checked,
								)?,
								InterfaceElement::Let {
									meta: _,
									value: Some(value),
								} => push_value(
									&mut result,
									member.id.clone(),
									attached(member),
									value,
									checked,
								)?,
								_ => {}
							}
						}
						InterfaceMember::Impl { .. } => {}
					}
				}
			}
			_ => {}
		}
	}
	for implementation in &interface.implementations {
		for slot in &implementation.member_slots {
			if slot.implementation_id != implementation.id
				|| slot.placement_owner != implementation.id
				|| slot.member_id.module != implementation.id.module
			{
				return Err(RuntimeExtractionError::CorruptImplementationMemberMapping(
					slot.member_id.clone(),
				));
			}
			if slot.source != crate::ImplementationMemberSource::InheritedDefault {
				continue;
			}
			if result
				.iter()
				.any(|artifact| artifact.definition == slot.member_id)
			{
				return Err(RuntimeExtractionError::CorruptImplementationMemberMapping(
					slot.member_id.clone(),
				));
			}
			result.push(RuntimeDefinition {
				definition: slot.member_id.clone(),
				source_owner: slot.body_definition_id.module.clone(),
				placement: RuntimePlacement::Attached {
					owner: slot.placement_owner.clone(),
					name: slot.name.clone(),
				},
				payload: RuntimePayload::MaterializedInterfaceMember {
					body_definition: slot.body_definition_id.clone(),
					interface_member: slot.interface_member_id.clone(),
				},
			});
		}
	}
	Ok(result)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum RuntimeExtractionError {
	IncompleteCanonicalType,
	MissingStableId(EcoString),
	MissingImplementation,
	MissingSourceIdentity,
	MissingExternalAbi,
	IncompleteDispatchTarget(EcoString),
	IncompleteVariantTarget(EcoString),
	MissingIterationProtocol,
	CorruptImplementationMemberMapping(DefinitionId),
	DuplicateRuntimeDefinition(DefinitionId),
	MissingBodyExpressionIdentity,
}

fn required_top_level(
	checked: &crate::CheckedFacts,
	name: &str,
) -> Result<DefinitionId, RuntimeExtractionError> {
	checked
		.semantic
		.definitions
		.get(name)
		.and_then(|id| checked.semantic.definitions.stable(id))
		.cloned()
		.ok_or_else(|| RuntimeExtractionError::MissingStableId(name.into()))
}
fn binding_name(meta: &LetDeclaration) -> Result<&str, RuntimeExtractionError> {
	if let nymph_ast::expr::Pattern::Binding { name, .. } = &meta.name.0 {
		Ok(&name.0)
	} else {
		Err(RuntimeExtractionError::MissingStableId("<pattern>".into()))
	}
}
fn attached(member: &crate::MemberShape<InterfaceType>) -> RuntimePlacement {
	RuntimePlacement::Attached {
		owner: member
			.runtime_owner
			.clone()
			.unwrap_or_else(|| match &member.id.key {
				crate::DeclarationKey::Member { owner, .. } => (**owner).clone(),
				_ => member.id.clone(),
			}),
		name: member.name.clone(),
	}
}
fn push_external(
	result: &mut Vec<RuntimeDefinition>,
	definition: DefinitionId,
	placement: RuntimePlacement,
	abi: Option<crate::ExternalAbi>,
) -> Result<(), RuntimeExtractionError> {
	result.push(RuntimeDefinition {
		source_owner: definition.module.clone(),
		definition,
		placement,
		payload: RuntimePayload::External(abi.ok_or(RuntimeExtractionError::MissingExternalAbi)?),
	});
	Ok(())
}
fn extract_members(
	result: &mut Vec<RuntimeDefinition>,
	syntax: &[nymph_ast::Spanned<ImplMember>],
	shapes: &[crate::MemberShape<InterfaceType>],
	module_placed: bool,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	if syntax.len() != shapes.len() {
		return Err(RuntimeExtractionError::MissingImplementation);
	}
	for (syntax, shape) in syntax.iter().zip(shapes) {
		let placement = || {
			if module_placed {
				RuntimePlacement::TopLevel
			} else {
				attached(shape)
			}
		};
		match &syntax.0 {
			ImplMember::Func { meta, body, .. } => {
				push_body(result, shape.id.clone(), placement(), meta, body, checked)?
			}
			ImplMember::Let { value, .. } => {
				push_value(result, shape.id.clone(), placement(), value, checked)?
			}
			ImplMember::ExternalFunc(..) | ImplMember::ExternalLet(..) => push_external(
				result,
				shape.id.clone(),
				placement(),
				shape.external.clone(),
			)?,
		}
	}
	Ok(())
}

fn required_implementation<'a>(
	interface: &'a crate::ModuleInterface,
	checked: &crate::CheckedFacts,
	path: crate::annotate::ImplementationSourcePath,
) -> Result<&'a crate::ExportedImpl, RuntimeExtractionError> {
	let id = checked
		.source_identities
		.implementations
		.get(&path)
		.ok_or(RuntimeExtractionError::MissingSourceIdentity)?;
	interface
		.implementations
		.iter()
		.find(|implementation| &implementation.id == id)
		.ok_or(RuntimeExtractionError::MissingImplementation)
}

fn extract_implementation_members(
	result: &mut Vec<RuntimeDefinition>,
	syntax: &[nymph_ast::Spanned<ImplMember>],
	implementation: &crate::ExportedImpl,
	checked: &crate::CheckedFacts,
	path: crate::annotate::ImplementationSourcePath,
) -> Result<(), RuntimeExtractionError> {
	for (member_index, syntax) in syntax.iter().enumerate() {
		let name = match &syntax.0 {
			ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => meta.name.0.as_str(),
			ImplMember::Let { meta, .. } | ImplMember::ExternalLet(_, _, meta) => binding_name(meta)?,
		};
		let definition = checked
			.source_identities
			.members
			.get(&crate::annotate::ImplementationMemberSourcePath {
				implementation: path,
				member: member_index as u32,
			})
			.cloned()
			.ok_or(RuntimeExtractionError::MissingSourceIdentity)?;
		let member_shape = implementation
			.members
			.iter()
			.find(|member| member.id == definition)
			.ok_or_else(|| {
				RuntimeExtractionError::CorruptImplementationMemberMapping(definition.clone())
			})?;
		let placement = RuntimePlacement::Attached {
			owner: implementation.id.clone(),
			name: name.into(),
		};
		match &syntax.0 {
			ImplMember::Func { meta, body, .. } => {
				push_body(result, definition, placement, meta, body, checked)?
			}
			ImplMember::Let { value, .. } => push_value(result, definition, placement, value, checked)?,
			ImplMember::ExternalFunc(..) => {
				push_external(result, definition, placement, member_shape.external.clone())?
			}
			ImplMember::ExternalLet(..) => {
				push_external(result, definition, placement, member_shape.external.clone())?
			}
		}
	}
	Ok(())
}
fn push_value(
	result: &mut Vec<RuntimeDefinition>,
	definition: DefinitionId,
	placement: RuntimePlacement,
	value: &Expr,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	push_canonical_body(
		result,
		definition,
		placement,
		RuntimeBodyKind::Value,
		&[],
		value,
		checked,
	)
}

fn push_body(
	result: &mut Vec<RuntimeDefinition>,
	definition: DefinitionId,
	placement: RuntimePlacement,
	meta: &FuncDeclaration,
	body: &Expr,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	push_canonical_body(
		result,
		definition,
		placement,
		if meta.kind == nymph_ast::decl::FuncKind::Namespace {
			RuntimeBodyKind::StaticFunction
		} else {
			RuntimeBodyKind::InstanceFunction
		},
		&meta.params,
		body,
		checked,
	)
}

fn push_canonical_body(
	result: &mut Vec<RuntimeDefinition>,
	definition: DefinitionId,
	placement: RuntimePlacement,
	kind: RuntimeBodyKind,
	params: &[nymph_ast::Spanned<FuncParam>],
	body: &Expr,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	let mut nodes = Vec::new();
	walk_expr(body, &mut nodes);
	let local = nodes
		.iter()
		.enumerate()
		.map(|(index, expr)| (expr.id, BodyNodeId(index as u32)))
		.collect::<std::collections::HashMap<_, _>>();
	let native_range_nodes = nodes
		.iter()
		.filter(|expr| {
			matches!(
				expr.kind,
				ExprKind::Range(RangeKind::Exclusive { .. } | RangeKind::Inclusive { .. })
			)
		})
		.filter_map(|expr| local.get(&expr.id).copied())
		.collect::<std::collections::HashSet<_>>();
	let required_type_nodes = required_type_nodes(&nodes, checked);
	let builder = StableBodyBuilder::new(&local);
	let stable = builder.body(params, body)?;
	let pattern_sites = builder.pattern_sites.into_inner();
	let positional_sites = builder.positional_sites.into_inner();
	let annotations = runtime_annotations(
		&definition,
		&local,
		&pattern_sites,
		&positional_sites,
		&native_range_nodes,
		&required_type_nodes,
		checked,
	)?;
	result.push(RuntimeDefinition {
		source_owner: definition.module.clone(),
		definition,
		placement,
		payload: RuntimePayload::NymphBody(CheckedRuntimeBody {
			kind,
			stable,
			annotations,
		}),
	});
	Ok(())
}

struct StableBodyBuilder<'a> {
	expressions: &'a std::collections::HashMap<nymph_ast::NodeId, BodyNodeId>,
	next_pattern: std::cell::Cell<u32>,
	pattern_sites: std::cell::RefCell<std::collections::HashMap<nymph_ast::Span, PatternNodeId>>,
	positional_sites: std::cell::RefCell<std::collections::HashMap<nymph_ast::Span, PatternNodeId>>,
}

impl<'a> StableBodyBuilder<'a> {
	fn new(expressions: &'a std::collections::HashMap<nymph_ast::NodeId, BodyNodeId>) -> Self {
		Self {
			expressions,
			next_pattern: std::cell::Cell::new(0),
			pattern_sites: std::cell::RefCell::new(std::collections::HashMap::new()),
			positional_sites: std::cell::RefCell::new(std::collections::HashMap::new()),
		}
	}
	fn next_pattern_id(&self) -> PatternNodeId {
		let next = self.next_pattern.get();
		self.next_pattern.set(next + 1);
		PatternNodeId(next)
	}
	fn body(
		&self,
		params: &[nymph_ast::Spanned<FuncParam>],
		root: &Expr,
	) -> Result<StableBody, RuntimeExtractionError> {
		Ok(StableBody {
			params: params
				.iter()
				.map(|p| self.func_param(&p.0))
				.collect::<Result<_, _>>()?,
			root: self.expr(root)?,
		})
	}
	fn func_param(&self, param: &FuncParam) -> Result<StableParameter, RuntimeExtractionError> {
		Ok(StableParameter {
			pattern: self.pattern_param(&param.name)?,
			mutable: param.mutable,
			spread: param.spread,
		})
	}
	fn closure_param(&self, param: &ClosureParam) -> Result<StableParameter, RuntimeExtractionError> {
		Ok(StableParameter {
			pattern: self.pattern_param(&param.name)?,
			mutable: param.mutable,
			spread: param.spread,
		})
	}
	fn pattern_param(
		&self,
		pattern: &nymph_ast::Spanned<Pattern>,
	) -> Result<StablePattern, RuntimeExtractionError> {
		self.pattern(pattern)
	}
	fn pattern(
		&self,
		pattern: &nymph_ast::Spanned<Pattern>,
	) -> Result<StablePattern, RuntimeExtractionError> {
		let id = self.next_pattern_id();
		self
			.pattern_sites
			.borrow_mut()
			.entry(pattern.1)
			.or_insert(id);
		self.pattern_with_id(pattern, id)
	}
	fn pattern_with_id(
		&self,
		pattern: &nymph_ast::Spanned<Pattern>,
		id: PatternNodeId,
	) -> Result<StablePattern, RuntimeExtractionError> {
		let kind = match &pattern.0 {
			Pattern::Int(v) => StablePatternKind::Int(v.0),
			Pattern::UInt(v) => StablePatternKind::UInt(v.0),
			Pattern::Float(v) => StablePatternKind::Float(v.0),
			Pattern::Char(v) => StablePatternKind::Char(v.0),
			Pattern::Boolean(v) => StablePatternKind::Boolean(v.0),
			Pattern::String(parts) => StablePatternKind::String(
				parts
					.iter()
					.map(|p| {
						Ok(match &p.0 {
							StringPatternPart::Text(v) => StableStringPatternPart::Text(v.clone()),
							StringPatternPart::EscapeSequence(v) => StableStringPatternPart::Escape(*v),
						})
					})
					.collect::<Result<_, _>>()?,
			),
			Pattern::Binding { name, inner } => StablePatternKind::Binding {
				name: name.0.clone(),
				inner: Box::new(self.pattern(inner)?),
			},
			Pattern::List(v) => StablePatternKind::List(
				v.iter()
					.map(|e| self.list_pattern(&e.0))
					.collect::<Result<_, _>>()?,
			),
			Pattern::Tuple(v) => StablePatternKind::Tuple(
				v.iter()
					.map(|e| self.list_pattern(&e.0))
					.collect::<Result<_, _>>()?,
			),
			Pattern::Map(v) => StablePatternKind::Map(
				v.iter()
					.map(|e| {
						Ok(match &e.0 {
							MapPatternEntry::Entry(k, v) => {
								StableMapPatternEntry::Entry(self.pattern(k)?, self.pattern(v)?)
							}
							MapPatternEntry::Rest(n) => {
								StableMapPatternEntry::Rest(n.as_ref().map(|n| n.0.clone()))
							}
						})
					})
					.collect::<Result<_, _>>()?,
			),
			Pattern::Range(v) => StablePatternKind::Range(self.pattern_range(v)?),
			Pattern::Struct { path, fields } => StablePatternKind::Struct {
				path: path.iter().map(|n| n.0.clone()).collect(),
				fields: fields
					.iter()
					.map(|f| {
						Ok(match &f.0 {
							StructPatternField::Value { name, value } => StableStructPatternField::Value {
								name: name.0.clone(),
								value: self.pattern(value)?,
							},
							StructPatternField::Named(n) => {
								let id = self.next_pattern_id();
								self.pattern_sites.borrow_mut().insert(f.1, id);
								self.positional_sites.borrow_mut().insert(f.1, id);
								StableStructPatternField::Named {
									id,
									name: n.0.clone(),
								}
							}
							StructPatternField::Positional(v) => {
								let id = self.next_pattern_id();
								self.positional_sites.borrow_mut().insert(f.1, id);
								let pattern = self.pattern(v)?;
								StableStructPatternField::Positional { id, pattern }
							}
							StructPatternField::Rest => StableStructPatternField::Rest,
						})
					})
					.collect::<Result<_, _>>()?,
			},
			Pattern::Placeholder => StablePatternKind::Placeholder,
			Pattern::Union(a, b) => {
				StablePatternKind::Union(Box::new(self.pattern(a)?), Box::new(self.pattern(b)?))
			}
			Pattern::Grouped(v) => StablePatternKind::Grouped(Box::new(self.pattern(v)?)),
		};
		Ok(StablePattern { id, kind })
	}
	fn list_pattern(
		&self,
		entry: &ListPatternEntry,
	) -> Result<StableListPatternEntry, RuntimeExtractionError> {
		Ok(match entry {
			ListPatternEntry::Item(v) => StableListPatternEntry::Item(self.pattern(v)?),
			ListPatternEntry::Rest(n) => StableListPatternEntry::Rest(n.as_ref().map(|n| n.0.clone())),
		})
	}
	fn pattern_range(
		&self,
		range: &RangePatternKind,
	) -> Result<StablePatternRange, RuntimeExtractionError> {
		Ok(match range {
			RangePatternKind::From(v) => StablePatternRange::From(Box::new(self.pattern(v)?)),
			RangePatternKind::To(v) => StablePatternRange::To(Box::new(self.pattern(v)?)),
			RangePatternKind::Exclusive { min, max } => StablePatternRange::Exclusive {
				min: Box::new(self.pattern(min)?),
				max: Box::new(self.pattern(max)?),
			},
			RangePatternKind::ToInclusive(v) => {
				StablePatternRange::ToInclusive(Box::new(self.pattern(v)?))
			}
			RangePatternKind::Inclusive { min, max } => StablePatternRange::Inclusive {
				min: Box::new(self.pattern(min)?),
				max: Box::new(self.pattern(max)?),
			},
		})
	}
	fn expr(&self, expr: &Expr) -> Result<StableExpr, RuntimeExtractionError> {
		let id = self
			.expressions
			.get(&expr.id)
			.copied()
			.ok_or(RuntimeExtractionError::MissingBodyExpressionIdentity)?;
		let boxed = |v: &Expr| self.expr(v).map(Box::new);
		let label = |v: &Option<nymph_ast::Ident>| v.as_ref().map(|n| n.0.clone());
		let kind = match &expr.kind {
			ExprKind::Int(v) => StableExprKind::Int(v.0),
			ExprKind::UInt(v) => StableExprKind::UInt(v.0),
			ExprKind::Float(v) => StableExprKind::Float(v.0),
			ExprKind::Char(v) => StableExprKind::Char(v.0),
			ExprKind::Boolean(v) => StableExprKind::Boolean(v.0),
			ExprKind::Identifier(v) => StableExprKind::Identifier(v.0.clone()),
			ExprKind::AnonymousParam(v) => StableExprKind::AnonymousParam(*v),
			ExprKind::This => StableExprKind::This,
			ExprKind::String(v) => StableExprKind::String(
				v.iter()
					.map(|p| {
						Ok(match &p.0 {
							StringPart::Text(v) => StableStringPart::Text(v.clone()),
							StringPart::EscapeSequence(v) => StableStringPart::Escape(*v),
							StringPart::InterpolatedExpr(v) => StableStringPart::Expr(self.expr(v)?),
						})
					})
					.collect::<Result<_, _>>()?,
			),
			ExprKind::List(v) => StableExprKind::List(
				v.iter()
					.map(|v| self.list_item(&v.0))
					.collect::<Result<_, _>>()?,
			),
			ExprKind::Tuple(v) => StableExprKind::Tuple(
				v.iter()
					.map(|v| self.list_item(&v.0))
					.collect::<Result<_, _>>()?,
			),
			ExprKind::Map(v) => StableExprKind::Map(
				v.iter()
					.map(|v| {
						Ok(match &v.0 {
							MapEntry::Entry(k, v) => StableMapEntry::Entry(self.expr(k)?, self.expr(v)?),
							MapEntry::Spread(v) => StableMapEntry::Spread(self.expr(v)?),
						})
					})
					.collect::<Result<_, _>>()?,
			),
			ExprKind::Range(v) => StableExprKind::Range(match v {
				RangeKind::From(v) => StableRange::From(boxed(v)?),
				RangeKind::To(v) => StableRange::To(boxed(v)?),
				RangeKind::Exclusive { min, max } => StableRange::Exclusive {
					min: boxed(min)?,
					max: boxed(max)?,
				},
				RangeKind::ToInclusive(v) => StableRange::ToInclusive(boxed(v)?),
				RangeKind::Inclusive { min, max } => StableRange::Inclusive {
					min: boxed(min)?,
					max: boxed(max)?,
				},
			}),
			ExprKind::Call { func, args, .. } => StableExprKind::Call {
				func: boxed(func)?,
				args: args
					.iter()
					.map(|a| self.call_arg(&a.0))
					.collect::<Result<_, _>>()?,
			},
			ExprKind::MemberAccess {
				parent,
				member,
				optional,
			} => StableExprKind::MemberAccess {
				parent: boxed(parent)?,
				member: member.0.clone(),
				optional: *optional,
			},
			ExprKind::IndexAccess {
				parent,
				index,
				optional,
			} => StableExprKind::IndexAccess {
				parent: boxed(parent)?,
				index: boxed(index)?,
				optional: *optional,
			},
			ExprKind::Closure { params, body, .. } => StableExprKind::Closure {
				params: params
					.iter()
					.map(|p| self.closure_param(&p.0))
					.collect::<Result<_, _>>()?,
				body: boxed(body)?,
			},
			ExprKind::PrefixOp { op, value } => StableExprKind::PrefixOp {
				op: *op,
				value: boxed(value)?,
			},
			ExprKind::PostfixOp { op, value } => StableExprKind::PostfixOp {
				op: *op,
				value: boxed(value)?,
			},
			ExprKind::BinaryOp { lhs, op, rhs } => StableExprKind::BinaryOp {
				lhs: boxed(lhs)?,
				op: *op,
				rhs: boxed(rhs)?,
			},
			ExprKind::TypeOp { lhs, op, .. } => StableExprKind::TypeOp {
				lhs: boxed(lhs)?,
				op: *op,
			},
			ExprKind::PatternOp { lhs, op, rhs } => StableExprKind::PatternOp {
				lhs: boxed(lhs)?,
				op: *op,
				rhs: self.pattern(rhs)?,
			},
			ExprKind::AssignOp { lhs, op, rhs } => StableExprKind::AssignOp {
				lhs: boxed(lhs)?,
				op: *op,
				rhs: boxed(rhs)?,
			},
			ExprKind::Return { value, label: l } => StableExprKind::Return {
				value: value.as_deref().map(boxed).transpose()?,
				label: label(l),
			},
			ExprKind::Break { value, label: l } => StableExprKind::Break {
				value: value.as_deref().map(boxed).transpose()?,
				label: label(l),
			},
			ExprKind::Continue { label: l } => StableExprKind::Continue { label: label(l) },
			ExprKind::While {
				condition,
				body,
				label: l,
			} => StableExprKind::While {
				condition: boxed(condition)?,
				body: boxed(body)?,
				label: label(l),
			},
			ExprKind::For {
				variable,
				iterable,
				body,
				label: l,
			} => StableExprKind::For {
				variable: self.pattern(variable)?,
				iterable: boxed(iterable)?,
				body: boxed(body)?,
				label: label(l),
			},
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => StableExprKind::If {
				condition: boxed(condition)?,
				then: boxed(then)?,
				otherwise: otherwise.as_deref().map(boxed).transpose()?,
			},
			ExprKind::Match { value, arms } => StableExprKind::Match {
				value: boxed(value)?,
				arms: arms
					.iter()
					.map(|a| {
						Ok(StableMatchArm {
							pattern: self.pattern(&a.pattern)?,
							guard: a.guard.as_ref().map(|v| self.expr(v)).transpose()?,
							body: self.expr(&a.body)?,
						})
					})
					.collect::<Result<_, _>>()?,
			},
			ExprKind::Block { body, label: l } => StableExprKind::Block {
				body: body
					.iter()
					.map(|s| {
						Ok(match &s.0 {
							Statement::Expr(v) => StableStatement::Expr(self.expr(v)?),
							Statement::Let { meta, value } => StableStatement::Let {
								pattern: self.pattern(&meta.name)?,
								mutable: meta.is_mutable(),
								value: self.expr(value)?,
							},
						})
					})
					.collect::<Result<_, _>>()?,
				label: label(l),
			},
			ExprKind::Grouped(v) => StableExprKind::Grouped(boxed(v)?),
		};
		Ok(StableExpr { id, kind })
	}
	fn list_item(&self, item: &ListItem) -> Result<StableListItem, RuntimeExtractionError> {
		Ok(match item {
			ListItem::Expr(v) => StableListItem::Expr(self.expr(v)?),
			ListItem::Spread(v) => StableListItem::Spread(self.expr(v)?),
		})
	}
	fn call_arg(&self, arg: &CallArg) -> Result<StableCallArg, RuntimeExtractionError> {
		Ok(StableCallArg {
			value: self.expr(&arg.value)?,
			name: arg.name.as_ref().map(|n| n.0.clone()),
			spread: arg.spread,
		})
	}
}

fn runtime_annotations(
	definition: &DefinitionId,
	local: &std::collections::HashMap<nymph_ast::NodeId, BodyNodeId>,
	pattern_sites: &std::collections::HashMap<nymph_ast::Span, PatternNodeId>,
	positional_sites: &std::collections::HashMap<nymph_ast::Span, PatternNodeId>,
	native_range_nodes: &std::collections::HashSet<BodyNodeId>,
	required_type_nodes: &std::collections::HashSet<nymph_ast::NodeId>,
	checked: &crate::CheckedFacts,
) -> Result<RuntimeAnnotations, RuntimeExtractionError> {
	let definitions: std::collections::HashMap<crate::DefId, DefinitionId> = checked
		.semantic
		.definitions
		.defs
		.iter()
		.enumerate()
		.filter_map(|(index, definition)| {
			definition
				.stable
				.clone()
				.map(|stable| (crate::DefId(index as u32), stable))
		})
		.collect();
	let parameters = body_parameters(definition, checked);
	let self_parameter = definition_owner(definition).is_some_and(|owner| {
		matches!(
			&owner.key,
			crate::DeclarationKey::TopLevel {
				category: crate::DeclarationCategory::Interface,
				..
			}
		)
	});
	let self_parameter = self_parameter.then(|| crate::ParamIdx(parameters.len() as u32));
	let context = CanonicalizationContext::new(definitions, parameters);
	let context = if let Some(parameter) = self_parameter {
		context.with_self_parameter(parameter)
	} else {
		context
	};
	let mut types = Vec::new();
	let mut dispatches = Vec::new();
	for (node, info) in checked.annotations.infos() {
		let Some(&id) = local.get(&node) else {
			continue;
		};
		if required_type_nodes.contains(&node) {
			types.push((
				id,
				required_canonical_type(&checked.interner, info.ty, &context)?,
			));
		}
		if let Some(resolution) = &info.resolution {
			dispatches.push((id, stable_dispatch(checked, resolution)?));
		}
	}
	let mut definition_targets = checked
		.annotations
		.definition_targets()
		.filter_map(|(id, target)| local.get(&id).map(|id| (*id, target.clone())))
		.collect::<Vec<_>>();
	let mut variants = checked
		.annotations
		.variants()
		.filter_map(|(id, variant)| local.get(&id).map(|id| (*id, variant)))
		.map(|(id, variant)| Ok((id, expression_variant(checked, variant)?)))
		.collect::<Result<Vec<_>, RuntimeExtractionError>>()?;
	let mut pattern_variants = checked
		.annotations
		.pattern_variants()
		.filter_map(|(span, variant)| pattern_sites.get(&span).map(|id| (*id, variant)))
		.map(|(id, variant)| Ok((id, pattern_variant(checked, variant)?)))
		.collect::<Result<Vec<_>, RuntimeExtractionError>>()?;
	let mut positional_fields = positional_sites
		.iter()
		.filter_map(|(span, id)| {
			checked.annotations.positional_field_of(*span).map(|field| {
				field
					.definition
					.clone()
					.ok_or_else(|| RuntimeExtractionError::IncompleteVariantTarget(field.name.clone()))
					.map(|definition| {
						(
							*id,
							StableVariantField {
								name: field.name.clone(),
								definition,
							},
						)
					})
			})
		})
		.collect::<Result<Vec<_>, RuntimeExtractionError>>()?;
	let mut iterations = Vec::new();
	let mut anonymous_closures = Vec::new();
	let mut generic_namespaced_calls = Vec::new();
	for (&source, &id) in local {
		if let Some(mode) = checked.annotations.iter_mode_of(source).or_else(|| {
			native_range_nodes
				.contains(&id)
				.then_some(crate::IterMode::Direct)
		}) {
			let protocols = iteration_protocol(checked)?;
			let iteration = match mode {
				crate::IterMode::Direct => RuntimeIteration::Direct {
					iterator_interface: protocols.0.clone(),
					next: protocols.1.clone(),
					option: protocols.2.clone(),
				},
				crate::IterMode::ViaIter => {
					let resolution = checked
						.annotations
						.iter_resolution_of(source)
						.ok_or(RuntimeExtractionError::MissingIterationProtocol)?;
					let (iterable_interface, iter_interface_member) =
						match resolution.resolved_target.as_ref() {
							Some(crate::annotate::ResolvedMethodTarget::InterfaceImplementation {
								interface,
								slot,
							}) => (interface.clone(), slot.interface_member_id.clone()),
							Some(crate::annotate::ResolvedMethodTarget::GenericBound {
								interface,
								interface_member,
							}) => (interface.clone(), interface_member.clone()),
							_ => return Err(RuntimeExtractionError::MissingIterationProtocol),
						};
					RuntimeIteration::ViaIter {
						iterable_interface,
						iter_interface_member,
						iter: stable_dispatch(checked, resolution)?,
						iterator_interface: protocols.0.clone(),
						next: protocols.1.clone(),
						option: protocols.2.clone(),
					}
				}
			};
			iterations.push((id, iteration));
		}
		if let Some(arity) = checked.annotations.anon_boundary_arity(source) {
			anonymous_closures.push((id, arity));
		}
		if checked.annotations.is_generic_namespaced_call(source) {
			generic_namespaced_calls.push(id);
		}
	}
	types.sort_by_key(|item| item.0);
	definition_targets.sort_by_key(|item| item.0);
	dispatches.sort_by_key(|item| item.0);
	variants.sort_by_key(|item| item.0);
	pattern_variants.sort_by_key(|item| item.0);
	positional_fields.sort_by_key(|item| item.0);
	iterations.sort_by_key(|item| item.0);
	anonymous_closures.sort_by_key(|item| item.0);
	generic_namespaced_calls.sort_unstable();
	let mut direct_namespace_members = checked
		.annotations
		.direct_namespace_members()
		.filter_map(|source| local.get(&source).copied())
		.collect::<Vec<_>>();
	direct_namespace_members.sort_unstable();
	let mut external_marshals = Vec::new();
	for (id, target) in &definition_targets {
		if let Some(marshal) = external_marshal(checked, target) {
			external_marshals.push((*id, marshal));
		}
	}
	Ok(RuntimeAnnotations {
		option: checked.runtime_roles.option.clone(),
		types: types.into(),
		definition_targets: definition_targets.into(),
		direct_namespace_members: direct_namespace_members.into(),
		dispatches: dispatches.into(),
		variants: variants.into(),
		pattern_variants: pattern_variants.into(),
		positional_fields: positional_fields.into(),
		iterations: iterations.into(),
		anonymous_closures: anonymous_closures.into(),
		generic_namespaced_calls: generic_namespaced_calls.into(),
		external_marshals: external_marshals.into(),
	})
}

fn required_canonical_type(
	interner: &crate::ty::Interner,
	ty: crate::Ty,
	context: &CanonicalizationContext,
) -> Result<InterfaceType, RuntimeExtractionError> {
	canonicalize_type(interner, ty, context)
		.map_err(|_| RuntimeExtractionError::IncompleteCanonicalType)
}

fn required_type_nodes(
	nodes: &[&Expr],
	checked: &crate::CheckedFacts,
) -> std::collections::HashSet<nymph_ast::NodeId> {
	let mut required = std::collections::HashSet::new();
	for expression in nodes {
		match &expression.kind {
			ExprKind::While { .. } => {
				required.insert(expression.id);
			}
			// Persist only a contextual widening. A plain `int` literal already
			// carries its runtime kind syntactically and some recovered declaration
			// paths do not annotate that redundant fact.
			ExprKind::Int(_)
				if checked.annotations.get(expression.id).is_some_and(|info| {
					matches!(
						checked.interner.kind(info.ty),
						crate::TyKind::UInt | crate::TyKind::Float
					)
				}) =>
			{
				required.insert(expression.id);
			}
			ExprKind::MemberAccess { .. }
				if checked
					.annotations
					.get(expression.id)
					.and_then(|info| info.resolution)
					.is_some() =>
			{
				required.insert(expression.id);
			}
			ExprKind::IndexAccess { parent, .. } => {
				required.insert(parent.id);
			}
			ExprKind::BinaryOp { lhs, op, .. } => {
				if matches!(op, BinaryOperator::Equals | BinaryOperator::NotEquals) {
					if checked
						.annotations
						.get(lhs.id)
						.is_some_and(|info| !matches!(checked.interner.kind(info.ty), crate::TyKind::Error))
					{
						required.insert(lhs.id);
					}
				}
				if checked
					.annotations
					.get(expression.id)
					.and_then(|info| info.resolution)
					.is_some_and(|resolution| {
						matches!(
							resolution.dispatch,
							DispatchKind::BuiltinEager | DispatchKind::BuiltinShortCircuit
						)
					}) {
					required.insert(expression.id);
				}
			}
			ExprKind::PrefixOp { .. }
				if checked
					.annotations
					.get(expression.id)
					.and_then(|info| info.resolution)
					.is_some_and(|resolution| {
						matches!(
							resolution.dispatch,
							DispatchKind::BuiltinEager | DispatchKind::BuiltinShortCircuit
						)
					}) =>
			{
				required.insert(expression.id);
			}
			ExprKind::AssignOp { lhs, op, .. }
				if *op != AssignOperator::Assign
					&& checked
						.annotations
						.get(expression.id)
						.and_then(|info| info.resolution)
						.is_some_and(|resolution| {
							matches!(
								resolution.dispatch,
								DispatchKind::BuiltinEager | DispatchKind::BuiltinShortCircuit
							)
						}) =>
			{
				required.insert(lhs.id);
			}
			ExprKind::List(items) | ExprKind::Tuple(items) => {
				for item in items {
					if let ListItem::Spread(value) = &item.0 {
						required.insert(value.id);
					}
				}
			}
			ExprKind::Map(entries) => {
				for entry in entries {
					if let MapEntry::Spread(value) = &entry.0 {
						required.insert(value.id);
					}
				}
			}
			ExprKind::TypeOp { lhs, .. }
				if checked
					.annotations
					.get(expression.id)
					.and_then(|info| info.resolution)
					.is_some_and(|resolution| {
						matches!(
							resolution.dispatch,
							DispatchKind::BuiltinEager | DispatchKind::BuiltinShortCircuit
						)
					}) =>
			{
				required.insert(expression.id);
				required.insert(lhs.id);
			}
			ExprKind::For { iterable, .. } => {
				required.insert(expression.id);
				if checked.annotations.iter_mode_of(iterable.id) == Some(crate::IterMode::ViaIter) {
					required.insert(iterable.id);
				}
			}
			_ => {}
		}
	}
	required
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{InterfaceConversionError, ParamIdx, ty::Interner};

	#[test]
	fn required_type_canonicalization_errors_are_loud() {
		let mut interner = Interner::new();
		let missing_binder = interner.mk_param(ParamIdx(7));

		assert_eq!(
			required_canonical_type(
				&interner,
				missing_binder,
				&CanonicalizationContext::default()
			),
			Err(RuntimeExtractionError::IncompleteCanonicalType)
		);
		assert_eq!(
			canonicalize_type(
				&interner,
				missing_binder,
				&CanonicalizationContext::default()
			),
			Err(InterfaceConversionError::UnknownBinder(ParamIdx(7)))
		);
	}
}

fn stable_dispatch(
	checked: &crate::CheckedFacts,
	resolution: &crate::annotate::Resolution,
) -> Result<StableDispatch, RuntimeExtractionError> {
	let category = match resolution.dispatch {
		DispatchKind::BuiltinEager => BuiltinDispatch::Eager,
		DispatchKind::BuiltinShortCircuit => BuiltinDispatch::ShortCircuit,
		DispatchKind::UserImpl | DispatchKind::UserImplDefaultMethod => {
			return exact_method_dispatch(checked, resolution);
		}
	};
	Ok(StableDispatch::Builtin {
		method: resolution.method.clone(),
		category,
	})
}

fn exact_method_dispatch(
	checked: &crate::CheckedFacts,
	resolution: &crate::annotate::Resolution,
) -> Result<StableDispatch, RuntimeExtractionError> {
	let target = resolution
		.resolved_target
		.as_ref()
		.ok_or_else(|| RuntimeExtractionError::IncompleteDispatchTarget(resolution.method.clone()))?;
	match target {
		crate::annotate::ResolvedMethodTarget::Inherent {
			member,
			implementation,
		} => {
			let inherent_external = checked.semantic.inherent.iter().any(|inherent| {
				inherent
					.methods
					.values()
					.any(|method| method.definition.as_ref() == Some(member) && method.external)
			});
			if inherent_external {
				return Ok(StableDispatch::External {
					member: member.clone(),
					implementation: implementation.clone(),
					marshal: None,
				});
			}
			if let Some(abi) = checked
				.semantic
				.definitions
				.by_stable(member)
				.and_then(|definition| checked.semantic.external_abis.get(&definition))
			{
				Ok(StableDispatch::External {
					member: member.clone(),
					implementation: implementation.clone(),
					marshal: abi.marshal,
				})
			} else {
				Ok(StableDispatch::Direct {
					member: member.clone(),
					implementation: implementation.clone(),
					materialization: DispatchMaterialization::Attached,
				})
			}
		}
		crate::annotate::ResolvedMethodTarget::InterfaceImplementation { interface, slot } => {
			if slot.external {
				return Ok(StableDispatch::External {
					member: slot.member_id.clone(),
					implementation: slot.implementation_id.clone(),
					marshal: None,
				});
			}
			if let Some(abi) = checked
				.semantic
				.definitions
				.by_stable(&slot.member_id)
				.and_then(|definition| checked.semantic.external_abis.get(&definition))
			{
				return Ok(StableDispatch::External {
					member: slot.member_id.clone(),
					implementation: slot.implementation_id.clone(),
					marshal: abi.marshal,
				});
			}
			Ok(match slot.source {
				crate::ImplementationMemberSource::Override => StableDispatch::SelectedImplementation {
					interface: interface.clone(),
					member: slot.member_id.clone(),
					implementation: slot.implementation_id.clone(),
					materialization: DispatchMaterialization::Attached,
				},
				crate::ImplementationMemberSource::InheritedDefault => StableDispatch::InterfaceDefault {
					interface: interface.clone(),
					member: slot.member_id.clone(),
					implementation: slot.implementation_id.clone(),
					materialization: DispatchMaterialization::CanonicalBody,
				},
			})
		}
		crate::annotate::ResolvedMethodTarget::GenericBound {
			interface,
			interface_member,
		} => Ok(StableDispatch::GenericBound {
			interface: interface.clone(),
			member: interface_member.clone(),
		}),
	}
}

fn variant_parts(
	_checked: &crate::CheckedFacts,
	resolution: &crate::annotate::VariantResolution,
) -> Result<(DefinitionId, DefinitionId), RuntimeExtractionError> {
	Ok((
		resolution
			.enum_target
			.clone()
			.ok_or_else(|| RuntimeExtractionError::IncompleteVariantTarget(resolution.variant.clone()))?,
		resolution
			.variant_target
			.clone()
			.ok_or_else(|| RuntimeExtractionError::IncompleteVariantTarget(resolution.variant.clone()))?,
	))
}
fn variant_fields(
	checked: &crate::CheckedFacts,
	resolution: &crate::annotate::VariantResolution,
) -> Result<Vec<StableVariantField>, RuntimeExtractionError> {
	let (enum_definition, _) = variant_parts(checked, resolution)?;
	let def = checked
		.semantic
		.definitions
		.defs
		.iter()
		.position(|item| item.stable.as_ref() == Some(&enum_definition))
		.map(|index| crate::DefId(index as u32))
		.ok_or_else(|| RuntimeExtractionError::IncompleteVariantTarget(resolution.variant.clone()))?;
	let variant = checked
		.semantic
		.signatures
		.enums
		.get(&def)
		.and_then(|item| {
			item
				.variants
				.iter()
				.find(|item| item.name == resolution.variant)
		})
		.ok_or_else(|| RuntimeExtractionError::IncompleteVariantTarget(resolution.variant.clone()))?;
	variant
		.fields
		.iter()
		.zip(&variant.field_metadata)
		.map(|(field, metadata)| {
			Ok(StableVariantField {
				name: field.0.clone(),
				definition: metadata
					.target
					.clone()
					.ok_or_else(|| RuntimeExtractionError::IncompleteVariantTarget(field.0.clone()))?,
			})
		})
		.collect()
}
fn expression_variant(
	checked: &crate::CheckedFacts,
	resolution: &crate::annotate::VariantResolution,
) -> Result<ExpressionVariant, RuntimeExtractionError> {
	let (enum_definition, variant_definition) = variant_parts(checked, resolution)?;
	let fields = variant_fields(checked, resolution)?;
	Ok(ExpressionVariant {
		enum_definition,
		variant_definition,
		variant_name: resolution.variant.clone(),
		mode: if fields.is_empty() {
			VariantExpressionMode::Value
		} else {
			VariantExpressionMode::Constructor
		},
		fields,
	})
}
fn pattern_variant(
	checked: &crate::CheckedFacts,
	resolution: &crate::annotate::VariantResolution,
) -> Result<PatternVariant, RuntimeExtractionError> {
	let (enum_definition, variant_definition) = variant_parts(checked, resolution)?;
	let fields = variant_fields(checked, resolution)?;
	Ok(PatternVariant {
		enum_definition,
		variant_definition,
		variant_name: resolution.variant.clone(),
		mode: if fields.is_empty() {
			VariantPatternMode::Unit
		} else {
			VariantPatternMode::Destructure
		},
		fields,
	})
}

fn body_parameters(
	definition: &DefinitionId,
	checked: &crate::CheckedFacts,
) -> std::collections::HashMap<crate::ParamIdx, crate::GenericParameterId> {
	// Rigid parameter indices are body-local and allocated in declaration order.
	// Implementation-header binders are allocated before member-local binders,
	// matching interface extraction's owner-then-member canonicalization context.
	let member_count = checked
		.semantic
		.definitions
		.defs
		.iter()
		.position(|item| item.stable.as_ref() == Some(definition))
		.and_then(|index| {
			checked
				.semantic
				.signatures
				.funcs
				.get(&crate::DefId(index as u32))
		})
		.map_or(0, |signature| signature.generics.len());
	let owner = definition_owner(definition);
	let owner_count = owner
		.map(|owner| match &owner.key {
			crate::DeclarationKey::Implementation { header, .. } => header.binders.len(),
			_ => checked
				.semantic
				.definitions
				.defs
				.iter()
				.position(|item| item.stable.as_ref() == Some(owner))
				.map(|index| crate::DefId(index as u32))
				.and_then(|owner| {
					checked
						.semantic
						.signatures
						.structs
						.get(&owner)
						.map(|signature| signature.generics.len())
						.or_else(|| {
							checked
								.semantic
								.signatures
								.enums
								.get(&owner)
								.map(|signature| signature.generics.len())
								.or_else(|| {
									checked
										.semantic
										.interfaces
										.get(&owner)
										.map(|interface| interface.generics.len())
								})
						})
				})
				.unwrap_or(0),
		})
		.unwrap_or(0);
	let owner_parameters = owner.into_iter().flat_map(|owner| {
		(0..owner_count).map(move |index| {
			(
				crate::ParamIdx(index as u32),
				crate::GenericParameterId::new(
					owner.binder(crate::BinderScope::Definition, 0),
					index as u32,
				),
			)
		})
	});
	let member_scope = if matches!(definition.key, crate::DeclarationKey::TopLevel { .. }) {
		crate::BinderScope::Definition
	} else {
		crate::BinderScope::Member
	};
	owner_parameters
		.chain((0..member_count).map(|index| {
			(
				crate::ParamIdx((owner_count + index) as u32),
				crate::GenericParameterId::new(definition.binder(member_scope, 0), index as u32),
			)
		}))
		.collect()
}

fn definition_owner(definition: &DefinitionId) -> Option<&DefinitionId> {
	match &definition.key {
		crate::DeclarationKey::Member { owner, .. }
		| crate::DeclarationKey::MethodBody { owner, .. } => Some(owner),
		_ => None,
	}
}

fn iteration_protocol(
	checked: &crate::CheckedFacts,
) -> Result<(DefinitionId, DefinitionId, crate::OptionRuntimeRole), RuntimeExtractionError> {
	let iterator = checked
		.semantic
		.compiler_runtime_roles
		.iterator
		.as_ref()
		.ok_or(RuntimeExtractionError::MissingIterationProtocol)?;
	let option = checked
		.semantic
		.compiler_runtime_roles
		.option
		.as_ref()
		.ok_or(RuntimeExtractionError::MissingIterationProtocol)?;
	Ok((
		iterator.interface.clone(),
		iterator.member.clone(),
		option.clone(),
	))
}

fn external_marshal(
	checked: &crate::CheckedFacts,
	target: &DefinitionId,
) -> Option<nymph_hir::hir::MarshalKind> {
	let definition = checked.semantic.definitions.by_stable(target)?;
	if checked.semantic.definitions.is_local(definition) {
		checked
			.external_value_marshals
			.get(&checked.semantic.definitions.data(definition).span)
			.copied()
	} else {
		checked
			.semantic
			.external_abis
			.get(&definition)
			.and_then(|abi| abi.marshal)
	}
}

fn walk_expr<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
	out.push(expr);
	let mut walk = |child: &'a Expr| walk_expr(child, out);
	match &expr.kind {
		ExprKind::String(parts) => {
			for part in parts {
				if let StringPart::InterpolatedExpr(expr) = &part.0 {
					walk(expr);
				}
			}
		}
		ExprKind::List(items) | ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(expr) | ListItem::Spread(expr) => walk(expr),
				}
			}
		}
		ExprKind::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapEntry::Entry(key, value) => {
						walk(key);
						walk(value);
					}
					MapEntry::Spread(expr) => walk(expr),
				}
			}
		}
		ExprKind::Range(range) => match range {
			RangeKind::From(a) | RangeKind::To(a) | RangeKind::ToInclusive(a) => walk(a),
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				walk(min);
				walk(max);
			}
		},
		ExprKind::Call { func, args, .. } => {
			walk(func);
			for arg in args {
				walk(&arg.0.value);
			}
		}
		ExprKind::MemberAccess { parent, .. } => walk(parent),
		ExprKind::IndexAccess { parent, index, .. } => {
			walk(parent);
			walk(index);
		}
		ExprKind::Closure { body, .. }
		| ExprKind::PrefixOp { value: body, .. }
		| ExprKind::PostfixOp { value: body, .. }
		| ExprKind::Grouped(body) => walk(body),
		ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
			walk(lhs);
			walk(rhs);
		}
		ExprKind::TypeOp { lhs, .. } | ExprKind::PatternOp { lhs, .. } => walk(lhs),
		ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
			if let Some(value) = value {
				walk(value);
			}
		}
		ExprKind::While {
			condition, body, ..
		} => {
			walk(condition);
			walk(body);
		}
		ExprKind::For { iterable, body, .. } => {
			walk(iterable);
			walk(body);
		}
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			walk(condition);
			walk(then);
			if let Some(otherwise) = otherwise {
				walk(otherwise);
			}
		}
		ExprKind::Match { value, arms } => {
			walk(value);
			for arm in arms {
				if let Some(guard) = &arm.guard {
					walk(guard);
				}
				walk(&arm.body);
			}
		}
		ExprKind::Block { body, .. } => {
			for statement in body {
				match &statement.0 {
					Statement::Expr(expr) => walk(expr),
					Statement::Let { value, .. } => walk(value),
				}
			}
		}
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
