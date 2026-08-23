//! Per-definition checked runtime artifacts.
//!
//! These values are deliberately upstream of HIR. Each artifact owns one source
//! definition and the exact stable checker decisions required to lower that
//! definition; it never retains a module or dependency AST.

use std::sync::Arc;

use ecow::EcoString;
use nymph_ast::{
	Span,
	decl::{
		FuncDeclaration, FuncParam, ImplMember, InterfaceElement, InterfaceMember, LetDeclaration,
	},
	expr::{
		CallArg, ClosureParam, Expr, ExprKind, ListItem, ListPatternEntry, MapEntry, MapPatternEntry,
		Pattern, RangeKind, RangePatternKind, Statement, StringEscape, StringPart, StringPatternPart,
		StructPatternField,
	},
	ops::{BinaryOperator, PatternOperator, PostfixOperator, PrefixOperator, TypeOperator},
};

use crate::{
	CanonicalizationContext, DefinitionId, DispatchKind, InterfaceType, ModuleIdentity,
	canonicalize_type,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct BodyNodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct PatternNodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum RangeDecision {
	Invalid,
	Safe,
	Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum RangeOperation {
	Arithmetic,
	Division,
	Remainder,
	Power,
	Shift,
	Conversion,
	HostIndex,
	Index,
	SliceExclusive,
	SliceInclusive,
}

/// Canonical evidence retained with a body-local range decision. Intervals use
/// `i128`, which exactly contains every source `int`/`uint` value and the signed
/// pair bounds needed by the bounded proof domain.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum RangeEvidence {
	Interval {
		min: i128,
		max: i128,
	},
	Target {
		min: i128,
		max: i128,
	},
	Excluded {
		operand: u8,
		value: i128,
	},
	SignedPairBound {
		left_sign: i8,
		right_sign: i8,
		upper: i128,
	},
	KnownLength(u64),
	SliceBound {
		min: i128,
		max: i128,
		inclusive: bool,
	},
	SymbolicSliceBound {
		min: i128,
		max: i128,
		inclusive: bool,
		lower: bool,
		upper: bool,
	},
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct RangeProof {
	pub operation: RangeOperation,
	pub decision: RangeDecision,
	pub evidence: Arc<[RangeEvidence]>,
}

impl RangeProof {
	/// Replays the compact certificate without consulting checker state. Safe and
	/// invalid decisions must carry evidence; unknown decisions deliberately may
	/// carry only the declared operand interval.
	pub fn audit(&self) -> bool {
		if self.decision == RangeDecision::Unknown {
			return true;
		}
		let intervals = self
			.evidence
			.iter()
			.filter_map(|evidence| match evidence {
				RangeEvidence::Interval { min, max } => Some((*min, *max)),
				_ => None,
			})
			.collect::<Vec<_>>();
		let target = self.evidence.iter().find_map(|evidence| match evidence {
			RangeEvidence::Target { min, max } => Some((*min, *max)),
			_ => None,
		});
		let length = self.evidence.iter().find_map(|evidence| match evidence {
			RangeEvidence::KnownLength(length) => Some(*length as i128),
			_ => None,
		});
		let classify_interval = |interval: Option<&(i128, i128)>, target: Option<(i128, i128)>| {
			let (Some(&(min, max)), Some((lo, hi))) = (interval, target) else {
				return RangeDecision::Unknown;
			};
			if min >= lo && max <= hi {
				RangeDecision::Safe
			} else if max < lo || min > hi {
				RangeDecision::Invalid
			} else {
				RangeDecision::Unknown
			}
		};
		let replayed = match self.operation {
			RangeOperation::Arithmetic => classify_interval(intervals.last(), target),
			RangeOperation::Conversion => classify_interval(intervals.first(), target),
			RangeOperation::Division | RangeOperation::Remainder => {
				if intervals.get(1) == Some(&(0, 0)) {
					RangeDecision::Invalid
				} else if self.evidence.iter().any(|evidence| {
					matches!(
						evidence,
						RangeEvidence::Excluded {
							operand: 1,
							value: 0
						}
					)
				}) {
					RangeDecision::Safe
				} else {
					RangeDecision::Unknown
				}
			}
			RangeOperation::Shift => match intervals.get(1) {
				Some((min, max)) if *max < 0 || *min >= 64 => RangeDecision::Invalid,
				Some((min, max)) if *min >= 0 && *max < 64 => classify_interval(intervals.last(), target),
				_ => RangeDecision::Unknown,
			},
			RangeOperation::Index => {
				if intervals.first().is_some_and(|&(min, max)| {
					length.is_some_and(|length| min >= -length && max < length)
						|| self.evidence.iter().any(|evidence| match evidence {
							RangeEvidence::SignedPairBound {
								left_sign: 1,
								right_sign: -1,
								upper,
							} => min >= 0 && *upper <= -1,
							RangeEvidence::SignedPairBound {
								left_sign: -1,
								right_sign: -1,
								upper,
							} => max <= -1 && *upper <= 0,
							_ => false,
						})
				}) {
					RangeDecision::Safe
				} else {
					match (intervals.first(), length) {
						(Some((min, max)), Some(length)) if *max < -length || *min >= length => {
							RangeDecision::Invalid
						}
						_ => RangeDecision::Unknown,
					}
				}
			}
			RangeOperation::SliceExclusive | RangeOperation::SliceInclusive => {
				let bounds = self
					.evidence
					.iter()
					.filter_map(|evidence| match evidence {
						RangeEvidence::SliceBound {
							min,
							max,
							inclusive,
						} if length.is_some() => {
							let length = length.unwrap();
							let maximum = if *inclusive { length - 1 } else { length };
							Some(if *min >= -length && *max <= maximum {
								RangeDecision::Safe
							} else if *max < -length || *min > maximum {
								RangeDecision::Invalid
							} else {
								RangeDecision::Unknown
							})
						}
						RangeEvidence::SymbolicSliceBound {
							min,
							max,
							inclusive: _,
							lower,
							upper,
						} => Some(if (*min >= 0 || *lower) && (*max < 0 || *upper) {
							RangeDecision::Safe
						} else {
							RangeDecision::Unknown
						}),
						_ => None,
					})
					.collect::<Vec<_>>();
				if bounds.is_empty() {
					RangeDecision::Unknown
				} else {
					bounds
						.into_iter()
						.fold(RangeDecision::Safe, |left, right| match (left, right) {
							(RangeDecision::Invalid, _) | (_, RangeDecision::Invalid) => RangeDecision::Invalid,
							(RangeDecision::Unknown, _) | (_, RangeDecision::Unknown) => RangeDecision::Unknown,
							_ => RangeDecision::Safe,
						})
				}
			}
			RangeOperation::HostIndex => match intervals.first() {
				Some((min, max)) if *min >= 0 && *max <= 9_007_199_254_740_991 => RangeDecision::Safe,
				Some((min, max)) if *max < 0 || *min > 9_007_199_254_740_991 => RangeDecision::Invalid,
				_ => RangeDecision::Unknown,
			},
			RangeOperation::Power => match intervals.get(1) {
				Some((_, max)) if *max < 0 => RangeDecision::Invalid,
				Some((min, _)) if *min >= 0 => classify_interval(intervals.last(), target),
				_ => RangeDecision::Unknown,
			},
		};
		replayed == self.decision
	}
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct StableBody {
	pub params: Arc<[StableParameter]>,
	pub root: StableExpr,
	pub is_async: bool,
}

#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct StableParameter {
	pub pattern: StablePattern,
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
	AsyncBlock(Box<StableExpr>),
	Await(Box<StableExpr>),
	PrefixOp {
		op: PrefixOperator,
		value: Box<StableExpr>,
	},
	PostfixOp {
		op: PostfixOperator,
		value: Box<StableExpr>,
		label: Option<EcoString>,
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
		replacements: Arc<[StableStateReplacement]>,
	},
	Echo {
		operand: Box<StableExpr>,
		keyword: Span,
	},
	For {
		variable: StablePattern,
		iterable: Box<StableExpr>,
		body: Box<StableExpr>,
		label: Option<EcoString>,
	},
	StateLoop {
		bindings: Arc<[StableStateBinding]>,
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
pub struct StableStateBinding {
	pub name: EcoString,
	pub managed: bool,
	pub value: StableExpr,
}
#[derive(Clone, Debug, PartialEq, salsa::SalsaValue)]
pub struct StableStateReplacement {
	pub name: EcoString,
	pub value: StableExpr,
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
		managed: bool,
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
	StructuralEquality,
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
		implementation_arguments: Arc<[RuntimeTypeArgument]>,
		method_arguments: Arc<[RuntimeTypeArgument]>,
	},
	InterfaceDefault {
		interface: DefinitionId,
		member: DefinitionId,
		implementation: DefinitionId,
		materialization: DispatchMaterialization,
		implementation_arguments: Arc<[RuntimeTypeArgument]>,
		method_arguments: Arc<[RuntimeTypeArgument]>,
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
pub enum StableStructConstructionMode {
	Fresh,
	CloneUpdate { source: BodyNodeId },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct StableStructConstructionPlan {
	pub definition: DefinitionId,
	pub mode: StableStructConstructionMode,
	pub explicit_fields: Arc<[(DefinitionId, BodyNodeId)]>,
	pub omitted_defaults: Arc<[DefinitionId]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
// This stable query value mirrors the runtime protocol directly; boxing its
// larger arm would leak allocation details into every consumer.
#[allow(clippy::large_enum_variant)]
pub enum RuntimeIteration {
	Direct {
		iterator_interface: DefinitionId,
		next: DefinitionId,
		next_dispatch: Option<StableDispatch>,
		iteration: crate::IterationRuntimeRole,
	},
	ViaIter {
		iterable_interface: DefinitionId,
		iter_interface_member: DefinitionId,
		iter: StableDispatch,
		iterator_interface: DefinitionId,
		next: DefinitionId,
		next_dispatch: Option<StableDispatch>,
		iteration: crate::IterationRuntimeRole,
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
	pub result: Option<crate::ResultRuntimeRole>,
	pub types: Arc<[(BodyNodeId, InterfaceType)]>,
	pub definition_targets: Arc<[(BodyNodeId, DefinitionId)]>,
	pub direct_namespace_members: Arc<[BodyNodeId]>,
	pub dispatches: Arc<[(BodyNodeId, StableDispatch)]>,
	/// Managed initializer → exact `Close.close` dispatch.
	pub managed_cleanups: Arc<[(BodyNodeId, StableDispatch)]>,
	pub variants: Arc<[(BodyNodeId, ExpressionVariant)]>,
	pub pattern_variants: Arc<[(PatternNodeId, PatternVariant)]>,
	pub positional_fields: Arc<[(PatternNodeId, StableVariantField)]>,
	pub struct_constructions: Arc<[(BodyNodeId, StableStructConstructionPlan)]>,
	pub iterations: Arc<[(BodyNodeId, RuntimeIteration)]>,
	pub anonymous_closures: Arc<[(BodyNodeId, u8)]>,
	pub generic_namespaced_calls: Arc<[(BodyNodeId, u32, DefinitionId, DefinitionId)]>,
	pub generic_call_arguments: Arc<[(BodyNodeId, Arc<[RuntimeTypeArgument]>)]>,
	/// Generic-call site → exact directly invoked definition.
	pub generic_call_targets: Arc<[(BodyNodeId, DefinitionId)]>,
	pub external_marshals: Arc<[(BodyNodeId, nymph_hir::hir::MarshalKind)]>,
	/// Resolved jump node → typed lexical target, projected to stable body ids.
	pub control_targets: Arc<[(BodyNodeId, RuntimeControlTarget)]>,
	pub propagations: Arc<[(BodyNodeId, RuntimePropagation)]>,
	pub range_proofs: Arc<[(BodyNodeId, RangeProof)]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, salsa::SalsaValue)]
pub enum RuntimePropagationKind {
	Option,
	Result,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct RuntimePropagation {
	pub kind: RuntimePropagationKind,
	pub conversion: Option<StableDispatch>,
}

impl RuntimeAnnotations {
	pub fn type_of(&self, node: BodyNodeId) -> Option<&InterfaceType> {
		self
			.types
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value))
	}

	pub fn definition_target(&self, node: BodyNodeId) -> Option<&DefinitionId> {
		self
			.definition_targets
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value))
	}

	pub fn is_direct_namespace_member(&self, node: BodyNodeId) -> bool {
		self.direct_namespace_members.contains(&node)
	}

	pub fn dispatch(&self, node: BodyNodeId) -> Option<&StableDispatch> {
		self
			.dispatches
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value))
	}

	pub fn managed_cleanup(&self, node: BodyNodeId) -> Option<&StableDispatch> {
		self
			.managed_cleanups
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value))
	}

	pub fn variant(&self, node: BodyNodeId) -> Option<&ExpressionVariant> {
		self
			.variants
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value))
	}

	pub fn pattern_variant(&self, node: PatternNodeId) -> Option<&PatternVariant> {
		self
			.pattern_variants
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value))
	}

	pub fn positional_field(&self, node: PatternNodeId) -> Option<&StableVariantField> {
		self
			.positional_fields
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value))
	}

	pub fn struct_construction(&self, node: BodyNodeId) -> Option<&StableStructConstructionPlan> {
		self
			.struct_constructions
			.iter()
			.find_map(|(id, plan)| (*id == node).then_some(plan))
	}

	pub fn iteration(&self, node: BodyNodeId) -> Option<&RuntimeIteration> {
		self
			.iterations
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value))
	}

	pub fn anonymous_closure_arity(&self, node: BodyNodeId) -> Option<u8> {
		self
			.anonymous_closures
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(*value))
	}

	pub fn generic_namespaced_call(
		&self,
		node: BodyNodeId,
	) -> Option<(u32, &DefinitionId, &DefinitionId)> {
		self
			.generic_namespaced_calls
			.iter()
			.find_map(|(id, parameter, interface, member)| {
				(*id == node).then_some((*parameter, interface, member))
			})
	}

	pub fn generic_call_arguments(&self, node: BodyNodeId) -> Option<&[RuntimeTypeArgument]> {
		self
			.generic_call_arguments
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value.as_ref()))
	}

	pub fn generic_call_target(&self, node: BodyNodeId) -> Option<&DefinitionId> {
		self
			.generic_call_targets
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(value))
	}

	pub fn external_marshal(&self, node: BodyNodeId) -> Option<nymph_hir::hir::MarshalKind> {
		self
			.external_marshals
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(*value))
	}

	pub fn control_target(&self, node: BodyNodeId) -> Option<RuntimeControlTarget> {
		self
			.control_targets
			.iter()
			.find_map(|(id, value)| (*id == node).then_some(*value))
	}

	pub fn range_proof(&self, node: BodyNodeId) -> Option<&RangeProof> {
		self
			.range_proofs
			.iter()
			.find_map(|(id, proof)| (*id == node).then_some(proof))
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum RuntimeTypeArgument {
	Canonical(InterfaceType),
	Erased,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, salsa::SalsaValue)]
pub enum RuntimeControlTarget {
	Loop(BodyNodeId),
	Block(BodyNodeId),
	Callable(BodyNodeId),
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
	/// Whether a value body is bound immutably and can therefore retain an exact
	/// callable identity for initializer dependency analysis.
	pub immutable: bool,
	/// Declared hidden type-object parameters, in canonical binder order.
	pub type_parameters: Arc<[crate::GenericParameterId]>,
	pub stable: StableBody,
	pub annotations: RuntimeAnnotations,
}

impl PartialEq for CheckedRuntimeBody {
	fn eq(&self, other: &Self) -> bool {
		self.kind == other.kind
			&& self.immutable == other.immutable
			&& self.type_parameters == other.type_parameters
			&& self.stable == other.stable
			&& self.annotations == other.annotations
	}
}
impl Eq for CheckedRuntimeBody {}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct StructShell {
	pub binders: Vec<crate::GenericParameter>,
	pub constraints: Vec<crate::GenericConstraint>,
	pub fields: Vec<crate::FieldShape<InterfaceType>>,
	pub defaults: Vec<StructFieldDefault>,
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct StructFieldDefault {
	pub field: DefinitionId,
	pub body: CheckedRuntimeBody,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct EnumShell {
	pub binders: Vec<crate::GenericParameter>,
	pub constraints: Vec<crate::GenericConstraint>,
	pub variants: Vec<crate::VariantShape<InterfaceType>>,
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
// Payloads are a public, stable semantic contract. Keep their direct ownership
// model rather than imposing boxes solely to equalize variant sizes.
#[allow(clippy::large_enum_variant)]
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
/// aggregate channels are represented by the schema and projected through
/// stable definition identity; no name- or span-based lookup is used.
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
					true,
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
				fields,
				members,
				impls,
				..
			} => {
				let item = shapes
					.iter()
					.copied()
					.find(|s| s.name == name.0 && s.kind == crate::DefinitionShapeKind::Struct)
					.ok_or_else(|| RuntimeExtractionError::MissingStableId(name.0.clone()))?;
				let defaults = fields
					.iter()
					.zip(&item.fields)
					.filter_map(|(field, shape)| {
						field.0.default.as_ref().map(|default| {
							checked_runtime_body(
								&shape.id,
								RuntimeBodyKind::Value,
								true,
								&[],
								default,
								false,
								checked,
							)
							.map(|body| StructFieldDefault {
								field: shape.id.clone(),
								body,
							})
						})
					})
					.collect::<Result<Vec<_>, _>>()?;
				result.push(RuntimeDefinition {
					definition: item.id.clone(),
					source_owner: item.id.module.clone(),
					placement: RuntimePlacement::TopLevel,
					payload: RuntimePayload::Struct(StructShell {
						binders: item.binders.clone(),
						constraints: item.constraints.clone(),
						fields: item.fields.clone(),
						defaults,
					}),
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
			nymph_ast::decl::Declaration::Enum {
				name,
				members,
				impls,
				..
			} => {
				let item = shapes
					.iter()
					.copied()
					.find(|s| s.name == name.0 && s.kind == crate::DefinitionShapeKind::Enum)
					.ok_or_else(|| RuntimeExtractionError::MissingStableId(name.0.clone()))?;
				result.push(RuntimeDefinition {
					definition: item.id.clone(),
					source_owner: item.id.module.clone(),
					placement: RuntimePlacement::TopLevel,
					payload: RuntimePayload::Enum(EnumShell {
						binders: item.binders.clone(),
						constraints: item.constraints.clone(),
						variants: item.variants.clone(),
					}),
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
									true,
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
				push_value(result, shape.id.clone(), placement(), value, true, checked)?
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
			ImplMember::Let { value, .. } => {
				push_value(result, definition, placement, value, true, checked)?
			}
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
	immutable: bool,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	push_canonical_body(
		result,
		definition,
		placement,
		RuntimeBodyKind::Value,
		immutable,
		&[],
		value,
		false,
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
		true,
		&meta.params,
		body,
		meta.is_async,
		checked,
	)
}

#[allow(clippy::too_many_arguments)]
fn push_canonical_body(
	result: &mut Vec<RuntimeDefinition>,
	definition: DefinitionId,
	placement: RuntimePlacement,
	kind: RuntimeBodyKind,
	immutable: bool,
	params: &[nymph_ast::Spanned<FuncParam>],
	body: &Expr,
	is_async: bool,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	let body = checked_runtime_body(
		&definition,
		kind,
		immutable,
		params,
		body,
		is_async,
		checked,
	)?;
	result.push(RuntimeDefinition {
		source_owner: definition.module.clone(),
		definition,
		placement,
		payload: RuntimePayload::NymphBody(body),
	});
	Ok(())
}

fn checked_runtime_body(
	definition: &DefinitionId,
	kind: RuntimeBodyKind,
	immutable: bool,
	params: &[nymph_ast::Spanned<FuncParam>],
	body: &Expr,
	is_async: bool,
	checked: &crate::CheckedFacts,
) -> Result<CheckedRuntimeBody, RuntimeExtractionError> {
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
	let stable = builder.body(params, body, is_async)?;
	let pattern_sites = builder.pattern_sites.into_inner();
	let positional_sites = builder.positional_sites.into_inner();
	let annotations = runtime_annotations(
		definition,
		&nodes,
		&local,
		&pattern_sites,
		&positional_sites,
		&native_range_nodes,
		&required_type_nodes,
		checked,
	)?;
	let mut type_parameters = body_parameters(definition, checked)
		.into_iter()
		.collect::<Vec<_>>();
	type_parameters.sort_by_key(|(index, _)| index.0);
	Ok(CheckedRuntimeBody {
		kind,
		immutable,
		type_parameters: type_parameters
			.into_iter()
			.map(|(_, parameter)| parameter)
			.collect(),
		stable,
		annotations,
	})
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
		is_async: bool,
	) -> Result<StableBody, RuntimeExtractionError> {
		Ok(StableBody {
			params: params
				.iter()
				.map(|p| self.func_param(&p.0))
				.collect::<Result<_, _>>()?,
			root: self.expr(root)?,
			is_async,
		})
	}
	fn func_param(&self, param: &FuncParam) -> Result<StableParameter, RuntimeExtractionError> {
		Ok(StableParameter {
			pattern: self.pattern(&param.name)?,
			spread: param.spread,
		})
	}
	fn closure_param(&self, param: &ClosureParam) -> Result<StableParameter, RuntimeExtractionError> {
		Ok(StableParameter {
			pattern: self.pattern(&param.name)?,
			spread: param.spread,
		})
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
			ExprKind::AsyncBlock { body, .. } => StableExprKind::AsyncBlock(boxed(body)?),
			ExprKind::Await { value, .. } => StableExprKind::Await(boxed(value)?),
			ExprKind::PrefixOp { op, value } => StableExprKind::PrefixOp {
				op: *op,
				value: boxed(value)?,
			},
			ExprKind::PostfixOp {
				op,
				value,
				label: l,
			} => StableExprKind::PostfixOp {
				op: *op,
				value: boxed(value)?,
				label: label(l),
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
			ExprKind::Return { value, label: l } => StableExprKind::Return {
				value: value.as_deref().map(boxed).transpose()?,
				label: label(l),
			},
			ExprKind::Break { value, label: l } => StableExprKind::Break {
				value: value.as_deref().map(boxed).transpose()?,
				label: label(l),
			},
			ExprKind::Continue {
				label: l,
				replacements,
			} => StableExprKind::Continue {
				label: label(l),
				replacements: replacements
					.iter()
					.map(|replacement| {
						Ok(StableStateReplacement {
							name: replacement.name.0.clone(),
							value: self.expr(&replacement.value)?,
						})
					})
					.collect::<Result<_, _>>()?,
			},
			ExprKind::Echo { operand, keyword } => StableExprKind::Echo {
				operand: boxed(operand)?,
				keyword: *keyword,
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
			ExprKind::StateLoop {
				bindings,
				body,
				label: l,
			} => StableExprKind::StateLoop {
				bindings: bindings
					.iter()
					.map(|binding| {
						let name = binding
							.meta
							.name
							.0
							.as_binding()
							.map(|name| name.0.clone())
							.unwrap_or_default();
						Ok(StableStateBinding {
							name,
							managed: binding.meta.is_managed(),
							value: self.expr(&binding.value)?,
						})
					})
					.collect::<Result<_, _>>()?,
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
								managed: meta.is_managed(),
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
			value: self.expr(arg.value())?,
			name: arg.name().map(|n| n.0.clone()),
			spread: arg.is_spread(),
		})
	}
}

#[allow(clippy::too_many_arguments)]
fn runtime_annotations(
	definition: &DefinitionId,
	nodes: &[&Expr],
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
	let self_parameter = self_parameter.then_some(crate::ParamIdx(parameters.len() as u32));
	let context = CanonicalizationContext::new(definitions, parameters);
	let context = if let Some(parameter) = self_parameter {
		context.with_self_parameter(parameter)
	} else {
		context
	};
	let mut types = Vec::new();
	let mut dispatches = Vec::new();
	let construction_type_nodes = nodes
		.iter()
		.filter_map(|expression| {
			let required = match &expression.kind {
				ExprKind::List(_) | ExprKind::Tuple(_) | ExprKind::Map(_) | ExprKind::Range(_) => true,
				ExprKind::Call { func, .. } => {
					checked
						.annotations
						.definition_target_of(func.id)
						.is_some_and(|target| {
							matches!(
								target.key,
								crate::DeclarationKey::TopLevel {
									category: crate::DeclarationCategory::Struct,
									..
								}
							)
						}) || checked.annotations.variant_of(expression.id).is_some()
				}
				_ => checked.annotations.variant_of(expression.id).is_some(),
			};
			required.then_some(expression.id)
		})
		.collect::<std::collections::HashSet<_>>();
	let async_type_nodes = nodes
		.iter()
		.flat_map(|expression| match &expression.kind {
			ExprKind::Await { value, .. } => vec![value.id],
			ExprKind::Call { func, .. }
				if matches!(&func.kind, ExprKind::MemberAccess { member, .. } if member.0 == "spawn") =>
			{
				vec![expression.id]
			}
			_ => Vec::new(),
		})
		.collect::<std::collections::HashSet<_>>();
	for (node, info) in checked.annotations.infos() {
		let Some(&id) = local.get(&node) else {
			continue;
		};
		if required_type_nodes.contains(&node) || async_type_nodes.contains(&node) {
			types.push((
				id,
				required_canonical_type(&checked.interner, info.ty, &context)?,
			));
		} else if construction_type_nodes.contains(&node)
			&& let Ok(type_) = required_canonical_type(&checked.interner, info.ty, &context)
			&& runtime_type_object_supported(&type_)
		{
			types.push((id, type_));
		}
		if let Some(resolution) = &info.resolution {
			dispatches.push((id, stable_dispatch(checked, resolution, &context)?));
		}
	}
	let mut definition_targets = checked
		.annotations
		.definition_targets()
		.filter_map(|(id, target)| local.get(&id).map(|id| (*id, target.clone())))
		.collect::<Vec<_>>();
	let mut managed_cleanups = checked
		.annotations
		.managed_cleanups()
		.filter_map(|(node, resolution)| local.get(&node).map(|id| (*id, resolution)))
		.map(|(id, resolution)| Ok((id, stable_dispatch(checked, resolution, &context)?)))
		.collect::<Result<Vec<_>, RuntimeExtractionError>>()?;
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
	let mut struct_constructions = checked
		.annotations
		.struct_constructions()
		.filter_map(|(node, plan)| {
			let node = *local.get(&node)?;
			let mode = match plan.mode {
				crate::annotate::StructConstructionMode::Fresh => StableStructConstructionMode::Fresh,
				crate::annotate::StructConstructionMode::CloneUpdate { source } => {
					StableStructConstructionMode::CloneUpdate {
						source: *local.get(&source)?,
					}
				}
			};
			let explicit_fields = plan
				.explicit_fields
				.iter()
				.filter_map(|(field, value)| local.get(value).map(|value| (field.clone(), *value)))
				.collect::<Vec<_>>();
			Some((
				node,
				StableStructConstructionPlan {
					definition: plan.definition.clone(),
					mode,
					explicit_fields: explicit_fields.into(),
					omitted_defaults: plan.omitted_defaults.clone().into(),
				},
			))
		})
		.collect::<Vec<_>>();
	let mut iterations = Vec::new();
	let mut anonymous_closures = Vec::new();
	let mut generic_namespaced_calls = Vec::new();
	let mut control_targets = checked
		.annotations
		.control_targets()
		.filter_map(|(jump, target)| {
			let jump = *local.get(&jump)?;
			let target_id = *local.get(&target.source)?;
			let target = match target.kind {
				crate::annotate::ResolvedControlTargetKind::Loop => RuntimeControlTarget::Loop(target_id),
				crate::annotate::ResolvedControlTargetKind::Block => RuntimeControlTarget::Block(target_id),
				crate::annotate::ResolvedControlTargetKind::Callable => {
					RuntimeControlTarget::Callable(target_id)
				}
			};
			Some((jump, target))
		})
		.collect::<Vec<_>>();
	let mut propagations = checked
		.annotations
		.propagations()
		.filter_map(|(node, propagation)| {
			let node = *local.get(&node)?;
			let kind = match propagation.kind {
				crate::annotate::PropagationKind::Option => RuntimePropagationKind::Option,
				crate::annotate::PropagationKind::Result => RuntimePropagationKind::Result,
			};
			Some(
				propagation
					.conversion
					.as_ref()
					.map(|resolution| stable_dispatch(checked, resolution, &context))
					.transpose()
					.map(|conversion| (node, RuntimePropagation { kind, conversion })),
			)
		})
		.collect::<Result<Vec<_>, _>>()?;
	let mut range_proofs = checked
		.annotations
		.range_proofs()
		.filter_map(|(node, proof)| local.get(&node).map(|id| (*id, proof.clone())))
		.collect::<Vec<_>>();
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
					next_dispatch: stable_iteration_next_dispatch(checked, source, &context)?,
					iteration: protocols.2.clone(),
				},
				crate::IterMode::ViaIter => {
					let (iterable_interface, iter_interface_member, iter) =
						if let Some(resolution) = checked.annotations.iter_resolution_of(source) {
							let (interface, member) = match resolution.resolved_target.as_ref() {
								Some(crate::annotate::ResolvedMethodTarget::InterfaceImplementation {
									interface,
									slot,
									..
								}) => (interface.clone(), slot.interface_member_id.clone()),
								Some(crate::annotate::ResolvedMethodTarget::GenericBound {
									interface,
									interface_member,
								}) => (interface.clone(), interface_member.clone()),
								_ => return Err(RuntimeExtractionError::MissingIterationProtocol),
							};
							(
								interface,
								member,
								stable_dispatch(checked, resolution, &context)?,
							)
						} else {
							let source_type = checked
								.annotations
								.get(source)
								.ok_or(RuntimeExtractionError::MissingIterationProtocol)?
								.ty;
							if !matches!(
								required_canonical_type(&checked.interner, source_type, &context)?,
								InterfaceType::List(_)
							) {
								return Err(RuntimeExtractionError::MissingIterationProtocol);
							}
							let iterable = checked
								.semantic
								.compiler_runtime_roles
								.iterable
								.as_ref()
								.ok_or(RuntimeExtractionError::MissingIterationProtocol)?;
							(
								iterable.interface.clone(),
								iterable.member.clone(),
								StableDispatch::Builtin {
									method: "iter".into(),
									category: BuiltinDispatch::Eager,
								},
							)
						};
					RuntimeIteration::ViaIter {
						iterable_interface,
						iter_interface_member,
						iter,
						iterator_interface: protocols.0.clone(),
						next: protocols.1.clone(),
						next_dispatch: stable_iteration_next_dispatch(checked, source, &context)?,
						iteration: protocols.2.clone(),
					}
				}
			};
			iterations.push((id, iteration));
		}
		if let Some(arity) = checked.annotations.anon_boundary_arity(source) {
			anonymous_closures.push((id, arity));
		}
		if let Some(call) = checked.annotations.generic_namespaced_call(source) {
			generic_namespaced_calls.push((
				id,
				call.parameter.0,
				call.interface.clone(),
				call.member.clone(),
			));
		}
	}
	let mut generic_call_arguments = checked
		.annotations
		.generic_call_arguments()
		.filter_map(|(source, arguments)| local.get(&source).map(|id| (*id, arguments)))
		.map(|(id, arguments)| {
			let arguments = arguments
				.iter()
				.map(
					|ty| match required_canonical_type(&checked.interner, *ty, &context) {
						Ok(type_) if runtime_type_object_supported(&type_) => {
							Ok(RuntimeTypeArgument::Canonical(type_))
						}
						Ok(_) => Ok(RuntimeTypeArgument::Erased),
						// Preserve the declared slot so later concrete arguments do not
						// shift when an unrelated inferred generic remains erased.
						Err(RuntimeExtractionError::IncompleteCanonicalType) => Ok(RuntimeTypeArgument::Erased),
						Err(error) => Err(error),
					},
				)
				.collect::<Result<Vec<_>, _>>()?;
			Ok((id, arguments.into()))
		})
		.collect::<Result<Vec<_>, RuntimeExtractionError>>()?;
	let generic_argument_sites = generic_call_arguments
		.iter()
		.map(|(id, _)| *id)
		.collect::<std::collections::HashSet<_>>();
	let mut generic_call_targets = nodes
		.iter()
		.filter_map(|expr| {
			let id = *local.get(&expr.id)?;
			if !generic_argument_sites.contains(&id) {
				return None;
			}
			let mut source = *expr;
			while let ExprKind::Grouped(inner) = &source.kind {
				source = inner;
			}
			let source_id = *local.get(&source.id)?;
			let target_node = match &source.kind {
				ExprKind::Call { func, .. } => func.id,
				_ => source.id,
			};
			let direct = checked
				.annotations
				.definition_targets()
				.find_map(|(candidate, target)| (candidate == target_node).then(|| target.clone()))
				.or_else(|| {
					let resolution = checked.annotations.resolution_of(source.id)?;
					match resolution.resolved_target.as_ref()? {
						crate::annotate::ResolvedMethodTarget::Inherent { member, .. } => Some(member.clone()),
						crate::annotate::ResolvedMethodTarget::InterfaceImplementation { slot, .. } => {
							Some(slot.member_id.clone())
						}
						crate::annotate::ResolvedMethodTarget::GenericBound { .. } => None,
					}
				})
				.or_else(|| {
					let (_, dispatch) = dispatches
						.iter()
						.find(|(candidate, _)| *candidate == source_id)?;
					match dispatch {
						StableDispatch::Direct { member, .. }
						| StableDispatch::SelectedImplementation { member, .. }
						| StableDispatch::InterfaceDefault { member, .. }
						| StableDispatch::GenericBound { member, .. } => Some(member.clone()),
						StableDispatch::Builtin { .. } | StableDispatch::External { .. } => None,
					}
				});
			direct.map(|target| (id, target))
		})
		.collect::<Vec<_>>();
	types.sort_by_key(|item| item.0);
	definition_targets.sort_by_key(|item| item.0);
	dispatches.sort_by_key(|item| item.0);
	managed_cleanups.sort_by_key(|item| item.0);
	variants.sort_by_key(|item| item.0);
	pattern_variants.sort_by_key(|item| item.0);
	positional_fields.sort_by_key(|item| item.0);
	struct_constructions.sort_by_key(|item| item.0);
	iterations.sort_by_key(|item| item.0);
	anonymous_closures.sort_by_key(|item| item.0);
	generic_namespaced_calls.sort_unstable();
	generic_call_arguments.sort_by_key(|item| item.0);
	generic_call_targets.sort_by_key(|item| item.0);
	control_targets.sort_unstable();
	propagations.sort_unstable_by_key(|(node, _)| *node);
	range_proofs.sort_by_key(|item| item.0);
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
		result: checked.runtime_roles.result.clone(),
		types: types.into(),
		definition_targets: definition_targets.into(),
		direct_namespace_members: direct_namespace_members.into(),
		dispatches: dispatches.into(),
		managed_cleanups: managed_cleanups.into(),
		variants: variants.into(),
		pattern_variants: pattern_variants.into(),
		positional_fields: positional_fields.into(),
		struct_constructions: struct_constructions.into(),
		iterations: iterations.into(),
		anonymous_closures: anonymous_closures.into(),
		generic_namespaced_calls: generic_namespaced_calls.into(),
		generic_call_arguments: generic_call_arguments.into(),
		generic_call_targets: generic_call_targets.into(),
		external_marshals: external_marshals.into(),
		control_targets: control_targets.into(),
		propagations: propagations.into(),
		range_proofs: range_proofs.into(),
	})
}

fn runtime_type_object_supported(ty: &InterfaceType) -> bool {
	match ty {
		InterfaceType::Int
		| InterfaceType::UInt
		| InterfaceType::Float
		| InterfaceType::Char
		| InterfaceType::String
		| InterfaceType::Boolean
		| InterfaceType::SelfType
		| InterfaceType::Generic(_) => true,
		InterfaceType::List(argument) => runtime_type_object_supported(argument),
		InterfaceType::Tuple(arguments) => arguments.iter().all(runtime_type_object_supported),
		InterfaceType::Map(key, value) => {
			runtime_type_object_supported(key) && runtime_type_object_supported(value)
		}
		InterfaceType::Named {
			positional, named, ..
		} => {
			positional.iter().all(runtime_type_object_supported)
				&& named
					.iter()
					.all(|(_, argument)| runtime_type_object_supported(argument))
		}
		_ => false,
	}
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
		// Lowering must erase strict shells whose checked operand cannot complete.
		// Those shells intentionally have no dispatch annotation, so retain the
		// semantic `never` fact even when this expression shape otherwise needs no
		// runtime type metadata. Do not retain `Error`: recovered programs must not
		// silently masquerade as control transfer.
		if checked
			.annotations
			.get(expression.id)
			.is_some_and(|info| matches!(checked.interner.kind(info.ty), crate::TyKind::Never))
		{
			required.insert(expression.id);
		}
		if checked
			.annotations
			.get(expression.id)
			.and_then(|info| info.resolution)
			.is_some_and(|resolution| {
				matches!(
					resolution.resolved_target,
					Some(crate::annotate::ResolvedMethodTarget::GenericBound { .. })
				)
			}) {
			match &expression.kind {
				ExprKind::BinaryOp { lhs, rhs, .. } => {
					required.insert(lhs.id);
					required.insert(rhs.id);
				}
				ExprKind::Call { func, args, .. } if !args.is_empty() => {
					if let ExprKind::MemberAccess { parent, .. } = &func.kind {
						required.insert(parent.id);
					}
					required.extend(args.iter().map(|argument| argument.value().value().id));
				}
				_ => {}
			}
		}
		match &expression.kind {
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
			ExprKind::MemberAccess {
				parent,
				optional: true,
				..
			} => {
				required.insert(parent.id);
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
			ExprKind::Call { func, .. }
				if matches!(func.kind, ExprKind::MemberAccess { optional: true, .. }) =>
			{
				if let ExprKind::MemberAccess { parent, .. } = &func.kind {
					required.insert(parent.id);
				}
				required.insert(expression.id);
			}
			ExprKind::IndexAccess { parent, .. } => {
				required.insert(parent.id);
				if matches!(
					expression.kind,
					ExprKind::IndexAccess { optional: true, .. }
				) {
					required.insert(expression.id);
				}
			}
			ExprKind::BinaryOp { lhs, op, .. } => {
				if matches!(op, BinaryOperator::Equals | BinaryOperator::NotEquals)
					&& checked
						.annotations
						.get(lhs.id)
						.is_some_and(|info| !matches!(checked.interner.kind(info.ty), crate::TyKind::Error))
				{
					required.insert(lhs.id);
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

fn stable_iteration_next_dispatch(
	checked: &crate::CheckedFacts,
	source: nymph_ast::NodeId,
	context: &CanonicalizationContext,
) -> Result<Option<StableDispatch>, RuntimeExtractionError> {
	let dispatch = checked
		.annotations
		.iteration_next_resolution_of(source)
		.map(|resolution| stable_dispatch(checked, resolution, context))
		.transpose()?;
	Ok(dispatch.filter(|dispatch| !matches!(dispatch, StableDispatch::GenericBound { .. })))
}

fn stable_dispatch(
	checked: &crate::CheckedFacts,
	resolution: &crate::annotate::Resolution,
	context: &CanonicalizationContext,
) -> Result<StableDispatch, RuntimeExtractionError> {
	let category = match resolution.dispatch {
		DispatchKind::BuiltinEager => BuiltinDispatch::Eager,
		DispatchKind::BuiltinShortCircuit => BuiltinDispatch::ShortCircuit,
		DispatchKind::BuiltinStructuralEquality => BuiltinDispatch::StructuralEquality,
		DispatchKind::UserImpl | DispatchKind::UserImplDefaultMethod => {
			return exact_method_dispatch(checked, resolution, context);
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
	context: &CanonicalizationContext,
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
					marshal: abi.marshal.result,
				})
			} else {
				Ok(StableDispatch::Direct {
					member: member.clone(),
					implementation: implementation.clone(),
					materialization: DispatchMaterialization::Attached,
				})
			}
		}
		crate::annotate::ResolvedMethodTarget::InterfaceImplementation {
			interface,
			slot,
			implementation_arguments,
			method_arguments,
		} => {
			let canonical_arguments = |arguments: &[crate::Ty]| {
				arguments
					.iter()
					.map(
						|argument| match required_canonical_type(&checked.interner, *argument, context) {
							Ok(type_) if runtime_type_object_supported(&type_) => {
								Ok(RuntimeTypeArgument::Canonical(type_))
							}
							Ok(_) | Err(RuntimeExtractionError::IncompleteCanonicalType) => {
								Ok(RuntimeTypeArgument::Erased)
							}
							Err(error) => Err(error),
						},
					)
					.collect::<Result<Arc<[_]>, _>>()
			};
			let implementation_arguments = canonical_arguments(implementation_arguments)?;
			let method_arguments = canonical_arguments(method_arguments)?;
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
					marshal: abi.marshal.result,
				});
			}
			Ok(match slot.source {
				crate::ImplementationMemberSource::Override => StableDispatch::SelectedImplementation {
					interface: interface.clone(),
					member: slot.member_id.clone(),
					implementation: slot.implementation_id.clone(),
					materialization: DispatchMaterialization::Attached,
					implementation_arguments,
					method_arguments,
				},
				crate::ImplementationMemberSource::InheritedDefault => StableDispatch::InterfaceDefault {
					interface: interface.clone(),
					member: slot.member_id.clone(),
					implementation: slot.implementation_id.clone(),
					materialization: DispatchMaterialization::CanonicalBody,
					implementation_arguments,
					method_arguments,
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
	let function_signature = checked
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
		});
	let member_count = function_signature
		.map(|signature| signature.generics.len())
		.or_else(|| {
			checked
				.semantic
				.inherent
				.iter()
				.flat_map(|implementation| implementation.methods.values())
				.find(|method| method.definition.as_ref() == Some(definition))
				.map(|method| method.generic_count)
		})
		.or_else(|| {
			checked
				.semantic
				.interfaces
				.values()
				.flat_map(|interface| interface.methods.values())
				.find(|method| method.definition.as_ref() == Some(definition))
				.map(|method| method.generics.len())
		})
		.unwrap_or(0);
	let member_parameters = function_signature
		.map(|signature| {
			signature
				.generic_kinds
				.iter()
				.enumerate()
				.filter_map(|(index, kind)| (*kind == crate::GenericParameterKind::Type).then_some(index))
				.collect::<Vec<_>>()
		})
		.unwrap_or_else(|| (0..member_count).collect());
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
		.chain(member_parameters.into_iter().map(|index| {
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
) -> Result<(DefinitionId, DefinitionId, crate::IterationRuntimeRole), RuntimeExtractionError> {
	let iterator = checked
		.semantic
		.compiler_runtime_roles
		.iterator
		.as_ref()
		.ok_or(RuntimeExtractionError::MissingIterationProtocol)?;
	let iteration = checked
		.semantic
		.compiler_runtime_roles
		.iteration
		.as_ref()
		.ok_or(RuntimeExtractionError::MissingIterationProtocol)?;
	Ok((
		iterator.interface.clone(),
		iterator.member.clone(),
		iteration.clone(),
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
			.and_then(|abi| abi.marshal.result)
	}
}

fn walk_expr<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
	out.push(expr);
	expr.for_each_child(|child| walk_expr(child, out));
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
