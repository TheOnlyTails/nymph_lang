//! Lowering the surface type grammar to semantic [`Ty`]s, and building the lowered
//! [`Signatures`] of every top-level definition.
//!
//! Lowering is parameterised by the currently-active generic scope: a type
//! reference `T` becomes a rigid `Param` if `T` names a generic in scope, and a
//! nominal `Adt` if it names a struct/enum. Type aliases are expanded on demand
//! (guarded against recursion) so definition order does not matter.

use crate::errors::TypeError;
use ecow::EcoString;
use nymph_ast::{
	Span, Spanned,
	decl::{Declaration, ImplMember},
	ty::{GenericArg, GenericParam, Type},
};
use rustc_hash::FxHashMap;

use crate::check::{AliasLowerState, Checker};
use crate::def::{
	AliasSig, DefKind, EnumSig, FuncParamSig, FuncSig, NamespaceMemberSig, NamespaceSig, StructSig,
	ValueSig, VariantSig,
};
use crate::ids::{DefId, ParamIdx};
use crate::ty::{GenericArgs, Ty};

/// Map a generic parameter list to a name → [`ParamIdx`] scope. The `i`-th
/// declared parameter is `ParamIdx(i)`.
#[derive(Clone, Debug)]
pub(crate) struct GenericBinding {
	pub index: ParamIdx,
	pub declaration: Span,
	pub written_declarations: Vec<Span>,
}

pub(crate) fn build_param_scope(
	generics: &[Spanned<GenericParam>],
) -> FxHashMap<EcoString, GenericBinding> {
	build_param_scope_at(generics, 0)
}

/// Build a generic scope whose indices begin after an enclosing binder.
/// Duplicate written declarations are all retained for conservative symbol
/// provenance, while lookup continues to use the last declaration.
pub(crate) fn build_param_scope_at(
	generics: &[Spanned<GenericParam>],
	base: usize,
) -> FxHashMap<EcoString, GenericBinding> {
	let mut scope: FxHashMap<EcoString, GenericBinding> = FxHashMap::default();
	for (i, generic) in generics.iter().enumerate() {
		let declaration = generic.0.name.1;
		let mut written_declarations = scope
			.remove(&generic.0.name.0)
			.map(|binding| binding.written_declarations)
			.unwrap_or_default();
		written_declarations.push(declaration);
		scope.insert(
			generic.0.name.0.clone(),
			GenericBinding {
				index: ParamIdx((base + i) as u32),
				declaration,
				written_declarations,
			},
		);
	}
	scope
}

fn generic_names(generics: &[Spanned<GenericParam>]) -> Vec<EcoString> {
	generics.iter().map(|g| g.0.name.0.clone()).collect()
}

impl Checker<'_> {
	fn stabilize_generics(&mut self, generics: &[Spanned<GenericParam>], owner: DefId) {
		let Some(owner) = self.defs.stable(owner).cloned() else {
			return;
		};
		for (index, generic) in generics.iter().enumerate() {
			let identity = crate::GenericParameterId::new(
				owner.binder(crate::BinderScope::Definition, 0),
				index as u32,
			);
			self
				.annotations
				.stabilize_generic_declaration(generic.0.name.1, identity);
		}
	}

	/// Lower every top-level definition's signature into semantic types.
	pub(crate) fn lower_signatures(&mut self) {
		self.collecting_signatures = true;
		let module = self.module;
		let defs: Vec<(DefId, DefKind, usize)> = self
			.defs
			.defs
			.iter()
			.enumerate()
			.filter_map(|(i, d)| {
				let id = DefId(i as u32);
				self
					.defs
					.local_member(id)
					.map(|member| (id, d.kind, member))
			})
			.collect();

		for (id, kind, member) in defs {
			match kind {
				DefKind::Struct => {
					let Declaration::Struct {
						generics, fields, ..
					} = &module.members[member]
					else {
						continue;
					};
					self.stabilize_generics(generics, id);
					let scope = build_param_scope(generics);
					let names = generic_names(generics);
					self.push_params(scope);
					let fields: Vec<(EcoString, Ty)> = fields
						.iter()
						.map(|f| (f.0.name.0.clone(), self.lower_type(&f.0.type_)))
						.collect();
					let owner = self.defs.stable(id).cloned();
					let field_metadata = fields
						.iter()
						.map(|(name, _)| crate::def::FieldSigMetadata {
							target: owner.clone().map(|owner| {
								crate::DefinitionId::new(
									owner.module.clone(),
									crate::DeclarationKey::member(
										owner,
										crate::DeclarationCategory::Field,
										name.clone(),
									),
								)
							}),
							mutable: false,
							has_default: false,
						})
						.collect();
					// Lower the struct's own generics' bounds while their param scope is
					// still active (Slice 4G-b), so `Bound::ty`/`args` land in the same
					// `Param` index space as `fields` above.
					let bounds = self.lower_constraints(generics, 0);
					self.pop_params();
					self.sigs.structs.insert(
						id,
						StructSig {
							generics: names,
							fields,
							field_metadata,
							bounds,
						},
					);
				}
				DefKind::Enum => {
					let Declaration::Enum {
						generics, variants, ..
					} = &module.members[member]
					else {
						continue;
					};
					self.stabilize_generics(generics, id);
					let scope = build_param_scope(generics);
					let names = generic_names(generics);
					self.push_params(scope);
					let variants: Vec<VariantSig> = variants
						.iter()
						.enumerate()
						.map(|(variant_index, v)| {
							let target = self.defs.defs.iter().find_map(|data| match data.kind {
								DefKind::Variant { enum_def, variant }
									if enum_def == id && variant == variant_index =>
								{
									data.stable.clone()
								}
								_ => None,
							});
							let field_metadata = v
								.0
								.fields
								.iter()
								.map(|field| crate::def::FieldSigMetadata {
									target: target.clone().map(|owner| {
										crate::DefinitionId::new(
											owner.module.clone(),
											crate::DeclarationKey::member(
												owner,
												crate::DeclarationCategory::Field,
												field.0.name.0.clone(),
											),
										)
									}),
									mutable: false,
									has_default: false,
								})
								.collect();
							VariantSig {
								target,
								name: v.0.name.0.clone(),
								fields: v
									.0
									.fields
									.iter()
									.map(|f| (f.0.name.0.clone(), self.lower_type(&f.0.type_)))
									.collect(),
								field_metadata,
							}
						})
						.collect();
					// Lower the enum's own generics' bounds while their param scope is
					// still active (Slice 4G-b), same reasoning as the struct arm above.
					let bounds = self.lower_constraints(generics, 0);
					self.pop_params();
					self.sigs.enums.insert(
						id,
						EnumSig {
							generics: names,
							variants,
							bounds,
						},
					);
				}
				DefKind::Func => {
					let meta = match &module.members[member] {
						Declaration::Func { meta, .. } => meta,
						Declaration::ExternalFunc(_, _, meta) => meta,
						_ => continue,
					};
					self.stabilize_generics(&meta.generics, id);
					let scope = build_param_scope(&meta.generics);
					let names = generic_names(&meta.generics);
					self.push_params(scope);
					let params: Vec<FuncParamSig> = meta
						.params
						.iter()
						.map(|p| FuncParamSig {
							label: p.0.name.0.as_binding().map(|id| id.0.clone()),
							ty: self.lower_type(&p.0.type_),
							spread: p.0.spread,
						})
						.collect();
					let ret = match &meta.return_type {
						Some(ty) => self.lower_type(ty),
						None => self.fresh(),
					};
					// Lower the generics' own bounds while their param scope is still
					// active (Slice 4G), so `Bound::ty`/`args` land in the same `Param`
					// index space as `params`/`ret` above and a call site can substitute
					// them with the scheme's identical instantiation map.
					let bounds = self.lower_constraints(&meta.generics, 0);
					self.pop_params();
					self.sigs.funcs.insert(
						id,
						FuncSig {
							generics: names,
							params,
							ret,
							has_self: false,
							bounds,
						},
					);
				}
				DefKind::Let => {
					let meta = match &module.members[member] {
						Declaration::Let { meta, .. } => meta,
						Declaration::ExternalLet(_, _, meta) => meta,
						_ => continue,
					};
					let ty = match &meta.type_ {
						Some(ty) => self.lower_type(ty),
						None => self.fresh(),
					};
					self.sigs.lets.insert(
						id,
						ValueSig {
							generics: vec![],
							ty,
							bounds: vec![],
						},
					);
				}
				DefKind::TypeAlias => {
					self.lower_alias_signature(id, member, self.defs.data(id).span);
				}
				DefKind::Namespace => self.lower_namespace_signature(id, member),
				DefKind::Variant { .. } | DefKind::Interface => {}
			}
		}
		self.collecting_signatures = false;
	}

	/// Lower a surface type annotation to a semantic type under the active scopes.
	pub(crate) fn lower_type(&mut self, ast: &Spanned<Type>) -> Ty {
		match &ast.0 {
			Type::Int => self.interner.int(),
			Type::UInt => self.interner.uint(),
			Type::Float => self.interner.float(),
			Type::Char => self.interner.char(),
			Type::String => self.interner.string(),
			Type::Boolean => self.interner.boolean(),
			Type::Void => self.interner.void(),
			Type::Never => self.interner.never(),
			Type::SelfType => self.interner.self_ty(),
			Type::Infer => self.fresh(),
			Type::Intersection(a, b) => {
				let a = self.lower_type(a);
				let b = self.lower_type(b);
				self.interner.mk_intersection(vec![a, b])
			}
			Type::List(elem) => {
				let elem = self.lower_type(elem);
				self.interner.mk_list(elem)
			}
			Type::Tuple(items) => {
				let items = items.iter().map(|t| self.lower_type(t)).collect();
				self.interner.mk_tuple(items)
			}
			Type::Map(key, value) => {
				let key = self.lower_type(key);
				let value = self.lower_type(value);
				self.interner.mk_map(key, value)
			}
			Type::Function {
				params,
				return_type,
			} => {
				let params = params.iter().map(|(_, t)| self.lower_type(t)).collect();
				let ret = self.lower_type(return_type);
				self.interner.mk_fn(params, ret)
			}
			Type::Grouped(inner) => self.lower_type(inner),
			Type::Mut(inner) => {
				let inner = self.lower_type(inner);
				self.interner.mk_mut(inner)
			}
			Type::Reference { name, generics } => self.lower_reference(ast.1, name, generics),
		}
	}

	fn lower_reference(
		&mut self,
		span: Span,
		name: &Spanned<EcoString>,
		generics: &[Spanned<GenericArg>],
	) -> Ty {
		// A bare generic parameter in scope wins over any nominal type.
		if let Some(binding) = self.lookup_param_binding(&name.0) {
			if !generics.is_empty() {
				self.emit(
					span,
					TypeError::GenericParamWithArgs {
						name: name.0.clone(),
					},
				);
			}
			self.annotations.record_generic_symbol(
				name.1,
				crate::annotate::GenericSymbolIdentity::Local(binding.declaration),
				false,
			);
			return self.interner.mk_param(binding.index);
		}

		let mut positional = Vec::new();
		let mut named = Vec::new();
		for g in generics {
			let ty = self.lower_type(&g.0.value);
			match &g.0.name {
				Some(label) => named.push((label.0.clone(), ty)),
				None => positional.push(ty),
			}
		}

		let Some(def) = self.defs.get(&name.0) else {
			self.emit(
				span,
				TypeError::CannotFindType {
					name: name.0.clone(),
				},
			);
			return self.interner.error();
		};

		let kind = self.defs.data(def).kind;
		if let Some(owner) = self.defs.stable(def).cloned() {
			self.queue_named_generic_labels(owner, generics);
		}
		if matches!(
			kind,
			DefKind::Struct | DefKind::Enum | DefKind::TypeAlias | DefKind::Interface
		) {
			self
				.annotations
				.record_type_definition_target(name.1, self.defs.stable(def));
		}

		match kind {
			DefKind::Struct | DefKind::Enum => {
				let args = GenericArgs::new(positional, named);
				self.interner.mk_adt(def, args)
			}
			DefKind::TypeAlias => {
				if !self.sigs.aliases.contains_key(&def)
					&& self.collecting_signatures
					&& let Some(member) = self.defs.local_member(def)
				{
					self.lower_alias_signature(def, member, span);
				}
				self.expand_alias(def, positional, named, span)
			}
			// `impl Interface` in type position desugars to a fresh anonymous generic
			// parameter bounded by the interface (like Rust's `impl Trait`). The interface
			// arguments were lowered above to validate them; the parameter itself stands in
			// for "some type implementing it", and its bound is recorded so method calls on
			// it resolve through the interface.
			DefKind::Interface => self.mint_synthetic_param(def, positional, named),
			_ => {
				self.emit(
					span,
					TypeError::NotAType {
						name: name.0.clone(),
					},
				);
				self.interner.error()
			}
		}
	}

	/// Mint a fresh anonymous generic parameter for an `impl Interface` type, recording
	/// `interface` as its bound. Synthetic indices sit far above any declared generic so
	/// they never collide within a signature (see `Checker::SYNTHETIC_PARAM_BASE`, which
	/// callers instantiating a signature at a use site key off to freshen these too).
	fn mint_synthetic_param(
		&mut self,
		interface: DefId,
		positional: Vec<Ty>,
		named: Vec<(EcoString, Ty)>,
	) -> Ty {
		let idx = ParamIdx(Self::SYNTHETIC_PARAM_BASE + self.synthetic_params);
		self.synthetic_params += 1;
		let generic_names = self
			.interfaces
			.get(&interface)
			.map(|definition| definition.generics.clone())
			.unwrap_or_default();
		let args = generic_names
			.into_iter()
			.enumerate()
			.filter_map(|(position, name)| {
				positional
					.get(position)
					.copied()
					.or_else(|| {
						named
							.iter()
							.find(|(label, _)| label == &name)
							.map(|(_, ty)| *ty)
					})
					.map(|ty| (name, ty))
			})
			.collect();
		self
			.synthetic_bounds
			.entry(idx)
			.or_default()
			.push(interface);
		let ty = self.interner.mk_param(idx);
		self
			.synthetic_bound_details
			.entry(idx)
			.or_default()
			.push(crate::iface::Bound {
				ty,
				interface,
				args,
			});
		ty
	}

	fn lower_alias_signature(&mut self, def: DefId, member: usize, use_span: Span) {
		match self.alias_states.get(&def) {
			Some(AliasLowerState::Lowered) => return,
			Some(AliasLowerState::Lowering) => {
				self.emit(use_span, TypeError::RecursiveTypeAlias);
				return;
			}
			None => {}
		}
		self.alias_states.insert(def, AliasLowerState::Lowering);
		let module = self.module;
		let Declaration::TypeAlias { meta, value, .. } = &module.members[member] else {
			return;
		};
		self.stabilize_generics(&meta.generics, def);
		let scope = build_param_scope(&meta.generics);
		let generics = generic_names(&meta.generics);
		self.push_params(scope);
		let target = self.lower_type(value);
		let bounds = self.lower_constraints(&meta.generics, 0);
		self.pop_params();
		self.sigs.aliases.insert(
			def,
			AliasSig {
				generics,
				target,
				bounds,
			},
		);
		self.alias_states.insert(def, AliasLowerState::Lowered);
	}

	fn lower_namespace_signature(&mut self, def: DefId, member: usize) {
		let module = self.module;
		let Declaration::Namespace { members, .. } = &module.members[member] else {
			return;
		};
		let mut owned = NamespaceSig::default();
		let mut member_spans = FxHashMap::default();
		let namespace_name = self.defs.data(def).name.clone();
		for member in members {
			match &member.0 {
				ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => {
					if let Some(previous) = member_spans.insert(meta.name.0.clone(), meta.name.1) {
						self.emit(
							meta.name.1,
							TypeError::DuplicateMember {
								name: meta.name.0.clone(),
								ty: namespace_name.to_string(),
								redefined_span: meta.name.1,
								prev: previous,
							},
						);
					}
					let scope = build_param_scope(&meta.generics);
					let generics = generic_names(&meta.generics);
					self.push_params(scope);
					let params = meta
						.params
						.iter()
						.map(|param| FuncParamSig {
							label: param.0.name.0.as_binding().map(|name| name.0.clone()),
							ty: self.lower_type(&param.0.type_),
							spread: param.0.spread,
						})
						.collect();
					let ret = match &meta.return_type {
						Some(ty) => self.lower_type(ty),
						None => self.fresh(),
					};
					let bounds = self.lower_constraints(&meta.generics, 0);
					self.pop_params();
					owned.members.insert(
						meta.name.0.clone(),
						NamespaceMemberSig::Func {
							target: None,
							sig: FuncSig {
								generics,
								params,
								ret,
								has_self: false,
								bounds,
							},
						},
					);
				}
				ImplMember::Let { meta, .. } | ImplMember::ExternalLet(_, _, meta) => {
					let Some(name) = meta.name.0.as_binding() else {
						continue;
					};
					if let Some(previous) = member_spans.insert(name.0.clone(), meta.name.1) {
						self.emit(
							meta.name.1,
							TypeError::DuplicateMember {
								name: name.0.clone(),
								ty: namespace_name.to_string(),
								redefined_span: meta.name.1,
								prev: previous,
							},
						);
					}
					let ty = match &meta.type_ {
						Some(ty) => self.lower_type(ty),
						None => self.fresh(),
					};
					owned.members.insert(
						name.0.clone(),
						NamespaceMemberSig::Value {
							target: None,
							ty,
							mutable: meta.is_mutable(),
						},
					);
				}
			}
		}
		self.sigs.namespaces.insert(def, owned);
	}

	fn expand_alias(
		&mut self,
		def: DefId,
		positional: Vec<Ty>,
		named: Vec<(EcoString, Ty)>,
		_span: Span,
	) -> Ty {
		let Some(sig) = self.sigs.aliases.get(&def).cloned() else {
			return self.interner.error();
		};
		let arity = sig.generics.len();
		let mut subst: FxHashMap<ParamIdx, Ty> = FxHashMap::default();
		for (i, ty) in positional.iter().enumerate() {
			if i < arity {
				subst.insert(ParamIdx(i as u32), *ty);
			}
		}
		for (label, ty) in &named {
			if let Some(i) = sig.generics.iter().position(|name| name == label) {
				subst.insert(ParamIdx(i as u32), *ty);
			}
		}
		for i in 0..arity {
			subst
				.entry(ParamIdx(i as u32))
				.or_insert_with(|| self.fresh());
		}
		self.subst(sig.target, &subst, None)
	}
}
