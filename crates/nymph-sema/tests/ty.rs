//! Tests for the interned type layer: interning identity, substitution, and the
//! occurs-check.

use ecow::EcoString;
use nymph_sema::ids::{InferVar, ParamIdx};
use nymph_sema::ty::fold::{occurs, substitute_params};
use nymph_sema::{DefId, GenericArgs, Interner, TyKind};
use rustc_hash::FxHashMap;

#[test]
fn structurally_equal_types_share_a_handle() {
	let mut i = Interner::new();
	let a = i.mk_list(i.int());
	let b = i.mk_list(i.int());
	assert_eq!(a, b, "interning `#[int]` twice must yield the same handle");

	let c = i.mk_list(i.float());
	assert_ne!(a, c, "`#[int]` and `#[float]` must differ");
}

#[test]
fn cached_primitives_are_stable() {
	let i = Interner::new();
	assert_eq!(i.int(), i.int());
	assert_ne!(i.int(), i.uint());
	assert_ne!(i.void(), i.never());
	assert_eq!(*i.kind(i.boolean()), TyKind::Boolean);
}

#[test]
fn generic_args_are_order_independent() {
	let mut i = Interner::new();
	let def = DefId(0);
	let named1 = GenericArgs::new(
		vec![],
		vec![
			(EcoString::from("Output"), i.int()),
			(EcoString::from("Other"), i.float()),
		],
	);
	let named2 = GenericArgs::new(
		vec![],
		vec![
			(EcoString::from("Other"), i.float()),
			(EcoString::from("Output"), i.int()),
		],
	);
	let a = i.mk_adt(def, named1);
	let b = i.mk_adt(def, named2);
	assert_eq!(
		a, b,
		"named args must canonicalise regardless of source order"
	);
}

#[test]
fn intersection_flattens_and_dedups() {
	let mut i = Interner::new();
	let inner = i.mk_intersection(vec![i.int(), i.float()]);
	let outer = i.mk_intersection(vec![inner, i.int(), i.boolean()]);
	match i.kind(outer).clone() {
		TyKind::Intersection(parts) => {
			assert_eq!(
				parts.len(),
				3,
				"int, float, boolean — int deduped, inner flattened"
			);
		}
		other => panic!("expected an intersection, got {other:?}"),
	}

	// A single-element intersection collapses to that element.
	let single = i.mk_intersection(vec![i.int()]);
	assert_eq!(single, i.int());
}

#[test]
fn substitution_replaces_rigid_params() {
	let mut i = Interner::new();
	let p0 = i.mk_param(ParamIdx(0));
	// A function `(T) -> #[T]`.
	let list_p0 = i.mk_list(p0);
	let sig = i.mk_fn(vec![p0], list_p0);

	let mut subst = FxHashMap::default();
	subst.insert(ParamIdx(0), i.int());

	let instantiated = substitute_params(&mut i, sig, &subst);
	let expected_ret = i.mk_list(i.int());
	let expected = i.mk_fn(vec![i.int()], expected_ret);
	assert_eq!(
		instantiated, expected,
		"`(T) -> #[T]` with T=int becomes `(int) -> #[int]`"
	);
}

#[test]
fn substitution_leaves_unmapped_params_alone() {
	let mut i = Interner::new();
	let p1 = i.mk_param(ParamIdx(1));
	let mut subst = FxHashMap::default();
	subst.insert(ParamIdx(0), i.int());
	assert_eq!(substitute_params(&mut i, p1, &subst), p1);
}

#[test]
fn occurs_check_detects_and_rejects() {
	let mut i = Interner::new();
	let v = InferVar(0);
	let infer = i.mk_infer(v);
	let nested = i.mk_tuple(vec![i.int(), infer]);

	assert!(occurs(&i, v, infer), "a variable occurs in itself");
	assert!(occurs(&i, v, nested), "a variable occurs in `#(int, ?0)`");
	assert!(
		!occurs(&i, InferVar(1), nested),
		"a different variable does not occur"
	);

	let no_vars = i.mk_list(i.int());
	assert!(!occurs(&i, v, no_vars), "no variable occurs in `#[int]`");
}
