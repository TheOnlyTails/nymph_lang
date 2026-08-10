//! Query-local semantic facts allocated from dependency interfaces.
//!
//! The Task 7 brief proposed accepting an external `&mut Interner`. That cannot form a
//! coherent fact arena: every [`Ty`](crate::Ty) is meaningful only in the interner that
//! created it. Consequently the environment owns its interner and all imported facts
//! that later allocation passes will populate. Pass A below allocates every recoverable
//! stable identity before any interface type is instantiated.

use std::{collections::HashMap, sync::Arc};

use ecow::EcoString;
use rustc_hash::FxHashMap;

use crate::def::{
	EnumSig, FieldSigMetadata, FuncParamSig, FuncSig, OwnedMemberSig, StructSig, ValueSig, VariantSig,
};
use crate::iface::{Bound, ImplDef, head_of};
use crate::members::{InherentImpl, InherentMethod};
use crate::{
	AliasSig, DeclarationCategory, DeclarationKey, DefId, DefKind, DefMap, DefinitionId,
	DefinitionShapeKind, ExportedDefinition, ExportedImpl, ExternalAbi, GenericParameter,
	ImplRegistry, InherentRegistry, InstantiationContext, InterfaceDef, InterfaceType, Interner,
	MemberKind, MemberShape, ModuleEnvironment, ModuleIdentity, NamespaceMemberSig, NamespaceSig,
	RecoveredDefinitionReference, RecoveredExportedDefinition, RecoveredExportedImpl,
	RecoveredInterfaceType, Signatures, VariantShape, instantiate_interface_type,
};

/// Exact stable identities assigned by the compiler to declarations with runtime
/// protocol meaning. `None` is an explicit unavailable fact; semantic recovery must
/// not replace it with a same-named declaration.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompilerRuntimeRoles {
	pub display: Option<InterfaceRuntimeRole>,
	pub debug: Option<InterfaceRuntimeRole>,
	pub iterable: Option<InterfaceRuntimeRole>,
	pub iterator: Option<InterfaceRuntimeRole>,
	pub option: Option<OptionRuntimeRole>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceRuntimeRole {
	pub interface: DefinitionId,
	pub member: DefinitionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OptionRuntimeRole {
	pub option: DefinitionId,
	pub some: DefinitionId,
	pub some_value: DefinitionId,
	pub none: DefinitionId,
}

/// Records why protocol roles are present or absent. In particular, an explicitly
/// empty compiler inventory must not be mistaken for the standalone compatibility
/// fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeRoleProvenance {
	StandaloneFixture,
	CompilerSupplied,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct LocalCompilerRuntimeRoles {
	pub iterable: Option<(DefId, DefinitionId)>,
	pub iterator: Option<(DefId, DefinitionId)>,
	pub option: Option<DefId>,
}

#[derive(Debug, Default, Clone)]
pub struct ImportedFacts {
	pub defs: DefMap,
	pub signatures: Signatures,
	pub interfaces: FxHashMap<crate::DefId, InterfaceDef>,
	pub implementations: ImplRegistry,
	pub inherent: InherentRegistry,
	/// Stable host descriptors retained without inventing checker/lowering provenance.
	pub external_abis: FxHashMap<crate::DefId, ExternalAbi>,
	/// Lossless member facts for namespaces, interfaces, and nominal types.
	pub definition_members: FxHashMap<crate::DefId, Vec<OwnedMemberSig>>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ModuleExportIndex {
	pub by_name: FxHashMap<EcoString, DefinitionId>,
	pub stable_ids: Vec<DefinitionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedImportBinding {
	Definition(DefinitionId),
	Namespace(ModuleIdentity),
	Poison,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentConstructionError {
	TooManyDefinitions,
	MissingKnownIdentity(DefinitionId),
}

#[derive(Debug)]
pub struct SemanticEnvironment {
	pub current: ModuleIdentity,
	pub interner: Interner,
	pub imported: ImportedFacts,
	pub module_exports: FxHashMap<ModuleIdentity, ModuleExportIndex>,
	pub resolved_imports: FxHashMap<EcoString, ResolvedImportBinding>,
	pub contains_recovery: bool,
	pub compiler_runtime_roles: CompilerRuntimeRoles,
	pub(crate) runtime_role_provenance: RuntimeRoleProvenance,
}

impl SemanticEnvironment {
	/// Assigns compatibility-visible checker spellings without changing semantic names,
	/// stable identities, or lexical lookup.
	pub fn set_diagnostic_module_tags(&mut self, tags: &FxHashMap<ModuleIdentity, usize>) {
		self.imported.defs.set_imported_diagnostic_module_tags(tags);
	}

	/// Allocates dependency identities in the caller's dependency-first graph order.
	/// Export name overlays intentionally use insertion order, preserving compatibility's
	/// complete-transitive visibility and deterministic later-wins behavior.
	pub fn from_modules(
		current: ModuleIdentity,
		modules: &[Arc<ModuleEnvironment>],
	) -> Result<Self, EnvironmentConstructionError> {
		let mut environment =
			Self::from_modules_with_runtime_roles(current, modules, CompilerRuntimeRoles::default())?;
		environment.runtime_role_provenance = RuntimeRoleProvenance::StandaloneFixture;
		Ok(environment)
	}

	pub fn from_modules_with_runtime_roles(
		current: ModuleIdentity,
		modules: &[Arc<ModuleEnvironment>],
		compiler_runtime_roles: CompilerRuntimeRoles,
	) -> Result<Self, EnvironmentConstructionError> {
		let mut imported = ImportedFacts::default();
		let mut module_exports: FxHashMap<ModuleIdentity, ModuleExportIndex> = FxHashMap::default();
		let mut contains_recovery = false;

		for module in modules {
			match module.as_ref() {
				ModuleEnvironment::Complete(interface) => {
					let index = module_exports.entry(interface.module.clone()).or_default();
					for definition in &interface.exports {
						allocate_complete_definition(&mut imported.defs, definition, true)?;
						index
							.by_name
							.insert(definition.name.clone(), definition.id.clone());
						index.stable_ids.push(definition.id.clone());
					}
					for support in &interface.support_definitions {
						allocate_complete_definition(&mut imported.defs, &support.definition, false)?;
					}
					for implementation in &interface.implementations {
						allocate_complete_impl_references(&mut imported.defs, implementation)?;
					}
				}
				ModuleEnvironment::Recovered(interface) => {
					contains_recovery = true;
					let index = module_exports.entry(interface.module.clone()).or_default();
					for definition in &interface.exports {
						allocate_recovered_definition(&mut imported.defs, definition, true)?;
						index
							.by_name
							.insert(definition.name.clone(), definition.id.clone());
						index.stable_ids.push(definition.id.clone());
					}
					for support in &interface.support_definitions {
						allocate_recovered_definition(&mut imported.defs, &support.definition, false)?;
					}
					for implementation in &interface.implementations {
						allocate_recovered_impl_references(&mut imported.defs, implementation)?;
					}
				}
			}
		}

		// Pass B starts only now: every complete/recovered Known stable identity has a
		// checker-local allocation, so conversion can never allocate on demand.
		let definitions = Arc::new(definition_map(&imported.defs));
		let mut interner = Interner::new();
		for module in modules {
			match module.as_ref() {
				ModuleEnvironment::Complete(interface) => {
					for definition in interface.exports.iter().chain(
						interface
							.support_definitions
							.iter()
							.map(|support| &support.definition),
					) {
						instantiate_complete_definition(
							&mut imported,
							&mut interner,
							definition,
							&definitions,
						)?;
					}
				}
				ModuleEnvironment::Recovered(interface) => {
					for definition in interface.exports.iter().chain(
						interface
							.support_definitions
							.iter()
							.map(|support| &support.definition),
					) {
						instantiate_recovered_definition(
							&mut imported,
							&mut interner,
							definition,
							&definitions,
						)?;
					}
				}
			}
		}
		// Interfaces from every dependency are complete before candidates are indexed:
		// default-method completion and stable interface lookup must not depend on a
		// definition's position within one module summary.
		for module in modules {
			match module.as_ref() {
				ModuleEnvironment::Complete(interface) => {
					for implementation in &interface.implementations {
						instantiate_complete_impl(&mut imported, &mut interner, implementation, &definitions)?;
					}
				}
				ModuleEnvironment::Recovered(interface) => {
					for implementation in &interface.implementations {
						instantiate_recovered_impl(&mut imported, &mut interner, implementation, &definitions)?;
					}
				}
			}
		}

		Ok(Self {
			current,
			interner,
			imported,
			module_exports,
			resolved_imports: FxHashMap::default(),
			contains_recovery,
			compiler_runtime_roles,
			runtime_role_provenance: RuntimeRoleProvenance::CompilerSupplied,
		})
	}

	/// Replaces compatibility's transitive bare-name overlay with the lexical bindings
	/// resolved for the module currently being checked. All imported definitions remain
	/// allocated and stable-addressable for transported types and dispatch.
	pub fn set_resolved_imports(&mut self, bindings: FxHashMap<EcoString, ResolvedImportBinding>) {
		self.imported.defs.clear_lexical_imports();
		for (local, binding) in &bindings {
			match binding {
				ResolvedImportBinding::Definition(stable) => {
					if let Some(def) = self.imported.defs.by_stable(stable) {
						self.imported.defs.expose_name(local.clone(), def);
						if let Some(enum_sig) = self.imported.signatures.enums.get(&def) {
							for variant in &enum_sig.variants {
								let Some(target) = variant
									.target
									.as_ref()
									.and_then(|target| self.imported.defs.by_stable(target))
								else {
									continue;
								};
								self
									.imported
									.defs
									.expose_imported_variant(variant.name.clone(), target);
							}
						}
					}
				}
				ResolvedImportBinding::Namespace(module) => {
					let def = self.imported.defs.define_imported(
						local.clone(),
						DefKind::Namespace,
						module.clone(),
						None,
					);
					let mut namespace = NamespaceSig::default();
					if let Some(index) = self.module_exports.get(module) {
						for (name, stable) in &index.by_name {
							let Some(target) = self.imported.defs.by_stable(stable) else {
								continue;
							};
							let member = if let Some(sig) = self.imported.signatures.funcs.get(&target) {
								Some(NamespaceMemberSig::Func {
									target: Some(stable.clone()),
									sig: sig.clone(),
								})
							} else {
								self
									.imported
									.signatures
									.lets
									.get(&target)
									.map(|sig| NamespaceMemberSig::Value {
										target: Some(stable.clone()),
										ty: sig.ty,
										mutable: false,
									})
							};
							if let Some(member) = member {
								namespace.members.insert(name.clone(), member);
							}
						}
					}
					self.imported.signatures.namespaces.insert(def, namespace);
				}
				ResolvedImportBinding::Poison => {}
			}
		}
		self.resolved_imports = bindings;
	}
}

fn shape_kind(kind: DefinitionShapeKind) -> DefKind {
	match kind {
		DefinitionShapeKind::Function => DefKind::Func,
		DefinitionShapeKind::Let => DefKind::Let,
		DefinitionShapeKind::TypeAlias => DefKind::TypeAlias,
		DefinitionShapeKind::Struct => DefKind::Struct,
		DefinitionShapeKind::Enum => DefKind::Enum,
		DefinitionShapeKind::Interface => DefKind::Interface,
		DefinitionShapeKind::Namespace => DefKind::Namespace,
	}
}

fn inferred_kind(id: &DefinitionId) -> DefKind {
	let category = match &id.key {
		DeclarationKey::TopLevel { category, .. } | DeclarationKey::Member { category, .. } => {
			*category
		}
		_ => DeclarationCategory::Struct,
	};
	match category {
		DeclarationCategory::Function
		| DeclarationCategory::Method
		| DeclarationCategory::Static
		| DeclarationCategory::MethodBody => DefKind::Func,
		DeclarationCategory::Let | DeclarationCategory::Field => DefKind::Let,
		DeclarationCategory::TypeAlias => DefKind::TypeAlias,
		DeclarationCategory::Enum | DeclarationCategory::Variant => DefKind::Enum,
		DeclarationCategory::Interface => DefKind::Interface,
		DeclarationCategory::Namespace => DefKind::Namespace,
		DeclarationCategory::Struct | DeclarationCategory::Implementation => DefKind::Struct,
	}
}

fn allocate(
	defs: &mut DefMap,
	id: &DefinitionId,
	name: EcoString,
	kind: DefKind,
	visible: bool,
) -> Result<(), EnvironmentConstructionError> {
	if defs.by_stable(id).is_none() && defs.defs.len() == u32::MAX as usize {
		return Err(EnvironmentConstructionError::TooManyDefinitions);
	}
	defs.allocate_imported(name, kind, id.module.clone(), id.clone(), visible);
	Ok(())
}

fn allocate_reference(
	defs: &mut DefMap,
	id: &DefinitionId,
) -> Result<(), EnvironmentConstructionError> {
	let name = id_name(id);
	allocate(defs, id, name, inferred_kind(id), false)
}

fn id_name(id: &DefinitionId) -> EcoString {
	match &id.key {
		DeclarationKey::TopLevel { name, .. }
		| DeclarationKey::Member { name, .. }
		| DeclarationKey::MethodBody { name, .. } => name.clone(),
		DeclarationKey::Implementation { .. } | DeclarationKey::RecoveredImplementation { .. } => {
			"<impl>".into()
		}
		DeclarationKey::MaterializedInterfaceMember { .. } => "<materialized>".into(),
	}
}

fn walk_type(defs: &mut DefMap, ty: &InterfaceType) -> Result<(), EnvironmentConstructionError> {
	match ty {
		InterfaceType::Named {
			definition,
			positional,
			named,
		} => {
			allocate_reference(defs, definition)?;
			for ty in positional {
				walk_type(defs, ty)?;
			}
			for (_, ty) in named {
				walk_type(defs, ty)?;
			}
		}
		InterfaceType::List(ty) | InterfaceType::Mutable(ty) => walk_type(defs, ty)?,
		InterfaceType::Tuple(types) | InterfaceType::Intersection(types) => {
			for ty in types {
				walk_type(defs, ty)?;
			}
		}
		InterfaceType::Map(a, b) => {
			walk_type(defs, a)?;
			walk_type(defs, b)?;
		}
		InterfaceType::Function {
			parameters,
			return_type,
		} => {
			for ty in parameters {
				walk_type(defs, ty)?;
			}
			walk_type(defs, return_type)?;
		}
		_ => {}
	}
	Ok(())
}

fn walk_member(
	defs: &mut DefMap,
	member: &MemberShape<InterfaceType>,
) -> Result<(), EnvironmentConstructionError> {
	allocate_reference(defs, &member.id)?;
	for parameter in &member.parameters {
		walk_type(defs, &parameter.ty)?;
	}
	walk_type(defs, &member.return_type)?;
	if let Some(owner) = &member.runtime_owner {
		allocate_reference(defs, owner)?;
	}
	Ok(())
}

fn walk_variants(
	defs: &mut DefMap,
	enum_id: &DefinitionId,
	variants: &[VariantShape<InterfaceType>],
) -> Result<(), EnvironmentConstructionError> {
	let enum_def = defs
		.by_stable(enum_id)
		.expect("enum owner is allocated before its variants");
	for (variant_index, variant) in variants.iter().enumerate() {
		allocate(
			defs,
			&variant.id,
			variant.name.clone(),
			DefKind::Variant {
				enum_def,
				variant: variant_index,
			},
			false,
		)?;
		let variant_def = defs.by_stable(&variant.id).unwrap();
		defs.expose_imported_variant(variant.name.clone(), variant_def);
		for field in &variant.fields {
			allocate_reference(defs, &field.id)?;
			walk_type(defs, &field.ty)?;
		}
	}
	Ok(())
}

fn allocate_complete_definition(
	defs: &mut DefMap,
	definition: &ExportedDefinition,
	visible: bool,
) -> Result<(), EnvironmentConstructionError> {
	allocate(
		defs,
		&definition.id,
		definition.name.clone(),
		shape_kind(definition.kind),
		visible,
	)?;
	for constraint in &definition.constraints {
		allocate_reference(defs, &constraint.interface)?;
		for ty in &constraint.positional {
			walk_type(defs, ty)?;
		}
		for (_, ty) in &constraint.named {
			walk_type(defs, ty)?;
		}
	}
	for parameter in &definition.parameters {
		walk_type(defs, &parameter.ty)?;
	}
	if let Some(ty) = &definition.return_type {
		walk_type(defs, ty)?;
	}
	if let Some(ty) = &definition.ty {
		walk_type(defs, ty)?;
	}
	for field in &definition.fields {
		allocate_reference(defs, &field.id)?;
		walk_type(defs, &field.ty)?;
	}
	walk_variants(defs, &definition.id, &definition.variants)?;
	for member in &definition.members {
		walk_member(defs, member)?;
	}
	for super_interface in &definition.super_interfaces {
		allocate_reference(defs, &super_interface.interface)?;
	}
	if let Some(owner) = &definition.runtime_owner {
		allocate_reference(defs, owner)?;
	}
	Ok(())
}

fn allocate_complete_impl_references(
	defs: &mut DefMap,
	implementation: &ExportedImpl,
) -> Result<(), EnvironmentConstructionError> {
	if let Some(interface) = &implementation.interface {
		allocate_reference(defs, interface)?;
	}
	walk_type(defs, &implementation.self_type)?;
	for (_, ty) in &implementation.interface_arguments {
		walk_type(defs, ty)?;
	}
	for constraint in &implementation.constraints {
		allocate_reference(defs, &constraint.interface)?;
	}
	for member in &implementation.members {
		walk_member(defs, member)?;
	}
	if let Some(owner) = &implementation.runtime_owner {
		allocate_reference(defs, owner)?;
	}
	Ok(())
}

fn walk_recovered_type(
	defs: &mut DefMap,
	ty: &RecoveredInterfaceType,
) -> Result<(), EnvironmentConstructionError> {
	if let RecoveredInterfaceType::Known(ty) = ty {
		walk_type(defs, ty)?;
	}
	Ok(())
}

fn allocate_recovered_definition(
	defs: &mut DefMap,
	definition: &RecoveredExportedDefinition,
	visible: bool,
) -> Result<(), EnvironmentConstructionError> {
	allocate(
		defs,
		&definition.id,
		definition.name.clone(),
		shape_kind(definition.kind),
		visible,
	)?;
	for constraint in &definition.constraints {
		if let RecoveredDefinitionReference::Known(id) = &constraint.interface {
			allocate_reference(defs, id)?;
		}
	}
	for parameter in &definition.parameters {
		walk_recovered_type(defs, &parameter.ty)?;
	}
	if let Some(ty) = &definition.return_type {
		walk_recovered_type(defs, ty)?;
	}
	if let Some(ty) = &definition.ty {
		walk_recovered_type(defs, ty)?;
	}
	for field in &definition.fields {
		allocate_reference(defs, &field.id)?;
		walk_recovered_type(defs, &field.ty)?;
	}
	for variant in &definition.variants {
		allocate_reference(defs, &variant.id)?;
		for field in &variant.fields {
			allocate_reference(defs, &field.id)?;
			walk_recovered_type(defs, &field.ty)?;
		}
	}
	for member in &definition.members {
		allocate_reference(defs, &member.id)?;
		for parameter in &member.parameters {
			walk_recovered_type(defs, &parameter.ty)?;
		}
		walk_recovered_type(defs, &member.return_type)?;
	}
	for super_interface in &definition.super_interfaces {
		if let RecoveredDefinitionReference::Known(id) = &super_interface.interface {
			allocate_reference(defs, id)?;
		}
	}
	if let Some(owner) = &definition.runtime_owner {
		allocate_reference(defs, owner)?;
	}
	Ok(())
}

fn allocate_recovered_impl_references(
	defs: &mut DefMap,
	implementation: &RecoveredExportedImpl,
) -> Result<(), EnvironmentConstructionError> {
	if let Some(RecoveredDefinitionReference::Known(id)) = &implementation.interface {
		allocate_reference(defs, id)?;
	}
	walk_recovered_type(defs, &implementation.self_type)?;
	for (_, ty) in &implementation.interface_arguments {
		walk_recovered_type(defs, ty)?;
	}
	for constraint in &implementation.constraints {
		if let RecoveredDefinitionReference::Known(id) = &constraint.interface {
			allocate_reference(defs, id)?;
		}
	}
	for member in &implementation.members {
		allocate_reference(defs, &member.id)?;
	}
	if let Some(owner) = &implementation.runtime_owner {
		allocate_reference(defs, owner)?;
	}
	Ok(())
}

fn definition_map(defs: &DefMap) -> HashMap<DefinitionId, DefId> {
	defs
		.defs
		.iter()
		.enumerate()
		.filter_map(|(index, data)| data.stable.clone().map(|id| (id, DefId(index as u32))))
		.collect()
}

fn context(
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
	binders: &[GenericParameter],
) -> InstantiationContext {
	InstantiationContext::with_shared_definitions(
		Arc::clone(definitions),
		binders
			.iter()
			.enumerate()
			.map(|(index, binder)| (binder.id.clone(), crate::ParamIdx(index as u32)))
			.collect(),
	)
}

fn bounds(
	defs: &DefMap,
	interner: &mut Interner,
	binders: &[GenericParameter],
	constraints: &[crate::GenericConstraint],
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<Vec<Bound>, EnvironmentConstructionError> {
	let ctx = context(definitions, binders);
	constraints
		.iter()
		.map(|constraint| {
			let ty = binders
				.iter()
				.position(|binder| binder.id == constraint.parameter)
				.map(|index| interner.mk_param(crate::ParamIdx(index as u32)))
				.unwrap_or_else(|| interner.error());
			let interface = defs.by_stable(&constraint.interface).ok_or_else(|| {
				EnvironmentConstructionError::MissingKnownIdentity(constraint.interface.clone())
			})?;
			let mut args = constraint
				.positional
				.iter()
				.enumerate()
				.map(|(index, ty)| {
					(
						index.to_string().into(),
						instantiate_interface_type(interner, ty, &ctx),
					)
				})
				.collect::<Vec<_>>();
			args.extend(
				constraint
					.named
					.iter()
					.map(|(name, ty)| (name.clone(), instantiate_interface_type(interner, ty, &ctx))),
			);
			Ok(Bound {
				ty,
				interface,
				args,
			})
		})
		.collect()
}

fn func_sig(
	defs: &DefMap,
	interner: &mut Interner,
	binders: &[GenericParameter],
	constraints: &[crate::GenericConstraint],
	parameters: &[crate::ParameterShape<InterfaceType>],
	ret: &InterfaceType,
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<FuncSig, EnvironmentConstructionError> {
	let ctx = context(definitions, binders);
	let params = parameters
		.iter()
		.map(|parameter| FuncParamSig {
			label: parameter.name.clone(),
			ty: instantiate_interface_type(interner, &parameter.ty, &ctx),
			spread: parameter.spread,
		})
		.collect();
	let ret = instantiate_interface_type(interner, ret, &ctx);
	Ok(FuncSig {
		generics: binders.iter().map(|binder| binder.name.clone()).collect(),
		params,
		ret,
		has_self: false,
		bounds: bounds(defs, interner, binders, constraints, definitions)?,
	})
}

fn owned_member(
	defs: &DefMap,
	interner: &mut Interner,
	owner_binders: &[GenericParameter],
	member: &MemberShape<InterfaceType>,
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<OwnedMemberSig, EnvironmentConstructionError> {
	let mut binders = owner_binders.to_vec();
	binders.extend(member.binders.clone());
	let sig = func_sig(
		defs,
		interner,
		&binders,
		&member.constraints,
		&member.parameters,
		&member.return_type,
		definitions,
	)?;
	Ok(OwnedMemberSig {
		target: member.id.clone(),
		kind: member.kind,
		generics: sig.generics,
		bounds: sig.bounds,
		params: sig.params,
		ret: sig.ret,
		has_default: member.has_default,
		external: member.external.clone(),
	})
}

fn recovered_owned_member(
	defs: &DefMap,
	interner: &mut Interner,
	owner_binders: &[GenericParameter],
	member: &crate::RecoveredMemberShape,
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<OwnedMemberSig, EnvironmentConstructionError> {
	let mut binders = owner_binders.to_vec();
	binders.extend(member.binders.clone());
	let ctx = context(definitions, &binders);
	Ok(OwnedMemberSig {
		target: member.id.clone(),
		kind: member.kind,
		generics: binders.iter().map(|binder| binder.name.clone()).collect(),
		bounds: recovered_bounds(defs, interner, &binders, &member.constraints, definitions)?,
		params: member
			.parameters
			.iter()
			.map(|parameter| FuncParamSig {
				label: parameter.name.clone(),
				ty: recovered_ty(interner, &parameter.ty, &ctx),
				spread: parameter.spread,
			})
			.collect(),
		ret: recovered_ty(interner, &member.return_type, &ctx),
		has_default: member.has_default,
		external: member.external.clone(),
	})
}

fn instantiate_complete_definition(
	facts: &mut ImportedFacts,
	interner: &mut Interner,
	definition: &ExportedDefinition,
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<(), EnvironmentConstructionError> {
	let def = facts
		.defs
		.by_stable(&definition.id)
		.ok_or_else(|| EnvironmentConstructionError::MissingKnownIdentity(definition.id.clone()))?;
	let ctx = context(definitions, &definition.binders);
	let generics = definition
		.binders
		.iter()
		.map(|binder| binder.name.clone())
		.collect();
	let definition_bounds = bounds(
		&facts.defs,
		interner,
		&definition.binders,
		&definition.constraints,
		definitions,
	)?;
	let owned_members = definition
		.members
		.iter()
		.filter(|member| member.visibility != Some(nymph_ast::decl::Visibility::Private))
		.map(|member| {
			owned_member(
				&facts.defs,
				interner,
				&definition.binders,
				member,
				definitions,
			)
		})
		.collect::<Result<Vec<_>, _>>()?;
	facts.definition_members.insert(def, owned_members);
	match definition.kind {
		DefinitionShapeKind::Function => {
			let ret = definition
				.return_type
				.as_ref()
				.unwrap_or(&InterfaceType::Void);
			facts.signatures.funcs.insert(
				def,
				func_sig(
					&facts.defs,
					interner,
					&definition.binders,
					&definition.constraints,
					&definition.parameters,
					ret,
					definitions,
				)?,
			);
		}
		DefinitionShapeKind::Let => {
			let ty = match &definition.ty {
				Some(ty) => instantiate_interface_type(interner, ty, &ctx),
				None => interner.error(),
			};
			facts.signatures.lets.insert(
				def,
				ValueSig {
					generics,
					ty,
					bounds: definition_bounds,
				},
			);
		}
		DefinitionShapeKind::Struct => {
			let fields = definition
				.fields
				.iter()
				.filter(|field| field.visibility != Some(nymph_ast::decl::Visibility::Private))
				.map(|field| {
					(
						field.name.clone(),
						instantiate_interface_type(interner, &field.ty, &ctx),
					)
				})
				.collect();
			facts.signatures.structs.insert(
				def,
				StructSig {
					generics,
					fields,
					field_metadata: definition
						.fields
						.iter()
						.filter(|field| field.visibility != Some(nymph_ast::decl::Visibility::Private))
						.map(|field| FieldSigMetadata {
							target: Some(field.id.clone()),
							mutable: field.mutable,
							has_default: field.has_default,
						})
						.collect(),
					bounds: definition_bounds,
				},
			);
		}
		DefinitionShapeKind::Enum => {
			let variants = definition
				.variants
				.iter()
				.map(|variant| VariantSig {
					target: Some(variant.id.clone()),
					name: variant.name.clone(),
					fields: variant
						.fields
						.iter()
						.map(|field| {
							(
								field.name.clone(),
								instantiate_interface_type(interner, &field.ty, &ctx),
							)
						})
						.collect(),
					field_metadata: variant
						.fields
						.iter()
						.map(|field| FieldSigMetadata {
							target: Some(field.id.clone()),
							mutable: field.mutable,
							has_default: field.has_default,
						})
						.collect(),
				})
				.collect();
			facts.signatures.enums.insert(
				def,
				EnumSig {
					generics,
					variants,
					bounds: definition_bounds,
				},
			);
		}
		DefinitionShapeKind::TypeAlias => {
			let target = match &definition.ty {
				Some(ty) => instantiate_interface_type(interner, ty, &ctx),
				None => interner.error(),
			};
			facts.signatures.aliases.insert(
				def,
				AliasSig {
					generics,
					target,
					bounds: definition_bounds,
				},
			);
		}
		DefinitionShapeKind::Namespace => {
			let mut namespace = NamespaceSig::default();
			for member in definition
				.members
				.iter()
				.filter(|member| member.visibility != Some(nymph_ast::decl::Visibility::Private))
			{
				let mut member_binders = definition.binders.clone();
				member_binders.extend(member.binders.clone());
				let member_ctx = context(definitions, &member_binders);
				let target = Some(member.id.clone());
				let value = match member.kind {
					MemberKind::Function | MemberKind::MutatingFunction | MemberKind::StaticFunction => {
						NamespaceMemberSig::Func {
							target,
							sig: func_sig(
								&facts.defs,
								interner,
								&member_binders,
								&[],
								&member.parameters,
								&member.return_type,
								definitions,
							)?,
						}
					}
					_ => NamespaceMemberSig::Value {
						target,
						ty: instantiate_interface_type(interner, &member.return_type, &member_ctx),
						mutable: matches!(member.kind, MemberKind::MutableValue),
					},
				};
				namespace.members.insert(member.name.clone(), value);
			}
			facts.signatures.namespaces.insert(def, namespace);
		}
		DefinitionShapeKind::Interface => {
			let mut interface = InterfaceDef {
				generics,
				runtime_members: definition
					.members
					.iter()
					.filter(|member| member.visibility != Some(nymph_ast::decl::Visibility::Private))
					.map(|member| crate::iface::RuntimeMemberDef {
						definition: Some(member.id.clone()),
						name: member.name.clone(),
						kind: member.kind,
						has_default: member.has_default,
						external: member.external.is_some(),
						marshal: member.external.as_ref().and_then(|abi| abi.marshal),
					})
					.collect(),
				methods: FxHashMap::default(),
			};
			for member in definition
				.members
				.iter()
				.filter(|member| member.visibility != Some(nymph_ast::decl::Visibility::Private))
			{
				if matches!(
					member.kind,
					MemberKind::Function | MemberKind::MutatingFunction
				) {
					let mut member_binders = definition.binders.clone();
					member_binders.extend(member.binders.clone());
					let sig = func_sig(
						&facts.defs,
						interner,
						&member_binders,
						&[],
						&member.parameters,
						&member.return_type,
						definitions,
					)?;
					interface.methods.insert(
						member.name.clone(),
						crate::iface::IfaceMethod {
							definition: Some(member.id.clone()),
							has_default: member.has_default,
							params: sig.params.into_iter().map(|p| p.ty).collect(),
							ret: sig.ret,
							generics: member.binders.iter().map(|b| b.name.clone()).collect(),
							bounds: sig.bounds,
							mutating: matches!(member.kind, MemberKind::MutatingFunction),
						},
					);
				}
			}
			facts.interfaces.insert(def, interface);
		}
	}
	if let Some(external) = &definition.external {
		facts.external_abis.insert(def, external.clone());
	}
	if matches!(
		definition.kind,
		DefinitionShapeKind::Struct | DefinitionShapeKind::Enum
	) {
		instantiate_definition_inherent(facts, interner, definition, definitions)?;
	}
	Ok(())
}

fn complete_inherent_method(
	facts: &ImportedFacts,
	interner: &mut Interner,
	owner_binders: &[GenericParameter],
	member: &MemberShape<InterfaceType>,
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<InherentMethod, EnvironmentConstructionError> {
	let owned = owned_member(&facts.defs, interner, owner_binders, member, definitions)?;
	Ok(InherentMethod {
		definition: Some(owned.target),
		local_span: None,
		generic_names: member
			.binders
			.iter()
			.map(|binder| binder.name.clone())
			.collect(),
		params: owned
			.params
			.into_iter()
			.map(|parameter| parameter.ty)
			.collect(),
		ret: owned.ret,
		bounds: owned.bounds,
		namespaced: matches!(
			member.kind,
			MemberKind::StaticFunction | MemberKind::StaticValue
		),
		mutating: matches!(member.kind, MemberKind::MutatingFunction),
		external: member.external.is_some(),
	})
}

fn instantiate_definition_inherent(
	facts: &mut ImportedFacts,
	interner: &mut Interner,
	definition: &ExportedDefinition,
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<(), EnvironmentConstructionError> {
	let ctx = context(definitions, &definition.binders);
	let self_ty = instantiate_interface_type(
		interner,
		&InterfaceType::Named {
			definition: definition.id.clone(),
			positional: definition
				.binders
				.iter()
				.map(|binder| InterfaceType::Generic(binder.id.clone()))
				.collect(),
			named: vec![],
		},
		&ctx,
	);
	let methods: FxHashMap<_, _> = definition
		.members
		.iter()
		.filter(|member| {
			member.visibility != Some(nymph_ast::decl::Visibility::Private)
				&& matches!(
					member.kind,
					MemberKind::Function | MemberKind::MutatingFunction | MemberKind::StaticFunction
				)
		})
		.map(|member| {
			Ok((
				member.name.clone(),
				complete_inherent_method(facts, interner, &definition.binders, member, definitions)?,
			))
		})
		.collect::<Result<_, EnvironmentConstructionError>>()?;
	if !methods.is_empty() {
		facts.inherent.add(
			head_of(interner, self_ty),
			InherentImpl {
				definition: Some(definition.id.clone()),
				owner_generic_names: definition
					.binders
					.iter()
					.map(|binder| binder.name.clone())
					.collect(),
				self_ty,
				methods,
				constraints: bounds(
					&facts.defs,
					interner,
					&definition.binders,
					&definition.constraints,
					definitions,
				)?,
				imported: true,
			},
		);
	}
	Ok(())
}

fn instantiate_complete_impl(
	facts: &mut ImportedFacts,
	interner: &mut Interner,
	implementation: &ExportedImpl,
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<(), EnvironmentConstructionError> {
	let ctx = context(definitions, &implementation.binders);
	let self_ty = instantiate_interface_type(interner, &implementation.self_type, &ctx);
	let constraints = bounds(
		&facts.defs,
		interner,
		&implementation.binders,
		&implementation.constraints,
		definitions,
	)?;
	let methods = implementation
		.members
		.iter()
		.filter(|member| {
			matches!(
				member.kind,
				MemberKind::Function | MemberKind::MutatingFunction | MemberKind::StaticFunction
			)
		})
		.map(|member| {
			let method = complete_inherent_method(
				facts,
				interner,
				&implementation.binders,
				member,
				definitions,
			)?;
			Ok((
				member.name.clone(),
				crate::iface::IfaceMethod {
					definition: method.definition,
					has_default: false,
					params: method.params,
					ret: method.ret,
					generics: method.generic_names,
					bounds: method.bounds,
					mutating: method.mutating,
				},
			))
		})
		.collect::<Result<FxHashMap<_, _>, EnvironmentConstructionError>>()?;
	if let Some(interface_id) = &implementation.interface {
		let interface = facts
			.defs
			.by_stable(interface_id)
			.ok_or_else(|| EnvironmentConstructionError::MissingKnownIdentity(interface_id.clone()))?;
		let args = implementation
			.interface_arguments
			.iter()
			.map(|(name, ty)| (name.clone(), instantiate_interface_type(interner, ty, &ctx)))
			.collect();
		let blanket = matches!(interner.kind(self_ty), crate::TyKind::Param(_));
		facts.implementations.add(
			interner,
			ImplDef {
				definition: Some(implementation.id.clone()),
				member_catalog: implementation.member_slots.clone(),
				runtime_members: implementation
					.members
					.iter()
					.map(|member| crate::iface::RuntimeMemberDef {
						definition: Some(member.id.clone()),
						name: member.name.clone(),
						kind: member.kind,
						has_default: member.has_default,
						external: member.external.is_some(),
						marshal: member.external.as_ref().and_then(|abi| abi.marshal),
					})
					.collect(),
				generics: implementation
					.binders
					.iter()
					.map(|b| b.name.clone())
					.collect(),
				self_ty,
				interface,
				legacy_span: None,
				args,
				methods,
				constraints,
				blanket,
			},
		);
	} else {
		let inherent_methods = implementation
			.members
			.iter()
			.filter(|member| {
				member.visibility != Some(nymph_ast::decl::Visibility::Private)
					&& matches!(
						member.kind,
						MemberKind::Function | MemberKind::MutatingFunction | MemberKind::StaticFunction
					)
			})
			.map(|member| {
				Ok((
					member.name.clone(),
					complete_inherent_method(
						facts,
						interner,
						&implementation.binders,
						member,
						definitions,
					)?,
				))
			})
			.collect::<Result<_, EnvironmentConstructionError>>()?;
		facts.inherent.add(
			head_of(interner, self_ty),
			InherentImpl {
				definition: Some(implementation.id.clone()),
				owner_generic_names: implementation
					.binders
					.iter()
					.map(|b| b.name.clone())
					.collect(),
				self_ty,
				methods: inherent_methods,
				constraints,
				imported: true,
			},
		);
	}
	Ok(())
}

fn recovered_ty(
	interner: &mut Interner,
	ty: &RecoveredInterfaceType,
	ctx: &InstantiationContext,
) -> crate::Ty {
	match ty {
		RecoveredInterfaceType::Known(ty) => instantiate_interface_type(interner, ty, ctx),
		RecoveredInterfaceType::Poison => interner.error(),
	}
}

fn recovered_bounds(
	defs: &DefMap,
	interner: &mut Interner,
	binders: &[GenericParameter],
	constraints: &[crate::RecoveredGenericConstraint],
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<Vec<Bound>, EnvironmentConstructionError> {
	let ctx = context(definitions, binders);
	constraints
		.iter()
		.filter_map(|constraint| {
			let crate::RecoveredDefinitionReference::Known(interface_id) = &constraint.interface else {
				// A poisoned interface identity is not an unconstrained candidate.
				return None;
			};
			Some((constraint, interface_id))
		})
		.map(|(constraint, interface_id)| {
			let ty = binders
				.iter()
				.position(|binder| binder.id == constraint.parameter)
				.map(|index| interner.mk_param(crate::ParamIdx(index as u32)))
				.unwrap_or_else(|| interner.error());
			let interface = defs
				.by_stable(interface_id)
				.ok_or_else(|| EnvironmentConstructionError::MissingKnownIdentity(interface_id.clone()))?;
			let mut args = constraint
				.positional
				.iter()
				.enumerate()
				.map(|(index, arg)| (index.to_string().into(), recovered_ty(interner, arg, &ctx)))
				.collect::<Vec<_>>();
			args.extend(
				constraint
					.named
					.iter()
					.map(|(name, arg)| (name.clone(), recovered_ty(interner, arg, &ctx))),
			);
			Ok(Bound {
				ty,
				interface,
				args,
			})
		})
		.collect()
}

fn instantiate_recovered_definition(
	facts: &mut ImportedFacts,
	interner: &mut Interner,
	definition: &RecoveredExportedDefinition,
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<(), EnvironmentConstructionError> {
	let def = facts
		.defs
		.by_stable(&definition.id)
		.ok_or_else(|| EnvironmentConstructionError::MissingKnownIdentity(definition.id.clone()))?;
	let ctx = context(definitions, &definition.binders);
	let generics = definition
		.binders
		.iter()
		.map(|b| b.name.clone())
		.collect::<Vec<_>>();
	let owned_members = definition
		.members
		.iter()
		.filter(|member| member.visibility != Some(nymph_ast::decl::Visibility::Private))
		.map(|member| {
			recovered_owned_member(
				&facts.defs,
				interner,
				&definition.binders,
				member,
				definitions,
			)
		})
		.collect::<Result<Vec<_>, _>>()?;
	facts.definition_members.insert(def, owned_members);
	match definition.kind {
		DefinitionShapeKind::Let => {
			let ty = match &definition.ty {
				Some(ty) => recovered_ty(interner, ty, &ctx),
				None => interner.error(),
			};
			let recovered_bounds = recovered_bounds(
				&facts.defs,
				interner,
				&definition.binders,
				&definition.constraints,
				definitions,
			)?;
			facts.signatures.lets.insert(
				def,
				ValueSig {
					generics,
					ty,
					bounds: recovered_bounds,
				},
			);
		}
		DefinitionShapeKind::TypeAlias => {
			let target = match &definition.ty {
				Some(ty) => recovered_ty(interner, ty, &ctx),
				None => interner.error(),
			};
			facts.signatures.aliases.insert(
				def,
				AliasSig {
					generics,
					target,
					bounds: recovered_bounds(
						&facts.defs,
						interner,
						&definition.binders,
						&definition.constraints,
						definitions,
					)?,
				},
			);
		}
		DefinitionShapeKind::Struct => {
			facts.signatures.structs.insert(
				def,
				StructSig {
					generics,
					fields: definition
						.fields
						.iter()
						.filter(|field| field.visibility != Some(nymph_ast::decl::Visibility::Private))
						.map(|f| (f.name.clone(), recovered_ty(interner, &f.ty, &ctx)))
						.collect(),
					field_metadata: definition
						.fields
						.iter()
						.filter(|field| field.visibility != Some(nymph_ast::decl::Visibility::Private))
						.map(|field| FieldSigMetadata {
							target: Some(field.id.clone()),
							mutable: field.mutable,
							has_default: field.has_default,
						})
						.collect(),
					bounds: recovered_bounds(
						&facts.defs,
						interner,
						&definition.binders,
						&definition.constraints,
						definitions,
					)?,
				},
			);
		}
		DefinitionShapeKind::Enum => {
			facts.signatures.enums.insert(
				def,
				EnumSig {
					generics,
					variants: definition
						.variants
						.iter()
						.map(|v| VariantSig {
							target: Some(v.id.clone()),
							name: v.name.clone(),
							fields: v
								.fields
								.iter()
								.map(|f| (f.name.clone(), recovered_ty(interner, &f.ty, &ctx)))
								.collect(),
							field_metadata: v
								.fields
								.iter()
								.map(|field| FieldSigMetadata {
									target: Some(field.id.clone()),
									mutable: field.mutable,
									has_default: field.has_default,
								})
								.collect(),
						})
						.collect(),
					bounds: recovered_bounds(
						&facts.defs,
						interner,
						&definition.binders,
						&definition.constraints,
						definitions,
					)?,
				},
			);
		}
		DefinitionShapeKind::Function => {
			let params = definition
				.parameters
				.iter()
				.map(|p| FuncParamSig {
					label: p.name.clone(),
					ty: recovered_ty(interner, &p.ty, &ctx),
					spread: p.spread,
				})
				.collect();
			let ret = match &definition.return_type {
				Some(ty) => recovered_ty(interner, ty, &ctx),
				None => interner.error(),
			};
			facts.signatures.funcs.insert(
				def,
				FuncSig {
					generics,
					params,
					ret,
					has_self: false,
					bounds: recovered_bounds(
						&facts.defs,
						interner,
						&definition.binders,
						&definition.constraints,
						definitions,
					)?,
				},
			);
		}
		DefinitionShapeKind::Namespace => {
			let mut namespace = NamespaceSig::default();
			for (shape, member) in definition
				.members
				.iter()
				.filter(|member| member.visibility != Some(nymph_ast::decl::Visibility::Private))
				.zip(&facts.definition_members[&def])
			{
				let target = Some(member.target.clone());
				let value = match member.kind {
					MemberKind::Function | MemberKind::MutatingFunction | MemberKind::StaticFunction => {
						NamespaceMemberSig::Func {
							target,
							sig: FuncSig {
								generics: member.generics.clone(),
								params: member.params.clone(),
								ret: member.ret,
								has_self: false,
								bounds: member.bounds.clone(),
							},
						}
					}
					_ => NamespaceMemberSig::Value {
						target,
						ty: member.ret,
						mutable: matches!(member.kind, MemberKind::MutableValue),
					},
				};
				namespace.members.insert(shape.name.clone(), value);
			}
			facts.signatures.namespaces.insert(def, namespace);
		}
		DefinitionShapeKind::Interface => {
			let visible = definition
				.members
				.iter()
				.filter(|member| member.visibility != Some(nymph_ast::decl::Visibility::Private))
				.zip(&facts.definition_members[&def]);
			let mut interface = InterfaceDef {
				generics,
				runtime_members: visible
					.clone()
					.map(|(shape, member)| crate::iface::RuntimeMemberDef {
						definition: Some(member.target.clone()),
						name: shape.name.clone(),
						kind: member.kind,
						has_default: member.has_default,
						external: shape.external.is_some(),
						marshal: shape.external.as_ref().and_then(|abi| abi.marshal),
					})
					.collect(),
				methods: FxHashMap::default(),
			};
			for (shape, member) in visible {
				if matches!(
					member.kind,
					MemberKind::Function | MemberKind::MutatingFunction
				) {
					interface.methods.insert(
						shape.name.clone(),
						crate::iface::IfaceMethod {
							definition: Some(member.target.clone()),
							has_default: member.has_default,
							params: member.params.iter().map(|p| p.ty).collect(),
							ret: member.ret,
							generics: shape.binders.iter().map(|b| b.name.clone()).collect(),
							bounds: member.bounds.clone(),
							mutating: matches!(member.kind, MemberKind::MutatingFunction),
						},
					);
				}
			}
			facts.interfaces.insert(def, interface);
		}
	}
	if let Some(external) = &definition.external {
		facts.external_abis.insert(def, external.clone());
	}
	if definition.availability == crate::SemanticAvailability::Available
		&& matches!(
			definition.kind,
			DefinitionShapeKind::Struct | DefinitionShapeKind::Enum
		) {
		let positional = definition
			.binders
			.iter()
			.enumerate()
			.map(|(index, _)| interner.mk_param(crate::ParamIdx(index as u32)))
			.collect();
		let self_ty = interner.mk_adt(def, crate::GenericArgs::new(positional, vec![]));
		let methods = definition
			.members
			.iter()
			.filter(|member| {
				member.visibility != Some(nymph_ast::decl::Visibility::Private)
					&& matches!(
						member.kind,
						MemberKind::Function | MemberKind::MutatingFunction | MemberKind::StaticFunction
					)
			})
			.map(|member| {
				let owned = recovered_owned_member(
					&facts.defs,
					interner,
					&definition.binders,
					member,
					definitions,
				)?;
				Ok((
					member.name.clone(),
					InherentMethod {
						definition: Some(owned.target),
						local_span: None,
						generic_names: member
							.binders
							.iter()
							.map(|binder| binder.name.clone())
							.collect(),
						params: owned
							.params
							.into_iter()
							.map(|parameter| parameter.ty)
							.collect(),
						ret: owned.ret,
						bounds: owned.bounds,
						namespaced: matches!(
							member.kind,
							MemberKind::StaticFunction | MemberKind::StaticValue
						),
						mutating: matches!(member.kind, MemberKind::MutatingFunction),
						external: member.external.is_some(),
					},
				))
			})
			.collect::<Result<FxHashMap<_, _>, EnvironmentConstructionError>>()?;
		if !methods.is_empty() {
			let constraints = recovered_bounds(
				&facts.defs,
				interner,
				&definition.binders,
				&definition.constraints,
				definitions,
			)?;
			facts.inherent.add(
				head_of(interner, self_ty),
				InherentImpl {
					definition: Some(definition.id.clone()),
					owner_generic_names: definition
						.binders
						.iter()
						.map(|binder| binder.name.clone())
						.collect(),
					self_ty,
					methods,
					constraints,
					imported: true,
				},
			);
		}
	}
	Ok(())
}

fn instantiate_recovered_impl(
	facts: &mut ImportedFacts,
	interner: &mut Interner,
	implementation: &RecoveredExportedImpl,
	definitions: &Arc<HashMap<DefinitionId, DefId>>,
) -> Result<(), EnvironmentConstructionError> {
	if implementation.availability == crate::SemanticAvailability::StructureUnavailable
		|| matches!(implementation.self_type, RecoveredInterfaceType::Poison)
		|| matches!(
			implementation.interface,
			Some(RecoveredDefinitionReference::Poison)
		) || implementation.constraints.iter().any(|constraint| {
		matches!(constraint.interface, RecoveredDefinitionReference::Poison)
			|| constraint
				.positional
				.iter()
				.any(|ty| matches!(ty, RecoveredInterfaceType::Poison))
			|| constraint
				.named
				.iter()
				.any(|(_, ty)| matches!(ty, RecoveredInterfaceType::Poison))
	}) {
		return Ok(());
	}
	let ctx = context(definitions, &implementation.binders);
	let self_ty = recovered_ty(interner, &implementation.self_type, &ctx);
	let constraints = recovered_bounds(
		&facts.defs,
		interner,
		&implementation.binders,
		&implementation.constraints,
		definitions,
	)?;
	let methods = implementation
		.members
		.iter()
		.filter(|member| {
			member.visibility != Some(nymph_ast::decl::Visibility::Private)
				&& matches!(
					member.kind,
					MemberKind::Function | MemberKind::MutatingFunction | MemberKind::StaticFunction
				)
		})
		.map(|member| {
			let owned = recovered_owned_member(
				&facts.defs,
				interner,
				&implementation.binders,
				member,
				definitions,
			)?;
			Ok((
				member.name.clone(),
				crate::iface::IfaceMethod {
					definition: Some(owned.target),
					has_default: member.has_default,
					params: owned
						.params
						.into_iter()
						.map(|parameter| parameter.ty)
						.collect(),
					ret: owned.ret,
					generics: member
						.binders
						.iter()
						.map(|binder| binder.name.clone())
						.collect(),
					bounds: owned.bounds,
					mutating: matches!(member.kind, MemberKind::MutatingFunction),
				},
			))
		})
		.collect::<Result<FxHashMap<_, _>, EnvironmentConstructionError>>()?;
	match &implementation.interface {
		Some(RecoveredDefinitionReference::Known(interface_id)) => {
			let interface = facts
				.defs
				.by_stable(interface_id)
				.ok_or_else(|| EnvironmentConstructionError::MissingKnownIdentity(interface_id.clone()))?;
			let args = implementation
				.interface_arguments
				.iter()
				.map(|(name, ty)| (name.clone(), recovered_ty(interner, ty, &ctx)))
				.collect();
			let blanket = matches!(interner.kind(self_ty), crate::TyKind::Param(_));
			facts.implementations.add(
				interner,
				ImplDef {
					definition: Some(implementation.id.clone()),
					member_catalog: implementation.member_slots.clone(),
					runtime_members: implementation
						.members
						.iter()
						.map(|member| crate::iface::RuntimeMemberDef {
							definition: Some(member.id.clone()),
							name: member.name.clone(),
							kind: member.kind,
							has_default: member.has_default,
							external: member.external.is_some(),
							marshal: member.external.as_ref().and_then(|abi| abi.marshal),
						})
						.collect(),
					generics: implementation
						.binders
						.iter()
						.map(|b| b.name.clone())
						.collect(),
					self_ty,
					interface,
					legacy_span: None,
					args,
					methods,
					constraints,
					blanket,
				},
			);
		}
		None => {
			let inherent = methods
				.into_iter()
				.map(|(name, method)| {
					(
						name.clone(),
						InherentMethod {
							definition: method.definition,
							local_span: None,
							generic_names: method.generics,
							params: method.params,
							ret: method.ret,
							bounds: method.bounds,
							namespaced: implementation
								.members
								.iter()
								.find(|member| member.name == name)
								.is_some_and(|member| {
									matches!(
										member.kind,
										MemberKind::StaticFunction | MemberKind::StaticValue
									)
								}),
							mutating: method.mutating,
							external: implementation
								.members
								.iter()
								.find(|member| member.name == name)
								.is_some_and(|member| member.external.is_some()),
						},
					)
				})
				.collect();
			facts.inherent.add(
				head_of(interner, self_ty),
				InherentImpl {
					definition: Some(implementation.id.clone()),
					owner_generic_names: implementation
						.binders
						.iter()
						.map(|b| b.name.clone())
						.collect(),
					self_ty,
					methods: inherent,
					constraints,
					imported: true,
				},
			);
		}
		Some(RecoveredDefinitionReference::Poison) => unreachable!(),
	}
	Ok(())
}
