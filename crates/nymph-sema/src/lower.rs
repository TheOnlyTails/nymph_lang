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
	decl::Declaration,
	ty::{GenericArg, GenericParam, Type},
};
use rustc_hash::FxHashMap;

use crate::check::Checker;
use crate::def::{DefKind, EnumSig, FuncParamSig, FuncSig, StructSig, VariantSig};
use crate::ids::{DefId, ParamIdx};
use crate::ty::{GenericArgs, Ty};

/// Map a generic parameter list to a name → [`ParamIdx`] scope. The `i`-th
/// declared parameter is `ParamIdx(i)`.
pub(crate) fn build_param_scope(
	generics: &[Spanned<GenericParam>],
) -> FxHashMap<EcoString, ParamIdx> {
	generics
		.iter()
		.enumerate()
		.map(|(i, g)| (g.0.name.0.clone(), ParamIdx(i as u32)))
		.collect()
}

fn generic_names(generics: &[Spanned<GenericParam>]) -> Vec<EcoString> {
	generics.iter().map(|g| g.0.name.0.clone()).collect()
}

impl Checker<'_> {
	/// Lower every top-level definition's signature into semantic types.
	pub(crate) fn lower_signatures(&mut self) {
		let module = self.module;
		let defs: Vec<(DefId, DefKind)> = self
			.defs
			.defs
			.iter()
			.enumerate()
			.map(|(i, d)| (DefId(i as u32), d.kind))
			.collect();

		for (id, kind) in defs {
			match kind {
				DefKind::Struct { member } => {
					let Declaration::Struct {
						generics, fields, ..
					} = &module.members[member]
					else {
						continue;
					};
					let scope = build_param_scope(generics);
					let names = generic_names(generics);
					self.push_params(scope);
					let fields: Vec<(EcoString, Ty)> = fields
						.iter()
						.map(|f| (f.0.name.0.clone(), self.lower_type(&f.0.type_)))
						.collect();
					self.pop_params();
					self.sigs.structs.insert(
						id,
						StructSig {
							generics: names,
							fields,
						},
					);
				}
				DefKind::Enum { member } => {
					let Declaration::Enum {
						generics, variants, ..
					} = &module.members[member]
					else {
						continue;
					};
					let scope = build_param_scope(generics);
					let names = generic_names(generics);
					self.push_params(scope);
					let variants: Vec<VariantSig> = variants
						.iter()
						.map(|v| VariantSig {
							name: v.0.name.0.clone(),
							fields: v
								.0
								.fields
								.iter()
								.map(|f| (f.0.name.0.clone(), self.lower_type(&f.0.type_)))
								.collect(),
						})
						.collect();
					self.pop_params();
					self.sigs.enums.insert(
						id,
						EnumSig {
							generics: names,
							variants,
						},
					);
				}
				DefKind::Func { member } => {
					let meta = match &module.members[member] {
						Declaration::Func { meta, .. } => meta,
						Declaration::ExternalFunc(_, _, meta) => meta,
						_ => continue,
					};
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
					self.pop_params();
					self.sigs.funcs.insert(
						id,
						FuncSig {
							generics: names,
							params,
							ret,
							has_self: false,
						},
					);
				}
				DefKind::Let { member } => {
					let meta = match &module.members[member] {
						Declaration::Let { meta, .. } => meta,
						Declaration::ExternalLet(_, _, meta) => meta,
						_ => continue,
					};
					let ty = match &meta.type_ {
						Some(ty) => self.lower_type(ty),
						None => self.fresh(),
					};
					self.sigs.lets.insert(id, ty);
				}
				// Type aliases are expanded on demand from the AST (see `expand_alias`),
				// so they need no pre-lowered signature. Variants are lowered with
				// their enum; namespaces are Milestone B.
				DefKind::TypeAlias { .. }
				| DefKind::Variant { .. }
				| DefKind::Namespace { .. }
				| DefKind::Interface { .. } => {}
			}
		}
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
		if let Some(idx) = self.lookup_param(&name.0) {
			if !generics.is_empty() {
				self.emit(
					span,
					TypeError::GenericParamWithArgs {
						name: name.0.clone(),
					},
				);
			}
			return self.interner.mk_param(idx);
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

		match self.defs.data(def).kind {
			DefKind::Struct { .. } | DefKind::Enum { .. } => {
				let args = GenericArgs::new(positional, named);
				self.interner.mk_adt(def, args)
			}
			DefKind::TypeAlias { member } => self.expand_alias(def, member, positional, named, span),
			// `impl Interface` in type position desugars to a fresh anonymous generic
			// parameter bounded by the interface (like Rust's `impl Trait`). The interface
			// arguments were lowered above to validate them; the parameter itself stands in
			// for "some type implementing it", and its bound is recorded so method calls on
			// it resolve through the interface.
			DefKind::Interface { .. } => self.mint_synthetic_param(def),
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
	/// they never collide within a signature.
	fn mint_synthetic_param(&mut self, interface: DefId) -> Ty {
		const SYNTHETIC_BASE: u32 = 1 << 28;
		let idx = ParamIdx(SYNTHETIC_BASE + self.synthetic_params);
		self.synthetic_params += 1;
		self
			.synthetic_bounds
			.entry(idx)
			.or_default()
			.push(interface);
		self.interner.mk_param(idx)
	}

	fn expand_alias(
		&mut self,
		_def: DefId,
		member: usize,
		positional: Vec<Ty>,
		named: Vec<(EcoString, Ty)>,
		span: Span,
	) -> Ty {
		if self.alias_depth > 64 {
			self.emit(span, TypeError::RecursiveTypeAlias);
			return self.interner.error();
		}
		let module = self.module;
		let Declaration::TypeAlias { meta, value, .. } = &module.members[member] else {
			return self.interner.error();
		};
		let arity = meta.generics.len();
		let mut subst: FxHashMap<ParamIdx, Ty> = FxHashMap::default();
		for (i, ty) in positional.iter().enumerate() {
			if i < arity {
				subst.insert(ParamIdx(i as u32), *ty);
			}
		}
		for (label, ty) in &named {
			if let Some(i) = meta.generics.iter().position(|g| &g.0.name.0 == label) {
				subst.insert(ParamIdx(i as u32), *ty);
			}
		}
		for i in 0..arity {
			subst
				.entry(ParamIdx(i as u32))
				.or_insert_with(|| self.fresh());
		}

		let scope = build_param_scope(&meta.generics);
		self.push_params(scope);
		self.alias_depth += 1;
		let target = self.lower_type(value);
		self.alias_depth -= 1;
		self.pop_params();

		self.subst(target, &subst, None)
	}
}
