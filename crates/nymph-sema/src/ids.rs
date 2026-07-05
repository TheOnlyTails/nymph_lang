//! Stable identity newtypes used throughout semantic analysis.
//!
//! These are deliberately opaque integer handles rather than names or structural
//! values. The old checker keyed behaviour on variable *names* (`name == "self"`,
//! `name.starts_with('_')`), which tangled identity with text; here identity is an
//! integer and text is carried separately only for diagnostics.

/// A resolved top-level definition: a `func`, `struct`, `enum`, `interface`,
/// `type` alias, `let`, or `namespace`. Assigned by name resolution (`resolve/`).
///
/// This will be backed by a `#[salsa::interned]` key once the driver lands; for now
/// it is a plain index so the type layer can be built and tested in isolation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct DefId(pub u32);

/// The index of a generic parameter within the parameter list of its *enclosing*
/// item (function, impl, struct, …). A `TyKind::Param(ParamIdx)` is a **rigid**
/// type variable — a skolem the checker may not unify away, unlike an inference
/// variable. Keeping these distinct from [`InferVar`] is the single most important
/// fix over the old `Type::Variable`, which conflated the two and led to its
/// unsound "a variable is compatible with anything" rule.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ParamIdx(pub u32);

/// A flexible unification variable — a hole the inference engine is free to solve.
/// Created fresh during checking and resolved through the union-find table in
/// `infer/unify.rs`. Distinct from [`ParamIdx`] (rigid) on purpose.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct InferVar(pub u32);
