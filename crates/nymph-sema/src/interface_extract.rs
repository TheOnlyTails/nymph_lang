//! Conversion from owned checker facts to stable, diagnostic-free interfaces.

use std::collections::{HashMap, HashSet, VecDeque};

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
}

impl DeclaredHeaders {
	fn id(&self, name: &str) -> Option<DefinitionId> {
		let name = source_name(name);
		self
			.definitions
			.iter()
			.find(|(n, _)| n == name)
			.map(|(_, id)| id.clone())
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
	let definitions = module
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
			headers
				.id(&data.name)
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
			headers
				.id(&data.name)
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
				interface: headers
					.id(&checked.semantic.definitions.data(bound.interface).name)
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
	let binders = meta
		.generics
		.iter()
		.enumerate()
		.map(|(index, generic)| GenericParameter {
			id: GenericParameterId::new(id.binder(BinderScope::Member, 0), index as u32),
			name: generic.0.name.0.clone(),
		})
		.collect::<Vec<_>>();
	let parameters = owner_binders
		.iter()
		.chain(&binders)
		.enumerate()
		.map(|(index, binder)| (crate::ParamIdx(index as u32), binder.id.clone()))
		.collect();
	let definitions = context(checked, headers).definitions();
	let member_context = CanonicalizationContext::new(definitions, parameters);
	Ok(MemberShape {
		id,
		name: meta.name.0.clone(),
		visibility,
		kind: match meta.kind {
			FuncKind::Instance => MemberKind::Function,
			FuncKind::Mut => MemberKind::MutatingFunction,
			FuncKind::Namespace => MemberKind::StaticFunction,
		},
		binders,
		constraints: checked_constraints(&facts.bounds, checked, headers, &member_context)?,
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
		external: external_symbol.map(|symbol| ExternalAbi {
			marker: "external".into(),
			module: "host".into(),
			symbol: symbol.clone(),
			marshal: None,
		}),
		runtime_owner: Some(owner.clone()),
		has_default: false,
	})
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
				ImplMember::ExternalFunc(_, symbol, _) => Some(ExternalAbi {
					marker: "external".into(),
					module: "host".into(),
					symbol: symbol.clone(),
					marshal: None,
				}),
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
				ImplMember::ExternalLet(_, symbol, _) => Some(ExternalAbi {
					marker: "external".into(),
					module: "host".into(),
					symbol: symbol.clone(),
					marshal: None,
				}),
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
			let (generic_context, binders) = definition_context(
				checked,
				headers,
				&shape.id,
				checked.semantic.signatures.funcs[&def].generics.clone(),
			);
			shape.binders = binders;
			shape.constraints = generic_constraints(&meta.generics, headers, &shape.binders)?;
			function_shape(&mut shape, meta, def, checked, &generic_context)?;
			shape.runtime_owner = Some(shape.id.clone());
			if let Declaration::ExternalFunc(_, symbol, meta) = declaration {
				shape.external = Some(ExternalAbi {
					marker: "external".into(),
					module: "host".into(),
					symbol: symbol.clone(),
					marshal: checked.external_value_marshals.get(&meta.name.1).copied(),
				});
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
				checked.semantic.signatures.lets[&def],
				context,
			)?);
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
			let (generic_context, binders) = definition_context(
				checked,
				headers,
				&shape.id,
				generics.iter().map(|g| g.0.name.0.clone()),
			);
			shape.constraints = generic_constraints(generics, headers, &binders)?;
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
			let (generic_context, binders) =
				definition_context(checked, headers, &shape.id, signature.generics.clone());
			shape.binders = binders;
			shape.constraints = generic_constraints(generics, headers, &shape.binders)?;
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
) -> Result<Vec<ExportedImpl>, InterfaceConversionError> {
	let mut declarations = module
		.members
		.iter()
		.filter_map(|declaration| match declaration {
			Declaration::ImplFor {
				visibility,
				mutable,
				members,
				..
			} => Some((*visibility, *mutable, members)),
			_ => None,
		})
		.collect::<Vec<_>>();
	for declaration in &module.members {
		let impls = match declaration {
			Declaration::Struct { impls, .. } | Declaration::Enum { impls, .. } => impls,
			_ => continue,
		};
		declarations.extend(
			impls
				.iter()
				.map(|implementation| (None, false, &implementation.0.members)),
		);
	}
	let mut extracted = declarations
		.into_iter()
		.zip(&checked.semantic.implementations.impls)
		.map(|((visibility, mutable, members), implementation)| {
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
			let interface = checked.semantic.definitions.data(implementation.interface);
			let interface = headers
				.id(&interface.name)
				.expect("interface header exists");
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
			let (context, binders) =
				definition_context(checked, headers, &id, implementation.generics.clone());
			let mut member_ids = StableIdBuilder::new(headers.module.clone());
			let members = members
				.iter()
				.filter_map(|member| match &member.0 {
					ImplMember::Func {
						visibility,
						meta,
						body,
					} => Some((*visibility, meta, false, Some(body))),
					ImplMember::ExternalFunc(visibility, _, meta) => Some((*visibility, meta, true, None)),
					_ => None,
				})
				.map(|(visibility, meta, external, body)| {
					let signature = &implementation.methods[&meta.name.0];
					let facts = crate::annotate::CheckedMethod {
						params: signature.params.clone(),
						ret: signature.ret,
						bounds: signature.bounds.clone(),
					};
					let mut member = checked_member_shape(
						meta,
						visibility,
						external.then_some(&meta.name.0),
						&facts,
						&id,
						&binders,
						checked,
						headers,
						&mut member_ids,
					)?;
					member.runtime_owner = body.map(|_| id.clone());
					Ok(member)
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
				runtime_owner: Some(id),
			})
		})
		.collect::<Result<Vec<_>, _>>()?;

	let top_level_inherent = module
		.members
		.iter()
		.filter_map(|declaration| match declaration {
			Declaration::Impl {
				visibility,
				members,
				..
			} => Some((*visibility, members)),
			_ => None,
		});
	let inherent_facts = checked.semantic.inherent.iter().skip(
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
	for ((visibility, members), implementation) in top_level_inherent.zip(inherent_facts) {
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
				mutable: false,
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
		let (impl_context, binders) =
			definition_context(checked, headers, &id, implementation.generics.clone());
		let mut member_ids = StableIdBuilder::new(headers.module.clone());
		let members = members
			.iter()
			.filter_map(|member| match &member.0 {
				ImplMember::Func {
					visibility, meta, ..
				} => Some((*visibility, meta, None)),
				ImplMember::ExternalFunc(visibility, symbol, meta) => {
					Some((*visibility, meta, Some(symbol)))
				}
				_ => None,
			})
			.map(|(member_visibility, meta, symbol)| {
				checked_member_shape(
					meta,
					member_visibility,
					symbol,
					&implementation.methods[&meta.name.0],
					&id,
					&binders,
					checked,
					headers,
					&mut member_ids,
				)
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
	let implementations = extract_implementations(module, checked, headers)?;
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

fn recover_func_member(
	meta: &FuncDeclaration,
	visibility: Option<Visibility>,
	owner: &DefinitionId,
	headers: &DeclaredHeaders,
	owner_binders: &[GenericParameter],
	ids: &mut StableIdBuilder,
	has_default: bool,
) -> MemberShape<RecoveredInterfaceType> {
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
		binders,
		constraints: Vec::new(),
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
) -> MemberShape<RecoveredInterfaceType> {
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
) -> Vec<MemberShape<RecoveredInterfaceType>> {
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
	checked: &Checked,
	headers: &DeclaredHeaders,
) -> Vec<RecoveredExportedImpl> {
	let mut declarations = module
		.members
		.iter()
		.filter_map(|declaration| match declaration {
			Declaration::ImplFor {
				visibility,
				mutable,
				members,
				..
			} => Some((*visibility, *mutable, members)),
			_ => None,
		})
		.collect::<Vec<_>>();
	for declaration in &module.members {
		if let Declaration::Struct { impls, .. } | Declaration::Enum { impls, .. } = declaration {
			declarations.extend(
				impls
					.iter()
					.map(|implementation| (None, false, &implementation.0.members)),
			);
		}
	}
	declarations
		.into_iter()
		.zip(&checked.semantic.implementations.impls)
		.filter_map(|((visibility, mutable, members), implementation)| {
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
			)
			.ok()?;
			let interface = headers.id(
				&checked
					.semantic
					.definitions
					.data(implementation.interface)
					.name,
			)?;
			let arguments = implementation
				.args
				.iter()
				.map(|(name, ty)| {
					Some((
						name.clone(),
						canonicalize_type(&checked.interner, *ty, &temporary_context).ok()?,
					))
				})
				.collect::<Option<Vec<_>>>()?;
			let id = DefinitionId::new(
				headers.module.clone(),
				DeclarationKey::implementation(ImplementationHeader {
					interface: Some(interface.clone()),
					interface_arguments: arguments
						.iter()
						.map(|(name, ty)| (name.clone(), header_type(ty, &temporary_binders)))
						.collect(),
					self_type: header_type(&self_type, &temporary_binders),
					mutable,
					binders: (0..temporary_binders.len())
						.map(|index| HeaderBinder {
							parameter: HeaderParameterId(index as u32),
						})
						.collect(),
					constraints: Vec::new(),
				}),
			);
			let (_, binders) = definition_context(checked, headers, &id, implementation.generics.clone());
			let mut ids = StableIdBuilder::new(headers.module.clone());
			Some(RecoveredExportedImpl {
				id: id.clone(),
				visibility,
				availability: SemanticAvailability::Available,
				interface: Some(interface),
				interface_arguments: arguments
					.into_iter()
					.map(|(name, ty)| (name, RecoveredInterfaceType::Known(ty)))
					.collect(),
				self_type: RecoveredInterfaceType::Known(self_type),
				mutable,
				binders: binders.clone(),
				constraints: Vec::new(),
				members: recover_members(members, &id, headers, &binders, &mut ids),
				runtime_owner: Some(id),
			})
		})
		.collect()
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
	recovered.constraints = generics
		.iter()
		.zip(&recovered.binders)
		.filter_map(|(generic, binder)| {
			let bound = generic.0.constraint.as_ref()?;
			let RecoveredInterfaceType::Known(InterfaceType::Named {
				definition,
				positional,
				named,
			}) = recover_ast_type(&bound.0, headers, &recovered.binders)
			else {
				return None;
			};
			Some(crate::ConstraintShape {
				parameter: binder.id.clone(),
				interface: definition,
				positional: positional
					.into_iter()
					.map(RecoveredInterfaceType::Known)
					.collect(),
				named: named
					.into_iter()
					.map(|(name, ty)| (name, RecoveredInterfaceType::Known(ty)))
					.collect(),
			})
		})
		.collect();
	if let Declaration::Interface {
		super_interfaces, ..
	} = declaration
	{
		recovered.super_interfaces = super_interfaces
			.iter()
			.filter_map(|super_interface| {
				let (name, arguments) = &super_interface.0;
				let interface = headers.id(&name.0)?;
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
	if !checked
		.diags
		.iter()
		.any(nymph_diagnostics::Diagnostic::is_error)
	{
		return match extract_module_interface(module_identity.clone(), module, checked, headers) {
			Ok(interface) => ModuleEnvironment::Complete(interface),
			Err(_) => ModuleEnvironment::Recovered(RecoveredModuleInterface {
				module: module_identity,
				exports: Vec::new(),
				support_definitions: Vec::new(),
				implementations: Vec::<RecoveredExportedImpl>::new(),
				fingerprint: 0,
			}),
		};
	}
	let context = context(checked, headers);
	let exports = module
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
	let mut support_definitions = module
		.members
		.iter()
		.filter_map(|declaration| poison_definition(declaration, headers))
		.filter(|definition| !visible(definition.visibility))
		.map(|definition| crate::RecoveredSupportDefinition { definition })
		.collect::<Vec<_>>();
	support_definitions.sort_by(|left, right| left.definition.id.cmp(&right.definition.id));
	let mut recovered = RecoveredModuleInterface {
		module: module_identity,
		exports,
		support_definitions,
		implementations: recover_implementations(module, checked, headers),
		fingerprint: 0,
	};
	recovered.fingerprint = recovered.structural_fingerprint();
	ModuleEnvironment::Recovered(recovered)
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
					interface: c.interface,
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
							interface: c.interface,
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
					interface: c.interface,
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
