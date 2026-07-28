//! Owned, diagnostic-free semantic results for a source module.

use std::{collections::HashMap, ops::Deref, sync::Arc};

use nymph_ast::NodeId;
use nymph_ast::decl::Module;

use crate::annotate::{CheckedDefinitionTarget, VariantResolution};

use crate::{
	Annotations, CanonicalizationContext, CheckedFacts, DeclarationCategory, DeclarationKey,
	DefinitionId, DispatchKind, InterfaceType, ModuleInterface, canonicalize_type,
};

/// Stable annotation projection used only by differential inspection.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StableAnnotationView {
	pub types: Vec<(NodeId, InterfaceType)>,
	pub definition_targets: Vec<(NodeId, DefinitionId)>,
	pub resolutions: Vec<(
		NodeId,
		ecow::EcoString,
		DispatchKind,
		Option<DefinitionId>,
		Option<DefinitionId>,
	)>,
	pub variants: Vec<(NodeId, VariantResolution)>,
	pub pattern_variants: Vec<(nymph_ast::Span, VariantResolution)>,
}

/// Canonicalize checker annotations through exact checked-name bindings and complete
/// interface catalogs. This is an inspection helper, not a checking input.
#[doc(hidden)]
pub fn stable_annotation_view(
	facts: &CheckedFacts,
	checked_definitions: &[(ecow::EcoString, DefinitionId)],
	interfaces: &[Arc<ModuleInterface>],
) -> StableAnnotationView {
	let exact = checked_definitions
		.iter()
		.cloned()
		.collect::<HashMap<_, _>>();
	let definitions = facts
		.semantic
		.definitions
		.defs
		.iter()
		.enumerate()
		.filter_map(|(index, definition)| {
			definition
				.stable
				.clone()
				.or_else(|| exact.get(&definition.name).cloned())
				.map(|stable| (crate::DefId(index as u32), stable))
		})
		.collect::<HashMap<_, _>>();
	let context = CanonicalizationContext::new(definitions.clone(), HashMap::new());
	let canonical_variant = |resolution: &VariantResolution| {
		if resolution.enum_target.is_some() && resolution.variant_target.is_some() {
			return resolution.clone();
		}
		let enum_name = resolution
			.enum_name
			.rsplit('$')
			.next()
			.unwrap_or(&resolution.enum_name);
		let matches = interfaces
			.iter()
			.flat_map(|interface| interface.exports.iter())
			.filter(|definition| definition.name == enum_name)
			.filter_map(|definition| {
				definition
					.variants
					.iter()
					.find(|variant| variant.name == resolution.variant)
					.map(|variant| (definition, variant))
			})
			.collect::<Vec<_>>();
		if let [(definition, variant)] = matches.as_slice() {
			VariantResolution {
				enum_name: definition.name.clone(),
				variant: variant.name.clone(),
				enum_target: Some(definition.id.clone()),
				variant_target: Some(variant.id.clone()),
			}
		} else {
			resolution.clone()
		}
	};
	let types = facts
		.annotations
		.infos()
		.filter(|(id, _)| id.0 < 1 << 30)
		.filter_map(|(id, info)| {
			canonicalize_type(&facts.interner, info.ty, &context)
				.ok()
				.map(|ty| (id, ty))
		})
		.collect::<Vec<_>>();
	let mut definition_targets = facts
		.annotations
		.definition_targets()
		.filter(|(id, _)| id.0 < 1 << 30)
		.map(|(id, target)| (id, target.clone()))
		.collect::<Vec<_>>();
	for (id, checked_target) in facts.annotations.checked_definition_targets() {
		if id.0 >= 1 << 30 || definition_targets.iter().any(|(found, _)| *found == id) {
			continue;
		}
		let target = match checked_target {
			CheckedDefinitionTarget::Definition(def) => definitions.get(&def).cloned(),
			CheckedDefinitionTarget::Field { owner, index, name } => {
				definitions.get(&owner).and_then(|owner| {
					interfaces
						.iter()
						.flat_map(|interface| {
							interface.exports.iter().chain(
								interface
									.support_definitions
									.iter()
									.map(|item| &item.definition),
							)
						})
						.find(|definition| &definition.id == owner)
						.and_then(|definition| definition.fields.get(index))
						.map(|field| field.id.clone())
						.or_else(|| {
							Some(DefinitionId {
								module: owner.module.clone(),
								key: DeclarationKey::member(owner.clone(), DeclarationCategory::Field, name),
							})
						})
				})
			}
		};
		if let Some(target) = target {
			definition_targets.push((id, target));
		}
	}
	for (id, resolution) in facts.annotations.variants() {
		if id.0 < 1 << 30
			&& !definition_targets.iter().any(|(found, _)| *found == id)
			&& let Some(target) = canonical_variant(resolution).variant_target
		{
			definition_targets.push((id, target));
		}
	}
	let mut resolutions = Vec::new();
	for (id, info) in facts.annotations.infos() {
		if id.0 >= 1 << 30 {
			continue;
		}
		let Some(resolution) = &info.resolution else {
			continue;
		};
		let (dispatch, target, implementation) = match &resolution.resolved_target {
			Some(crate::annotate::ResolvedMethodTarget::Inherent {
				member,
				implementation,
			}) => (
				DispatchKind::UserImpl,
				Some(member.clone()),
				Some(implementation.clone()),
			),
			Some(crate::annotate::ResolvedMethodTarget::InterfaceImplementation { slot, .. }) => (
				match slot.source {
					crate::ImplementationMemberSource::Override => DispatchKind::UserImpl,
					crate::ImplementationMemberSource::InheritedDefault => {
						DispatchKind::UserImplDefaultMethod
					}
				},
				Some(slot.member_id.clone()),
				Some(slot.implementation_id.clone()),
			),
			Some(crate::annotate::ResolvedMethodTarget::GenericBound {
				interface,
				interface_member,
			}) => (
				DispatchKind::UserImplDefaultMethod,
				Some(interface_member.clone()),
				Some(interface.clone()),
			),
			None => (resolution.dispatch, None, None),
		};
		resolutions.push((
			id,
			resolution.method.clone(),
			dispatch,
			target,
			implementation,
		));
	}
	// A function-valued identifier has no call Resolution. Match its canonical type
	// against the exact complete function catalog; uniqueness prevents guesswork.
	for (id, ty) in &types {
		if definition_targets
			.iter()
			.any(|(target_id, _)| target_id == id)
		{
			continue;
		}
		let matches = interfaces
			.iter()
			.flat_map(|interface| interface.exports.iter())
			.filter(|definition| {
				matches!(
					definition.id.key,
					DeclarationKey::TopLevel {
						category: DeclarationCategory::Function,
						..
					}
				)
			})
			.filter(|definition| {
				definition.return_type.as_ref().is_some_and(|ret| {
					&InterfaceType::Function {
						parameters: definition
							.parameters
							.iter()
							.map(|parameter| parameter.ty.clone())
							.collect(),
						return_type: Box::new(ret.clone()),
					} == ty
				})
			})
			.collect::<Vec<_>>();
		if let [definition] = matches.as_slice() {
			definition_targets.push((*id, definition.id.clone()));
		}
	}
	definition_targets.sort_by_key(|(id, _)| *id);
	let variants = facts
		.annotations
		.variants()
		.filter(|(id, _)| id.0 < 1 << 30)
		.map(|(id, resolution)| (id, canonical_variant(resolution)))
		.collect();
	let pattern_variants = facts
		.annotations
		.pattern_variants()
		.map(|(span, resolution)| (span, canonical_variant(resolution)))
		.collect();
	StableAnnotationView {
		types,
		definition_targets,
		resolutions,
		variants,
		pattern_variants,
	}
}

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
/// Checked facts and annotations are owned independently from the diagnostics in
/// [`SemanticCheckResult`].
#[derive(Clone, Debug)]
pub struct SemanticAnalysis {
	pub module: Arc<Module>,
	pub checked: Arc<CheckedFacts>,
	pub annotations: Arc<ModuleAnnotations>,
}

#[derive(Clone, Debug)]
pub struct SemanticCheckResult {
	pub analysis: Arc<SemanticAnalysis>,
	pub diagnostics: Arc<[nymph_diagnostics::Diagnostic]>,
	pub lowerable: bool,
}
