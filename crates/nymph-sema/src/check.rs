//! The type checker's core state and driver.
//!
//! `Checker` owns the interner, the resolved [`DefMap`], the lowered [`Signatures`],
//! the unification table, and the accumulated diagnostics, plus the transient
//! per-body state (local scopes, the active generic-parameter scope, the current
//! `self`/return types). The inference rules themselves live in `infer_expr.rs`,
//! `infer_pattern.rs`, `lower.rs`, and `coerce.rs` as further `impl Checker` blocks;
//! keeping them in separate files is the deliberate anti-monolith split.

use ecow::EcoString;
use nymph_ast::{Span, decl::Declaration, decl::Module};
use nymph_diagnostics::Diagnostic;
use rustc_hash::FxHashMap;

use crate::annotate::Checked;
use crate::def::{DefMap, Signatures, build_def_map};
use crate::ids::{DefId, InferVar, ParamIdx};
use crate::ty::fold::occurs;
use crate::ty::{Interner, Ty, TyKind};
use crate::unify::{TyVarValue, UnifyTable};

/// A local variable binding in a lexical scope.
pub(crate) struct Binding {
	pub ty: Ty,
	pub mutable: bool,
}

pub struct Checker<'m> {
	pub(crate) module: &'m Module,
	pub(crate) interner: Interner,
	pub(crate) defs: DefMap,
	pub(crate) sigs: Signatures,
	/// Collected interface definitions (method signatures), keyed by interface def.
	pub(crate) interfaces: FxHashMap<DefId, crate::iface::InterfaceDef>,
	/// Collected `impl` blocks, indexed for candidate lookup by the solver.
	pub(crate) impls: crate::iface::ImplRegistry,
	/// Inherent methods and statics (methods not attached to an interface), indexed
	/// by the implementing type's head constructor.
	pub(crate) inherent: crate::members::InherentRegistry<'m>,
	pub(crate) table: UnifyTable,
	pub(crate) diags: Vec<Diagnostic>,

	// ── Transient per-body state ─────────────────────────────────────────────
	pub(crate) scopes: Vec<FxHashMap<EcoString, Binding>>,
	/// Stack of generic-parameter scopes (name → rigid `ParamIdx`).
	pub(crate) params: Vec<FxHashMap<EcoString, ParamIdx>>,
	/// The interface bounds on the generic parameters of the body currently being
	/// checked (`ParamIdx` → the interfaces it must implement). Rebuilt per body; used
	/// to resolve a namespaced call through a type parameter, e.g. `R.default()` where
	/// `R: Default`.
	pub(crate) param_bounds: FxHashMap<ParamIdx, Vec<DefId>>,
	/// The type `this`/`self` refers to inside the current method, if any.
	pub(crate) self_ty: Option<Ty>,
	/// The declared/expected return type of the function currently being checked.
	pub(crate) ret_ty: Option<Ty>,
	/// Recursion guard for on-demand type-alias expansion.
	pub(crate) alias_depth: u32,
	/// Counter minting anonymous generic parameters for `impl Interface` used in type
	/// position (an interface reference desugars to a fresh generic bounded by it).
	pub(crate) synthetic_params: u32,
	/// The interface bounds on the anonymous parameters minted for `impl Interface` types,
	/// by `ParamIdx`. Unlike `param_bounds` (rebuilt per body for declared generics), these
	/// are recorded once at mint time and persist, because the parameter is baked into a
	/// stored signature and its bound must still resolve at every call site.
	pub(crate) synthetic_bounds: FxHashMap<ParamIdx, Vec<DefId>>,

	/// The per-expression decisions recorded for the lowering pass (resolved type,
	/// selected operator/method impl). Keyed by [`nymph_ast::NodeId`]. Emitted
	/// alongside diagnostics as part of [`crate::Checked`].
	pub(crate) annotations: crate::annotate::Annotations,
}

/// Check a whole (single) module and return every diagnostic produced.
///
/// This is the Milestone-A entry point. It runs the three conceptual passes in
/// order: item resolution (`build_def_map`), signature lowering, then body
/// inference. The signature/body split mirrors the incremental query boundary the
/// full salsa driver will formalise later.
pub fn check_module(module: &Module) -> Checked {
	let mut diags = Vec::new();
	let defs = build_def_map(module, &mut diags);
	let mut checker = Checker::new(module, defs, diags);
	checker.lower_signatures();
	checker.collect_interfaces();
	checker.collect_impls();
	checker.collect_inner_impls();
	checker.check_coherence();
	checker.collect_inherent();
	checker.generalize_returns();
	checker.check_bodies();
	checker.check_member_bodies();
	Checked {
		diags: checker.diags,
		annotations: checker.annotations,
	}
}

/// Check several modules together as one program.
///
/// This is the **minimal multi-module driver** (the module-resolution shim the plan
/// anticipated): it flattens every module's top-level declarations into one combined
/// module and runs the single-module checker over the result. `import` statements are
/// dropped — after flattening, every item shares a single global namespace, so an
/// `import @/x with (Y)` needs no separate binding step. This lets the cross-file
/// stdlib typecheck before the full salsa module graph (a later project-layer concern)
/// exists. It deliberately does *not* yet enforce per-module visibility or import
/// aliasing; those arrive with the real module graph.
pub fn check_program(modules: &[Module]) -> Checked {
	let mut members = Vec::new();
	for module in modules {
		for decl in &module.members {
			if matches!(decl, Declaration::Import { .. }) {
				continue;
			}
			members.push(decl.clone());
		}
	}
	let combined = Module {
		members,
		path: "<program>".into(),
	};
	check_module(&combined)
}

impl<'m> Checker<'m> {
	fn new(module: &'m Module, defs: DefMap, diags: Vec<Diagnostic>) -> Self {
		Self {
			module,
			interner: Interner::new(),
			defs,
			sigs: Signatures::default(),
			interfaces: FxHashMap::default(),
			impls: crate::iface::ImplRegistry::default(),
			inherent: crate::members::InherentRegistry::default(),
			table: UnifyTable::new(),
			diags,
			scopes: Vec::new(),
			params: Vec::new(),
			param_bounds: FxHashMap::default(),
			self_ty: None,
			ret_ty: None,
			alias_depth: 0,
			synthetic_params: 0,
			synthetic_bounds: FxHashMap::default(),
			annotations: crate::annotate::Annotations::default(),
		}
	}

	// ── Diagnostics ──────────────────────────────────────────────────────────
	pub(crate) fn error(&mut self, message: impl Into<EcoString>, span: Span) {
		self.diags.push(Diagnostic::error(message, span));
	}

	// ── Annotations ──────────────────────────────────────────────────────────
	/// Record the checker's decision about an expression node so the lowering pass
	/// can read it back. `ty` is the node's resolved type; `resolution` is set only
	/// for desugared operator/cast/method nodes (whose selected impl codegen needs).
	pub(crate) fn record(
		&mut self,
		id: nymph_ast::NodeId,
		ty: Ty,
		resolution: Option<crate::annotate::Resolution>,
	) {
		self
			.annotations
			.record(id, crate::annotate::ExprInfo { ty, resolution });
	}

	// ── Inference variables ──────────────────────────────────────────────────
	pub(crate) fn fresh(&mut self) -> Ty {
		let var = self.table.new_var();
		self.interner.mk_infer(var)
	}

	// ── Local scopes ─────────────────────────────────────────────────────────
	pub(crate) fn push_scope(&mut self) {
		self.scopes.push(FxHashMap::default());
	}

	pub(crate) fn pop_scope(&mut self) {
		self.scopes.pop();
	}

	pub(crate) fn define_local(&mut self, name: EcoString, ty: Ty, mutable: bool) {
		if let Some(scope) = self.scopes.last_mut() {
			scope.insert(name, Binding { ty, mutable });
		}
	}

	pub(crate) fn lookup_local(&self, name: &str) -> Option<&Binding> {
		self.scopes.iter().rev().find_map(|scope| scope.get(name))
	}

	// ── Generic-parameter scopes ─────────────────────────────────────────────
	pub(crate) fn push_params(&mut self, scope: FxHashMap<EcoString, ParamIdx>) {
		self.params.push(scope);
	}

	pub(crate) fn pop_params(&mut self) {
		self.params.pop();
	}

	pub(crate) fn lookup_param(&self, name: &str) -> Option<ParamIdx> {
		self
			.params
			.iter()
			.rev()
			.find_map(|scope| scope.get(name).copied())
	}

	// ── Type resolution ──────────────────────────────────────────────────────
	/// Peel a chain of bound inference variables from the top of a type. The result
	/// is either a non-variable type or an *unbound* variable (in canonical form).
	pub(crate) fn shallow_resolve(&mut self, ty: Ty) -> Ty {
		let var = match self.interner.kind(ty) {
			TyKind::Infer(v) => *v,
			_ => return ty,
		};
		match self.table.probe(var) {
			TyVarValue::Known(bound) => self.shallow_resolve(bound),
			TyVarValue::Unknown => {
				let root = self.table.root(var);
				self.interner.mk_infer(root)
			}
		}
	}

	/// Fully resolve a type, replacing every bound variable throughout. Unbound
	/// variables are left as canonical `Infer` handles.
	pub(crate) fn resolve_deep(&mut self, ty: Ty) -> Ty {
		let ty = self.shallow_resolve(ty);
		match self.interner.kind(ty).clone() {
			TyKind::List(elem) => {
				let elem = self.resolve_deep(elem);
				self.interner.mk_list(elem)
			}
			TyKind::Tuple(elems) => {
				let elems = elems.iter().map(|&e| self.resolve_deep(e)).collect();
				self.interner.mk_tuple(elems)
			}
			TyKind::Map(key, value) => {
				let key = self.resolve_deep(key);
				let value = self.resolve_deep(value);
				self.interner.mk_map(key, value)
			}
			TyKind::Fn { params, ret } => {
				let params = params.iter().map(|&p| self.resolve_deep(p)).collect();
				let ret = self.resolve_deep(ret);
				self.interner.mk_fn(params, ret)
			}
			TyKind::Adt(def, args) => {
				let positional = args
					.positional
					.iter()
					.map(|&t| self.resolve_deep(t))
					.collect();
				let named = args
					.named
					.iter()
					.map(|(n, t)| (n.clone(), self.resolve_deep(*t)))
					.collect();
				self
					.interner
					.mk_adt(def, crate::ty::GenericArgs { positional, named })
			}
			TyKind::Intersection(parts) => {
				let parts = parts.iter().map(|&p| self.resolve_deep(p)).collect();
				self.interner.mk_intersection(parts)
			}
			_ => ty,
		}
	}

	/// Whether a (deeply resolved) type still contains any inference variable.
	pub(crate) fn has_infer(&self, ty: Ty) -> bool {
		match self.interner.kind(ty) {
			TyKind::Infer(_) => true,
			TyKind::List(elem) => self.has_infer(*elem),
			TyKind::Tuple(elems) => elems.iter().any(|&e| self.has_infer(e)),
			TyKind::Map(key, value) => self.has_infer(*key) || self.has_infer(*value),
			TyKind::Fn { params, ret } => {
				params.iter().any(|&p| self.has_infer(p)) || self.has_infer(*ret)
			}
			TyKind::Adt(_, args) => {
				args.positional.iter().any(|&t| self.has_infer(t))
					|| args.named.iter().any(|(_, t)| self.has_infer(*t))
			}
			TyKind::Intersection(parts) => parts.iter().any(|&p| self.has_infer(p)),
			_ => false,
		}
	}

	// ── Substitution (instantiation) ─────────────────────────────────────────
	/// Substitute rigid parameters and `self` throughout a type. Used to instantiate
	/// a stored signature at a use site: each `ParamIdx` is mapped to a fresh
	/// inference variable (or concrete argument), and `SelfTy` to the receiver.
	pub(crate) fn subst(
		&mut self,
		ty: Ty,
		params: &FxHashMap<ParamIdx, Ty>,
		self_ty: Option<Ty>,
	) -> Ty {
		match self.interner.kind(ty).clone() {
			TyKind::Param(p) => params.get(&p).copied().unwrap_or(ty),
			TyKind::SelfTy => self_ty.unwrap_or(ty),
			TyKind::List(elem) => {
				let elem = self.subst(elem, params, self_ty);
				self.interner.mk_list(elem)
			}
			TyKind::Tuple(elems) => {
				let elems = elems
					.iter()
					.map(|&e| self.subst(e, params, self_ty))
					.collect();
				self.interner.mk_tuple(elems)
			}
			TyKind::Map(key, value) => {
				let key = self.subst(key, params, self_ty);
				let value = self.subst(value, params, self_ty);
				self.interner.mk_map(key, value)
			}
			TyKind::Fn { params: ps, ret } => {
				let ps = ps.iter().map(|&p| self.subst(p, params, self_ty)).collect();
				let ret = self.subst(ret, params, self_ty);
				self.interner.mk_fn(ps, ret)
			}
			TyKind::Adt(def, args) => {
				let positional = args
					.positional
					.iter()
					.map(|&t| self.subst(t, params, self_ty))
					.collect();
				let named = args
					.named
					.iter()
					.map(|(n, t)| (n.clone(), self.subst(*t, params, self_ty)))
					.collect();
				self
					.interner
					.mk_adt(def, crate::ty::GenericArgs { positional, named })
			}
			TyKind::Intersection(parts) => {
				let parts = parts
					.iter()
					.map(|&p| self.subst(p, params, self_ty))
					.collect();
				self.interner.mk_intersection(parts)
			}
			_ => ty,
		}
	}

	/// Build a substitution mapping a signature's generic parameters `0..n` to fresh
	/// inference variables, to be solved from the use site.
	pub(crate) fn fresh_subst(&mut self, count: usize) -> FxHashMap<ParamIdx, Ty> {
		(0..count)
			.map(|i| (ParamIdx(i as u32), self.fresh()))
			.collect()
	}

	// ── Unification helpers shared with coerce.rs ────────────────────────────
	/// Bind an unbound variable to a type, guarding against infinite types.
	pub(crate) fn bind_var(&mut self, var: InferVar, ty: Ty, span: Span) {
		if occurs(&self.interner, var, ty) {
			let rendered = self.display(ty);
			self.error(
				format!("this expression has an infinite type `{rendered}`"),
				span,
			);
			let error = self.interner.error();
			self.table.assign(var, error);
			return;
		}
		self.table.assign(var, ty);
	}

	// ── Display ──────────────────────────────────────────────────────────────
	/// Render a (deeply resolved) type for a diagnostic message.
	pub(crate) fn display(&mut self, ty: Ty) -> String {
		let ty = self.resolve_deep(ty);
		self.display_resolved(ty)
	}

	fn display_resolved(&self, ty: Ty) -> String {
		match self.interner.kind(ty) {
			TyKind::Int => "int".into(),
			TyKind::UInt => "uint".into(),
			TyKind::Float => "float".into(),
			TyKind::Char => "char".into(),
			TyKind::String => "string".into(),
			TyKind::Boolean => "boolean".into(),
			TyKind::Void => "void".into(),
			TyKind::Never => "never".into(),
			TyKind::SelfTy => "self".into(),
			TyKind::Error => "<error>".into(),
			TyKind::Infer(_) => "_".into(),
			TyKind::Param(p) => self
				.params
				.iter()
				.rev()
				.find_map(|scope| {
					scope
						.iter()
						.find(|(_, idx)| **idx == *p)
						.map(|(n, _)| n.to_string())
				})
				.unwrap_or_else(|| format!("T{}", p.0)),
			TyKind::List(elem) => format!("#[{}]", self.display_resolved(*elem)),
			TyKind::Tuple(elems) => {
				let inner: Vec<_> = elems.iter().map(|&e| self.display_resolved(e)).collect();
				format!("#({})", inner.join(", "))
			}
			TyKind::Map(key, value) => format!(
				"#{{{}: {}}}",
				self.display_resolved(*key),
				self.display_resolved(*value)
			),
			TyKind::Fn { params, ret } => {
				let inner: Vec<_> = params.iter().map(|&p| self.display_resolved(p)).collect();
				format!("({}) -> {}", inner.join(", "), self.display_resolved(*ret))
			}
			TyKind::Adt(def, args) => {
				let name = self.defs.data(*def).name.clone();
				if args.is_empty() {
					name.to_string()
				} else {
					let mut inner: Vec<String> = args
						.positional
						.iter()
						.map(|&t| self.display_resolved(t))
						.collect();
					inner.extend(
						args
							.named
							.iter()
							.map(|(n, t)| format!("{n} = {}", self.display_resolved(*t))),
					);
					format!("{name}<{}>", inner.join(", "))
				}
			}
			TyKind::Intersection(parts) => {
				let inner: Vec<_> = parts.iter().map(|&p| self.display_resolved(p)).collect();
				inner.join(" + ")
			}
		}
	}
}
