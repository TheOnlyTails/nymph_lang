//! Structural traversal of types: substitution and the occurs-check.
//!
//! These are the primitives the inference engine builds on. Substitution replaces
//! rigid [`ParamIdx`] skolems with concrete types when a generic signature is
//! instantiated at a use site; the occurs-check rejects the infinite types that
//! would otherwise arise from unifying a variable with a term containing it.

use rustc_hash::FxHashMap;

use super::{GenericArgs, Interner, Ty, TyKind};
use crate::ids::{InferVar, ParamIdx};

/// Replace every [`TyKind::Param`] in `ty` according to `subst`, rebuilding the type
/// through `interner`. Parameters absent from `subst` are left untouched, so a
/// partial substitution is fine (e.g. instantiating only an impl's parameters).
pub fn substitute_params(interner: &mut Interner, ty: Ty, subst: &FxHashMap<ParamIdx, Ty>) -> Ty {
	if subst.is_empty() {
		return ty;
	}
	// Clone the kind so we can mutate the interner while rebuilding children.
	match interner.kind(ty).clone() {
		TyKind::Param(idx) => subst.get(&idx).copied().unwrap_or(ty),

		TyKind::List(elem) => {
			let elem = substitute_params(interner, elem, subst);
			interner.mk_list(elem)
		}
		TyKind::Tuple(elems) => {
			let elems = substitute_each(interner, &elems, subst);
			interner.mk_tuple(elems)
		}
		TyKind::Map(key, value) => {
			let key = substitute_params(interner, key, subst);
			let value = substitute_params(interner, value, subst);
			interner.mk_map(key, value)
		}
		TyKind::Fn { params, ret } => {
			let params = substitute_each(interner, &params, subst);
			let ret = substitute_params(interner, ret, subst);
			interner.mk_fn(params, ret)
		}
		TyKind::Adt(def, args) => {
			let args = substitute_args(interner, &args, subst);
			interner.mk_adt(def, args)
		}
		TyKind::Intersection(parts) => {
			let parts = substitute_each(interner, &parts, subst);
			interner.mk_intersection(parts)
		}

		// Primitives, `self`, inference variables, error, and unmapped params are
		// all leaves with nothing to rewrite.
		TyKind::Int
		| TyKind::UInt
		| TyKind::Float
		| TyKind::Char
		| TyKind::String
		| TyKind::Boolean
		| TyKind::Void
		| TyKind::Never
		| TyKind::SelfTy
		| TyKind::Infer(_)
		| TyKind::Error => ty,
	}
}

fn substitute_each(
	interner: &mut Interner,
	tys: &[Ty],
	subst: &FxHashMap<ParamIdx, Ty>,
) -> Vec<Ty> {
	tys
		.iter()
		.map(|&t| substitute_params(interner, t, subst))
		.collect()
}

fn substitute_args(
	interner: &mut Interner,
	args: &GenericArgs,
	subst: &FxHashMap<ParamIdx, Ty>,
) -> GenericArgs {
	let positional = substitute_each(interner, &args.positional, subst);
	let named = args
		.named
		.iter()
		.map(|(name, t)| (name.clone(), substitute_params(interner, *t, subst)))
		.collect();
	// Already canonical order; rebuild directly to avoid re-sorting.
	GenericArgs { positional, named }
}

/// The occurs-check: does the inference variable `var` occur anywhere within `ty`?
///
/// Rejects unification of a variable with a type containing it (prevents infinite types).
/// Caller must first resolve `ty` through the union-find table; structural walk rejects
/// if `var` appears in any nested component.
pub fn occurs(interner: &Interner, var: InferVar, ty: Ty) -> bool {
	match interner.kind(ty) {
		TyKind::Infer(v) => *v == var,
		TyKind::List(elem) => occurs(interner, var, *elem),
		TyKind::Tuple(elems) | TyKind::Intersection(elems) => {
			elems.iter().any(|&e| occurs(interner, var, e))
		}
		TyKind::Map(key, value) => occurs(interner, var, *key) || occurs(interner, var, *value),
		TyKind::Fn { params, ret } => {
			params.iter().any(|&p| occurs(interner, var, p)) || occurs(interner, var, *ret)
		}
		TyKind::Adt(_, args) => {
			args.positional.iter().any(|&t| occurs(interner, var, t))
				|| args.named.iter().any(|(_, t)| occurs(interner, var, *t))
		}
		TyKind::Int
		| TyKind::UInt
		| TyKind::Float
		| TyKind::Char
		| TyKind::String
		| TyKind::Boolean
		| TyKind::Void
		| TyKind::Never
		| TyKind::SelfTy
		| TyKind::Param(_)
		| TyKind::Error => false,
	}
}
