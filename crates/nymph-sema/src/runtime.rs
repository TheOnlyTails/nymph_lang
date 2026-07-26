//! Per-definition checked runtime artifacts.
//!
//! These values are deliberately upstream of HIR. Each artifact owns one source
//! definition and the exact stable checker decisions required to lower that
//! definition; it never retains a module or dependency AST.

use std::sync::Arc;

use ecow::EcoString;
use nymph_ast::{
	decl::{FuncDeclaration, ImplMember, InterfaceElement, InterfaceMember, LetDeclaration},
	expr::{
		Expr, ExprKind, ListItem, ListPatternEntry, MapEntry, MapPatternEntry, Pattern, RangeKind,
		RangePatternKind, Statement, StringPart, StructPatternField,
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
	},
	ViaIter {
		iterable_interface: DefinitionId,
		iter: StableDispatch,
		iterator_interface: DefinitionId,
		next: DefinitionId,
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
	pub types: Arc<[(BodyNodeId, InterfaceType)]>,
	pub definition_targets: Arc<[(BodyNodeId, DefinitionId)]>,
	pub dispatches: Arc<[(BodyNodeId, StableDispatch)]>,
	pub variants: Arc<[(BodyNodeId, ExpressionVariant)]>,
	pub pattern_variants: Arc<[(PatternNodeId, PatternVariant)]>,
	pub positional_fields: Arc<[(PatternNodeId, StableVariantField)]>,
	pub iterations: Arc<[(BodyNodeId, RuntimeIteration)]>,
	pub anonymous_closures: Arc<[(BodyNodeId, u8)]>,
	pub external_marshals: Arc<[(BodyNodeId, nymph_hir::hir::MarshalKind)]>,
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct CheckedRuntimeBody {
	/// Canonical, declaration-local source forms. Neither contains parser identity
	/// or absolute locations.
	pub signature: Arc<str>,
	pub expression: Arc<str>,
	pub annotations: RuntimeAnnotations,
}

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
	source: &str,
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
	let mut used_implementations = std::collections::BTreeSet::new();
	for declaration in &module.members {
		match declaration {
			nymph_ast::decl::Declaration::Func { meta, body, .. } => {
				let definition = required_top_level(checked, &meta.name.0)?;
				push_body(
					&mut result,
					definition,
					RuntimePlacement::TopLevel,
					meta,
					body,
					source,
					checked,
				)?;
			}
			nymph_ast::decl::Declaration::Let { meta, value, .. } => {
				let name = binding_name(meta)?;
				push_value(
					&mut result,
					required_top_level(checked, name)?,
					RuntimePlacement::TopLevel,
					meta,
					value,
					source,
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
				extract_members(&mut result, members, &item.members, source, checked)?;
				for nested in impls {
					let implementation = implementation_for_members(
						interface,
						&nested.0.members,
						Some(&item.id),
						&mut used_implementations,
					)?;
					extract_implementation_members(
						&mut result,
						&nested.0.members,
						implementation,
						source,
						checked,
					)?;
				}
			}
			nymph_ast::decl::Declaration::Namespace { name, members, .. } => {
				let item = shape(crate::DeclarationCategory::Namespace, &name.0)
					.ok_or_else(|| RuntimeExtractionError::MissingStableId(name.0.clone()))?;
				extract_members(&mut result, members, &item.members, source, checked)?;
			}
			nymph_ast::decl::Declaration::Impl { members, .. }
			| nymph_ast::decl::Declaration::ImplFor { members, .. } => {
				let implementation =
					implementation_for_members(interface, members, None, &mut used_implementations)?;
				extract_implementation_members(&mut result, members, implementation, source, checked)?;
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
									source,
									checked,
								)?,
								InterfaceElement::Let {
									meta,
									value: Some(value),
								} => push_value(
									&mut result,
									member.id.clone(),
									attached(member),
									meta,
									value,
									source,
									checked,
								)?,
								_ => {}
							}
						}
						InterfaceMember::Impl { members, .. } => {
							let implementation = implementation_for_members(
								interface,
								members,
								Some(&item.id),
								&mut used_implementations,
							)?;
							extract_implementation_members(
								&mut result,
								members,
								implementation,
								source,
								checked,
							)?;
						}
					}
				}
			}
			_ => {}
		}
	}
	Ok(result)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum RuntimeExtractionError {
	InvalidSourceProjection,
	IncompleteCanonicalType,
	MissingStableId(EcoString),
	MissingImplementation,
	MissingExternalAbi,
	IncompleteDispatchTarget(EcoString),
	IncompleteVariantTarget(EcoString),
	MissingIterationProtocol,
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
	source: &str,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	if syntax.len() != shapes.len() {
		return Err(RuntimeExtractionError::MissingImplementation);
	}
	for (syntax, shape) in syntax.iter().zip(shapes) {
		match &syntax.0 {
			ImplMember::Func { meta, body, .. } => push_body(
				result,
				shape.id.clone(),
				attached(shape),
				meta,
				body,
				source,
				checked,
			)?,
			ImplMember::Let { meta, value, .. } => push_value(
				result,
				shape.id.clone(),
				attached(shape),
				meta,
				value,
				source,
				checked,
			)?,
			ImplMember::ExternalFunc(..) | ImplMember::ExternalLet(..) => push_external(
				result,
				shape.id.clone(),
				attached(shape),
				shape.external.clone(),
			)?,
		}
	}
	Ok(())
}

fn implementation_for_members<'a>(
	interface: &'a crate::ModuleInterface,
	syntax: &[nymph_ast::Spanned<ImplMember>],
	runtime_owner: Option<&DefinitionId>,
	used: &mut std::collections::BTreeSet<DefinitionId>,
) -> Result<&'a crate::ExportedImpl, RuntimeExtractionError> {
	let names = syntax
		.iter()
		.filter_map(|member| match &member.0 {
			ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => {
				Some(meta.name.0.as_str())
			}
			ImplMember::Let { .. } | ImplMember::ExternalLet(..) => None,
		})
		.collect::<Vec<_>>();
	let matches = interface
		.implementations
		.iter()
		.filter(|implementation| {
			!used.contains(&implementation.id)
				&& runtime_owner.is_none_or(|owner| {
					implementation.runtime_owner.as_ref() == Some(owner)
						|| matches!(&implementation.self_type, InterfaceType::Named { definition, .. } if definition == owner)
				})
				&& implementation.members.len() == names.len()
				&& implementation
					.members
					.iter()
					.map(|member| member.name.as_str())
					.eq(names.iter().copied())
		})
		.collect::<Vec<_>>();
	let implementation = matches
		.first()
		.copied()
		.ok_or(RuntimeExtractionError::MissingImplementation)?;
	used.insert(implementation.id.clone());
	Ok(implementation)
}

fn extract_implementation_members(
	result: &mut Vec<RuntimeDefinition>,
	syntax: &[nymph_ast::Spanned<ImplMember>],
	implementation: &crate::ExportedImpl,
	source: &str,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	for syntax in syntax {
		let name = match &syntax.0 {
			ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => meta.name.0.as_str(),
			ImplMember::Let { meta, .. } | ImplMember::ExternalLet(_, _, meta) => binding_name(meta)?,
		};
		let existing = implementation
			.members
			.iter()
			.find(|member| member.name == name);
		let definition = existing.map_or_else(
			|| {
				DefinitionId::new(
					implementation.id.module.clone(),
					crate::DeclarationKey::member(
						implementation.id.clone(),
						crate::DeclarationCategory::Method,
						name,
					),
				)
			},
			|member| member.id.clone(),
		);
		let placement = RuntimePlacement::Attached {
			owner: implementation.id.clone(),
			name: name.into(),
		};
		match &syntax.0 {
			ImplMember::Func { meta, body, .. } => {
				push_body(result, definition, placement, meta, body, source, checked)?
			}
			ImplMember::Let { meta, value, .. } => {
				push_value(result, definition, placement, meta, value, source, checked)?
			}
			ImplMember::ExternalFunc(_, marker, _) => push_external(
				result,
				definition,
				placement,
				Some(crate::interface_extract::external_function_abi(marker)),
			)?,
			ImplMember::ExternalLet(_, marker, meta) => push_external(
				result,
				definition,
				placement,
				Some(crate::interface_extract::external_value_abi(
					marker,
					checked.external_value_marshals.get(&meta.name.1).copied(),
				)),
			)?,
		}
	}
	Ok(())
}
fn push_value(
	result: &mut Vec<RuntimeDefinition>,
	definition: DefinitionId,
	placement: RuntimePlacement,
	meta: &LetDeclaration,
	value: &Expr,
	source: &str,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	let name = binding_name(meta)?;
	push_canonical_body(result, definition, placement, name, value, source, checked)
}

fn push_body(
	result: &mut Vec<RuntimeDefinition>,
	definition: DefinitionId,
	placement: RuntimePlacement,
	meta: &FuncDeclaration,
	body: &Expr,
	source: &str,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	let signature_source = source
		.get(meta.name.1.start as usize..body.span.start as usize)
		.ok_or(RuntimeExtractionError::InvalidSourceProjection)?
		.trim_end_matches(|character: char| character == '=' || character.is_whitespace());
	push_canonical_body(
		result,
		definition,
		placement,
		signature_source,
		body,
		source,
		checked,
	)
}

fn push_canonical_body(
	result: &mut Vec<RuntimeDefinition>,
	definition: DefinitionId,
	placement: RuntimePlacement,
	signature_source: &str,
	body: &Expr,
	source: &str,
	checked: &crate::CheckedFacts,
) -> Result<(), RuntimeExtractionError> {
	let body_source = source
		.get(body.span.start as usize..body.span.end as usize)
		.ok_or(RuntimeExtractionError::InvalidSourceProjection)?
		.trim();
	let mut nodes = Vec::new();
	walk_expr(body, &mut nodes);
	let local = nodes
		.iter()
		.enumerate()
		.map(|(index, expr)| (expr.id, BodyNodeId(index as u32)))
		.collect::<std::collections::HashMap<_, _>>();
	let mut patterns = Vec::new();
	walk_body_patterns(body, &mut patterns);
	let patterns = patterns
		.into_iter()
		.enumerate()
		.map(|(index, pattern)| (pattern.1, PatternNodeId(index as u32)))
		.collect::<std::collections::HashMap<_, _>>();
	let annotations = runtime_annotations(&definition, &local, &patterns, checked)?;
	result.push(RuntimeDefinition {
		source_owner: definition.module.clone(),
		definition,
		placement,
		payload: RuntimePayload::NymphBody(CheckedRuntimeBody {
			signature: Arc::from(signature_source),
			expression: Arc::from(body_source),
			annotations,
		}),
	});
	Ok(())
}

fn runtime_annotations(
	definition: &DefinitionId,
	local: &std::collections::HashMap<nymph_ast::NodeId, BodyNodeId>,
	patterns: &std::collections::HashMap<nymph_ast::Span, PatternNodeId>,
	checked: &crate::CheckedFacts,
) -> Result<RuntimeAnnotations, RuntimeExtractionError> {
	let definitions = checked
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
	let context = CanonicalizationContext::new(definitions, parameters);
	let mut types = Vec::new();
	let mut dispatches = Vec::new();
	for (id, info) in checked.annotations.infos() {
		let Some(&id) = local.get(&id) else { continue };
		types.push((
			id,
			canonicalize_type(&checked.interner, info.ty, &context)
				.map_err(|_| RuntimeExtractionError::IncompleteCanonicalType)?,
		));
		if let Some(resolution) = &info.resolution {
			dispatches.push((id, stable_dispatch(checked, resolution)?));
		}
	}
	let definition_targets = checked
		.annotations
		.definition_targets()
		.filter_map(|(id, target)| local.get(&id).map(|id| (*id, target.clone())))
		.collect::<Vec<_>>();
	let variants = checked
		.annotations
		.variants()
		.filter_map(|(id, variant)| local.get(&id).map(|id| (*id, variant)))
		.map(|(id, variant)| Ok((id, expression_variant(checked, variant)?)))
		.collect::<Result<Vec<_>, RuntimeExtractionError>>()?;
	let pattern_variants = checked
		.annotations
		.pattern_variants()
		.filter_map(|(span, variant)| patterns.get(&span).map(|id| (*id, variant)))
		.map(|(id, variant)| Ok((id, pattern_variant(checked, variant)?)))
		.collect::<Result<Vec<_>, RuntimeExtractionError>>()?;
	let positional_fields = patterns
		.iter()
		.filter_map(|(span, id)| {
			checked
				.annotations
				.positional_field_of(*span)
				.map(|name| (*id, *span, name))
		})
		.map(|(id, span, name)| {
			let variant = checked
				.annotations
				.pattern_variant_of(span)
				.or_else(|| {
					checked
						.annotations
						.pattern_variants()
						.filter(|(candidate, _)| candidate.start <= span.start && candidate.end >= span.end)
						.min_by_key(|(candidate, _)| candidate.end - candidate.start)
						.map(|(_, variant)| variant)
				})
				.ok_or_else(|| RuntimeExtractionError::IncompleteVariantTarget(name.clone()))?;
			let exact = variant_fields(checked, variant)?
				.into_iter()
				.find(|field| field.name == *name)
				.ok_or_else(|| RuntimeExtractionError::IncompleteVariantTarget(name.clone()))?;
			Ok((id, exact))
		})
		.collect::<Result<Vec<_>, RuntimeExtractionError>>()?;
	let mut iterations = Vec::new();
	let mut anonymous_closures = Vec::new();
	for (&source, &id) in local {
		if let Some(mode) = checked.annotations.iter_mode_of(source) {
			let protocols = iteration_protocol(checked)?;
			let iteration = match mode {
				crate::IterMode::Direct => RuntimeIteration::Direct {
					iterator_interface: protocols.2.clone(),
					next: protocols.3.clone(),
				},
				crate::IterMode::ViaIter => RuntimeIteration::ViaIter {
					iterable_interface: protocols.0.clone(),
					iter: stable_dispatch(
						checked,
						checked
							.annotations
							.iter_resolution_of(source)
							.ok_or(RuntimeExtractionError::MissingIterationProtocol)?,
					)?,
					iterator_interface: protocols.2.clone(),
					next: protocols.3.clone(),
				},
			};
			iterations.push((id, iteration));
		}
		if let Some(arity) = checked.annotations.anon_boundary_arity(source) {
			anonymous_closures.push((id, arity));
		}
	}
	types.sort_by_key(|item| item.0);
	dispatches.sort_by_key(|item| item.0);
	iterations.sort_by_key(|item| item.0);
	anonymous_closures.sort_by_key(|item| item.0);
	let mut external_marshals = Vec::new();
	for (id, target) in &definition_targets {
		if let Some(marshal) = external_marshal(checked, target) {
			external_marshals.push((*id, marshal));
		}
	}
	Ok(RuntimeAnnotations {
		types: types.into(),
		definition_targets: definition_targets.into(),
		dispatches: dispatches.into(),
		variants: variants.into(),
		pattern_variants: pattern_variants.into(),
		positional_fields: positional_fields.into(),
		iterations: iterations.into(),
		anonymous_closures: anonymous_closures.into(),
		external_marshals: external_marshals.into(),
	})
}

fn stable_dispatch(
	checked: &crate::CheckedFacts,
	resolution: &crate::annotate::Resolution,
) -> Result<StableDispatch, RuntimeExtractionError> {
	match resolution.dispatch {
		DispatchKind::BuiltinEager => Ok(StableDispatch::Builtin {
			method: resolution.method.clone(),
			category: BuiltinDispatch::Eager,
		}),
		DispatchKind::BuiltinShortCircuit => Ok(StableDispatch::Builtin {
			method: resolution.method.clone(),
			category: BuiltinDispatch::ShortCircuit,
		}),
		kind => {
			if let Some(implementation) = resolution.implementation.clone() {
				let member = resolution.target.clone().unwrap_or_else(|| {
					DefinitionId::new(
						implementation.module.clone(),
						crate::DeclarationKey::member(
							implementation.clone(),
							crate::DeclarationCategory::Method,
							resolution.method.clone(),
						),
					)
				});
				let interface = match &implementation.key {
					crate::DeclarationKey::Implementation { header, .. } => header.interface.clone(),
					_ => None,
				};
				let uses_default = checked
					.semantic
					.implementations
					.impls
					.iter()
					.find(|candidate| candidate.definition.as_ref() == Some(&implementation))
					.is_some_and(|candidate| !candidate.methods.contains_key(&resolution.method));
				return Ok(match (kind, interface) {
					(_, Some(interface)) if uses_default => StableDispatch::InterfaceDefault {
						interface,
						member,
						implementation,
						materialization: DispatchMaterialization::CanonicalBody,
					},
					(DispatchKind::UserImpl, Some(interface)) => StableDispatch::SelectedImplementation {
						interface,
						member,
						implementation,
						materialization: DispatchMaterialization::Attached,
					},
					_ => StableDispatch::Direct {
						member,
						implementation,
						materialization: DispatchMaterialization::Attached,
					},
				});
			}
			if kind == DispatchKind::UserImplDefaultMethod
				&& let Some(span) = resolution.impl_span
				&& let Some(implementation) = checked
					.semantic
					.implementations
					.impls
					.iter()
					.find(|implementation| implementation.legacy_span == Some(span))
					.and_then(|implementation| implementation.definition.clone())
				&& let Some((interface, member)) = stable_interface_member(checked, resolution)
			{
				return Ok(StableDispatch::InterfaceDefault {
					interface,
					member,
					implementation,
					materialization: DispatchMaterialization::CanonicalBody,
				});
			}
			let member = resolution
				.target
				.clone()
				.or_else(|| stable_interface_member(checked, resolution).map(|(_, member)| member))
				.ok_or_else(|| {
					RuntimeExtractionError::IncompleteDispatchTarget(resolution.method.clone())
				})?;
			let interface = match &member.key {
				crate::DeclarationKey::Member { owner, .. } => (**owner).clone(),
				_ => {
					return Err(RuntimeExtractionError::IncompleteDispatchTarget(
						resolution.method.clone(),
					));
				}
			};
			if kind == DispatchKind::UserImplDefaultMethod {
				return Ok(StableDispatch::InterfaceDefault {
					interface: interface.clone(),
					member,
					implementation: interface,
					materialization: DispatchMaterialization::CanonicalBody,
				});
			}
			if matches!(
				interface.key,
				crate::DeclarationKey::TopLevel {
					category: crate::DeclarationCategory::Interface,
					..
				}
			) {
				Ok(StableDispatch::GenericBound { interface, member })
			} else {
				Ok(StableDispatch::Direct {
					member,
					implementation: interface,
					materialization: DispatchMaterialization::Attached,
				})
			}
		}
	}
}

fn stable_interface_member(
	checked: &crate::CheckedFacts,
	resolution: &crate::annotate::Resolution,
) -> Option<(DefinitionId, DefinitionId)> {
	let matches = checked
		.semantic
		.interfaces
		.iter()
		.filter_map(|(id, interface)| {
			let method = interface.methods.get(&resolution.method)?;
			let owner = checked.semantic.definitions.stable(*id)?.clone();
			let member = method.definition.clone().unwrap_or_else(|| {
				DefinitionId::new(
					owner.module.clone(),
					crate::DeclarationKey::member(
						owner.clone(),
						crate::DeclarationCategory::Method,
						resolution.method.clone(),
					),
				)
			});
			Some((owner, member))
		})
		.collect::<Vec<_>>();
	(matches.len() == 1).then(|| matches[0].clone())
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
	// Stable binder IDs in all extracted declarations use that same order.
	let count = checked
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
	let scope = if matches!(definition.key, crate::DeclarationKey::TopLevel { .. }) {
		crate::BinderScope::Definition
	} else {
		crate::BinderScope::Member
	};
	(0..count)
		.map(|index| {
			(
				crate::ParamIdx(index as u32),
				crate::GenericParameterId::new(definition.binder(scope, 0), index as u32),
			)
		})
		.collect()
}

fn iteration_protocol(
	checked: &crate::CheckedFacts,
) -> Result<(DefinitionId, DefinitionId, DefinitionId, DefinitionId), RuntimeExtractionError> {
	let find = |interface_name: &str, method_name: &str| {
		let (id, stable) = checked
			.semantic
			.definitions
			.defs
			.iter()
			.enumerate()
			.find_map(|(index, item)| {
				(item.name == interface_name)
					.then(|| {
						item
							.stable
							.clone()
							.map(|stable| (crate::DefId(index as u32), stable))
					})
					.flatten()
			})?;
		let method = checked
			.semantic
			.interfaces
			.get(&id)?
			.methods
			.get(method_name)?
			.definition
			.clone()?;
		Some((stable, method))
	};
	let (iterable, iter) =
		find("Iterable", "iter").ok_or(RuntimeExtractionError::MissingIterationProtocol)?;
	let (iterator, next) =
		find("Iterator", "next").ok_or(RuntimeExtractionError::MissingIterationProtocol)?;
	Ok((iterable, iter, iterator, next))
}

fn external_marshal(
	checked: &crate::CheckedFacts,
	target: &DefinitionId,
) -> Option<nymph_hir::hir::MarshalKind> {
	checked
		.semantic
		.definitions
		.defs
		.iter()
		.find(|item| item.stable.as_ref() == Some(target))
		.and_then(|item| checked.external_value_marshals.get(&item.span).copied())
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

fn walk_body_patterns<'a>(expr: &'a Expr, out: &mut Vec<&'a nymph_ast::Spanned<Pattern>>) {
	let mut expressions = Vec::new();
	walk_expr(expr, &mut expressions);
	for expression in expressions {
		match &expression.kind {
			ExprKind::Closure { params, .. } => {
				for parameter in params {
					walk_pattern(&parameter.0.name, out);
				}
			}
			ExprKind::For { variable, .. } => walk_pattern(variable, out),
			ExprKind::Match { arms, .. } => {
				for arm in arms {
					walk_pattern(&arm.pattern, out);
				}
			}
			ExprKind::Block { body, .. } => {
				for statement in body {
					if let Statement::Let { meta, .. } = &statement.0 {
						walk_pattern(&meta.name, out);
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
		Pattern::List(entries) | Pattern::Tuple(entries) => {
			for entry in entries {
				if let ListPatternEntry::Item(pattern) = &entry.0 {
					walk_pattern(pattern, out);
				}
			}
		}
		Pattern::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapPatternEntry::Entry(key, value) => {
						walk_pattern(key, out);
						walk_pattern(value, out);
					}
					MapPatternEntry::Rest(_) => {}
				}
			}
		}
		Pattern::Range(range) => match range {
			RangePatternKind::From(value)
			| RangePatternKind::To(value)
			| RangePatternKind::ToInclusive(value) => walk_pattern(value, out),
			RangePatternKind::Exclusive { min, max } | RangePatternKind::Inclusive { min, max } => {
				walk_pattern(min, out);
				walk_pattern(max, out);
			}
		},
		Pattern::Struct { fields, .. } => {
			for field in fields {
				match &field.0 {
					StructPatternField::Value { value, .. } | StructPatternField::Positional(value) => {
						walk_pattern(value, out)
					}
					StructPatternField::Named(_) | StructPatternField::Rest => {}
				}
			}
		}
		Pattern::Union(left, right) => {
			walk_pattern(left, out);
			walk_pattern(right, out);
		}
		Pattern::Int(_)
		| Pattern::UInt(_)
		| Pattern::Float(_)
		| Pattern::Char(_)
		| Pattern::String(_)
		| Pattern::Boolean(_)
		| Pattern::Placeholder => {}
	}
}
