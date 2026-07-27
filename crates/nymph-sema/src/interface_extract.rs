//! Conversion from owned checker facts to stable, diagnostic-free interfaces.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;

use ecow::EcoString;
use nymph_ast::{
	decl::{Declaration, FuncDeclaration, FuncKind, ImplMember, Module, Visibility},
	expr::Pattern,
	ty::Type,
};

use crate::{
	BinderScope, CanonicalizationContext, Checked, DeclarationCategory, DeclarationKey, DefinitionId,
	DefinitionShapeKind, ExportedDefinition, ExportedImpl, ExternalAbi, FieldShape, GenericParameter,
	GenericParameterId, HeaderBinder, HeaderParameterId, HeaderType, ImplementationHeader,
	InterfaceConversionError, InterfaceType, MemberKind, MemberShape, ModuleEnvironment,
	ModuleIdentity, ModuleInterface, ParameterShape, RecoveredExportedDefinition,
	RecoveredExportedImpl, RecoveredInterfaceType, RecoveredModuleInterface, SemanticAvailability,
	StableIdBuilder, SuperInterfaceShape, SupportDefinition, VariantShape, canonicalize_type,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct DeclaredHeaders {
	pub module: ModuleIdentity,
	pub definitions: Vec<(EcoString, DefinitionId)>,
	/// Exact checker-visible names, including compatibility rewrite prefixes.
	/// This is deliberately separate from source headers: imported definitions
	/// may share a source name with an item owned by this module.
	pub checked_definitions: Vec<(EcoString, DefinitionId)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionFactSelection {
	pub implementations: Range<usize>,
	pub inherent: Range<usize>,
}

impl ExtractionFactSelection {
	#[must_use]
	pub fn current_module(module: &Module, checked: &Checked) -> Self {
		if checked.semantic.has_explicit_local_ranges {
			return Self {
				implementations: checked.semantic.local_implementations.clone(),
				inherent: checked.semantic.local_inherent.clone(),
			};
		}
		let current_implementations = checked
			.semantic
			.implementations
			.impls
			.iter()
			.enumerate()
			.filter(|(_, implementation)| {
				implementation
					.legacy_span
					.is_some_and(|span| span.start < crate::prelude::SPAN_BASE)
			})
			.map(|(index, _)| index)
			.collect::<Vec<_>>();
		let inherent = module
			.members
			.iter()
			.filter(|declaration| {
				matches!(
					declaration,
					Declaration::Struct { .. } | Declaration::Enum { .. } | Declaration::Impl { .. }
				)
			})
			.count();
		let implementations = match (
			current_implementations.first(),
			current_implementations.last(),
		) {
			(Some(first), Some(last)) => *first..last + 1,
			(None, None) => 0..0,
			_ => unreachable!(),
		};
		let inherent_end = checked.semantic.inherent.len();
		Self {
			implementations,
			inherent: inherent_end.saturating_sub(inherent)..inherent_end,
		}
	}

	fn all(checked: &Checked) -> Self {
		Self {
			implementations: 0..checked.semantic.implementations.impls.len(),
			inherent: 0..checked.semantic.inherent.len(),
		}
	}
}

impl DeclaredHeaders {
	fn id(&self, name: &str) -> Option<DefinitionId> {
		self
			.checked_definitions
			.iter()
			.find(|(n, _)| n == name)
			.or_else(|| self.definitions.iter().find(|(n, _)| n == name))
			.map(|(_, id)| id.clone())
	}
}

impl DeclaredHeaders {
	#[must_use]
	pub fn with_checked_definitions(
		mut self,
		checked_definitions: Vec<(EcoString, DefinitionId)>,
	) -> Self {
		self.checked_definitions = checked_definitions;
		self
	}
}

fn source_name(name: &str) -> &str {
	name
		.strip_prefix("$m")
		.and_then(|rest| rest.split_once('$').map(|(_, name)| name))
		.unwrap_or(name)
}

pub fn declared_headers(identity: ModuleIdentity, module: &Module) -> DeclaredHeaders {
	let mut ids = StableIdBuilder::new(identity.clone());
	let definitions: Vec<(EcoString, DefinitionId)> = module
		.members
		.iter()
		.filter_map(|declaration| {
			let (category, name) = declaration_identity(declaration)?;
			let source_name: EcoString = source_name(name).into();
			Some((
				source_name.clone(),
				ids.allocate(DeclarationKey::top_level(category, source_name)),
			))
		})
		.collect();
	DeclaredHeaders {
		module: identity,
		checked_definitions: definitions.clone(),
		definitions,
	}
}

fn declaration_identity(declaration: &Declaration) -> Option<(DeclarationCategory, &EcoString)> {
	Some(match declaration {
		Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
			(DeclarationCategory::Function, &meta.name.0)
		}
		Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
			let Pattern::Binding { name, .. } = &meta.name.0 else {
				return None;
			};
			(DeclarationCategory::Let, &name.0)
		}
		Declaration::TypeAlias { meta, .. } => (DeclarationCategory::TypeAlias, &meta.name.0),
		Declaration::Struct { name, .. } => (DeclarationCategory::Struct, &name.0),
		Declaration::Enum { name, .. } => (DeclarationCategory::Enum, &name.0),
		Declaration::Interface { name, .. } => (DeclarationCategory::Interface, &name.0),
		Declaration::Namespace { name, .. } => (DeclarationCategory::Namespace, &name.0),
		Declaration::Import { .. } | Declaration::Impl { .. } | Declaration::ImplFor { .. } => {
			return None;
		}
	})
}

fn visible(visibility: Option<Visibility>) -> bool {
	!matches!(visibility, Some(Visibility::Private))
}

pub(crate) fn external_function_abi(marker: &EcoString) -> ExternalAbi {
	external_function_abi_for_receiver(marker, None)
}

fn external_function_abi_for_receiver(
	marker: &EcoString,
	receiver_tag: Option<&str>,
) -> ExternalAbi {
	let linked = nymph_hir::linkage::lookup(marker, receiver_tag);
	ExternalAbi {
		marker: marker.clone(),
		module: linked.map(|linkage| linkage.module.into()),
		symbol: linked.map(|linkage| linkage.symbol.into()),
		marshal: None,
	}
}

fn implementation_receiver_tag(owner: &DefinitionId) -> Option<&'static str> {
	let DeclarationKey::Implementation { header, .. } = &owner.key else {
		return None;
	};
	let mut mutable = header.mutable;
	let mut self_type = &header.self_type;
	while let HeaderType::Mutable(inner) = self_type {
		mutable = true;
		self_type = inner;
	}
	let base = match self_type {
		HeaderType::List(_) => "list",
		HeaderType::Map(_, _) => "map",
		HeaderType::Int => "int",
		HeaderType::UInt => "uint",
		HeaderType::Float => "float",
		HeaderType::Char => "char",
		HeaderType::String => "string",
		HeaderType::Boolean => "boolean",
		_ => return None,
	};
	match (mutable, base) {
		(true, "list") => Some("mut_list"),
		(true, "map") => Some("mut_map"),
		_ => Some(base),
	}
}

pub(crate) fn external_value_abi(
	marker: &EcoString,
	marshal: Option<nymph_hir::hir::MarshalKind>,
) -> ExternalAbi {
	let linked = nymph_hir::linkage::lookup_value(marker).ok();
	ExternalAbi {
		marker: marker.clone(),
		module: linked.map(|linkage| linkage.linked.module.into()),
		symbol: linked.map(|linkage| linkage.linked.symbol.into()),
		marshal,
	}
}

fn empty_definition(
	id: DefinitionId,
	name: EcoString,
	visibility: Option<Visibility>,
	kind: DefinitionShapeKind,
) -> ExportedDefinition {
	ExportedDefinition {
		id,
		name,
		visibility,
		kind,
		binders: Vec::new(),
		constraints: Vec::new(),
		parameters: Vec::new(),
		return_type: None,
		ty: None,
		fields: Vec::new(),
		variants: Vec::new(),
		members: Vec::new(),
		super_interfaces: Vec::new(),
		external: None,
		runtime_owner: None,
	}
}

fn context(checked: &Checked, headers: &DeclaredHeaders) -> CanonicalizationContext {
	let definitions = checked
		.semantic
		.definitions
		.defs
		.iter()
		.enumerate()
		.filter_map(|(index, data)| {
			data
				.stable
				.clone()
				.or_else(|| headers.id(&data.name))
				.map(|stable| (crate::DefId(index as u32), stable))
		})
		.collect::<HashMap<_, _>>();
	CanonicalizationContext::new(definitions, HashMap::new())
}

fn definition_context(
	checked: &Checked,
	headers: &DeclaredHeaders,
	id: &DefinitionId,
	generic_names: impl IntoIterator<Item = EcoString>,
) -> (CanonicalizationContext, Vec<GenericParameter>) {
	let definitions = checked
		.semantic
		.definitions
		.defs
		.iter()
		.enumerate()
		.filter_map(|(index, data)| {
			data
				.stable
				.clone()
				.or_else(|| headers.id(&data.name))
				.map(|stable| (crate::DefId(index as u32), stable))
		})
		.collect();
	let binders = generic_names
		.into_iter()
		.enumerate()
		.map(|(index, name)| GenericParameter {
			id: GenericParameterId::new(id.binder(BinderScope::Definition, 0), index as u32),
			name,
		})
		.collect::<Vec<_>>();
	let parameters = binders
		.iter()
		.enumerate()
		.map(|(index, binder)| (crate::ParamIdx(index as u32), binder.id.clone()))
		.collect();
	(
		CanonicalizationContext::new(definitions, parameters),
		binders,
	)
}

fn ast_type(
	ty: &Type,
	headers: &DeclaredHeaders,
	binders: &[GenericParameter],
) -> Result<InterfaceType, InterfaceConversionError> {
	Ok(match ty {
		Type::Int => InterfaceType::Int,
		Type::UInt => InterfaceType::UInt,
		Type::Float => InterfaceType::Float,
		Type::Char => InterfaceType::Char,
		Type::String => InterfaceType::String,
		Type::Boolean => InterfaceType::Boolean,
		Type::Void => InterfaceType::Void,
		Type::Never => InterfaceType::Never,
		Type::SelfType => InterfaceType::SelfType,
		Type::Infer => return Err(InterfaceConversionError::ErrorType),
		Type::Grouped(inner) => ast_type(&inner.0, headers, binders)?,
		Type::List(inner) => InterfaceType::List(Box::new(ast_type(&inner.0, headers, binders)?)),
		Type::Mut(inner) => InterfaceType::Mutable(Box::new(ast_type(&inner.0, headers, binders)?)),
		Type::Tuple(items) => InterfaceType::Tuple(
			items
				.iter()
				.map(|t| ast_type(&t.0, headers, binders))
				.collect::<Result<_, _>>()?,
		),
		Type::Map(key, value) => InterfaceType::Map(
			Box::new(ast_type(&key.0, headers, binders)?),
			Box::new(ast_type(&value.0, headers, binders)?),
		),
		Type::Function {
			params,
			return_type,
		} => InterfaceType::Function {
			parameters: params
				.iter()
				.map(|(_, t)| ast_type(&t.0, headers, binders))
				.collect::<Result<_, _>>()?,
			return_type: Box::new(ast_type(&return_type.0, headers, binders)?),
		},
		Type::Intersection(a, b) => InterfaceType::Intersection(vec![
			ast_type(&a.0, headers, binders)?,
			ast_type(&b.0, headers, binders)?,
		]),
		Type::Reference { name, generics } => {
			if let Some(parameter) = binders.iter().find(|parameter| parameter.name == name.0) {
				InterfaceType::Generic(parameter.id.clone())
			} else {
				let definition = headers.id(&name.0).ok_or_else(|| {
					InterfaceConversionError::UnknownStableDefinition(DefinitionId::new(
						headers.module.clone(),
						DeclarationKey::top_level(DeclarationCategory::TypeAlias, name.0.clone()),
					))
				})?;
				let mut positional = Vec::new();
				let mut named = Vec::new();
				for argument in generics {
					let value = ast_type(&argument.0.value.0, headers, binders)?;
					if let Some(name) = &argument.0.name {
						named.push((name.0.clone(), value));
					} else {
						positional.push(value);
					}
				}
				InterfaceType::Named {
					definition,
					positional,
					named,
				}
			}
		}
	})
}

fn generic_constraints(
	generics: &[nymph_ast::Spanned<nymph_ast::ty::GenericParam>],
	headers: &DeclaredHeaders,
	binders: &[GenericParameter],
) -> Result<Vec<crate::GenericConstraint>, InterfaceConversionError> {
	let mut constraints = Vec::new();
	for (generic, binder) in generics.iter().zip(binders) {
		let Some(bound) = &generic.0.constraint else {
			continue;
		};
		let InterfaceType::Named {
			definition,
			positional,
			named,
		} = ast_type(&bound.0, headers, binders)?
		else {
			continue;
		};
		constraints.push(crate::ConstraintShape {
			parameter: binder.id.clone(),
			interface: definition,
			positional,
			named,
		});
	}
	Ok(constraints)
}

fn checked_constraints(
	bounds: &[crate::iface::Bound],
	checked: &Checked,
	headers: &DeclaredHeaders,
	context: &CanonicalizationContext,
) -> Result<Vec<crate::GenericConstraint>, InterfaceConversionError> {
	bounds
		.iter()
		.map(|bound| {
			let InterfaceType::Generic(parameter) =
				canonicalize_type(&checked.interner, bound.ty, context)?
			else {
				return Err(InterfaceConversionError::ErrorType);
			};
			Ok(crate::ConstraintShape {
				parameter,
				interface: checked
					.semantic
					.definitions
					.data(bound.interface)
					.stable
					.clone()
					.or_else(|| headers.id(&checked.semantic.definitions.data(bound.interface).name))
					.ok_or(InterfaceConversionError::UnknownDefinition(bound.interface))?,
				positional: Vec::new(),
				named: bound
					.args
					.iter()
					.map(|(name, ty)| {
						Ok((
							name.clone(),
							canonicalize_type(&checked.interner, *ty, context)?,
						))
					})
					.collect::<Result<_, InterfaceConversionError>>()?,
			})
		})
		.collect()
}

fn checked_member_shape(
	meta: &FuncDeclaration,
	visibility: Option<Visibility>,
	external_symbol: Option<&EcoString>,
	facts: &crate::annotate::CheckedMethod,
	owner: &DefinitionId,
	owner_binders: &[GenericParameter],
	checked: &Checked,
	headers: &DeclaredHeaders,
	ids: &mut StableIdBuilder,
) -> Result<MemberShape<InterfaceType>, InterfaceConversionError> {
	let id = ids.allocate(DeclarationKey::member(
		owner.clone(),
		DeclarationCategory::Method,
		meta.name.0.clone(),
	));
	let mut binders = meta
		.generics
		.iter()
		.enumerate()
		.map(|(index, generic)| GenericParameter {
			id: GenericParameterId::new(id.binder(BinderScope::Member, 0), index as u32),
			name: generic.0.name.0.clone(),
		})
		.collect::<Vec<_>>();
	let mut anonymous = HashSet::new();
	for ty in facts.params.iter().chain(std::iter::once(&facts.ret)) {
		collect_anonymous_parameters(&checked.interner, *ty, &mut anonymous);
	}
	let mut anonymous = anonymous.into_iter().collect::<Vec<_>>();
	anonymous.sort_by_key(|parameter| parameter.0);
	for (index, parameter) in anonymous.iter().enumerate() {
		binders.push(GenericParameter {
			id: GenericParameterId::new(
				id.binder(BinderScope::Member, 0),
				(meta.generics.len() + index) as u32,
			),
			name: format!("$anonymous{}", parameter.0).into(),
		});
	}
	let parameters = owner_binders
		.iter()
		.chain(binders.iter().take(meta.generics.len()))
		.enumerate()
		.map(|(index, binder)| (crate::ParamIdx(index as u32), binder.id.clone()))
		.chain(
			anonymous
				.iter()
				.zip(binders.iter().skip(meta.generics.len()))
				.map(|(parameter, binder)| (*parameter, binder.id.clone())),
		)
		.collect();
	let definitions = context(checked, headers).definitions();
	let member_context = CanonicalizationContext::new(definitions, parameters);
	let mut bounds = facts.bounds.clone();
	for parameter in &anonymous {
		bounds.extend(
			checked
				.semantic
				.anonymous_bounds
				.get(parameter)
				.cloned()
				.unwrap_or_default(),
		);
	}
	Ok(MemberShape {
		id: facts.definition.clone().unwrap_or(id),
		name: meta.name.0.clone(),
		visibility,
		kind: match meta.kind {
			FuncKind::Instance => MemberKind::Function,
			FuncKind::Mut => MemberKind::MutatingFunction,
			FuncKind::Namespace => MemberKind::StaticFunction,
		},
		binders,
		constraints: checked_constraints(&bounds, checked, headers, &member_context)?,
		parameters: facts
			.params
			.iter()
			.zip(&meta.params)
			.map(|(ty, parameter)| {
				Ok(ParameterShape {
					name: match &parameter.0.name.0 {
						Pattern::Binding { name, .. } => Some(name.0.clone()),
						_ => None,
					},
					ty: canonicalize_type(&checked.interner, *ty, &member_context)?,
					mutable: parameter.0.mutable,
					spread: parameter.0.spread,
				})
			})
			.collect::<Result<_, InterfaceConversionError>>()?,
		return_type: canonicalize_type(&checked.interner, facts.ret, &member_context)?,
		external: external_symbol
			.map(|symbol| external_function_abi_for_receiver(symbol, implementation_receiver_tag(owner))),
		runtime_owner: Some(owner.clone()),
		has_default: false,
	})
}

fn collect_anonymous_parameters(
	interner: &nymph_hir::ty::Interner,
	ty: crate::Ty,
	parameters: &mut HashSet<crate::ParamIdx>,
) {
	use nymph_hir::ty::TyKind;
	match interner.kind(ty) {
		TyKind::Param(parameter) if parameter.0 >= 1 << 28 => {
			parameters.insert(*parameter);
		}
		TyKind::List(inner) | TyKind::Mut(inner) => {
			collect_anonymous_parameters(interner, *inner, parameters);
		}
		TyKind::Tuple(items) | TyKind::Intersection(items) => {
			for item in items {
				collect_anonymous_parameters(interner, *item, parameters);
			}
		}
		TyKind::Map(key, value) => {
			collect_anonymous_parameters(interner, *key, parameters);
			collect_anonymous_parameters(interner, *value, parameters);
		}
		TyKind::Fn { params, ret } => {
			for parameter in params {
				collect_anonymous_parameters(interner, *parameter, parameters);
			}
			collect_anonymous_parameters(interner, *ret, parameters);
		}
		TyKind::Adt(_, arguments) => {
			for argument in &arguments.positional {
				collect_anonymous_parameters(interner, *argument, parameters);
			}
			for (_, argument) in &arguments.named {
				collect_anonymous_parameters(interner, *argument, parameters);
			}
		}
		_ => {}
	}
}

fn definition_anonymous_context(
	owner: &DefinitionId,
	mut binders: Vec<GenericParameter>,
	types: impl IntoIterator<Item = crate::Ty>,
	checked: &Checked,
	headers: &DeclaredHeaders,
) -> Result<
	(
		CanonicalizationContext,
		Vec<GenericParameter>,
		Vec<crate::GenericConstraint>,
	),
	InterfaceConversionError,
> {
	let mut anonymous = HashSet::new();
	for ty in types {
		collect_anonymous_parameters(&checked.interner, ty, &mut anonymous);
	}
	let mut anonymous = anonymous.into_iter().collect::<Vec<_>>();
	anonymous.sort_by_key(|parameter| parameter.0);
	// A synthetic parameter is meaningful only with the checked bound facts that
	// created it. Leaving an unowned reference in the context deliberately keeps
	// conversion strict instead of fabricating an unconstrained public generic.
	anonymous.retain(|parameter| checked.semantic.anonymous_bounds.contains_key(parameter));
	let declared_len = binders.len();
	for (index, parameter) in anonymous.iter().enumerate() {
		binders.push(GenericParameter {
			id: GenericParameterId::new(
				owner.binder(BinderScope::Definition, 0),
				(declared_len + index) as u32,
			),
			name: format!("$anonymous{}", parameter.0).into(),
		});
	}
	let parameters = binders
		.iter()
		.take(declared_len)
		.enumerate()
		.map(|(index, binder)| (crate::ParamIdx(index as u32), binder.id.clone()))
		.chain(
			anonymous
				.iter()
				.zip(binders.iter().skip(declared_len))
				.map(|(parameter, binder)| (*parameter, binder.id.clone())),
		)
		.collect();
	let context = CanonicalizationContext::new(context(checked, headers).definitions(), parameters);
	let bounds = anonymous
		.iter()
		.flat_map(|parameter| checked.semantic.anonymous_bounds[parameter].iter().cloned())
		.collect::<Vec<_>>();
	let constraints = checked_constraints(&bounds, checked, headers, &context)?;
	Ok((context, binders, constraints))
}

fn member_shape(
	member: &ImplMember,
	owner: &DefinitionId,
	headers: &DeclaredHeaders,
	binders: &[GenericParameter],
	ids: &mut StableIdBuilder,
) -> Result<MemberShape<InterfaceType>, InterfaceConversionError> {
	let (visibility, name, kind, parameters, return_type, external, has_default) = match member {
		ImplMember::Func {
			visibility, meta, ..
		}
		| ImplMember::ExternalFunc(visibility, _, meta) => {
			let parameters = meta
				.params
				.iter()
				.map(|p| {
					Ok(ParameterShape {
						name: match &p.0.name.0 {
							Pattern::Binding { name, .. } => Some(name.0.clone()),
							_ => None,
						},
						ty: ast_type(&p.0.type_.0, headers, binders)?,
						mutable: p.0.mutable,
						spread: p.0.spread,
					})
				})
				.collect::<Result<_, InterfaceConversionError>>()?;
			let ret = meta
				.return_type
				.as_ref()
				.map(|t| ast_type(&t.0, headers, binders))
				.transpose()?
				.unwrap_or(InterfaceType::Void);
			let external = match member {
				ImplMember::ExternalFunc(_, symbol, _) => Some(external_function_abi_for_receiver(
					symbol,
					implementation_receiver_tag(owner),
				)),
				_ => None,
			};
			(
				*visibility,
				meta.name.0.clone(),
				match meta.kind {
					FuncKind::Instance => MemberKind::Function,
					FuncKind::Mut => MemberKind::MutatingFunction,
					FuncKind::Namespace => MemberKind::StaticFunction,
				},
				parameters,
				ret,
				external,
				true,
			)
		}
		ImplMember::Let {
			visibility, meta, ..
		}
		| ImplMember::ExternalLet(visibility, _, meta) => {
			let Pattern::Binding { name, .. } = &meta.name.0 else {
				return Err(InterfaceConversionError::ErrorType);
			};
			let ty = meta
				.type_
				.as_ref()
				.map(|t| ast_type(&t.0, headers, binders))
				.transpose()?
				.unwrap_or(InterfaceType::Void);
			let external = match member {
				ImplMember::ExternalLet(_, symbol, _) => Some(external_value_abi(symbol, None)),
				_ => None,
			};
			(
				*visibility,
				name.0.clone(),
				match meta.kind {
					nymph_ast::decl::LetKind::Instance => MemberKind::Value,
					nymph_ast::decl::LetKind::Mut => MemberKind::MutableValue,
					nymph_ast::decl::LetKind::Namespace => MemberKind::StaticValue,
				},
				Vec::new(),
				ty,
				external,
				true,
			)
		}
	};
	Ok(MemberShape {
		id: ids.allocate(DeclarationKey::member(
			owner.clone(),
			DeclarationCategory::Method,
			name.clone(),
		)),
		name,
		visibility,
		kind,
		binders: Vec::new(),
		constraints: Vec::new(),
		parameters,
		return_type,
		external,
		runtime_owner: Some(owner.clone()),
		has_default,
	})
}

fn definition_members(
	members: &[nymph_ast::Spanned<ImplMember>],
	def: crate::DefId,
	owner: &DefinitionId,
	owner_binders: &[GenericParameter],
	checked: &Checked,
	headers: &DeclaredHeaders,
	ids: &mut StableIdBuilder,
) -> Result<Vec<MemberShape<InterfaceType>>, InterfaceConversionError> {
	let facts = checked.semantic.inherent.iter().find(|implementation| {
		matches!(
			checked.interner.kind(implementation.self_ty),
			nymph_hir::ty::TyKind::Adt(found, _) if *found == def
		)
	});
	members
		.iter()
		.map(|member| match &member.0 {
			ImplMember::Func {
				visibility, meta, ..
			} => checked_member_shape(
				meta,
				*visibility,
				None,
				&facts.expect("ADT inherent facts").methods[&meta.name.0],
				owner,
				owner_binders,
				checked,
				headers,
				ids,
			),
			ImplMember::ExternalFunc(visibility, symbol, meta) => checked_member_shape(
				meta,
				*visibility,
				Some(symbol),
				&facts.expect("ADT inherent facts").methods[&meta.name.0],
				owner,
				owner_binders,
				checked,
				headers,
				ids,
			),
			_ => member_shape(&member.0, owner, headers, owner_binders, ids),
		})
		.collect()
}

fn header_type(ty: &InterfaceType, binders: &[GenericParameter]) -> HeaderType {
	match ty {
		InterfaceType::Int => HeaderType::Int,
		InterfaceType::UInt => HeaderType::UInt,
		InterfaceType::Float => HeaderType::Float,
		InterfaceType::Char => HeaderType::Char,
		InterfaceType::String => HeaderType::String,
		InterfaceType::Boolean => HeaderType::Boolean,
		InterfaceType::Void => HeaderType::Void,
		InterfaceType::Never => HeaderType::Never,
		InterfaceType::SelfType => HeaderType::SelfType,
		InterfaceType::List(t) => HeaderType::List(Box::new(header_type(t, binders))),
		InterfaceType::Tuple(ts) => {
			HeaderType::Tuple(ts.iter().map(|t| header_type(t, binders)).collect())
		}
		InterfaceType::Map(a, b) => HeaderType::Map(
			Box::new(header_type(a, binders)),
			Box::new(header_type(b, binders)),
		),
		InterfaceType::Function {
			parameters,
			return_type,
		} => HeaderType::Function {
			parameters: parameters.iter().map(|t| header_type(t, binders)).collect(),
			return_type: Box::new(header_type(return_type, binders)),
		},
		InterfaceType::Named {
			definition,
			positional,
			named,
		} => HeaderType::Named {
			definition: definition.clone(),
			positional: positional.iter().map(|t| header_type(t, binders)).collect(),
			named: named
				.iter()
				.map(|(n, t)| (n.clone(), header_type(t, binders)))
				.collect(),
		},
		InterfaceType::Intersection(ts) => {
			HeaderType::Intersection(ts.iter().map(|t| header_type(t, binders)).collect())
		}
		InterfaceType::Mutable(t) => HeaderType::Mutable(Box::new(header_type(t, binders))),
		InterfaceType::Generic(id) => HeaderType::Generic(HeaderParameterId(
			binders
				.iter()
				.position(|b| b.id == *id)
				.expect("impl binder exists") as u32,
		)),
	}
}

pub(crate) fn assign_runtime_body_identities(
	checker: &mut crate::check::Checker<'_>,
	identity: &ModuleIdentity,
) -> crate::annotate::SourceIdentities {
	let mut source_identities = crate::annotate::SourceIdentities::default();
	let headers = DeclaredHeaders {
		module: identity.clone(),
		definitions: checker
			.defs
			.defs
			.iter()
			.filter_map(|definition| {
				definition
					.stable
					.clone()
					.map(|stable| (definition.name.clone(), stable))
			})
			.collect(),
		checked_definitions: checker
			.defs
			.defs
			.iter()
			.filter_map(|definition| {
				definition
					.stable
					.clone()
					.map(|stable| (definition.name.clone(), stable))
			})
			.collect(),
	};

	for (&interface, definition) in &mut checker.interfaces {
		let Some(owner) = checker.defs.stable(interface).cloned() else {
			continue;
		};
		let mut ids = StableIdBuilder::new(identity.clone());
		let names = checker
			.module
			.members
			.iter()
			.find_map(|declaration| match declaration {
				Declaration::Interface { name, members, .. }
					if checker.defs.get(&name.0) == Some(interface) =>
				{
					Some(
						members
							.iter()
							.filter_map(|member| {
								let nymph_ast::decl::InterfaceMember::Element(element) = &member.0 else {
									return None;
								};
								let nymph_ast::decl::InterfaceElement::Func { meta, .. } = &element.0 else {
									return None;
								};
								Some(meta.name.0.clone())
							})
							.collect::<Vec<_>>(),
					)
				}
				_ => None,
			})
			.unwrap_or_default();
		for name in names {
			if let Some(method) = definition.methods.get_mut(&name) {
				method.definition = Some(ids.allocate(DeclarationKey::member(
					owner.clone(),
					DeclarationCategory::Method,
					name,
				)));
			}
		}
	}

	let impl_mutability = checker
		.module
		.members
		.iter()
		.filter_map(|declaration| match declaration {
			Declaration::ImplFor { mutable, .. } => Some(*mutable),
			_ => None,
		})
		.chain(
			checker
				.module
				.members
				.iter()
				.flat_map(|declaration| match declaration {
					Declaration::Struct { impls, .. } | Declaration::Enum { impls, .. } => {
						vec![false; impls.len()]
					}
					_ => Vec::new(),
				}),
		)
		.collect::<Vec<_>>();
	let mut implementation_ids = StableIdBuilder::new(identity.clone());
	let snapshot = checker_snapshot(checker);
	for (implementation, mutable) in checker
		.impls
		.impls
		.iter_mut()
		.filter(|implementation| implementation.definition.is_none())
		.zip(impl_mutability)
	{
		let temporary = DefinitionId::new(
			identity.clone(),
			DeclarationKey::top_level(DeclarationCategory::Namespace, "$impl"),
		);
		let (context, binders) = definition_context(
			&snapshot,
			&headers,
			&temporary,
			implementation.generics.clone(),
		);
		let Ok(self_type) = canonicalize_type(&checker.interner, implementation.self_ty, &context)
		else {
			continue;
		};
		let Some(interface) = checker.defs.stable(implementation.interface).cloned() else {
			continue;
		};
		let Ok(arguments) = implementation
			.args
			.iter()
			.map(|(name, ty)| {
				Ok((
					name.clone(),
					canonicalize_type(&checker.interner, *ty, &context)?,
				))
			})
			.collect::<Result<Vec<_>, InterfaceConversionError>>()
		else {
			continue;
		};
		let Ok(constraints) =
			checked_constraints(&implementation.constraints, &snapshot, &headers, &context)
		else {
			continue;
		};
		let id = implementation_ids.allocate(DeclarationKey::implementation(ImplementationHeader {
			interface: Some(interface),
			interface_arguments: arguments
				.iter()
				.map(|(name, ty)| (name.clone(), header_type(ty, &binders)))
				.collect(),
			self_type: header_type(&self_type, &binders),
			mutable,
			binders: (0..binders.len())
				.map(|index| HeaderBinder {
					parameter: HeaderParameterId(index as u32),
				})
				.collect(),
			constraints: constraints
				.iter()
				.map(|constraint| crate::HeaderConstraint {
					parameter: HeaderParameterId(
						binders
							.iter()
							.position(|binder| binder.id == constraint.parameter)
							.unwrap() as u32,
					),
					interface: constraint.interface.clone(),
					positional: constraint
						.positional
						.iter()
						.map(|ty| header_type(ty, &binders))
						.collect(),
					named: constraint
						.named
						.iter()
						.map(|(name, ty)| (name.clone(), header_type(ty, &binders)))
						.collect(),
				})
				.collect(),
		}));
		implementation.definition = Some(id.clone());
		for (name, method) in &mut implementation.methods {
			method.definition = Some(DefinitionId::new(
				identity.clone(),
				DeclarationKey::member(id.clone(), DeclarationCategory::Method, name.clone()),
			));
		}
	}

	// Inherent ADT groups are owned by the type. Standalone inherent blocks are
	// implementation-owned; their exact canonical IDs are finalized below by the
	// same extraction header machinery (and are not confused with the ADT owner).
	let adt_groups = checker
		.module
		.members
		.iter()
		.filter(|declaration| {
			matches!(
				declaration,
				Declaration::Struct { .. } | Declaration::Enum { .. }
			)
		})
		.count();
	let inherent_mutability =
		std::iter::repeat_n(false, adt_groups).chain(checker.module.members.iter().filter_map(
			|declaration| match declaration {
				Declaration::Impl { mutable, .. } => Some(*mutable),
				_ => None,
			},
		));
	for ((index, implementation), mutable) in checker
		.inherent
		.impls
		.iter_mut()
		.filter(|implementation| !implementation.imported)
		.enumerate()
		.zip(inherent_mutability)
	{
		let owner = (index < adt_groups)
			.then(|| match checker.interner.kind(implementation.self_ty) {
				nymph_hir::ty::TyKind::Adt(def, _) => checker.defs.stable(*def).cloned(),
				_ => None,
			})
			.flatten();
		if let Some(owner) = owner {
			implementation.definition = Some(owner.clone());
			for (name, method) in &mut implementation.methods {
				method.definition = Some(DefinitionId::new(
					identity.clone(),
					DeclarationKey::member(owner.clone(), DeclarationCategory::Method, name.clone()),
				));
			}
		} else {
			let temporary = DefinitionId::new(
				identity.clone(),
				DeclarationKey::top_level(DeclarationCategory::Namespace, "$inherent"),
			);
			let (context, binders) = definition_context(
				&snapshot,
				&headers,
				&temporary,
				implementation.owner_generic_names.clone(),
			);
			if let Ok(self_type) = canonicalize_type(&checker.interner, implementation.self_ty, &context)
			{
				let id =
					implementation_ids.allocate(DeclarationKey::implementation(ImplementationHeader {
						interface: None,
						interface_arguments: Vec::new(),
						self_type: header_type(&self_type, &binders),
						mutable,
						binders: (0..binders.len())
							.map(|index| HeaderBinder {
								parameter: HeaderParameterId(index as u32),
							})
							.collect(),
						constraints: Vec::new(),
					}));
				implementation.definition = Some(id.clone());
				for (name, method) in &mut implementation.methods {
					method.definition = Some(DefinitionId::new(
						identity.clone(),
						DeclarationKey::member(id.clone(), DeclarationCategory::Method, name.clone()),
					));
				}
			}
		}
	}

	let top_paths = checker
		.module
		.members
		.iter()
		.enumerate()
		.filter_map(|(declaration, item)| {
			matches!(item, Declaration::ImplFor { .. }).then_some(
				crate::annotate::ImplementationSourcePath {
					declaration: declaration as u32,
					nested: None,
				},
			)
		});
	let nested_paths = checker
		.module
		.members
		.iter()
		.enumerate()
		.flat_map(|(declaration, item)| match item {
			Declaration::Struct { impls, .. } | Declaration::Enum { impls, .. } => impls
				.iter()
				.enumerate()
				.map(
					move |(index, _)| crate::annotate::ImplementationSourcePath {
						declaration: declaration as u32,
						nested: Some(index as u32),
					},
				)
				.collect(),
			_ => Vec::new(),
		});
	let interface_paths = top_paths.chain(nested_paths).collect::<Vec<_>>();
	let local_impls = checker.impls.impls.iter().filter(|implementation| {
		implementation
			.definition
			.as_ref()
			.is_some_and(|definition| definition.module == *identity)
	});
	for (path, implementation) in interface_paths.into_iter().zip(local_impls) {
		if let Some(id) = &implementation.definition {
			source_identities.implementations.insert(path, id.clone());
			let members = match &checker.module.members[path.declaration as usize] {
				Declaration::ImplFor { members, .. } => members,
				Declaration::Struct { impls, .. } | Declaration::Enum { impls, .. } => {
					&impls[path.nested.unwrap() as usize].0.members
				}
				_ => unreachable!(),
			};
			for (index, member) in members.iter().enumerate() {
				let name = impl_member_name(&member.0);
				let member_id = DefinitionId::new(
					identity.clone(),
					DeclarationKey::member(id.clone(), DeclarationCategory::Method, name),
				);
				source_identities.members.insert(
					crate::annotate::ImplementationMemberSourcePath {
						implementation: path,
						member: index as u32,
					},
					member_id,
				);
			}
		}
	}
	let standalone = checker
		.module
		.members
		.iter()
		.enumerate()
		.filter_map(|(declaration, item)| match item {
			Declaration::Impl { members, .. } => Some((declaration, members)),
			_ => None,
		});
	let inherent = checker
		.inherent
		.impls
		.iter()
		.filter(|implementation| !implementation.imported)
		.skip(adt_groups);
	for ((declaration, members), implementation) in standalone.zip(inherent) {
		let path = crate::annotate::ImplementationSourcePath {
			declaration: declaration as u32,
			nested: None,
		};
		let Some(id) = implementation.definition.clone() else {
			continue;
		};
		source_identities.implementations.insert(path, id.clone());
		for (index, member) in members.iter().enumerate() {
			source_identities.members.insert(
				crate::annotate::ImplementationMemberSourcePath {
					implementation: path,
					member: index as u32,
				},
				DefinitionId::new(
					identity.clone(),
					DeclarationKey::member(
						id.clone(),
						DeclarationCategory::Method,
						impl_member_name(&member.0),
					),
				),
			);
		}
	}
	source_identities
}

fn impl_member_name(member: &ImplMember) -> EcoString {
	match member {
		ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => meta.name.0.clone(),
		ImplMember::Let { meta, .. } | ImplMember::ExternalLet(_, _, meta) => match &meta.name.0 {
			Pattern::Binding { name, .. } => name.0.clone(),
			_ => "<pattern>".into(),
		},
	}
}

fn checker_snapshot(checker: &crate::check::Checker<'_>) -> Checked {
	Checked {
		diags: Vec::new(),
		facts: crate::CheckedFacts {
			annotations: crate::Annotations::default(),
			external_value_marshals: Default::default(),
			interner: checker.interner.clone(),
			semantic: crate::CheckedSemantic {
				definitions: checker.defs.clone(),
				signatures: checker.sigs.clone(),
				interfaces: checker.interfaces.clone(),
				external_abis: Default::default(),
				implementations: checker.impls.clone(),
				inherent: Vec::new(),
				anonymous_bounds: checker.synthetic_bound_details.clone(),
				local_definitions: 0..0,
				local_implementations: 0..0,
				local_inherent: 0..0,
				has_explicit_local_ranges: false,
			},
			source_identities: Default::default(),
		},
	}
}

fn function_shape(
	definition: &mut ExportedDefinition,
	meta: &FuncDeclaration,
	def: crate::DefId,
	checked: &Checked,
	context: &CanonicalizationContext,
) -> Result<(), InterfaceConversionError> {
	let signature = &checked.semantic.signatures.funcs[&def];
	definition.parameters = signature
		.params
		.iter()
		.zip(&meta.params)
		.map(|(sig, source)| {
			Ok(ParameterShape {
				name: sig.label.clone(),
				ty: canonicalize_type(&checked.interner, sig.ty, context)?,
				mutable: source.0.mutable,
				spread: sig.spread,
			})
		})
		.collect::<Result<_, _>>()?;
	definition.return_type = Some(canonicalize_type(
		&checked.interner,
		signature.ret,
		context,
	)?);
	Ok(())
}

fn extract_definition(
	declaration: &Declaration,
	checked: &Checked,
	headers: &DeclaredHeaders,
	context: &CanonicalizationContext,
) -> Result<Option<ExportedDefinition>, InterfaceConversionError> {
	let Some((_, name)) = declaration_identity(declaration) else {
		return Ok(None);
	};
	let id = headers.id(name).expect("declared header exists");
	let source_name: EcoString = source_name(name).into();
	let def = checked
		.semantic
		.definitions
		.get(name)
		.expect("checked definition exists");
	let mut result = match declaration {
		Declaration::Func {
			visibility, meta, ..
		}
		| Declaration::ExternalFunc(visibility, _, meta) => {
			let mut shape = empty_definition(
				id,
				source_name.clone(),
				*visibility,
				DefinitionShapeKind::Function,
			);
			let (_, binders) = definition_context(
				checked,
				headers,
				&shape.id,
				checked.semantic.signatures.funcs[&def].generics.clone(),
			);
			let signature = &checked.semantic.signatures.funcs[&def];
			let (generic_context, binders, anonymous_constraints) = definition_anonymous_context(
				&shape.id,
				binders,
				signature
					.params
					.iter()
					.map(|parameter| parameter.ty)
					.chain([signature.ret]),
				checked,
				headers,
			)?;
			shape.binders = binders;
			shape.constraints =
				checked_constraints(&signature.bounds, checked, headers, &generic_context)?;
			shape.constraints.extend(anonymous_constraints);
			function_shape(&mut shape, meta, def, checked, &generic_context)?;
			shape.runtime_owner = Some(shape.id.clone());
			if let Declaration::ExternalFunc(_, symbol, _) = declaration {
				shape.external = Some(external_function_abi(symbol));
			}
			shape
		}
		Declaration::Let { visibility, .. } | Declaration::ExternalLet(visibility, ..) => {
			let mut shape = empty_definition(
				id,
				source_name.clone(),
				*visibility,
				DefinitionShapeKind::Let,
			);
			shape.ty = Some(canonicalize_type(
				&checked.interner,
				checked.semantic.signatures.lets[&def].ty,
				context,
			)?);
			if let Declaration::ExternalLet(_, marker, meta) = declaration {
				shape.external = Some(external_value_abi(
					marker,
					checked.external_value_marshals.get(&meta.name.1).copied(),
				));
			}
			shape
		}
		Declaration::Struct {
			visibility,
			generics,
			fields,
			members,
			..
		} => {
			let mut shape = empty_definition(
				id.clone(),
				source_name.clone(),
				*visibility,
				DefinitionShapeKind::Struct,
			);
			let signature = &checked.semantic.signatures.structs[&def];
			let (_, binders) = definition_context(
				checked,
				headers,
				&shape.id,
				generics.iter().map(|g| g.0.name.0.clone()),
			);
			let (generic_context, binders, anonymous_constraints) = definition_anonymous_context(
				&shape.id,
				binders,
				signature.fields.iter().map(|(_, ty)| *ty),
				checked,
				headers,
			)?;
			shape.constraints = generic_constraints(generics, headers, &binders)?;
			shape.constraints.extend(anonymous_constraints);
			shape.binders = binders;
			let mut member_ids = StableIdBuilder::new(headers.module.clone());
			shape.fields = fields
				.iter()
				.zip(&signature.fields)
				.map(|(field, (_, ty))| {
					Ok(FieldShape {
						id: member_ids.allocate(DeclarationKey::member(
							id.clone(),
							DeclarationCategory::Field,
							field.0.name.0.clone(),
						)),
						name: field.0.name.0.clone(),
						visibility: field.0.visibility,
						ty: canonicalize_type(&checked.interner, *ty, &generic_context)?,
						mutable: false,
						has_default: field.0.default.is_some(),
					})
				})
				.collect::<Result<_, _>>()?;
			shape.members = definition_members(
				members,
				def,
				&shape.id,
				&shape.binders,
				checked,
				headers,
				&mut member_ids,
			)?;
			shape.runtime_owner = Some(shape.id.clone());
			shape
		}
		Declaration::Enum {
			visibility,
			generics,
			variants,
			members,
			..
		} => {
			let mut shape = empty_definition(
				id,
				source_name.clone(),
				*visibility,
				DefinitionShapeKind::Enum,
			);
			let signature = &checked.semantic.signatures.enums[&def];
			let (_, binders) =
				definition_context(checked, headers, &shape.id, signature.generics.clone());
			let (generic_context, binders, anonymous_constraints) = definition_anonymous_context(
				&shape.id,
				binders,
				signature
					.variants
					.iter()
					.flat_map(|variant| variant.fields.iter().map(|(_, ty)| *ty)),
				checked,
				headers,
			)?;
			shape.binders = binders;
			shape.constraints = generic_constraints(generics, headers, &shape.binders)?;
			shape.constraints.extend(anonymous_constraints);
			let mut member_ids = StableIdBuilder::new(headers.module.clone());
			shape.variants = variants
				.iter()
				.zip(&signature.variants)
				.map(|(variant, signature)| {
					let variant_id = member_ids.allocate(DeclarationKey::member(
						shape.id.clone(),
						DeclarationCategory::Variant,
						variant.0.name.0.clone(),
					));
					let fields = variant
						.0
						.fields
						.iter()
						.zip(&signature.fields)
						.map(|(field, (_, ty))| {
							Ok(FieldShape {
								id: member_ids.allocate(DeclarationKey::member(
									variant_id.clone(),
									DeclarationCategory::Field,
									field.0.name.0.clone(),
								)),
								name: field.0.name.0.clone(),
								visibility: field.0.visibility,
								ty: canonicalize_type(&checked.interner, *ty, &generic_context)?,
								mutable: false,
								has_default: field.0.default.is_some(),
							})
						})
						.collect::<Result<_, InterfaceConversionError>>()?;
					Ok(VariantShape {
						id: variant_id,
						name: variant.0.name.0.clone(),
						fields,
					})
				})
				.collect::<Result<_, InterfaceConversionError>>()?;
			shape.members = definition_members(
				members,
				def,
				&shape.id,
				&shape.binders,
				checked,
				headers,
				&mut member_ids,
			)?;
			shape.runtime_owner = Some(shape.id.clone());
			shape
		}
		Declaration::TypeAlias {
			visibility,
			meta,
			value,
		} => {
			let mut shape = empty_definition(
				id,
				source_name.clone(),
				*visibility,
				DefinitionShapeKind::TypeAlias,
			);
			let (_, binders) = definition_context(
				checked,
				headers,
				&shape.id,
				meta.generics.iter().map(|g| g.0.name.0.clone()),
			);
			shape.ty = Some(ast_type(&value.0, headers, &binders)?);
			shape.constraints = generic_constraints(&meta.generics, headers, &binders)?;
			shape.binders = binders;
			shape
		}
		Declaration::Interface {
			visibility,
			generics,
			super_interfaces,
			members,
			..
		} => {
			let mut shape = empty_definition(
				id,
				source_name.clone(),
				*visibility,
				DefinitionShapeKind::Interface,
			);
			let (_, binders) = definition_context(
				checked,
				headers,
				&shape.id,
				generics.iter().map(|g| g.0.name.0.clone()),
			);
			shape.binders = binders;
			shape.constraints = generic_constraints(generics, headers, &shape.binders)?;
			shape.super_interfaces = super_interfaces
				.iter()
				.map(|super_| {
					let (name, arguments) = &super_.0;
					let mut positional = Vec::new();
					let mut named = Vec::new();
					for argument in arguments {
						let ty = ast_type(&argument.0.value.0, headers, &shape.binders)?;
						if let Some(name) = &argument.0.name {
							named.push((name.0.clone(), ty));
						} else {
							positional.push(ty);
						}
					}
					Ok(SuperInterfaceShape {
						interface: headers
							.id(&name.0)
							.ok_or(InterfaceConversionError::ErrorType)?,
						positional,
						named,
					})
				})
				.collect::<Result<_, InterfaceConversionError>>()?;
			let mut member_ids = StableIdBuilder::new(headers.module.clone());
			shape.members = members
				.iter()
				.filter_map(|m| match &m.0 {
					nymph_ast::decl::InterfaceMember::Element(e) => Some(&e.0),
					_ => None,
				})
				.map(|element| {
					if let nymph_ast::decl::InterfaceElement::Func { meta, body } = element {
						let signature = &checked.semantic.interfaces[&def].methods[&meta.name.0];
						let facts = crate::annotate::CheckedMethod {
							definition: signature.definition.clone(),
							params: signature.params.clone(),
							ret: signature.ret,
							bounds: signature.bounds.clone(),
						};
						let mut member = checked_member_shape(
							meta,
							None,
							None,
							&facts,
							&shape.id,
							&shape.binders,
							checked,
							headers,
							&mut member_ids,
						)?;
						member.has_default = body.is_some();
						member.runtime_owner = body.as_ref().map(|_| shape.id.clone());
						return Ok(member);
					}
					let dummy = || {
						nymph_ast::expr::Expr::new(
							nymph_ast::expr::ExprKind::Tuple(Vec::new()),
							nymph_ast::Span::new(0, 0),
							nymph_ast::NodeId::DUMMY,
						)
					};
					let synthetic = match element {
						nymph_ast::decl::InterfaceElement::Func { meta, body } => ImplMember::Func {
							visibility: None,
							meta: meta.clone(),
							body: body.clone().unwrap_or_else(dummy),
						},
						nymph_ast::decl::InterfaceElement::Let { meta, value } => ImplMember::Let {
							visibility: None,
							meta: meta.clone(),
							value: value.clone().unwrap_or_else(dummy),
						},
					};
					let mut member = member_shape(
						&synthetic,
						&shape.id,
						headers,
						&shape.binders,
						&mut member_ids,
					)?;
					member.has_default = match element {
						nymph_ast::decl::InterfaceElement::Func { body, .. } => body.is_some(),
						nymph_ast::decl::InterfaceElement::Let { value, .. } => value.is_some(),
					};
					Ok(member)
				})
				.collect::<Result<_, InterfaceConversionError>>()?;
			shape
		}
		Declaration::Namespace {
			visibility,
			members,
			..
		} => {
			let mut shape =
				empty_definition(id, source_name, *visibility, DefinitionShapeKind::Namespace);
			let mut member_ids = StableIdBuilder::new(headers.module.clone());
			shape.members = members
				.iter()
				.map(|m| member_shape(&m.0, &shape.id, headers, &[], &mut member_ids))
				.collect::<Result<_, _>>()?;
			shape
		}
		Declaration::Import { .. } | Declaration::Impl { .. } | Declaration::ImplFor { .. } => {
			unreachable!()
		}
	};
	// External ABI metadata is intentionally sourced from checked linkage in later
	// lowering slices; stable ownership is already represented here.
	result.binders.reserve(0);
	Ok(Some(result))
}

fn referenced_definitions(ty: &InterfaceType, out: &mut HashSet<DefinitionId>) {
	match ty {
		InterfaceType::Named {
			definition,
			positional,
			named,
		} => {
			out.insert(definition.clone());
			for ty in positional {
				referenced_definitions(ty, out);
			}
			for (_, ty) in named {
				referenced_definitions(ty, out);
			}
		}
		InterfaceType::List(ty) | InterfaceType::Mutable(ty) => referenced_definitions(ty, out),
		InterfaceType::Tuple(types) | InterfaceType::Intersection(types) => {
			for ty in types {
				referenced_definitions(ty, out);
			}
		}
		InterfaceType::Map(a, b) => {
			referenced_definitions(a, out);
			referenced_definitions(b, out);
		}
		InterfaceType::Function {
			parameters,
			return_type,
		} => {
			for ty in parameters {
				referenced_definitions(ty, out);
			}
			referenced_definitions(return_type, out);
		}
		_ => {}
	}
}

fn referenced_definition_shape(definition: &ExportedDefinition, out: &mut HashSet<DefinitionId>) {
	for constraint in &definition.constraints {
		out.insert(constraint.interface.clone());
		for ty in constraint
			.positional
			.iter()
			.chain(constraint.named.iter().map(|(_, ty)| ty))
		{
			referenced_definitions(ty, out);
		}
	}
	for super_interface in &definition.super_interfaces {
		out.insert(super_interface.interface.clone());
		for ty in super_interface
			.positional
			.iter()
			.chain(super_interface.named.iter().map(|(_, ty)| ty))
		{
			referenced_definitions(ty, out);
		}
	}
	for ty in definition
		.parameters
		.iter()
		.map(|parameter| &parameter.ty)
		.chain(definition.return_type.iter())
		.chain(definition.ty.iter())
		.chain(definition.fields.iter().map(|field| &field.ty))
		.chain(
			definition
				.variants
				.iter()
				.flat_map(|variant| variant.fields.iter().map(|field| &field.ty)),
		) {
		referenced_definitions(ty, out);
	}
	for member in &definition.members {
		for constraint in &member.constraints {
			out.insert(constraint.interface.clone());
			for ty in constraint
				.positional
				.iter()
				.chain(constraint.named.iter().map(|(_, ty)| ty))
			{
				referenced_definitions(ty, out);
			}
		}
		for ty in member
			.parameters
			.iter()
			.map(|parameter| &parameter.ty)
			.chain(std::iter::once(&member.return_type))
		{
			referenced_definitions(ty, out);
		}
	}
}

fn referenced_impl_shape(implementation: &ExportedImpl, out: &mut HashSet<DefinitionId>) {
	if let Some(interface) = &implementation.interface {
		out.insert(interface.clone());
	}
	for (_, ty) in &implementation.interface_arguments {
		referenced_definitions(ty, out);
	}
	referenced_definitions(&implementation.self_type, out);
	for constraint in &implementation.constraints {
		out.insert(constraint.interface.clone());
		for ty in constraint
			.positional
			.iter()
			.chain(constraint.named.iter().map(|(_, ty)| ty))
		{
			referenced_definitions(ty, out);
		}
	}
	for member in &implementation.members {
		for constraint in &member.constraints {
			out.insert(constraint.interface.clone());
			for ty in constraint
				.positional
				.iter()
				.chain(constraint.named.iter().map(|(_, ty)| ty))
			{
				referenced_definitions(ty, out);
			}
		}
		for ty in member
			.parameters
			.iter()
			.map(|parameter| &parameter.ty)
			.chain(std::iter::once(&member.return_type))
		{
			referenced_definitions(ty, out);
		}
	}
}

fn extract_implementations(
	module: &Module,
	checked: &Checked,
	headers: &DeclaredHeaders,
	facts: &ExtractionFactSelection,
) -> Result<Vec<ExportedImpl>, InterfaceConversionError> {
	let mut declarations = module
		.members
		.iter()
		.enumerate()
		.filter_map(|(index, declaration)| match declaration {
			Declaration::ImplFor {
				visibility,
				mutable,
				members,
				..
			} => Some((
				crate::annotate::ImplementationSourcePath {
					declaration: index as u32,
					nested: None,
				},
				*visibility,
				*mutable,
				members,
			)),
			_ => None,
		})
		.collect::<Vec<_>>();
	for (declaration_index, declaration) in module.members.iter().enumerate() {
		let impls = match declaration {
			Declaration::Struct { impls, .. } | Declaration::Enum { impls, .. } => impls,
			_ => continue,
		};
		declarations.extend(impls.iter().enumerate().map(|(nested, implementation)| {
			(
				crate::annotate::ImplementationSourcePath {
					declaration: declaration_index as u32,
					nested: Some(nested as u32),
				},
				None,
				false,
				&implementation.0.members,
			)
		}));
	}
	let mut remaining = checked.semantic.implementations.impls[facts.implementations.clone()]
		.iter()
		.collect::<Vec<_>>();
	let implementations = declarations
		.iter()
		.map(|(path, _, _, members)| {
			let position = if let Some(id) = checked.source_identities.implementations.get(path) {
				remaining
					.iter()
					.position(|implementation| implementation.definition.as_ref() == Some(id))
					.expect("checked implementation identity")
			} else {
				// Compatibility extraction for legacy identity-less `check_module` facts.
				// Runtime projection only accepts the authoritative branch above.
				let names = members
					.iter()
					.filter_map(|member| match &member.0 {
						ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => {
							Some(&meta.name.0)
						}
						_ => None,
					})
					.collect::<HashSet<_>>();
				remaining
					.iter()
					.position(|implementation| {
						implementation.methods.len() == names.len()
							&& names
								.iter()
								.all(|name| implementation.methods.contains_key(*name))
					})
					.expect("legacy checked implementation")
			};
			remaining.remove(position)
		})
		.collect::<Vec<_>>();
	let mut extracted = declarations
		.into_iter()
		.zip(implementations)
		.map(|((_path, visibility, mutable, members), implementation)| {
			let scope = DefinitionId::new(
				headers.module.clone(),
				DeclarationKey::top_level(DeclarationCategory::Namespace, "$impl"),
			);
			let (temporary_context, temporary_binders) =
				definition_context(checked, headers, &scope, implementation.generics.clone());
			let self_type = canonicalize_type(
				&checked.interner,
				implementation.self_ty,
				&temporary_context,
			)?;
			let interface_name = &checked
				.semantic
				.definitions
				.data(implementation.interface)
				.name;
			let interface = checked
				.semantic
				.definitions
				.stable(implementation.interface)
				.cloned()
				.or_else(|| headers.id(interface_name))
				.ok_or(InterfaceConversionError::ErrorType)?;
			let arguments = implementation
				.args
				.iter()
				.map(|(name, ty)| {
					Ok((
						name.clone(),
						canonicalize_type(&checked.interner, *ty, &temporary_context)?,
					))
				})
				.collect::<Result<Vec<_>, InterfaceConversionError>>()?;
			let temporary_constraints = checked_constraints(
				&implementation.constraints,
				checked,
				headers,
				&temporary_context,
			)?;
			let id = DefinitionId::new(
				headers.module.clone(),
				DeclarationKey::implementation(ImplementationHeader {
					interface: Some(interface.clone()),
					interface_arguments: arguments
						.iter()
						.map(|(n, t)| (n.clone(), header_type(t, &temporary_binders)))
						.collect(),
					self_type: header_type(&self_type, &temporary_binders),
					mutable,
					binders: (0..temporary_binders.len())
						.map(|i| HeaderBinder {
							parameter: HeaderParameterId(i as u32),
						})
						.collect(),
					constraints: temporary_constraints
						.iter()
						.map(|constraint| crate::HeaderConstraint {
							parameter: HeaderParameterId(
								temporary_binders
									.iter()
									.position(|binder| binder.id == constraint.parameter)
									.expect("constraint binder exists") as u32,
							),
							interface: constraint.interface.clone(),
							positional: constraint
								.positional
								.iter()
								.map(|ty| header_type(ty, &temporary_binders))
								.collect(),
							named: constraint
								.named
								.iter()
								.map(|(name, ty)| (name.clone(), header_type(ty, &temporary_binders)))
								.collect(),
						})
						.collect(),
				}),
			);
			let id = implementation.definition.clone().unwrap_or(id);
			let (context, binders) =
				definition_context(checked, headers, &id, implementation.generics.clone());
			let mut member_ids = StableIdBuilder::new(headers.module.clone());
			let members = members
				.iter()
				.map(|member| match &member.0 {
					ImplMember::Func {
						visibility, meta, ..
					} => {
						let signature = &implementation.methods[&meta.name.0];
						let facts = crate::annotate::CheckedMethod {
							definition: signature.definition.clone(),
							params: signature.params.clone(),
							ret: signature.ret,
							bounds: signature.bounds.clone(),
						};
						let mut member = checked_member_shape(
							meta,
							*visibility,
							None,
							&facts,
							&id,
							&binders,
							checked,
							headers,
							&mut member_ids,
						)?;
						member.runtime_owner = Some(id.clone());
						Ok(member)
					}
					ImplMember::ExternalFunc(visibility, symbol, meta) => {
						let signature = &implementation.methods[&meta.name.0];
						let facts = crate::annotate::CheckedMethod {
							definition: signature.definition.clone(),
							params: signature.params.clone(),
							ret: signature.ret,
							bounds: signature.bounds.clone(),
						};
						checked_member_shape(
							meta,
							*visibility,
							Some(symbol),
							&facts,
							&id,
							&binders,
							checked,
							headers,
							&mut member_ids,
						)
					}
					ImplMember::Let { .. } | ImplMember::ExternalLet(..) => {
						member_shape(&member.0, &id, headers, &binders, &mut member_ids)
					}
				})
				.collect::<Result<_, InterfaceConversionError>>()?;
			Ok(ExportedImpl {
				id: id.clone(),
				visibility,
				interface: Some(interface),
				interface_arguments: implementation
					.args
					.iter()
					.map(|(n, t)| {
						Ok((
							n.clone(),
							canonicalize_type(&checked.interner, *t, &context)?,
						))
					})
					.collect::<Result<_, InterfaceConversionError>>()?,
				self_type: canonicalize_type(&checked.interner, implementation.self_ty, &context)?,
				mutable,
				binders,
				constraints: checked_constraints(&implementation.constraints, checked, headers, &context)?,
				members,
				member_slots: Vec::new(),
				runtime_owner: Some(id),
			})
		})
		.collect::<Result<Vec<_>, _>>()?;

	let top_level_inherent = module
		.members
		.iter()
		.enumerate()
		.filter_map(|(declaration, item)| match item {
			Declaration::Impl {
				visibility,
				mutable,
				members,
				..
			} => Some((
				crate::annotate::ImplementationSourcePath {
					declaration: declaration as u32,
					nested: None,
				},
				*visibility,
				*mutable,
				members,
			)),
			_ => None,
		});
	let inherent_facts = checked.semantic.inherent[facts.inherent.clone()]
		.iter()
		.skip(
			module
				.members
				.iter()
				.filter(|declaration| {
					matches!(
						declaration,
						Declaration::Struct { .. } | Declaration::Enum { .. }
					)
				})
				.count(),
		);
	for ((path, visibility, mutable, members), ordered_implementation) in
		top_level_inherent.zip(inherent_facts)
	{
		let implementation = checked
			.source_identities
			.implementations
			.get(&path)
			.and_then(|id| {
				checked.semantic.inherent[facts.inherent.clone()]
					.iter()
					.find(|implementation| implementation.definition.as_ref() == Some(id))
			})
			.unwrap_or(ordered_implementation);
		let temporary_scope = DefinitionId::new(
			headers.module.clone(),
			DeclarationKey::top_level(DeclarationCategory::Namespace, "$inherent"),
		);
		let (temporary_context, temporary_binders) = definition_context(
			checked,
			headers,
			&temporary_scope,
			implementation.generics.clone(),
		);
		let self_type = canonicalize_type(
			&checked.interner,
			implementation.self_ty,
			&temporary_context,
		)?;
		let header_constraints = checked_constraints(
			&implementation.constraints,
			checked,
			headers,
			&temporary_context,
		)?;
		let id = DefinitionId::new(
			headers.module.clone(),
			DeclarationKey::implementation(ImplementationHeader {
				interface: None,
				interface_arguments: Vec::new(),
				self_type: header_type(&self_type, &temporary_binders),
				mutable,
				binders: (0..temporary_binders.len())
					.map(|index| HeaderBinder {
						parameter: HeaderParameterId(index as u32),
					})
					.collect(),
				constraints: header_constraints
					.iter()
					.map(|constraint| crate::HeaderConstraint {
						parameter: HeaderParameterId(
							temporary_binders
								.iter()
								.position(|binder| binder.id == constraint.parameter)
								.expect("constraint binder exists") as u32,
						),
						interface: constraint.interface.clone(),
						positional: constraint
							.positional
							.iter()
							.map(|ty| header_type(ty, &temporary_binders))
							.collect(),
						named: constraint
							.named
							.iter()
							.map(|(name, ty)| (name.clone(), header_type(ty, &temporary_binders)))
							.collect(),
					})
					.collect(),
			}),
		);
		let id = implementation.definition.clone().unwrap_or(id);
		let (impl_context, binders) =
			definition_context(checked, headers, &id, implementation.generics.clone());
		let mut member_ids = StableIdBuilder::new(headers.module.clone());
		let members = members
			.iter()
			.map(|member| match &member.0 {
				ImplMember::Func {
					visibility, meta, ..
				} => checked_member_shape(
					meta,
					*visibility,
					None,
					&implementation.methods[&meta.name.0],
					&id,
					&binders,
					checked,
					headers,
					&mut member_ids,
				),
				ImplMember::ExternalFunc(visibility, symbol, meta) => checked_member_shape(
					meta,
					*visibility,
					Some(symbol),
					&implementation.methods[&meta.name.0],
					&id,
					&binders,
					checked,
					headers,
					&mut member_ids,
				),
				ImplMember::Let { .. } | ImplMember::ExternalLet(..) => {
					member_shape(&member.0, &id, headers, &binders, &mut member_ids)
				}
			})
			.collect::<Result<Vec<_>, _>>()?;
		extracted.push(ExportedImpl {
			id: id.clone(),
			visibility,
			interface: None,
			interface_arguments: Vec::new(),
			self_type: canonicalize_type(&checked.interner, implementation.self_ty, &impl_context)?,
			mutable: false,
			binders,
			constraints: checked_constraints(
				&implementation.constraints,
				checked,
				headers,
				&impl_context,
			)?,
			members,
			member_slots: Vec::new(),
			runtime_owner: Some(id),
		});
	}
	Ok(extracted)
}

pub fn extract_module_interface(
	module_identity: ModuleIdentity,
	module: &Module,
	checked: &Checked,
	headers: &DeclaredHeaders,
) -> Result<ModuleInterface, InterfaceConversionError> {
	extract_module_interface_with_facts(
		module_identity,
		module,
		checked,
		headers,
		&ExtractionFactSelection::all(checked),
	)
}

pub fn extract_module_interface_with_facts(
	module_identity: ModuleIdentity,
	module: &Module,
	checked: &Checked,
	headers: &DeclaredHeaders,
	facts: &ExtractionFactSelection,
) -> Result<ModuleInterface, InterfaceConversionError> {
	let context = context(checked, headers);
	let all = module
		.members
		.iter()
		.map(|declaration| extract_definition(declaration, checked, headers, &context))
		.collect::<Result<Vec<_>, _>>()?;
	let mut exports = Vec::new();
	let mut private = HashMap::new();
	for (declaration, definition) in module
		.members
		.iter()
		.zip(all)
		.filter_map(|(d, v)| v.map(|v| (d, v)))
	{
		let visibility = match declaration {
			Declaration::Func { visibility, .. }
			| Declaration::Let { visibility, .. }
			| Declaration::Struct { visibility, .. }
			| Declaration::Enum { visibility, .. }
			| Declaration::TypeAlias { visibility, .. }
			| Declaration::Interface { visibility, .. }
			| Declaration::Namespace { visibility, .. }
			| Declaration::ExternalFunc(visibility, ..)
			| Declaration::ExternalLet(visibility, ..) => *visibility,
			_ => None,
		};
		if visible(visibility) {
			exports.push(definition);
		} else {
			private.insert(definition.id.clone(), definition);
		}
	}
	let mut implementations = extract_implementations(module, checked, headers, facts)?;
	for implementation in &mut implementations {
		let Some(interface_id) = &implementation.interface else {
			continue;
		};
		let interface_shape = exports
			.iter()
			.chain(private.values())
			.find(|definition| &definition.id == interface_id);
		let imported_members = interface_shape
			.is_none()
			.then(|| {
				let semantic_impl = checked
					.semantic
					.implementations
					.impls
					.iter()
					.find(|candidate| candidate.definition.as_ref() == Some(&implementation.id))?;
				let semantic_interface = checked.semantic.interfaces.get(&semantic_impl.interface)?;
				Some(
					semantic_interface
						.methods
						.iter()
						.filter_map(|(name, member)| {
							Some((
								member.definition.clone()?,
								name.clone(),
								if member.mutating {
									crate::MemberKind::MutatingFunction
								} else {
									crate::MemberKind::Function
								},
								member.has_default,
							))
						})
						.collect::<Vec<_>>(),
				)
			})
			.flatten();
		let members = interface_shape
			.map(|shape| {
				shape
					.members
					.iter()
					.map(|member| {
						(
							member.id.clone(),
							member.name.clone(),
							member.kind,
							member.has_default,
						)
					})
					.collect::<Vec<_>>()
			})
			.or(imported_members)
			.unwrap_or_default();
		implementation.member_slots = members
			.iter()
			.filter_map(
				|(interface_member_id, interface_member_name, interface_member_kind, has_default)| {
					let override_member = implementation.members.iter().find(|member| {
						member.name == *interface_member_name && member.kind == *interface_member_kind
					});
					if let Some(member) = override_member {
						return Some(crate::ImplementationMemberSlot {
							implementation_id: implementation.id.clone(),
							interface_member_id: interface_member_id.clone(),
							member_id: member.id.clone(),
							body_definition_id: member.id.clone(),
							placement_owner: implementation.id.clone(),
							kind: *interface_member_kind,
							name: interface_member_name.clone(),
							source: crate::ImplementationMemberSource::Override,
						});
					}
					(*has_default).then(|| {
						let member_id = DefinitionId::new(
							implementation.id.module.clone(),
							DeclarationKey::materialized_interface_member(
								implementation.id.clone(),
								interface_member_id.clone(),
							),
						);
						crate::ImplementationMemberSlot {
							implementation_id: implementation.id.clone(),
							interface_member_id: interface_member_id.clone(),
							member_id,
							body_definition_id: interface_member_id.clone(),
							placement_owner: implementation.id.clone(),
							kind: *interface_member_kind,
							name: interface_member_name.clone(),
							source: crate::ImplementationMemberSource::InheritedDefault,
						}
					})
				},
			)
			.collect();
	}
	let mut needed = HashSet::new();
	for export in &exports {
		referenced_definition_shape(export, &mut needed);
	}
	for implementation in &implementations {
		referenced_impl_shape(implementation, &mut needed);
	}
	let mut queue: VecDeque<_> = needed.into_iter().collect();
	let mut support_definitions = Vec::new();
	let mut seen = HashSet::new();
	while let Some(id) = queue.pop_front() {
		if !seen.insert(id.clone()) {
			continue;
		}
		if let Some(definition) = private.remove(&id) {
			let mut nested = HashSet::new();
			referenced_definition_shape(&definition, &mut nested);
			queue.extend(nested);
			support_definitions.push(SupportDefinition { definition });
		}
	}
	support_definitions.sort_by(|a, b| a.definition.id.cmp(&b.definition.id));
	let mut interface = ModuleInterface {
		module: module_identity,
		exports,
		support_definitions,
		implementations,
		fingerprint: 0,
	};
	interface.fingerprint = interface.structural_fingerprint();
	Ok(interface)
}

/// Exact private top-level identities retained solely for lexical import diagnostics.
///
/// These facts deliberately remain separate from [`ModuleInterface`]: they are not
/// type/runtime support and changing a private body must not change public shapes.
pub fn extract_lexical_private_definitions(
	module: &Module,
	checked: &Checked,
	headers: &DeclaredHeaders,
) -> Result<Vec<(EcoString, DefinitionId)>, InterfaceConversionError> {
	let context = context(checked, headers);
	let definitions = module
		.members
		.iter()
		.map(|declaration| extract_definition(declaration, checked, headers, &context))
		.collect::<Result<Vec<_>, _>>()?;
	let mut private = module
		.members
		.iter()
		.zip(definitions)
		.filter_map(|(declaration, definition)| {
			let visibility = match declaration {
				Declaration::Func { visibility, .. }
				| Declaration::Let { visibility, .. }
				| Declaration::Struct { visibility, .. }
				| Declaration::Enum { visibility, .. }
				| Declaration::TypeAlias { visibility, .. }
				| Declaration::Interface { visibility, .. }
				| Declaration::Namespace { visibility, .. }
				| Declaration::ExternalFunc(visibility, ..)
				| Declaration::ExternalLet(visibility, ..) => *visibility,
				_ => return None,
			};
			(!visible(visibility))
				.then_some(definition)
				.flatten()
				.map(|definition| (definition.name, definition.id))
		})
		.collect::<Vec<_>>();
	private.sort_by(|left, right| left.1.cmp(&right.1));
	Ok(private)
}

fn poison_binders(
	owner: DefinitionId,
	generics: &[nymph_ast::Spanned<nymph_ast::ty::GenericParam>],
) -> Vec<GenericParameter> {
	generics
		.iter()
		.enumerate()
		.map(|(index, generic)| GenericParameter {
			id: GenericParameterId::new(owner.binder(BinderScope::Definition, 0), index as u32),
			name: generic.0.name.0.clone(),
		})
		.collect()
}

fn recover_ast_type(
	ty: &Type,
	headers: &DeclaredHeaders,
	binders: &[GenericParameter],
) -> RecoveredInterfaceType {
	ast_type(ty, headers, binders)
		.map(RecoveredInterfaceType::Known)
		.unwrap_or(RecoveredInterfaceType::Poison)
}

fn recovered_source_header_type(
	ty: &Type,
	binders: &[GenericParameter],
) -> crate::RecoveredHeaderType {
	use crate::RecoveredHeaderType as R;
	let nested = |ty: &Type| recovered_source_header_type(ty, binders);
	match ty {
		Type::Int => R::Atom("int".into()),
		Type::UInt => R::Atom("uint".into()),
		Type::Float => R::Atom("float".into()),
		Type::Char => R::Atom("char".into()),
		Type::String => R::Atom("string".into()),
		Type::Boolean => R::Atom("boolean".into()),
		Type::Void => R::Atom("void".into()),
		Type::Never => R::Atom("never".into()),
		Type::SelfType => R::Atom("self".into()),
		Type::Infer => R::Atom("_".into()),
		Type::Intersection(left, right) => R::Intersection(vec![nested(&left.0), nested(&right.0)]),
		Type::List(inner) => R::List(Box::new(nested(&inner.0))),
		Type::Tuple(items) => R::Tuple(items.iter().map(|item| nested(&item.0)).collect()),
		Type::Map(key, value) => R::Map(Box::new(nested(&key.0)), Box::new(nested(&value.0))),
		Type::Function {
			params,
			return_type,
		} => R::Function {
			parameters: params.iter().map(|(_, ty)| nested(&ty.0)).collect(),
			return_type: Box::new(nested(&return_type.0)),
		},
		Type::Reference { name, generics } => {
			if generics.is_empty()
				&& let Some(index) = binders.iter().position(|binder| binder.name == name.0)
			{
				return R::Generic(HeaderParameterId(index as u32));
			}
			let mut positional = Vec::new();
			let mut named = Vec::new();
			for argument in generics {
				let value = nested(&argument.0.value.0);
				if let Some(name) = &argument.0.name {
					named.push((name.0.clone(), value));
				} else {
					positional.push(value);
				}
			}
			R::Reference {
				name: name.0.clone(),
				positional,
				named,
			}
		}
		Type::Grouped(inner) => nested(&inner.0),
		Type::Mut(inner) => R::Mutable(Box::new(nested(&inner.0))),
	}
}

fn recovered_source_header_constraints(
	generics: &[nymph_ast::Spanned<nymph_ast::ty::GenericParam>],
	binders: &[GenericParameter],
	offset: usize,
) -> Vec<crate::RecoveredHeaderConstraint> {
	generics
		.iter()
		.enumerate()
		.filter_map(|(index, generic)| {
			let bound = generic.0.constraint.as_ref()?;
			let Type::Reference { name, generics } = &bound.0 else {
				return None;
			};
			let mut positional = Vec::new();
			let mut named = Vec::new();
			for argument in generics {
				let ty = recovered_source_header_type(&argument.0.value.0, binders);
				if let Some(name) = &argument.0.name {
					named.push((name.0.clone(), ty));
				} else {
					positional.push(ty);
				}
			}
			Some(crate::RecoveredHeaderConstraint {
				parameter: HeaderParameterId((offset + index) as u32),
				interface: name.0.clone(),
				positional,
				named,
			})
		})
		.collect()
}

fn recovered_known(ty: &RecoveredInterfaceType) -> Option<&InterfaceType> {
	match ty {
		RecoveredInterfaceType::Known(ty) => Some(ty),
		RecoveredInterfaceType::Poison => None,
	}
}

fn recover_generic_constraints(
	generics: &[nymph_ast::Spanned<nymph_ast::ty::GenericParam>],
	binders: &[GenericParameter],
	headers: &DeclaredHeaders,
) -> Vec<crate::RecoveredGenericConstraint> {
	recover_generic_constraints_with_types(generics, binders, binders, headers)
}

fn recover_generic_constraints_with_types(
	generics: &[nymph_ast::Spanned<nymph_ast::ty::GenericParam>],
	constraint_binders: &[GenericParameter],
	type_binders: &[GenericParameter],
	headers: &DeclaredHeaders,
) -> Vec<crate::RecoveredGenericConstraint> {
	generics
		.iter()
		.zip(constraint_binders)
		.filter_map(|(generic, binder)| {
			let bound = generic.0.constraint.as_ref()?;
			let Type::Reference { name, generics } = &bound.0 else {
				return Some(crate::ConstraintShape {
					parameter: binder.id.clone(),
					interface: crate::RecoveredDefinitionReference::Poison,
					positional: Vec::new(),
					named: Vec::new(),
				});
			};
			let interface = headers
				.id(&name.0)
				.map(crate::RecoveredDefinitionReference::Known)
				.unwrap_or(crate::RecoveredDefinitionReference::Poison);
			let mut positional = Vec::new();
			let mut named = Vec::new();
			for argument in generics {
				let ty = recover_ast_type(&argument.0.value.0, headers, type_binders);
				if let Some(name) = &argument.0.name {
					named.push((name.0.clone(), ty));
				} else {
					positional.push(ty);
				}
			}
			Some(crate::ConstraintShape {
				parameter: binder.id.clone(),
				interface,
				positional,
				named,
			})
		})
		.collect()
}

fn recover_func_member(
	meta: &FuncDeclaration,
	visibility: Option<Visibility>,
	owner: &DefinitionId,
	headers: &DeclaredHeaders,
	owner_binders: &[GenericParameter],
	ids: &mut StableIdBuilder,
	has_default: bool,
) -> crate::RecoveredMemberShape {
	let id = ids.allocate(DeclarationKey::member(
		owner.clone(),
		DeclarationCategory::Method,
		meta.name.0.clone(),
	));
	let binders = meta
		.generics
		.iter()
		.enumerate()
		.map(|(index, generic)| GenericParameter {
			id: GenericParameterId::new(id.binder(BinderScope::Member, 0), index as u32),
			name: generic.0.name.0.clone(),
		})
		.collect::<Vec<_>>();
	let all = owner_binders
		.iter()
		.chain(&binders)
		.cloned()
		.collect::<Vec<_>>();
	MemberShape {
		id,
		name: meta.name.0.clone(),
		visibility,
		kind: match meta.kind {
			FuncKind::Instance => MemberKind::Function,
			FuncKind::Mut => MemberKind::MutatingFunction,
			FuncKind::Namespace => MemberKind::StaticFunction,
		},
		binders: binders.clone(),
		constraints: recover_generic_constraints_with_types(&meta.generics, &binders, &all, headers),
		parameters: meta
			.params
			.iter()
			.map(|parameter| ParameterShape {
				name: match &parameter.0.name.0 {
					Pattern::Binding { name, .. } => Some(name.0.clone()),
					_ => None,
				},
				ty: recover_ast_type(&parameter.0.type_.0, headers, &all),
				mutable: parameter.0.mutable,
				spread: parameter.0.spread,
			})
			.collect(),
		return_type: meta
			.return_type
			.as_ref()
			.map(|ty| recover_ast_type(&ty.0, headers, &all))
			.unwrap_or(RecoveredInterfaceType::Poison),
		external: None,
		runtime_owner: Some(owner.clone()),
		has_default,
	}
}

fn recover_value_member(
	member: &ImplMember,
	owner: &DefinitionId,
	headers: &DeclaredHeaders,
	binders: &[GenericParameter],
	ids: &mut StableIdBuilder,
	has_default: bool,
) -> crate::RecoveredMemberShape {
	let (visibility, meta) = match member {
		ImplMember::Let {
			visibility, meta, ..
		}
		| ImplMember::ExternalLet(visibility, _, meta) => (*visibility, meta),
		_ => unreachable!(),
	};
	let Pattern::Binding { name, .. } = &meta.name.0 else {
		panic!("checked member binding")
	};
	MemberShape {
		id: ids.allocate(DeclarationKey::member(
			owner.clone(),
			DeclarationCategory::Method,
			name.0.clone(),
		)),
		name: name.0.clone(),
		visibility,
		kind: match meta.kind {
			nymph_ast::decl::LetKind::Instance => MemberKind::Value,
			nymph_ast::decl::LetKind::Mut => MemberKind::MutableValue,
			nymph_ast::decl::LetKind::Namespace => MemberKind::StaticValue,
		},
		binders: Vec::new(),
		constraints: Vec::new(),
		parameters: Vec::new(),
		return_type: meta
			.type_
			.as_ref()
			.map(|ty| recover_ast_type(&ty.0, headers, binders))
			.unwrap_or(RecoveredInterfaceType::Poison),
		external: None,
		runtime_owner: Some(owner.clone()),
		has_default,
	}
}

fn recover_members(
	members: &[nymph_ast::Spanned<ImplMember>],
	owner: &DefinitionId,
	headers: &DeclaredHeaders,
	binders: &[GenericParameter],
	ids: &mut StableIdBuilder,
) -> Vec<crate::RecoveredMemberShape> {
	members
		.iter()
		.map(|member| match &member.0 {
			ImplMember::Func {
				visibility, meta, ..
			}
			| ImplMember::ExternalFunc(visibility, _, meta) => {
				recover_func_member(meta, *visibility, owner, headers, binders, ids, true)
			}
			value => recover_value_member(value, owner, headers, binders, ids, true),
		})
		.collect()
}

fn recover_implementations(
	module: &Module,
	_checked: &Checked,
	headers: &DeclaredHeaders,
	_facts: &ExtractionFactSelection,
) -> Vec<RecoveredExportedImpl> {
	let mut ids = StableIdBuilder::new(headers.module.clone());
	let mut recovered = module
		.members
		.iter()
		.filter_map(|declaration| {
			let Declaration::ImplFor {
				visibility,
				generics,
				mutable,
				type_,
				for_interface: (interface_name, source_arguments),
				members,
			} = declaration
			else {
				return None;
			};
			let temporary_owner = DefinitionId::new(
				headers.module.clone(),
				DeclarationKey::top_level(DeclarationCategory::Namespace, "$recovered_impl"),
			);
			let temporary_binders = poison_binders(temporary_owner, generics);
			let self_type = recover_ast_type(&type_.0, headers, &temporary_binders);
			let interface_arguments = source_arguments
				.iter()
				.enumerate()
				.map(|(index, argument)| {
					(
						argument
							.0
							.name
							.as_ref()
							.map(|name| name.0.clone())
							.unwrap_or_else(|| format!("${index}").into()),
						recover_ast_type(&argument.0.value.0, headers, &temporary_binders),
					)
				})
				.collect::<Vec<_>>();
			let constraints = recover_generic_constraints(generics, &temporary_binders, headers);
			let recovered_header = crate::RecoveredImplementationHeader {
				interface: Some(interface_name.0.clone()),
				interface_arguments: source_arguments
					.iter()
					.enumerate()
					.map(|(index, argument)| {
						(
							argument
								.0
								.name
								.as_ref()
								.map(|name| name.0.clone())
								.unwrap_or_else(|| format!("${index}").into()),
							recovered_source_header_type(&argument.0.value.0, &temporary_binders),
						)
					})
					.collect(),
				self_type: recovered_source_header_type(&type_.0, &temporary_binders),
				mutable: *mutable,
				binders: (0..temporary_binders.len())
					.map(|index| HeaderBinder {
						parameter: HeaderParameterId(index as u32),
					})
					.collect(),
				constraints: recovered_source_header_constraints(generics, &temporary_binders, 0),
			};
			let complete_header = (|| {
				let interface = headers.id(&interface_name.0)?;
				let self_type = recovered_known(&self_type)?;
				let interface_arguments = interface_arguments
					.iter()
					.map(|(name, ty)| {
						Some((
							name.clone(),
							header_type(recovered_known(ty)?, &temporary_binders),
						))
					})
					.collect::<Option<Vec<_>>>()?;
				let constraints = constraints
					.iter()
					.map(|constraint| {
						let crate::RecoveredDefinitionReference::Known(interface) = &constraint.interface
						else {
							return None;
						};
						Some(crate::HeaderConstraint {
							parameter: HeaderParameterId(
								temporary_binders
									.iter()
									.position(|binder| binder.id == constraint.parameter)? as u32,
							),
							interface: interface.clone(),
							positional: constraint
								.positional
								.iter()
								.map(|ty| Some(header_type(recovered_known(ty)?, &temporary_binders)))
								.collect::<Option<Vec<_>>>()?,
							named: constraint
								.named
								.iter()
								.map(|(name, ty)| {
									Some((
										name.clone(),
										header_type(recovered_known(ty)?, &temporary_binders),
									))
								})
								.collect::<Option<Vec<_>>>()?,
						})
					})
					.collect::<Option<Vec<_>>>()?;
				Some(ImplementationHeader {
					interface: Some(interface),
					interface_arguments,
					self_type: header_type(self_type, &temporary_binders),
					mutable: *mutable,
					binders: recovered_header.binders.clone(),
					constraints,
				})
			})();
			let key = complete_header
				.map(DeclarationKey::implementation)
				.unwrap_or_else(|| DeclarationKey::recovered_implementation(recovered_header));
			let id = ids.allocate(key);
			let binders = poison_binders(id.clone(), generics);
			let self_type = recover_ast_type(&type_.0, headers, &binders);
			let interface_arguments = source_arguments
				.iter()
				.enumerate()
				.map(|(index, argument)| {
					(
						argument
							.0
							.name
							.as_ref()
							.map(|n| n.0.clone())
							.unwrap_or_else(|| format!("${index}").into()),
						recover_ast_type(&argument.0.value.0, headers, &binders),
					)
				})
				.collect();
			let mut member_ids = StableIdBuilder::new(headers.module.clone());
			Some(RecoveredExportedImpl {
				id: id.clone(),
				visibility: *visibility,
				availability: SemanticAvailability::Available,
				interface: Some(
					headers
						.id(&interface_name.0)
						.map(crate::RecoveredDefinitionReference::Known)
						.unwrap_or(crate::RecoveredDefinitionReference::Poison),
				),
				interface_arguments,
				self_type,
				mutable: *mutable,
				binders: binders.clone(),
				constraints: recover_generic_constraints(generics, &binders, headers),
				members: recover_members(members, &id, headers, &binders, &mut member_ids),
				member_slots: Vec::new(),
				runtime_owner: Some(id),
			})
		})
		.collect::<Vec<_>>();
	for declaration in &module.members {
		let Declaration::Impl {
			visibility,
			generics,
			mutable,
			type_,
			members,
		} = declaration
		else {
			continue;
		};
		let temporary = DefinitionId::new(
			headers.module.clone(),
			DeclarationKey::top_level(DeclarationCategory::Namespace, "$recovered_inherent"),
		);
		let temporary_binders = poison_binders(temporary, generics);
		let header = crate::RecoveredImplementationHeader {
			interface: None,
			interface_arguments: Vec::new(),
			self_type: recovered_source_header_type(&type_.0, &temporary_binders),
			mutable: *mutable,
			binders: (0..temporary_binders.len())
				.map(|index| HeaderBinder {
					parameter: HeaderParameterId(index as u32),
				})
				.collect(),
			constraints: recovered_source_header_constraints(generics, &temporary_binders, 0),
		};
		let id = ids.allocate(DeclarationKey::recovered_implementation(header));
		let binders = poison_binders(id.clone(), generics);
		let mut member_ids = StableIdBuilder::new(headers.module.clone());
		recovered.push(RecoveredExportedImpl {
			id: id.clone(),
			visibility: *visibility,
			availability: SemanticAvailability::Available,
			interface: None,
			interface_arguments: Vec::new(),
			self_type: recover_ast_type(&type_.0, headers, &binders),
			mutable: *mutable,
			binders: binders.clone(),
			constraints: recover_generic_constraints(generics, &binders, headers),
			members: recover_members(members, &id, headers, &binders, &mut member_ids),
			member_slots: Vec::new(),
			runtime_owner: Some(id),
		});
	}
	for declaration in &module.members {
		let (name, owner_generics, nested) = match declaration {
			Declaration::Struct {
				name,
				generics,
				impls,
				..
			}
			| Declaration::Enum {
				name,
				generics,
				impls,
				..
			} => (name, generics, impls),
			_ => continue,
		};
		let Some(owner_id) = headers.id(&name.0) else {
			continue;
		};
		for implementation in nested {
			let temporary_owner = DefinitionId::new(
				headers.module.clone(),
				DeclarationKey::top_level(DeclarationCategory::Namespace, "$recovered_nested_impl"),
			);
			let temporary_binders = owner_generics
				.iter()
				.chain(&implementation.0.generics)
				.enumerate()
				.map(|(index, generic)| GenericParameter {
					id: GenericParameterId::new(
						temporary_owner.binder(BinderScope::Definition, 0),
						index as u32,
					),
					name: generic.0.name.0.clone(),
				})
				.collect::<Vec<_>>();
			let id = ids.allocate(DeclarationKey::recovered_implementation(
				crate::RecoveredImplementationHeader {
					interface: Some(implementation.0.interface.0.0.clone()),
					interface_arguments: implementation
						.0
						.interface
						.1
						.iter()
						.enumerate()
						.map(|(index, argument)| {
							(
								argument
									.0
									.name
									.as_ref()
									.map(|name| name.0.clone())
									.unwrap_or_else(|| format!("${index}").into()),
								recovered_source_header_type(&argument.0.value.0, &temporary_binders),
							)
						})
						.collect(),
					self_type: crate::RecoveredHeaderType::Reference {
						name: name.0.clone(),
						positional: temporary_binders[..owner_generics.len()]
							.iter()
							.enumerate()
							.map(|(index, _)| {
								crate::RecoveredHeaderType::Generic(HeaderParameterId(index as u32))
							})
							.collect(),
						named: Vec::new(),
					},
					mutable: false,
					binders: (0..temporary_binders.len())
						.map(|index| HeaderBinder {
							parameter: HeaderParameterId(index as u32),
						})
						.collect(),
					constraints: recovered_source_header_constraints(
						&implementation.0.generics,
						&temporary_binders,
						owner_generics.len(),
					),
				},
			));
			let binders = owner_generics
				.iter()
				.chain(&implementation.0.generics)
				.enumerate()
				.map(|(index, generic)| GenericParameter {
					id: GenericParameterId::new(id.binder(BinderScope::Definition, 0), index as u32),
					name: generic.0.name.0.clone(),
				})
				.collect::<Vec<_>>();
			let self_type = RecoveredInterfaceType::Known(InterfaceType::Named {
				definition: owner_id.clone(),
				positional: binders[..owner_generics.len()]
					.iter()
					.map(|binder| InterfaceType::Generic(binder.id.clone()))
					.collect(),
				named: Vec::new(),
			});
			let arguments = implementation
				.0
				.interface
				.1
				.iter()
				.enumerate()
				.map(|(index, argument)| {
					(
						argument
							.0
							.name
							.as_ref()
							.map(|n| n.0.clone())
							.unwrap_or_else(|| format!("${index}").into()),
						recover_ast_type(&argument.0.value.0, headers, &binders),
					)
				})
				.collect();
			let mut member_ids = StableIdBuilder::new(headers.module.clone());
			recovered.push(RecoveredExportedImpl {
				id: id.clone(),
				visibility: None,
				availability: SemanticAvailability::Available,
				interface: Some(
					headers
						.id(&implementation.0.interface.0.0)
						.map(crate::RecoveredDefinitionReference::Known)
						.unwrap_or(crate::RecoveredDefinitionReference::Poison),
				),
				interface_arguments: arguments,
				self_type,
				mutable: false,
				binders: binders.clone(),
				constraints: recover_generic_constraints(
					&implementation.0.generics,
					&binders[owner_generics.len()..],
					headers,
				),
				members: recover_members(
					&implementation.0.members,
					&id,
					headers,
					&binders,
					&mut member_ids,
				),
				member_slots: Vec::new(),
				runtime_owner: Some(id),
			});
		}
	}
	recovered
}

fn poison_definition(
	declaration: &Declaration,
	headers: &DeclaredHeaders,
) -> Option<RecoveredExportedDefinition> {
	let (_, name) = declaration_identity(declaration)?;
	let visibility = match declaration {
		Declaration::Func { visibility, .. }
		| Declaration::Let { visibility, .. }
		| Declaration::Struct { visibility, .. }
		| Declaration::Enum { visibility, .. }
		| Declaration::TypeAlias { visibility, .. }
		| Declaration::Interface { visibility, .. }
		| Declaration::Namespace { visibility, .. }
		| Declaration::ExternalFunc(visibility, ..)
		| Declaration::ExternalLet(visibility, ..) => *visibility,
		_ => None,
	};
	let (kind, binders, parameters, return_type, ty, fields, variants, members) = match declaration {
		Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => (
			DefinitionShapeKind::Function,
			poison_binders(headers.id(name)?, &meta.generics),
			meta
				.params
				.iter()
				.map(|p| ParameterShape {
					name: None,
					ty: RecoveredInterfaceType::Poison,
					mutable: p.0.mutable,
					spread: p.0.spread,
				})
				.collect(),
			Some(RecoveredInterfaceType::Poison),
			None,
			Vec::new(),
			Vec::new(),
			Vec::new(),
		),
		Declaration::Let { .. } | Declaration::ExternalLet(..) => (
			DefinitionShapeKind::Let,
			Vec::new(),
			Vec::new(),
			None,
			Some(RecoveredInterfaceType::Poison),
			Vec::new(),
			Vec::new(),
			Vec::new(),
		),
		Declaration::TypeAlias { meta, .. } => (
			DefinitionShapeKind::TypeAlias,
			poison_binders(headers.id(name)?, &meta.generics),
			Vec::new(),
			None,
			Some(RecoveredInterfaceType::Poison),
			Vec::new(),
			Vec::new(),
			Vec::new(),
		),
		Declaration::Struct {
			generics,
			fields,
			members,
			..
		} => {
			let owner = headers.id(name)?;
			let mut ids = StableIdBuilder::new(headers.module.clone());
			(
				DefinitionShapeKind::Struct,
				poison_binders(owner.clone(), generics),
				Vec::new(),
				None,
				None,
				fields
					.iter()
					.map(|field| FieldShape {
						id: ids.allocate(DeclarationKey::member(
							owner.clone(),
							DeclarationCategory::Field,
							field.0.name.0.clone(),
						)),
						name: field.0.name.0.clone(),
						visibility: field.0.visibility,
						ty: recover_ast_type(
							&field.0.type_.0,
							headers,
							&poison_binders(owner.clone(), generics),
						),
						mutable: false,
						has_default: field.0.default.is_some(),
					})
					.collect(),
				Vec::new(),
				recover_members(
					members,
					&owner,
					headers,
					&poison_binders(owner.clone(), generics),
					&mut ids,
				),
			)
		}
		Declaration::Enum {
			generics,
			variants,
			members,
			..
		} => {
			let owner = headers.id(name)?;
			let binders = poison_binders(owner.clone(), generics);
			let mut ids = StableIdBuilder::new(headers.module.clone());
			let variants = variants
				.iter()
				.map(|variant| {
					let id = ids.allocate(DeclarationKey::member(
						owner.clone(),
						DeclarationCategory::Variant,
						variant.0.name.0.clone(),
					));
					VariantShape {
						id: id.clone(),
						name: variant.0.name.0.clone(),
						fields: variant
							.0
							.fields
							.iter()
							.map(|field| FieldShape {
								id: ids.allocate(DeclarationKey::member(
									id.clone(),
									DeclarationCategory::Field,
									field.0.name.0.clone(),
								)),
								name: field.0.name.0.clone(),
								visibility: field.0.visibility,
								ty: recover_ast_type(&field.0.type_.0, headers, &binders),
								mutable: false,
								has_default: field.0.default.is_some(),
							})
							.collect(),
					}
				})
				.collect();
			(
				DefinitionShapeKind::Enum,
				binders.clone(),
				Vec::new(),
				None,
				None,
				Vec::new(),
				variants,
				recover_members(members, &owner, headers, &binders, &mut ids),
			)
		}
		Declaration::Interface {
			generics, members, ..
		} => {
			let owner = headers.id(name)?;
			let binders = poison_binders(owner.clone(), generics);
			let mut ids = StableIdBuilder::new(headers.module.clone());
			let elements = members
				.iter()
				.filter_map(|member| match &member.0 {
					nymph_ast::decl::InterfaceMember::Element(element) => Some(&element.0),
					_ => None,
				})
				.map(|element| match element {
					nymph_ast::decl::InterfaceElement::Func { meta, body } => recover_func_member(
						meta,
						None,
						&owner,
						headers,
						&binders,
						&mut ids,
						body.is_some(),
					),
					nymph_ast::decl::InterfaceElement::Let { meta, value } => {
						let synthetic = ImplMember::Let {
							visibility: None,
							meta: meta.clone(),
							value: value.clone().unwrap_or_else(|| {
								nymph_ast::expr::Expr::new(
									nymph_ast::expr::ExprKind::Tuple(Vec::new()),
									nymph_ast::Span::new(0, 0),
									nymph_ast::NodeId::DUMMY,
								)
							}),
						};
						recover_value_member(
							&synthetic,
							&owner,
							headers,
							&binders,
							&mut ids,
							value.is_some(),
						)
					}
				})
				.collect();
			(
				DefinitionShapeKind::Interface,
				binders,
				Vec::new(),
				None,
				None,
				Vec::new(),
				Vec::new(),
				elements,
			)
		}
		Declaration::Namespace { members, .. } => {
			let owner = headers.id(name)?;
			let mut ids = StableIdBuilder::new(headers.module.clone());
			(
				DefinitionShapeKind::Namespace,
				Vec::new(),
				Vec::new(),
				None,
				None,
				Vec::new(),
				Vec::new(),
				recover_members(members, &owner, headers, &[], &mut ids),
			)
		}
		Declaration::Import { .. } | Declaration::Impl { .. } | Declaration::ImplFor { .. } => {
			return None;
		}
	};
	let mut recovered = RecoveredExportedDefinition {
		id: headers.id(name)?,
		name: name.clone(),
		visibility,
		kind,
		availability: SemanticAvailability::Available,
		binders,
		constraints: Vec::new(),
		parameters,
		return_type,
		ty,
		fields,
		variants,
		members,
		super_interfaces: Vec::new(),
		external: None,
		runtime_owner: None,
	};
	let generics: &[nymph_ast::Spanned<nymph_ast::ty::GenericParam>] = match declaration {
		Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => &meta.generics,
		Declaration::TypeAlias { meta, .. } => &meta.generics,
		Declaration::Struct { generics, .. }
		| Declaration::Enum { generics, .. }
		| Declaration::Interface { generics, .. } => generics,
		_ => &[],
	};
	recovered.constraints = recover_generic_constraints(generics, &recovered.binders, headers);
	if let Declaration::Interface {
		super_interfaces, ..
	} = declaration
	{
		recovered.super_interfaces = super_interfaces
			.iter()
			.filter_map(|super_interface| {
				let (name, arguments) = &super_interface.0;
				let interface = headers
					.id(&name.0)
					.map(crate::RecoveredDefinitionReference::Known)
					.unwrap_or(crate::RecoveredDefinitionReference::Poison);
				let mut positional = Vec::new();
				let mut named = Vec::new();
				for argument in arguments {
					let ty = recover_ast_type(&argument.0.value.0, headers, &recovered.binders);
					if let Some(name) = &argument.0.name {
						named.push((name.0.clone(), ty));
					} else {
						positional.push(ty);
					}
				}
				Some(SuperInterfaceShape {
					interface,
					positional,
					named,
				})
			})
			.collect();
	}
	Some(recovered)
}

pub fn recover_module_environment(
	module_identity: ModuleIdentity,
	module: &Module,
	checked: &Checked,
	headers: &DeclaredHeaders,
) -> ModuleEnvironment {
	recover_module_environment_with_facts(
		module_identity,
		module,
		checked,
		headers,
		&ExtractionFactSelection::all(checked),
	)
}

pub fn recover_module_environment_with_facts(
	module_identity: ModuleIdentity,
	module: &Module,
	checked: &Checked,
	headers: &DeclaredHeaders,
	facts: &ExtractionFactSelection,
) -> ModuleEnvironment {
	if !checked
		.diags
		.iter()
		.any(nymph_diagnostics::Diagnostic::is_error)
	{
		if let Ok(interface) =
			extract_module_interface_with_facts(module_identity.clone(), module, checked, headers, facts)
		{
			return ModuleEnvironment::Complete(interface);
		}
	}
	let context = context(checked, headers);
	let exports: Vec<RecoveredExportedDefinition> = module
		.members
		.iter()
		.filter_map(|declaration| {
			extract_definition(declaration, checked, headers, &context)
				.ok()
				.flatten()
				.filter(|d| visible(d.visibility))
				.map(RecoveredExportedDefinition::from)
				.or_else(|| poison_definition(declaration, headers))
				.filter(|definition| visible(definition.visibility))
		})
		.collect();
	let mut private = module
		.members
		.iter()
		.filter_map(|declaration| poison_definition(declaration, headers))
		.filter(|definition| !visible(definition.visibility))
		.map(|definition| (definition.id.clone(), definition))
		.collect::<HashMap<_, _>>();
	let mut implementations = recover_implementations(module, checked, headers, facts);
	for implementation in &mut implementations {
		let Some(crate::RecoveredDefinitionReference::Known(interface_id)) = &implementation.interface
		else {
			continue;
		};
		let Some(interface_shape) = exports
			.iter()
			.chain(private.values())
			.find(|definition| &definition.id == interface_id)
		else {
			continue;
		};
		implementation.member_slots = interface_shape
			.members
			.iter()
			.filter_map(|interface_member| {
				let override_member = implementation.members.iter().find(|member| {
					member.name == interface_member.name && member.kind == interface_member.kind
				});
				let (member_id, body_definition_id, source) = if let Some(member) = override_member {
					(
						member.id.clone(),
						member.id.clone(),
						crate::ImplementationMemberSource::Override,
					)
				} else if interface_member.has_default {
					(
						DefinitionId::new(
							implementation.id.module.clone(),
							DeclarationKey::materialized_interface_member(
								implementation.id.clone(),
								interface_member.id.clone(),
							),
						),
						interface_member.id.clone(),
						crate::ImplementationMemberSource::InheritedDefault,
					)
				} else {
					return None;
				};
				Some(crate::ImplementationMemberSlot {
					implementation_id: implementation.id.clone(),
					interface_member_id: interface_member.id.clone(),
					member_id,
					body_definition_id,
					placement_owner: implementation.id.clone(),
					kind: interface_member.kind,
					name: interface_member.name.clone(),
					source,
				})
			})
			.collect();
	}
	let mut needed = HashSet::new();
	for definition in &exports {
		referenced_recovered_definition_shape(definition, &mut needed);
	}
	for implementation in &implementations {
		referenced_recovered_impl_shape(implementation, &mut needed);
	}
	let mut queue = needed.into_iter().collect::<VecDeque<_>>();
	let mut support_definitions = Vec::new();
	let mut seen = HashSet::new();
	while let Some(id) = queue.pop_front() {
		if !seen.insert(id.clone()) {
			continue;
		}
		if let Some(definition) = private.remove(&id) {
			let mut nested = HashSet::new();
			referenced_recovered_definition_shape(&definition, &mut nested);
			queue.extend(nested);
			support_definitions.push(crate::RecoveredSupportDefinition { definition });
		}
	}
	support_definitions.sort_by(|left, right| left.definition.id.cmp(&right.definition.id));
	let mut recovered = RecoveredModuleInterface {
		module: module_identity,
		exports,
		support_definitions,
		implementations,
		fingerprint: 0,
	};
	recovered.fingerprint = recovered.structural_fingerprint();
	ModuleEnvironment::Recovered(recovered)
}

fn recovered_type_references(ty: &RecoveredInterfaceType, out: &mut HashSet<DefinitionId>) {
	if let RecoveredInterfaceType::Known(ty) = ty {
		referenced_definitions(ty, out);
	}
}

fn recovered_reference(
	reference: &crate::RecoveredDefinitionReference,
	out: &mut HashSet<DefinitionId>,
) {
	if let crate::RecoveredDefinitionReference::Known(id) = reference {
		out.insert(id.clone());
	}
}

fn referenced_recovered_member(
	member: &crate::RecoveredMemberShape,
	out: &mut HashSet<DefinitionId>,
) {
	for constraint in &member.constraints {
		recovered_reference(&constraint.interface, out);
		for ty in constraint
			.positional
			.iter()
			.chain(constraint.named.iter().map(|(_, ty)| ty))
		{
			recovered_type_references(ty, out);
		}
	}
	for ty in member
		.parameters
		.iter()
		.map(|parameter| &parameter.ty)
		.chain(std::iter::once(&member.return_type))
	{
		recovered_type_references(ty, out);
	}
}

fn referenced_recovered_definition_shape(
	definition: &RecoveredExportedDefinition,
	out: &mut HashSet<DefinitionId>,
) {
	for constraint in &definition.constraints {
		recovered_reference(&constraint.interface, out);
		for ty in constraint
			.positional
			.iter()
			.chain(constraint.named.iter().map(|(_, ty)| ty))
		{
			recovered_type_references(ty, out);
		}
	}
	for super_interface in &definition.super_interfaces {
		recovered_reference(&super_interface.interface, out);
		for ty in super_interface
			.positional
			.iter()
			.chain(super_interface.named.iter().map(|(_, ty)| ty))
		{
			recovered_type_references(ty, out);
		}
	}
	for ty in definition
		.parameters
		.iter()
		.map(|parameter| &parameter.ty)
		.chain(definition.return_type.iter())
		.chain(definition.ty.iter())
		.chain(definition.fields.iter().map(|field| &field.ty))
		.chain(
			definition
				.variants
				.iter()
				.flat_map(|variant| variant.fields.iter().map(|field| &field.ty)),
		) {
		recovered_type_references(ty, out);
	}
	for member in &definition.members {
		referenced_recovered_member(member, out);
	}
}

fn referenced_recovered_impl_shape(
	implementation: &RecoveredExportedImpl,
	out: &mut HashSet<DefinitionId>,
) {
	if let Some(interface) = &implementation.interface {
		recovered_reference(interface, out);
	}
	for (_, ty) in &implementation.interface_arguments {
		recovered_type_references(ty, out);
	}
	recovered_type_references(&implementation.self_type, out);
	for constraint in &implementation.constraints {
		recovered_reference(&constraint.interface, out);
		for ty in constraint
			.positional
			.iter()
			.chain(constraint.named.iter().map(|(_, ty)| ty))
		{
			recovered_type_references(ty, out);
		}
	}
	for member in &implementation.members {
		referenced_recovered_member(member, out);
	}
}

impl From<ExportedDefinition> for RecoveredExportedDefinition {
	fn from(value: ExportedDefinition) -> Self {
		Self {
			id: value.id,
			name: value.name,
			visibility: value.visibility,
			kind: value.kind,
			availability: SemanticAvailability::Available,
			binders: value.binders,
			constraints: value
				.constraints
				.into_iter()
				.map(|c| crate::ConstraintShape {
					parameter: c.parameter,
					interface: crate::RecoveredDefinitionReference::Known(c.interface),
					positional: c
						.positional
						.into_iter()
						.map(RecoveredInterfaceType::Known)
						.collect(),
					named: c
						.named
						.into_iter()
						.map(|(n, t)| (n, RecoveredInterfaceType::Known(t)))
						.collect(),
				})
				.collect(),
			parameters: value
				.parameters
				.into_iter()
				.map(|p| ParameterShape {
					name: p.name,
					ty: RecoveredInterfaceType::Known(p.ty),
					mutable: p.mutable,
					spread: p.spread,
				})
				.collect(),
			return_type: value.return_type.map(RecoveredInterfaceType::Known),
			ty: value.ty.map(RecoveredInterfaceType::Known),
			fields: value
				.fields
				.into_iter()
				.map(|f| FieldShape {
					id: f.id,
					name: f.name,
					visibility: f.visibility,
					ty: RecoveredInterfaceType::Known(f.ty),
					mutable: f.mutable,
					has_default: f.has_default,
				})
				.collect(),
			variants: value
				.variants
				.into_iter()
				.map(|v| VariantShape {
					id: v.id,
					name: v.name,
					fields: v
						.fields
						.into_iter()
						.map(|f| FieldShape {
							id: f.id,
							name: f.name,
							visibility: f.visibility,
							ty: RecoveredInterfaceType::Known(f.ty),
							mutable: f.mutable,
							has_default: f.has_default,
						})
						.collect(),
				})
				.collect(),
			members: value
				.members
				.into_iter()
				.map(|m| MemberShape {
					id: m.id,
					name: m.name,
					visibility: m.visibility,
					kind: m.kind,
					binders: m.binders,
					constraints: m
						.constraints
						.into_iter()
						.map(|c| crate::ConstraintShape {
							parameter: c.parameter,
							interface: crate::RecoveredDefinitionReference::Known(c.interface),
							positional: c
								.positional
								.into_iter()
								.map(RecoveredInterfaceType::Known)
								.collect(),
							named: c
								.named
								.into_iter()
								.map(|(n, t)| (n, RecoveredInterfaceType::Known(t)))
								.collect(),
						})
						.collect(),
					parameters: m
						.parameters
						.into_iter()
						.map(|p| ParameterShape {
							name: p.name,
							ty: RecoveredInterfaceType::Known(p.ty),
							mutable: p.mutable,
							spread: p.spread,
						})
						.collect(),
					return_type: RecoveredInterfaceType::Known(m.return_type),
					external: m.external,
					runtime_owner: m.runtime_owner,
					has_default: m.has_default,
				})
				.collect(),
			super_interfaces: value
				.super_interfaces
				.into_iter()
				.map(|c| crate::SuperInterfaceShape {
					interface: crate::RecoveredDefinitionReference::Known(c.interface),
					positional: c
						.positional
						.into_iter()
						.map(RecoveredInterfaceType::Known)
						.collect(),
					named: c
						.named
						.into_iter()
						.map(|(n, t)| (n, RecoveredInterfaceType::Known(t)))
						.collect(),
				})
				.collect(),
			external: value.external,
			runtime_owner: value.runtime_owner,
		}
	}
}
