//! Bidirectional expression inference: `infer` synthesises a type, `check`
//! propagates an expected one. The check→infer boundary applies [`subtype`], where
//! coercions live. Also holds the body-checking driver that walks each function and
//! top-level `let` after signatures are lowered.
//!
//! Milestone A covers the value core: literals, paths, calls, ADT construction and
//! field access, closures, control flow, blocks, collections, and the *built-in*
//! operators. Method calls, operator overloading, `??`/`?`/`as`/`is` semantics, and
//! interfaces are Milestone B; where they occur we infer best-effort and move on.

use ecow::EcoString;
use nymph_ast::{
	NodeId, Span, Spanned,
	decl::{Declaration, ImplMember},
	expr::{CallArg, Expr, ExprKind, ListItem, MapEntry, RangeKind, Statement, StringPart},
	ops::{AssignOperator, BinaryOperator, PrefixOperator},
	ty::Type,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::annotate::{
	DispatchKind, IterMode, Resolution, ResolvedControlTarget, ResolvedControlTargetKind,
};
use crate::check::{
	Checker, ControlLabel, ControlLabelKind, InstantiatedObligation, LoopBreakKind, PendingBound,
};
use crate::def::{DefKind, FuncSig, NamespaceMemberSig};
use crate::errors::TypeError;
use crate::ids::{DefId, ParamIdx};
use crate::lower::build_param_scope;
use crate::solve::{MethodResolution, MethodSource};
use crate::ty::{GenericArgs, Ty, TyKind};

/// Whether a resolved method is owned by canonical compiler or importable-stdlib
/// runtime code rather than by the current project. Stable semantic identity is
/// the sole provenance channel; source spans are never used to infer ownership.
fn impl_is_unmaterialized(res: &MethodResolution) -> bool {
	if let Some(implementation) = &res.implementation {
		// Generic implementations have one canonical method body and cannot be
		// materialized correctly as receiver-specific prototype methods. Stable
		// implementation identity is authoritative here, including project-local
		// implementations regardless of their source location.
		if matches!(
			&implementation.key,
			crate::DeclarationKey::Implementation { header, .. }
				if matches!(header.self_type, crate::HeaderType::Generic(_))
		) {
			return true;
		}
		return matches!(
			implementation.module.origin,
			crate::ModuleOrigin::Compiler | crate::ModuleOrigin::ImportableStd
		);
	}
	res.source == MethodSource::GenericBound
		&& res.target.as_ref().is_some_and(|target| {
			matches!(
				target.module.origin,
				crate::ModuleOrigin::Compiler | crate::ModuleOrigin::ImportableStd
			)
		})
}

/// The `DispatchKind` a resolved *operator* desugaring (Slice 4B) must be lowered
/// with. `Inherent`/`ImplDirect`/`InterfaceDefault` are a real, directly-callable
/// method — Slice 4C-b materializes un-overridden interface default methods onto
/// every implementing struct's class — *provided* the matched impl isn't
/// canonical-runtime-only (`impl_is_unmaterialized`); `GenericBound` is always deferred
/// regardless of `impl_is_unmaterialized` (see `dispatch_operator`'s call site
/// for why a still-generic receiver can't pick a native operator or method name
/// at lowering time the way a plain method call — `dispatch_kind_for_method_call`,
/// below — safely can).
fn dispatch_kind_for_operator(res: &MethodResolution) -> DispatchKind {
	if impl_is_unmaterialized(res) {
		return DispatchKind::UserImplDefaultMethod;
	}
	match res.source {
		MethodSource::Inherent | MethodSource::ImplDirect | MethodSource::InterfaceDefault => {
			DispatchKind::UserImpl
		}
		MethodSource::GenericBound => DispatchKind::UserImplDefaultMethod,
	}
}

/// The `DispatchKind` a resolved plain `receiver.method(args…)` call (Finding 2,
/// stdlib linkage groundwork) must be lowered with. Unlike an operator, a plain
/// call's JS method name is always the literal identifier the user wrote
/// (`member.0`, recorded verbatim as `Resolution::method`) regardless of which
/// `MethodSource` it resolved through — there is no native-operator-vs-method-name
/// choice to make at lowering time — so a still-generic `GenericBound` dispatch
/// is *usually* safe here (type erasure: whichever concrete type instantiates the
/// bound at a given call site provides its own compiled method, because that
/// type's own impl is part of the user's module and gets lowered/materialized
/// along with everything else — confirmed by golden-program coverage of
/// `func f<T: Area>(shape: T): int = shape.area()`-shaped calls). The one real
/// hazard, caught here via `impl_is_unmaterialized` rather than a blanket
/// `GenericBound` deferral (contrast `dispatch_kind_for_operator`, which — for
/// unrelated codegen reasons already covered by that function's own doc comment —
/// defers *every* `GenericBound` operator, canonical-runtime-origin or not): a bound
/// satisfied *only* through a canonical-runtime interface/impl (round 2, Findings 1 &
/// 3), which is never materialized anywhere `compile_with_prelude` lowers.
fn dispatch_kind_for_method_call(res: &MethodResolution) -> DispatchKind {
	if impl_is_unmaterialized(res) {
		DispatchKind::UserImplDefaultMethod
	} else {
		DispatchKind::UserImpl
	}
}

fn method_resolution(method: EcoString, res: &MethodResolution) -> Resolution {
	Resolution {
		method,
		dispatch: dispatch_kind_for_method_call(res),
		target: res.target.clone(),
		implementation: res.implementation.clone(),
		resolved_target: res.resolved_target.clone(),
	}
}

/// Whether `expr` denotes a *place* (an lvalue with backing storage — a variable,
/// field, or element) rather than a freshly-produced temporary (a call result,
/// constructor, block, `if`/`match` value, …). A temporary receiver is exclusively
/// owned at the call site, so it is eligible to be a `mut` receiver even without a
/// `mut`-typed binding — mirroring calling a `&mut` method on a temporary in Rust, and
/// letting `a.map(f).fold(..)` chain a draining terminal straight off the adapter.
fn expr_is_place(expr: &Expr) -> bool {
	match &expr.kind {
		ExprKind::Identifier(_)
		| ExprKind::This
		| ExprKind::MemberAccess { .. }
		| ExprKind::IndexAccess { .. } => true,
		ExprKind::Grouped(inner) => expr_is_place(inner),
		_ => false,
	}
}

/// Recovery disposition for a deferred bound after its target is resolved.
/// Underdetermined and poisoned obligations intentionally remain silent; Stage 4
/// makes that distinction explicit without tightening either case.
enum BoundFinalizationDisposition {
	Underdetermined,
	Poisoned,
	Check(TyKind),
}

pub(crate) struct PendingMemberCompletion {
	receiver: NodeId,
	ty: Ty,
	span: Span,
	temporary_receiver: bool,
	param_bounds: FxHashMap<ParamIdx, Vec<DefId>>,
	param_bound_details: FxHashMap<ParamIdx, Vec<crate::iface::Bound>>,
	checking_interface_default: Option<(DefId, ParamIdx)>,
}

impl<'m> Checker<'m> {
	pub(crate) fn push_control_label(
		&mut self,
		label: Option<&nymph_ast::Ident>,
		id: NodeId,
		kind: ControlLabelKind,
		loop_index: Option<usize>,
		result_ty: Option<Ty>,
	) {
		if let Some(label) = label {
			if let Some(previous) = self
				.control_labels
				.iter()
				.rev()
				.find(|target| target.name.as_ref() == Some(&label.0))
			{
				self.emit(
					label.1,
					TypeError::DuplicateControlLabel {
						name: label.0.clone(),
						previous: previous.span,
					},
				);
			}
		}
		self.control_labels.push(ControlLabel {
			name: label.map(|label| label.0.clone()),
			id,
			span: label.map_or(Span::new(0, 0), |label| label.1),
			kind,
			loop_index,
			result_ty,
		});
	}

	fn resolve_control(
		&mut self,
		expr: &Expr,
		label: Option<&nymph_ast::Ident>,
		keyword: &'static str,
		allowed: &[ControlLabelKind],
	) -> Option<ControlLabel> {
		let target = if let Some(label) = label {
			self
				.control_labels
				.iter()
				.rev()
				.find(|target| target.name.as_ref() == Some(&label.0))
				.cloned()
				.or_else(|| {
					self.emit(
						label.1,
						TypeError::UnknownControlLabel {
							name: label.0.clone(),
						},
					);
					None
				})
		} else {
			self
				.control_labels
				.iter()
				.rev()
				.find(|target| {
					allowed.contains(&target.kind)
						&& (keyword != "return" || target.kind == ControlLabelKind::Callable)
				})
				.cloned()
		};
		let target = target?;
		if !allowed.contains(&target.kind) {
			let name = label.map_or_else(|| "<nearest>".into(), |label| label.0.clone());
			self.emit(
				label.map_or(expr.span, |label| label.1),
				TypeError::WrongControlLabelKind { name, keyword },
			);
			return None;
		}
		let kind = match target.kind {
			ControlLabelKind::Loop => ResolvedControlTargetKind::Loop,
			ControlLabelKind::Block => ResolvedControlTargetKind::Block,
			ControlLabelKind::Callable => ResolvedControlTargetKind::Callable,
		};
		self.annotations.record_control_target(
			expr.id,
			ResolvedControlTarget {
				source: target.id,
				kind,
			},
		);
		Some(target)
	}

	pub(crate) fn check_named_callable_body(
		&mut self,
		name: &nymph_ast::Ident,
		body: &Expr,
		ret: Ty,
	) {
		let outer_labels = std::mem::take(&mut self.control_labels);
		self.push_control_label(
			Some(name),
			body.id,
			ControlLabelKind::Callable,
			None,
			Some(ret),
		);
		self.resolve_anon(body, Some(ret));
		self.check(body, ret);
		self.control_labels = outer_labels;
	}
	// ── Body driver ──────────────────────────────────────────────────────────
	pub(crate) fn check_bodies(&mut self) {
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
				DefKind::Func => self.check_func_body(id, member),
				DefKind::Let => self.check_let_body(id, member),
				DefKind::Namespace => self.check_namespace_bodies(id, member),
				_ => {}
			}
		}
	}

	fn check_func_body(&mut self, id: DefId, member: usize) {
		let module = self.module;
		let (meta, body) = match &module.members[member] {
			Declaration::Func { meta, body, .. } => (meta, body),
			_ => return, // external funcs have no body
		};
		let sig = self.sigs.funcs[&id].clone();

		self.param_bounds.clear();
		self.param_bound_details.clear();
		self.push_params(build_param_scope(&meta.generics));
		self.record_param_bounds(&meta.generics, 0);
		self.push_scope();
		for (param, psig) in meta.params.iter().zip(&sig.params) {
			self.bind_pattern(&param.0.name, psig.ty, param.0.mutable);
		}
		let prev = self.ret_ty.replace(sig.ret);
		// A function's own body is exactly the same kind of closure slot a
		// `let` initializer is (see `check_let_body`'s matching call) — e.g.
		// `func pred(): (int) -> boolean = $ % 2 == 0` is just the top-level
		// spelling of the same boundary the canonical `xs.filter($ % 2 == 0)`
		// example forms as a call argument.
		self.check_named_callable_body(&meta.name, body, sig.ret);
		self.ret_ty = prev;
		// Drain this body's deferred operators now, while its `param_bounds` are
		// still the ones just built above — see `pending_operators`'s doc comment.
		self.finalize_pending_operators();
		self.finalize_pending_bounds();
		self.pop_scope();
		self.pop_params();
	}

	fn check_let_body(&mut self, id: DefId, member: usize) {
		let module = self.module;
		let value = match &module.members[member] {
			Declaration::Let { value, .. } => value,
			_ => return, // external lets have no value
		};
		let ty = self.sigs.lets[&id].ty;
		self.push_scope();
		self.resolve_anon(value, Some(ty));
		self.check(value, ty);
		self.finalize_pending_operators();
		self.finalize_pending_bounds();
		self.pop_scope();
	}

	fn check_namespace_bodies(&mut self, id: DefId, member: usize) {
		let module = self.module;
		let Declaration::Namespace { members, .. } = &module.members[member] else {
			return;
		};
		for member in members {
			match &member.0 {
				ImplMember::Func { meta, body, .. } => {
					let Some(NamespaceMemberSig::Func { sig, .. }) = self
						.sigs
						.namespaces
						.get(&id)
						.and_then(|namespace| namespace.members.get(&meta.name.0))
						.cloned()
					else {
						continue;
					};
					self.param_bounds.clear();
					self.param_bound_details.clear();
					self.push_params(build_param_scope(&meta.generics));
					self.record_param_bounds(&meta.generics, 0);
					self.push_scope();
					for (parameter, checked) in meta.params.iter().zip(&sig.params) {
						self.bind_pattern(&parameter.0.name, checked.ty, parameter.0.mutable);
					}
					let previous = self.ret_ty.replace(sig.ret);
					self.check_named_callable_body(&meta.name, body, sig.ret);
					self.ret_ty = previous;
					self.finalize_pending_operators();
					self.finalize_pending_bounds();
					self.pop_scope();
					self.pop_params();
				}
				ImplMember::Let { meta, value, .. } => {
					let Some(name) = meta.name.0.as_binding() else {
						continue;
					};
					let Some(NamespaceMemberSig::Value { ty, .. }) = self
						.sigs
						.namespaces
						.get(&id)
						.and_then(|namespace| namespace.members.get(&name.0))
						.cloned()
					else {
						continue;
					};
					self.push_scope();
					self.resolve_anon(value, Some(ty));
					self.check(value, ty);
					self.finalize_pending_operators();
					self.finalize_pending_bounds();
					self.pop_scope();
				}
				ImplMember::ExternalFunc(..) | ImplMember::ExternalLet(..) => {}
			}
		}
	}

	// ── check mode ───────────────────────────────────────────────────────────
	/// Thin wrapper around [`Self::check_dispatch`]: intercepts a committed
	/// anonymous-closure (`$N`) boundary (Slice: `$N` anonymous closure
	/// params) BEFORE the ordinary per-kind dispatch, forming the closure and
	/// subtyping it against `expected` exactly like an explicit closure would
	/// — see `anon_closure.rs`'s module doc for why this split is needed: calling
	/// `self.check` again from inside the boundary's own formation would just
	/// re-hit this same interception and recurse forever.
	pub(crate) fn check(&mut self, expr: &Expr, expected: Ty) {
		if let Some(arity) = self.annotations.anon_boundary_arity(expr.id) {
			let got = self.form_anon_closure(expr, arity);
			self.subtype(got, expected, expr.span);
			return;
		}
		self.check_dispatch(expr, expected);
	}

	fn check_dispatch(&mut self, expr: &Expr, expected: Ty) {
		// Owned-literal → `mut` coercion (Bug 2): a `#{…}`/`#[…]` literal never
		// infers as `mut` (see `infer_kind`'s `Map`/`List` arms), so without this it
		// would always fail the one-way `mut T <: T` `subtype` check below when
		// `expected` is `mut`. Handled once here, ahead of the per-kind match, so
		// every `check`-routed call site — struct/enum ctor fields, block/if/match
		// branches, the `List` arm just below — benefits uniformly. See
		// `try_coerce_owned_literal_to_mut`'s doc comment for why this can't leak
		// into accepting a named binding.
		if self.try_coerce_owned_literal_to_mut(expr, expected) {
			return;
		}
		match &expr.kind {
			ExprKind::Closure { .. } => self.check_closure(expr, expected),
			ExprKind::Block { body, label } => {
				if label.is_some() {
					self.push_control_label(
						label.as_ref(),
						expr.id,
						ControlLabelKind::Block,
						None,
						Some(expected),
					);
				}
				let ty = self.infer_block(body, Some(expected));
				if label.is_some() {
					self.control_labels.pop();
				}
				self.subtype(ty, expected, expr.span);
			}
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => {
				let boolean = self.interner.boolean();
				self.check(condition, boolean);
				self.check(then, expected);
				match otherwise {
					Some(else_) => self.check(else_, expected),
					None => {
						// A value is expected but there's no else branch.
						let void = self.interner.void();
						self.subtype(void, expected, expr.span);
					}
				}
			}
			ExprKind::Match { value, arms } => {
				let scrutinee = self.infer(value);
				for arm in arms {
					self.push_scope();
					self.check_pattern(&arm.pattern, scrutinee);
					if let Some(guard) = &arm.guard {
						let boolean = self.interner.boolean();
						self.check(guard, boolean);
					}
					self.check(&arm.body, expected);
					self.pop_scope();
				}
				self.check_exhaustive(scrutinee, arms, expr.span);
			}
			ExprKind::Grouped(inner) => self.check(inner, expected),
			// An integer literal implicitly widens to the expected `float`/`uint` (the
			// literal is retyped, e.g. `1` → `1f` / `1u`, rather than reported as a
			// mismatch). In any other expected context it synthesises `int` as usual.
			// The literal's node is recorded with the RETYPED (coerced) type, not the
			// syntactic `int` — uniform value boxing (slice #2) reads this back in
			// lowering to box the literal as `NFloat`/`NUint` rather than `NInt`; a
			// `func f(): float = 5` whose `5` stayed recorded as `int` would misbox.
			ExprKind::Int(_) if self.int_literal_coerces_to(expected) => {
				let coerced = self.shallow_resolve(expected);
				self.record(expr.id, coerced, None);
			}
			// Mirrors `infer_kind`'s own `ExprKind::List` arm, but — when `expected` is
			// (or resolves to) a concrete `List` — checks each element against the
			// ALREADY-concrete element type from `expected`, rather than a fresh var
			// only unified with `expected` after every element has been checked. That
			// ordering is what lets a nested bare variant (or anything else driven by
			// `check`'s `expected`) see the real element type: `#[Leaf]` checked
			// against `#[Tree]` must resolve `Leaf` against `Tree`, not fall back to
			// the ambiguous global lookup because the shared `elem` var was still
			// unbound at the moment `Leaf` itself was checked.
			ExprKind::List(items) => {
				let elem = self
					.expected_list_element(expected)
					.unwrap_or_else(|| self.fresh());
				let ty = self.check_list(items, elem, expr.span);
				// This arm bypasses the catch-all `_ => self.infer(expr)` below (whose
				// callee, `infer_dispatch`, is what normally records a node's type), so
				// it must record the list's own node explicitly — mirrors
				// `try_check_expected_variant`'s explicit `record` calls, needed for
				// exactly the same reason.
				self.record(expr.id, ty, None);
				self.subtype(ty, expected, expr.span);
			}
			// Mirrors the `ExprKind::List` arm just above (see its doc comment): when
			// `expected` pins a concrete `Map`, each entry checks against the
			// ALREADY-concrete key/value types instead of a fresh var only unified
			// after the fact — the fix that lets a nested `mut`-expected value (e.g.
			// `#{int: mut #[int]}`'s value) reach its own
			// `try_coerce_owned_literal_to_mut` in turn (Confirmed defect 1).
			ExprKind::Map(entries) => {
				let (key, value) = self
					.expected_map_entry(expected)
					.unwrap_or_else(|| (self.fresh(), self.fresh()));
				let ty = self.check_map(entries, key, value, expr.span);
				// Bypasses the catch-all `_ => self.infer(expr)` below, so — mirroring
				// the `List` arm — this must record the map's own node explicitly.
				self.record(expr.id, ty, None);
				self.subtype(ty, expected, expr.span);
			}
			_ => {
				let got = match self.try_check_expected_variant(expr, expected) {
					Some(ty) => ty,
					None => self.infer(expr),
				};
				self.subtype(got, expected, expr.span);
			}
		}
	}

	/// A bare nullary variant identifier (`Equal`) or a bare variant call (`Some(x)`)
	/// checked against a concrete expected enum type resolves against THAT enum before
	/// falling back to the global by-name lookup `infer` would otherwise use — this is
	/// what lets `let o: Order = Equal` (and a fn returning a bare variant) check even
	/// when another enum shares the variant name. Returns `None` (leaving `check_dispatch`
	/// to fall through to `self.infer`) for every other expression shape, for a bare name
	/// already resolved by a local binding or a top-level def (preserving local/def
	/// precedence exactly as `infer_identifier`/`infer_call` do), or when `expected`
	/// doesn't pin a concrete enum that declares the name as a variant.
	fn try_check_expected_variant(&mut self, expr: &Expr, expected: Ty) -> Option<Ty> {
		match &expr.kind {
			ExprKind::Identifier(name) => {
				if self.lookup_local(&name.0).is_some() || self.defs.get(&name.0).is_some() {
					return None;
				}
				let (enum_def, variant) = self.expected_enum_variant(expected, &name.0)?;
				let ty = self.variant_value(enum_def, variant, expr.id, expr.span);
				self.record(expr.id, ty, None);
				Some(ty)
			}
			ExprKind::Call { func, args, .. } => {
				let ExprKind::Identifier(name) = &func.kind else {
					return None;
				};
				if self.defs.get(&name.0).is_some() {
					return None;
				}
				let (enum_def, variant) = self.expected_enum_variant(expected, &name.0)?;
				let ty =
					self.infer_variant_ctor(enum_def, variant, args, expr.span, expr.id, Some(expected));
				self.record(expr.id, ty, None);
				Some(ty)
			}
			_ => None,
		}
	}

	/// Check every item of a list literal against `elem` — a concrete element type
	/// pinned by the caller's own expected type when one is available (`check_dispatch`
	/// uses `expected_list_element`), or a fresh var when there is none (`infer_kind`'s
	/// unchecked `infer`). Checking each item against the ALREADY-concrete `elem` — as
	/// opposed to a fresh var only unified with the outer expected type after every
	/// item has been checked — is what lets a nested bare variant (or anything else
	/// driven by `check`'s `expected`) see the real element type.
	fn check_list(&mut self, items: &[Spanned<ListItem>], elem: Ty, span: Span) -> Ty {
		for item in items {
			match &item.0 {
				ListItem::Expr(e) => self.check(e, elem),
				// SS1 (SMART spread): the source need not be a same-kind `#[T]` literal
				// — ANY `Iterator<T>`/`Iterable<T>` whose element unifies with `elem`
				// is accepted, reusing Track A's own iterable resolution
				// (`infer_iterable_element`, which itself already special-cases a
				// syntactic range and a native list before falling back to the
				// `Iterator`/`Iterable` interfaces) rather than forcing an exact
				// `#[elem]` `check`. A non-iterable source gets Track A's own
				// `NotIterable` diagnostic for free.
				ListItem::Spread(e) => {
					let src_elem = self.infer_iterable_element(e);
					self.unify(src_elem, elem, span);
				}
			}
		}
		self.interner.mk_list(elem)
	}

	/// Check every entry of a map literal against `(key, value)` — concrete
	/// key/value types pinned by the caller's own expected type when one is
	/// available (`check_dispatch` uses `expected_map_entry`), or fresh vars
	/// when there is none. Mirrors [`Self::check_list`] exactly (see its doc
	/// comment for why checking against an already-concrete type, rather than a
	/// fresh var only unified after the fact, matters); the spread-entry
	/// handling below mirrors `infer_kind`'s own `ExprKind::Map` arm.
	fn check_map(
		&mut self,
		entries: &[Spanned<nymph_ast::expr::MapEntry>],
		key: Ty,
		value: Ty,
		span: Span,
	) -> Ty {
		use nymph_ast::expr::MapEntry;
		for entry in entries {
			match &entry.0 {
				MapEntry::Entry(k, v) => {
					self.check(k, key);
					self.check(v, value);
				}
				MapEntry::Spread(e) => {
					let ty = self.infer(e);
					let stripped = self.strip_mut(ty);
					match self.interner.kind(stripped).clone() {
						TyKind::Map(k2, v2) => {
							self.unify(k2, key, span);
							self.unify(v2, value, span);
						}
						TyKind::List(elem) => {
							let pair = self.interner.mk_tuple(vec![key, value]);
							self.unify(elem, pair, span);
						}
						_ => {
							let elem = self.resolve_iterable_source(e, ty, stripped);
							let pair = self.interner.mk_tuple(vec![key, value]);
							self.unify(elem, pair, span);
						}
					}
				}
			}
		}
		self.interner.mk_map(key, value)
	}

	// ── infer mode ───────────────────────────────────────────────────────────
	/// Thin wrapper around [`Self::infer_dispatch`]: intercepts a committed
	/// anonymous-closure (`$N`) boundary before the ordinary per-kind dispatch
	/// — mirrors [`Self::check`]'s wrapper exactly; see its doc comment.
	pub(crate) fn infer(&mut self, expr: &Expr) -> Ty {
		if let Some(arity) = self.annotations.anon_boundary_arity(expr.id) {
			return self.form_anon_closure(expr, arity);
		}
		self.infer_dispatch(expr)
	}

	pub(crate) fn infer_dispatch(&mut self, expr: &Expr) -> Ty {
		// `BinaryOp` is special-cased: `infer_binary` also decides the operator's
		// `Resolution` (Slice 4B), which `record_resolution` requires the node to
		// already be `record`'d before attaching — so the type is recorded first here,
		// then the resolution is layered on, rather than threading it through the
		// generic `infer_kind` match (whose own `BinaryOp` arm below is unreachable
		// through this path, kept only so the match stays exhaustive).
		if let ExprKind::BinaryOp { lhs, op, rhs } = &expr.kind {
			let (ty, resolution, pending) = self.infer_binary(lhs, *op, rhs, expr.span);
			self.record(expr.id, ty, None);
			if let Some(resolution) = resolution {
				self.annotations.record_resolution(expr.id, resolution);
			}
			if let Some(pending_ty) = pending {
				self.pending_operators.push((
					expr.id,
					expr.span,
					pending_ty,
					ty,
					crate::check::PendingOperatorKind::BinaryOp(*op),
				));
			}
			return ty;
		}
		// `AssignOp` mirrors the `BinaryOp` special case above: a compound assign
		// (`v1 += v2`) desugars to a binary op whose `Resolution` (Finding 1, Slice
		// 4B follow-up) must be recorded on the `AssignOp` node itself — there is no
		// separate desugared `BinaryOp` AST node to hang it on. Plain `=` and `~=`
		// (`BitNotAssign`) never produce a resolution (`binary_of_assign` maps both
		// to `None`).
		if let ExprKind::AssignOp { lhs, op, rhs } = &expr.kind {
			let (ty, resolution, pending) = self.infer_assign(lhs, *op, rhs, expr.span);
			self.record(expr.id, ty, None);
			if let Some(resolution) = resolution {
				self.annotations.record_resolution(expr.id, resolution);
			}
			if let Some((binop, pending_ty, result_ty)) = pending {
				self.pending_operators.push((
					expr.id,
					expr.span,
					pending_ty,
					result_ty,
					crate::check::PendingOperatorKind::AssignOp(binop),
				));
			}
			return ty;
		}
		// `PrefixOp` mirrors the `BinaryOp` special case above (Slice 4C-a):
		// `infer_prefix` also decides the operator's `Resolution`, recorded on the
		// `PrefixOp` node itself once it exists in the annotation table.
		if let ExprKind::PrefixOp { op, value } = &expr.kind {
			let (ty, resolution, pending) = self.infer_prefix(*op, value, expr.span);
			self.record(expr.id, ty, None);
			if let Some(resolution) = resolution {
				self.annotations.record_resolution(expr.id, resolution);
			}
			if let Some(pending_ty) = pending {
				self.pending_operators.push((
					expr.id,
					expr.span,
					pending_ty,
					ty,
					crate::check::PendingOperatorKind::PrefixOp(*op),
				));
			}
			return ty;
		}
		// `TypeOp` (`as`, Slice 4K) mirrors the `BinaryOp` special case above:
		// `infer_cast` also decides the cast's `Resolution` (identity/scalar builtin
		// vs. a dispatched `Into` impl), recorded on the `TypeOp` node itself once it
		// exists in the annotation table.
		if let ExprKind::TypeOp { lhs, rhs, .. } = &expr.kind {
			let (ty, resolution) = self.infer_cast(lhs, rhs, expr.span);
			self.record(expr.id, ty, None);
			if let Some(resolution) = resolution {
				self.annotations.record_resolution(expr.id, resolution);
			}
			return ty;
		}
		// `Call` mirrors the same special case (Finding 2, stdlib linkage
		// groundwork): a plain `receiver.method(args…)` call resolved through the
		// interface solver carries a `Resolution` too (mirroring operator syntax),
		// which `record_resolution` requires the node to already be `record`'d
		// before attaching. Every other call shape returns `None` here and behaves
		// exactly as it did through the generic `infer_kind` match (whose own
		// `Call` arm below is unreachable through this path, kept only so the
		// match stays exhaustive).
		if let ExprKind::Call { func, args, .. } = &expr.kind {
			let (ty, resolution) = self.infer_call(func, args, expr.span, expr.id);
			self.record(expr.id, ty, None);
			if let Some(resolution) = resolution {
				self.annotations.record_resolution(expr.id, resolution);
			}
			return ty;
		}
		// Custom indexing is method dispatch too. Record its selected `Index::index`
		// implementation so lowering can honor prelude materialization and linkage.
		if let ExprKind::IndexAccess { parent, index, .. } = &expr.kind {
			let (ty, resolution) = self.infer_index_access(parent, index, expr.span);
			self.record(expr.id, ty, None);
			if let Some(resolution) = resolution {
				self.annotations.record_resolution(expr.id, resolution);
			}
			return ty;
		}
		if let ExprKind::MemberAccess { parent, member, .. } = &expr.kind {
			let (ty, resolution) =
				self.infer_member_with_resolution(parent, &member.0, member.1, expr.id);
			self.record(expr.id, ty, None);
			if let Some(resolution) = resolution {
				self
					.annotations
					.record_definition_target(expr.id, resolution.target.as_ref());
				self.annotations.record_resolution(expr.id, resolution);
			}
			return ty;
		}
		let ty = self.infer_kind(expr);
		// Record the node's resolved type for the lowering pass. Zonking happens
		// inside `record`. Returns the *raw* ty so callers can still unify against it.
		self.record(expr.id, ty, None);
		ty
	}

	fn infer_kind(&mut self, expr: &Expr) -> Ty {
		let span = expr.span;
		match &expr.kind {
			ExprKind::Int(lit) => {
				self.check_unsafe_int_literal(*lit.value(), lit.span());
				self.interner.int()
			}
			ExprKind::UInt(lit) => {
				self.check_unsafe_int_literal(*lit.value(), lit.span());
				self.interner.uint()
			}
			ExprKind::Float(_) => self.interner.float(),
			ExprKind::Char(_) => self.interner.char(),
			ExprKind::Boolean(_) => self.interner.boolean(),
			ExprKind::String(parts) => {
				for part in parts {
					if let StringPart::InterpolatedExpr(inner) = &part.0 {
						self.infer(inner);
					}
				}
				self.interner.string()
			}
			ExprKind::This => match self.self_ty {
				Some(ty) => ty,
				None => {
					self.emit(span, TypeError::ThisOutsideMethod);
					self.interner.error()
				}
			},
			ExprKind::Identifier(name) => self.infer_identifier(&name.0, span, expr.id),
			// `$N` never resolves through the ordinary local-scope lookup every
			// other identifier uses — it reads positionally out of the innermost
			// `anon_ctx` frame `Checker::form_anon_closure` pushed while forming
			// the enclosing committed boundary (`anon_closure.rs`). An empty
			// `anon_ctx` here means `resolve_anon`'s search found no boundary
			// (up to the enclosing slot) that type-checks — or, rarer still,
			// this `$N` sits somewhere `resolve_anon` was never invoked over at
			// all — either way, loud, not silent.
			ExprKind::AnonymousParam(idx) => match self.anon_ctx.last() {
				Some(params) => params
					.get(idx.unwrap_or(0) as usize)
					.copied()
					.unwrap_or_else(|| self.interner.error()),
				None => {
					self.emit(span, TypeError::AnonymousParamUnsupported);
					self.interner.error()
				}
			},
			// A bare `infer` of a list literal has no expected element type to work
			// from, so `check_list` mints a fresh one — see its doc comment for why
			// `check_dispatch`'s own `ExprKind::List` arm calls the same helper with a
			// concrete element type instead, when one is available.
			ExprKind::List(items) => {
				let elem = self.fresh();
				self.check_list(items, elem, span)
			}
			ExprKind::Tuple(items) => {
				let mut tys = Vec::new();
				for item in items {
					match &item.0 {
						ListItem::Expr(e) => tys.push(self.infer(e)),
						ListItem::Spread(e) => {
							let ty = self.infer(e);
							let stripped = self.strip_mut(ty);
							match self.interner.kind(stripped).clone() {
								TyKind::Tuple(items) => tys.extend(items),
								_ => {
									let ty = self.display(stripped);
									self.emit(e.span, TypeError::TupleSpreadRequiresStaticTuple { ty });
								}
							}
						}
					}
				}
				self.interner.mk_tuple(tys)
			}
			ExprKind::Map(entries) => {
				use nymph_ast::expr::MapEntry;
				let key = self.fresh();
				let value = self.fresh();
				for entry in entries {
					match &entry.0 {
						MapEntry::Entry(k, v) => {
							self.check(k, key);
							self.check(v, value);
						}
						// SS1 (SMART spread): a native `Map` source unifies its key/value
						// directly (the fast, no-drain merge path). A native `#[#(K, V)]`
						// list source (e.g. from `#[...pairs]`-style merges) is likewise
						// fast-pathed — `List` implements neither `Iterator` nor
						// `Iterable`, so without this arm `resolve_iterable_source` would
						// always reject it as `NotIterable`, even though
						// `lower_spread_source`'s own `is_list_like` check already treats
						// any `TyKind::List` source as a native JS array and lowers it
						// correctly with no drain (mirrors list-spread's own list fast
						// path in `infer_iterable_element`). Anything else must be a
						// non-map, non-list `Iterator`/`Iterable<#(K, V)>` of entry pairs
						// — only that fallback reuses Track A's own interface resolution
						// (`resolve_iterable_source`), unifying its resolved element
						// against the `#(K, V)` pair type.
						MapEntry::Spread(e) => {
							let ty = self.infer(e);
							let stripped = self.strip_mut(ty);
							match self.interner.kind(stripped).clone() {
								TyKind::Map(k2, v2) => {
									self.unify(k2, key, span);
									self.unify(v2, value, span);
								}
								TyKind::List(elem) => {
									let pair = self.interner.mk_tuple(vec![key, value]);
									self.unify(elem, pair, span);
								}
								_ => {
									let elem = self.resolve_iterable_source(e, ty, stripped);
									let pair = self.interner.mk_tuple(vec![key, value]);
									self.unify(elem, pair, span);
								}
							}
						}
					}
				}
				self.interner.mk_map(key, value)
			}
			ExprKind::Range(kind) => {
				let elem = self.infer_range_element(kind);
				let name = match kind {
					RangeKind::Exclusive { .. } => "Range",
					RangeKind::From(_) => "RangeFrom",
					RangeKind::To(_) => "RangeTo",
					RangeKind::Inclusive { .. } => "RangeInclusive",
					RangeKind::ToInclusive(_) => "RangeToInclusive",
				};
				let Some(def) = self.defs.get(name) else {
					return self.interner.error();
				};
				self
					.annotations
					.record_definition_target(expr.id, self.defs.stable(def));
				let inst = self.instantiate_struct(def);
				if let TyKind::Adt(_, args) = self.interner.kind(inst.ty).clone()
					&& let Some(parameter) = args.positional.first()
				{
					self.unify(*parameter, elem, span);
				}
				self.defer_obligations(span, inst.obligations.iter().cloned());
				inst.ty
			}
			ExprKind::Call { func, args, .. } => self.infer_call(func, args, span, expr.id).0,
			ExprKind::MemberAccess { parent, member, .. } => {
				self.infer_member(parent, &member.0, member.1, expr.id)
			}
			ExprKind::IndexAccess { parent, index, .. } => self.infer_index_access(parent, index, span).0,
			ExprKind::Closure { .. } => self.infer_closure(expr),
			ExprKind::PrefixOp { op, value } => {
				// Unreachable in practice: `infer` intercepts `PrefixOp` before it gets
				// here, for the same reason it intercepts `BinaryOp`/`AssignOp` above —
				// recording the operator's `Resolution` needs the node's type entry to
				// already exist. This arm only exists so the match stays exhaustive; it
				// discards the resolution/pending slot rather than duplicating that
				// recording logic.
				self.infer_prefix(*op, value, span).0
			}
			ExprKind::PostfixOp { value, .. } => {
				// `?` error propagation — Milestone B; unwrap best-effort.
				self.infer(value);
				self.fresh()
			}
			ExprKind::BinaryOp { lhs, op, rhs } => {
				// Unreachable in practice: `infer` (the sole caller of `infer_kind`)
				// intercepts `BinaryOp` before it gets here, because recording the
				// operator's `Resolution` needs the node's type entry to already exist
				// (see `infer`). This arm only exists so the match stays exhaustive; it
				// discards the resolution rather than duplicating that recording logic.
				self.infer_binary(lhs, *op, rhs, span).0
			}
			ExprKind::TypeOp { lhs, rhs, .. } => {
				// Unreachable in practice: `infer` intercepts `TypeOp` before it gets
				// here, for the same reason it intercepts `BinaryOp`/`PrefixOp` above —
				// recording the cast's `Resolution` needs the node's type entry to
				// already exist. This arm only exists so the match stays exhaustive.
				self.infer_cast(lhs, rhs, span).0
			}
			ExprKind::PatternOp { lhs, rhs, .. } => {
				let scrutinee = self.infer(lhs);
				self.push_scope();
				self.check_pattern(rhs, scrutinee);
				self.pop_scope();
				self.interner.boolean()
			}
			ExprKind::AssignOp { lhs, op, rhs } => {
				// Unreachable in practice: `infer` intercepts `AssignOp` before it gets
				// here, for the same reason it intercepts `BinaryOp` above — recording the
				// operator's `Resolution` needs the node's type entry to already exist.
				// This arm only exists so the match stays exhaustive.
				self.infer_assign(lhs, *op, rhs, span).0
			}
			ExprKind::Return { value, label } => {
				let target = self.resolve_control(
					expr,
					label.as_ref(),
					"return",
					&[ControlLabelKind::Block, ControlLabelKind::Callable],
				);
				let ret = target.and_then(|target| target.result_ty).or(self.ret_ty);
				if let Some(v) = value {
					match ret {
						Some(rt) => {
							self.resolve_anon(v, Some(rt));
							self.check(v, rt);
						}
						None => {
							self.resolve_anon(v, None);
							self.infer(v);
						}
					}
				} else if let Some(ret) = ret {
					let void = self.interner.void();
					self.unify(void, ret, expr.span);
				}
				self.interner.never()
			}
			ExprKind::Break { value, label } => {
				let found = value.as_ref().map(|v| self.infer(v));
				let Some(target) =
					self.resolve_control(expr, label.as_ref(), "break", &[ControlLabelKind::Loop])
				else {
					if label.is_none() {
						self.emit(
							expr.span,
							TypeError::LoopControlOutsideLoop { keyword: "break" },
						);
					}
					return self.interner.never();
				};
				let loop_index = target.loop_index.expect("loop target has contract");
				let previous = self.loop_controls[loop_index];
				match (previous, found) {
					(LoopBreakKind::None, found) => {
						self.loop_controls[loop_index] =
							found.map_or(LoopBreakKind::Bare, LoopBreakKind::Valued);
					}
					(LoopBreakKind::Bare, Some(_)) | (LoopBreakKind::Valued(_), None) => {
						self.emit(expr.span, TypeError::MixedBreakForms);
					}
					(LoopBreakKind::Valued(expected), Some(found))
						if !matches!(self.interner.kind(found), TyKind::Never) =>
					{
						self.unify(found, expected, expr.span)
					}
					(LoopBreakKind::Valued(_), Some(_)) => {}
					(LoopBreakKind::Bare, None) => {}
				}
				self.interner.never()
			}
			ExprKind::Continue { label } => {
				if self
					.resolve_control(expr, label.as_ref(), "continue", &[ControlLabelKind::Loop])
					.is_none()
				{
					if label.is_none() {
						self.emit(
							expr.span,
							TypeError::LoopControlOutsideLoop {
								keyword: "continue",
							},
						);
					}
				}
				self.interner.never()
			}
			ExprKind::While {
				condition,
				body,
				label,
			} => {
				let boolean = self.interner.boolean();
				self.check(condition, boolean);
				let break_kind = self.targeting_break_kind(body, label.as_ref());
				let break_ty = match break_kind {
					Some(false) => Some(LoopBreakKind::Bare),
					Some(true) => Some(LoopBreakKind::Valued(self.fresh())),
					None => Some(LoopBreakKind::None),
				};
				self.loop_controls.push(break_ty.unwrap());
				self.push_control_label(
					label.as_ref(),
					expr.id,
					ControlLabelKind::Loop,
					Some(self.loop_controls.len() - 1),
					None,
				);
				self.push_scope();
				self.infer(body);
				self.pop_scope();
				self.loop_controls.pop();
				self.control_labels.pop();
				self.loop_result_type(break_ty)
			}
			ExprKind::For {
				variable,
				iterable,
				body,
				label,
			} => {
				let elem = self.infer_iterable_element(iterable);
				let break_kind = self.targeting_break_kind(body, label.as_ref());
				let break_ty = match break_kind {
					Some(false) => Some(LoopBreakKind::Bare),
					Some(true) => Some(LoopBreakKind::Valued(self.fresh())),
					None => Some(LoopBreakKind::None),
				};
				self.loop_controls.push(break_ty.unwrap());
				self.push_control_label(
					label.as_ref(),
					expr.id,
					ControlLabelKind::Loop,
					Some(self.loop_controls.len() - 1),
					None,
				);
				self.push_scope();
				self.check_pattern(variable, elem);
				self.infer(body);
				self.pop_scope();
				self.loop_controls.pop();
				self.control_labels.pop();
				self.loop_result_type(break_ty)
			}
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => {
				let boolean = self.interner.boolean();
				self.check(condition, boolean);
				match otherwise {
					Some(else_) => {
						let then_ty = self.infer(then);
						if matches!(self.interner.kind(then_ty), TyKind::Never) {
							return self.infer(else_);
						}
						self.check(else_, then_ty);
						then_ty
					}
					None => {
						self.infer(then);
						self.interner.void()
					}
				}
			}
			ExprKind::Match { value, arms } => {
				let scrutinee = self.infer(value);
				let result = self.fresh();
				let mut has_value_arm = false;
				for arm in arms {
					self.push_scope();
					self.check_pattern(&arm.pattern, scrutinee);
					if let Some(guard) = &arm.guard {
						let boolean = self.interner.boolean();
						self.check(guard, boolean);
					}
					let arm_ty = self.infer(&arm.body);
					if !matches!(self.interner.kind(arm_ty), TyKind::Never) {
						has_value_arm = true;
						self.unify(arm_ty, result, arm.body.span);
					}
					self.pop_scope();
				}
				self.check_exhaustive(scrutinee, arms, span);
				if has_value_arm {
					result
				} else {
					self.interner.never()
				}
			}
			ExprKind::Block { body, label } => {
				if label.is_none() {
					self.infer_block(body, None)
				} else {
					let result = self.fresh();
					self.push_control_label(
						label.as_ref(),
						expr.id,
						ControlLabelKind::Block,
						None,
						Some(result),
					);
					let tail = self.infer_block(body, Some(result));
					self.control_labels.pop();
					self.unify(tail, result, expr.span);
					result
				}
			}
			ExprKind::Grouped(inner) => {
				let ty = self.infer(inner);
				// A first-class method owns its hidden generic arguments: its
				// receiver-capturing adapter must append them after future source
				// arguments. Moving them to a grouped or outer call would pass them
				// to the adapter itself, where JavaScript ignores the extra slots.
				if self.annotations.resolution_of(inner.id).is_none() {
					self
						.annotations
						.move_generic_call_arguments(inner.id, expr.id);
				}
				ty
			}
		}
	}

	/// Warn on a source `int`/`uint` literal whose magnitude can't round-trip
	/// through the `f64` Nymph's `int`/`uint` are backed by at runtime. `Int`/
	/// `UInt` literals (`ExprKind::Int`/`ExprKind::UInt`) store their magnitude as
	/// a `u64`; a negative `int` literal is this same (positive) literal node
	/// wrapped in a `PrefixOperator::Negate` at parse time, so calling this once
	/// per literal (regardless of any enclosing `Negate`) is already correct —
	/// no separate handling of the negative case is needed.
	fn check_unsafe_int_literal(&mut self, value: u64, span: Span) {
		const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991; // 2^53 - 1
		if value > MAX_SAFE_INTEGER {
			self.emit(span, TypeError::IntLiteralUnsafe { value });
		}
	}

	// ── Identifiers & definitions ────────────────────────────────────────────
	/// Infer `receiver[key]`. Structural collections use their built-in ABI;
	/// every other receiver resolves the equivalent `receiver.index(key)` call.
	fn infer_index_access(
		&mut self,
		parent: &Expr,
		index: &Expr,
		span: Span,
	) -> (Ty, Option<Resolution>) {
		let recv = self.infer(parent);
		let key = self.infer(index);
		if matches!(self.interner.kind(recv), TyKind::Never)
			|| matches!(self.interner.kind(key), TyKind::Never)
		{
			return (self.interner.never(), None);
		}
		let key = self.strip_mut(key);
		let recv_r = self.strip_mut(recv);
		match self.interner.kind(recv_r).clone() {
			TyKind::Error => (self.interner.error(), None),
			TyKind::List(elem) => {
				let int = self.interner.int();
				self.unify(key, int, span);
				(elem, None)
			}
			TyKind::Tuple(_) => (self.fresh(), None),
			TyKind::Map(k, v) => {
				self.unify(key, k, span);
				(v, None)
			}
			_ => {
				let key_lit = matches!(index.kind, ExprKind::Int(_));
				match self.resolve_method(recv, "index", &[key], &[key_lit], span) {
					Some(res) => {
						if key_lit && let Some(&expected) = res.params.first() {
							let expected = self.shallow_resolve(expected);
							if matches!(self.interner.kind(expected), TyKind::UInt | TyKind::Float) {
								self.record(index.id, expected, None);
							}
						}
						let resolution = Resolution {
							method: "index".into(),
							dispatch: dispatch_kind_for_method_call(&res),
							target: res.target.clone(),
							implementation: res.implementation.clone(),
							resolved_target: res.resolved_target.clone(),
						};
						(res.ty, Some(resolution))
					}
					None => {
						let ty = self.display(recv);
						self.emit(
							span,
							TypeError::NoMethod {
								method: "index".into(),
								ty,
							},
						);
						(self.interner.error(), None)
					}
				}
			}
		}
	}

	fn custom_index_value(&self, mut expr: &Expr) -> bool {
		while let ExprKind::Grouped(inner) = &expr.kind {
			expr = inner;
		}
		matches!(expr.kind, ExprKind::IndexAccess { .. })
			&& self.annotations.resolution_of(expr.id).is_some()
	}

	fn infer_identifier(&mut self, name: &str, span: Span, id: NodeId) -> Ty {
		if let Some((ty, declaration)) = self
			.lookup_local(name)
			.map(|binding| (binding.ty, binding.declaration))
		{
			self
				.annotations
				.record_local_definition_target(id, declaration);
			return ty;
		}
		if let Some(def) = self.defs.get(name) {
			return self.type_of_def(def, span, id);
		}
		match self.defs.resolve_variant(name) {
			Some(Ok((enum_def, variant))) => return self.variant_value(enum_def, variant, id, span),
			Some(Err(())) => {
				self.emit(span, TypeError::AmbiguousVariant { name: name.into() });
				return self.interner.error();
			}
			None => {}
		}
		self.emit(span, TypeError::CannotFind { name: name.into() });
		self.interner.error()
	}

	fn type_of_def(&mut self, def: DefId, span: Span, id: NodeId) -> Ty {
		self
			.annotations
			.record_definition_target(id, self.defs.stable(def));
		match self.defs.data(def).kind {
			DefKind::Let => {
				let ty = self
					.sigs
					.lets
					.get(&def)
					.map(|sig| sig.ty)
					.unwrap_or_else(|| self.fresh());
				if self.allow_imported_assignment && self.mutable_imports.contains(&def) {
					self.interner.mk_mut(ty)
				} else {
					ty
				}
			}
			DefKind::Func => {
				let (ty, arguments) = self.fn_type_of(def, span);
				self
					.annotations
					.record_generic_call_arguments(id, arguments);
				ty
			}
			DefKind::Variant { enum_def, variant } => self.variant_value(enum_def, variant, id, span),
			DefKind::Struct => {
				self.emit(span, TypeError::StructTypeAsValue);
				self.interner.error()
			}
			DefKind::Enum | DefKind::TypeAlias | DefKind::Namespace | DefKind::Interface => {
				self.emit(span, TypeError::TypeAsValue);
				self.interner.error()
			}
		}
	}

	/// The instantiated function type of a top-level `func`, with fresh variables
	/// for its generic parameters — declared (`0..sig.generics.len()`) and, per
	/// Slice 4F, synthetic (minted for `impl Interface` param sugar, see
	/// `Checker::synthetic_params_in`). Without the latter, a synthetic param
	/// leaks through `subst` rigid and unifying a concrete argument against it
	/// fails outright ("mismatched types: expected `T268435456`, found …") even
	/// though the exact same sugar resolves fine *inside* the callee's body.
	///
	/// Only synthetics occurring in *parameter* position are freshened here — a
	/// synthetic occurring only in return position (`impl Trait` return sugar)
	/// is left rigid, matching pre-4F behavior: the callee's own body-check
	/// already rejects it loudly (a distinct synthetic per mention never
	/// unifies with the body's result), and freshening it here would instead
	/// hand the caller an unconstrained variable carrying an unenforced bound.
	///
	/// Slice 4G: also defers one `pending_bounds` obligation per bound on every
	/// minted var — declared generics from `sig.bounds` (substituted through the
	/// same `subst` map as `params`/`ret`, so `Bound::ty`/`args` land on the
	/// freshly-minted variable) and synthetics from `synthetic_bounds` (bare
	/// interface only, no argument fidelity — `lower_reference` discards the
	/// interface's own arguments before minting the synthetic param). `span` is
	/// the call/reference site, so a violated bound diagnoses there rather than
	/// at the callee's declaration.
	fn fn_type_of(&mut self, def: DefId, span: Span) -> (Ty, Vec<Ty>) {
		let sig = self.sigs.funcs[&def].clone();
		// A recovered dependency may preserve a function's name/header while its
		// result slot is poisoned. Treat the exported value itself as poison so a
		// consumer cannot manufacture call/arity/bound cascades from an unavailable
		// signature. `Error` remains checker-local and permissive; the environment's
		// recovery bit is what prevents this result from reaching lowering.
		if matches!(self.interner.kind(sig.ret), TyKind::Error) {
			return (self.interner.error(), Vec::new());
		}
		let mut synthetics = FxHashSet::default();
		for p in &sig.params {
			self.synthetic_params_in(p.ty, &mut synthetics);
		}
		let indices = (0..sig.generics.len())
			.map(|i| ParamIdx(i as u32))
			.chain(synthetics.iter().copied());
		let inst = self.instantiate(sig.ret, &sig.bounds, indices, FxHashMap::default(), None);
		self.defer_obligations(span, inst.obligations.iter().cloned());
		let subst = inst.substitution;
		for idx in &synthetics {
			if let Some(interfaces) = self.synthetic_bounds.get(idx).cloned() {
				let ty = subst[idx];
				for interface in interfaces {
					self.pending_bounds.push(PendingBound {
						site: span,
						obligation: InstantiatedObligation {
							ty,
							interface,
							args: Vec::new(),
						},
					});
				}
			}
		}
		let params = sig
			.params
			.iter()
			.map(|p| self.subst(p.ty, &subst, None))
			.collect();
		let ret = self.subst(sig.ret, &subst, None);
		let arguments = (0..sig.generics.len())
			.map(|index| subst[&ParamIdx(index as u32)])
			.collect();
		(self.interner.mk_fn(params, ret), arguments)
	}

	/// Build the `(enum, variant)` resolution recorded for lowering.
	pub(crate) fn variant_resolution(
		&self,
		enum_def: DefId,
		variant: usize,
	) -> crate::annotate::VariantResolution {
		crate::annotate::VariantResolution {
			enum_name: self.defs.data(enum_def).name.clone(),
			variant: self.sigs.enums[&enum_def].variants[variant].name.clone(),
			enum_target: self.defs.stable(enum_def).cloned(),
			variant_target: self.sigs.enums[&enum_def].variants[variant].target.clone(),
		}
	}

	/// The value of an enum variant *referenced* by name (not constructed). A nullary
	/// variant is a value of the enum type; a field variant would be a first-class
	/// constructor, whose value ABI (an object-arg factory) does not match its
	/// positional function type — so it is rejected rather than silently miscompiled.
	/// Records the resolution on `id` (nullary only) so lowering can emit the ABI.
	fn variant_value(&mut self, enum_def: DefId, variant: usize, id: NodeId, span: Span) -> Ty {
		let vsig = &self.sigs.enums[&enum_def].variants[variant];
		let fields_empty = vsig.fields.is_empty();
		let name = vsig.name.clone();
		if fields_empty {
			let res = self.variant_resolution(enum_def, variant);
			self
				.annotations
				.record_definition_target(id, res.variant_target.as_ref());
			self.annotations.record_variant(id, res);
			let inst = self.instantiate_enum(enum_def);
			self.defer_obligations(span, inst.obligations.iter().cloned());
			let adt = inst.ty;
			// A nullary value commits the enum scheme's obligations. Pattern
			// callers instantiate the same scheme but deliberately do not defer
			// its returned obligations (CC3: they only destructure a value).
			adt
		} else {
			self.emit(span, TypeError::FieldVariantAsValue { variant: name });
			self.interner.error()
		}
	}

	pub(crate) fn instantiate_enum(&mut self, enum_def: DefId) -> crate::check::Instantiation {
		let arity = self.sigs.enums[&enum_def].generics.len();
		let positional = (0..arity)
			.map(|i| self.interner.mk_param(ParamIdx(i as u32)))
			.collect();
		let canonical = self
			.interner
			.mk_adt(enum_def, GenericArgs::new(positional, Vec::new()));
		let bounds = self.sigs.enums[&enum_def].bounds.clone();
		self.instantiate(
			canonical,
			&bounds,
			(0..arity).map(|i| ParamIdx(i as u32)),
			FxHashMap::default(),
			None,
		)
	}

	pub(crate) fn instantiate_struct(&mut self, struct_def: DefId) -> crate::check::Instantiation {
		let arity = self.sigs.structs[&struct_def].generics.len();
		let positional = (0..arity)
			.map(|i| self.interner.mk_param(ParamIdx(i as u32)))
			.collect();
		let canonical = self
			.interner
			.mk_adt(struct_def, GenericArgs::new(positional, Vec::new()));
		let bounds = self.sigs.structs[&struct_def].bounds.clone();
		self.instantiate(
			canonical,
			&bounds,
			(0..arity).map(|i| ParamIdx(i as u32)),
			FxHashMap::default(),
			None,
		)
	}

	// ── Calls & construction ─────────────────────────────────────────────────
	/// Infer a call's type. Also returns a `Resolution` when the call is a plain
	/// `receiver.method(args…)` dispatched through the interface solver (Finding
	/// 2, stdlib linkage groundwork) — mirroring how `infer_binary`/`infer_prefix`
	/// already return one for operator syntax — so `infer` (the sole caller) can
	/// record it once the node itself is recorded, and lowering can later refuse
	/// to emit a call to a method resolved through a prelude-only impl, which is
	/// never materialized anywhere lowering walks (see
	/// `dispatch_kind_for_method_call`). Every other call shape (constructor,
	/// namespaced, plain function call) carries no such resolution.
	fn infer_call(
		&mut self,
		func: &Expr,
		args: &[Spanned<CallArg>],
		span: Span,
		id: NodeId,
	) -> (Ty, Option<Resolution>) {
		// Constructor calls: `Struct(field = …)` / `Variant(field = …)`.
		if let ExprKind::Identifier(name) = &func.kind
			&& self.lookup_local(&name.0).is_none()
		{
			if let Some(def) = self.defs.get(&name.0)
				&& let DefKind::Struct = self.defs.data(def).kind
			{
				self
					.annotations
					.record_definition_target(id, self.defs.stable(def));
				self
					.annotations
					.record_definition_target(func.id, self.defs.stable(def));
				return (self.infer_struct_ctor(def, args, span), None);
			}
			match self.defs.resolve_variant(&name.0) {
				Some(Ok((enum_def, variant))) => {
					return (
						self.infer_variant_ctor(enum_def, variant, args, span, id, None),
						None,
					);
				}
				Some(Err(())) => {
					self.emit(
						span,
						TypeError::AmbiguousVariant {
							name: name.0.clone(),
						},
					);
					return (self.interner.error(), None);
				}
				None => {}
			}
		}

		// `Type.variant(…)` construction and `Type.static(…)` calls: the parent is a
		// type name, not a value.
		if let ExprKind::MemberAccess { parent, member, .. } = &func.kind
			&& let ExprKind::Identifier(type_name) = &parent.kind
			&& self.lookup_local(&type_name.0).is_none()
			&& let Some(def) = self.defs.get(&type_name.0)
		{
			self.record_member_completion_facts(parent, None, member.1);
			self
				.annotations
				.record_definition_target(parent.id, self.defs.stable(def));
			if self.defs.stable(def).is_none()
				&& let crate::DefOrigin::Imported { module } = &self.defs.data(def).origin
				&& matches!(self.defs.data(def).kind, DefKind::Namespace)
			{
				self
					.annotations
					.record_module_target(parent.id, Some(module));
			}
			match self.defs.data(def).kind {
				DefKind::Namespace => {
					let namespace_module = match &self.defs.data(def).origin {
						crate::DefOrigin::Imported { module } => Some(module.clone()),
						crate::DefOrigin::Local { .. } => None,
					};
					let namespace = self.sigs.namespaces.get(&def);
					let member_sig = namespace
						.and_then(|namespace| namespace.members.get(&member.0))
						.cloned();
					if let Some(NamespaceMemberSig::Func { target, sig }) = member_sig {
						self.annotations.record_direct_namespace_member(func.id);
						self
							.annotations
							.record_definition_target(id, target.as_ref());
						self
							.annotations
							.record_definition_target(func.id, target.as_ref());
						let (callee, type_arguments) = self.namespace_func_type(&sig, member.1);
						self
							.annotations
							.record_generic_call_arguments(id, type_arguments);
						self.annotations.record(
							func.id,
							crate::ExprInfo {
								ty: callee,
								resolution: None,
							},
						);
						return (self.check_direct_call(callee, args, span), None);
					}
					if let Some(module) = namespace_module {
						self
							.annotations
							.record_unresolved_qualified_access(module, member.0.clone(), member.1);
						return (self.interner.error(), None);
					}
				}
				DefKind::Enum => {
					let variant = self.sigs.enums[&def]
						.variants
						.iter()
						.position(|v| v.name == member.0);
					if let Some(variant) = variant {
						return (
							self.infer_variant_ctor(def, variant, args, member.1, id, None),
							None,
						);
					}
					let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(&a.0.value)).collect();
					let arg_lits = arg_int_lits(args);
					if let Some((ret, target, type_arguments)) =
						self.resolve_namespaced(def, &member.0, &arg_tys, &arg_lits, member.1)
					{
						self
							.annotations
							.record_generic_call_arguments(id, type_arguments);
						self.annotations.record_direct_namespace_member(func.id);
						self
							.annotations
							.record_definition_target(id, target.as_ref());
						self
							.annotations
							.record_definition_target(func.id, target.as_ref());
						return (ret, None);
					}
					self.emit(
						member.1,
						TypeError::NoVariantOrNamespacedFn {
							ty: type_name.0.clone(),
							name: member.0.clone(),
						},
					);
					return (self.interner.error(), None);
				}
				DefKind::Struct => {
					let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(&a.0.value)).collect();
					let arg_lits = arg_int_lits(args);
					if let Some((ret, target, type_arguments)) =
						self.resolve_namespaced(def, &member.0, &arg_tys, &arg_lits, member.1)
					{
						self
							.annotations
							.record_generic_call_arguments(id, type_arguments);
						self.annotations.record_direct_namespace_member(func.id);
						self
							.annotations
							.record_definition_target(id, target.as_ref());
						self
							.annotations
							.record_definition_target(func.id, target.as_ref());
						return (ret, None);
					}
					self.emit(
						member.1,
						TypeError::NoNamespacedFn {
							ty: type_name.0.clone(),
							name: member.0.clone(),
						},
					);
					return (self.interner.error(), None);
				}
				DefKind::TypeAlias => {
					let Some(alias) = self.sigs.aliases.get(&def).cloned() else {
						return (self.interner.error(), None);
					};
					let target = alias.target;
					let TyKind::Adt(owner, _) = self.interner.kind(target).clone() else {
						return (self.interner.error(), None);
					};
					if let Some(variant) = self
						.sigs
						.enums
						.get(&owner)
						.and_then(|enumeration| enumeration.variants.iter().position(|v| v.name == member.0))
					{
						return (
							self.infer_variant_ctor(owner, variant, args, member.1, id, Some(target)),
							None,
						);
					}
					let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(&a.0.value)).collect();
					let arg_lits = arg_int_lits(args);
					if let Some((ret, target, type_arguments)) = self.resolve_namespaced_on(
						owner,
						Some(target),
						&member.0,
						&arg_tys,
						&arg_lits,
						member.1,
					) {
						self
							.annotations
							.record_generic_call_arguments(id, type_arguments);
						self.annotations.record_direct_namespace_member(func.id);
						self
							.annotations
							.record_definition_target(id, target.as_ref());
						self
							.annotations
							.record_definition_target(func.id, target.as_ref());
						return (ret, None);
					}
					self.emit(
						member.1,
						TypeError::NoNamespacedFn {
							ty: type_name.0.clone(),
							name: member.0.clone(),
						},
					);
					return (self.interner.error(), None);
				}
				_ => {}
			}
		}

		// `P.name(args…)` where `P` is a generic type parameter: a namespaced interface
		// function reached through `P`'s bound, e.g. `R.default()` with `R: Default`.
		if let ExprKind::MemberAccess { parent, member, .. } = &func.kind
			&& let ExprKind::Identifier(pname) = &parent.kind
			&& self.lookup_local(&pname.0).is_none()
			&& let Some(pidx) = self.lookup_param(&pname.0)
		{
			self.record_member_completion_facts(parent, None, member.1);
			let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(&a.0.value)).collect();
			let (result, target, type_arguments) =
				self.resolve_param_namespaced(pidx, &member.0, &arg_tys, member.1);
			self
				.annotations
				.record_generic_call_arguments(id, type_arguments);
			if let Some((interface, member)) = target {
				self
					.annotations
					.record_definition_target(func.id, Some(&member));
				self.annotations.record_generic_namespaced_call(
					id,
					crate::annotate::GenericNamespacedCall {
						parameter: pidx,
						interface,
						member,
					},
				);
			}
			return (result, None);
		}

		// Method call: `receiver.method(args…)` resolves through the interface solver.
		if let ExprKind::MemberAccess { parent, member, .. } = &func.kind {
			let recv = self.infer(parent);
			self.record_member_completion_facts(parent, Some(recv), member.1);
			// A temporary (rvalue) receiver is owned here, so present it to the solver as
			// `mut`: a `mut func` such as an iterator's `fold`/`to_list`/`count` may then be
			// called directly on `a.map(f)` without an intermediate `let mut` binding.
			let recv_dispatch = {
				let resolved = self.shallow_resolve(recv);
				if (!expr_is_place(parent) || self.custom_index_value(parent))
					&& !matches!(self.interner.kind(resolved), TyKind::Mut(_))
				{
					self.interner.mk_mut(resolved)
				} else {
					recv
				}
			};
			let arg_tys: Vec<Ty> = args
				.iter()
				.map(|a| self.infer_method_call_arg(&a.0.value))
				.collect();
			let arg_lits = arg_int_lits(args);
			return match self.resolve_method(recv_dispatch, &member.0, &arg_tys, &arg_lits, member.1) {
				Some(res) => {
					self
						.annotations
						.record_definition_target(func.id, res.target.as_ref());
					self
						.annotations
						.record_generic_call_arguments(id, res.type_arguments.clone());
					for (argument, expected) in args.iter().zip(&res.params) {
						if matches!(argument.0.value.kind, ExprKind::Closure { .. }) {
							self.check_closure(&argument.0.value, *expected);
						}
					}
					let dispatch = dispatch_kind_for_method_call(&res);
					let resolution = Resolution {
						method: member.0.clone(),
						dispatch,
						target: res.target.clone(),
						implementation: res.implementation.clone(),
						resolved_target: res.resolved_target.clone(),
					};
					(res.ty, Some(resolution))
				}
				None => {
					let rendered = self.display(recv);
					self.emit(
						member.1,
						TypeError::NoMethod {
							method: member.0.clone(),
							ty: rendered,
						},
					);
					(self.interner.error(), None)
				}
			};
		}

		let callee = self.infer(func);
		// Hidden ABI metadata belongs to the call expression, not to a callee
		// reference which may be grouped or otherwise reused by lowering. A call
		// used as a callee keeps its own metadata: `factory(value)()` must pass the
		// hidden arguments to `factory`, not to the callable it returns.
		let mut metadata_source = func;
		while let ExprKind::Grouped(inner) = &metadata_source.kind {
			metadata_source = inner;
		}
		if matches!(
			metadata_source.kind,
			ExprKind::Identifier(_) | ExprKind::MemberAccess { .. }
		) {
			self.annotations.move_generic_call_arguments(func.id, id);
		}
		if matches!(self.interner.kind(callee), TyKind::Never) {
			return (self.interner.never(), None);
		}
		let callee = self.strip_mut(callee);
		let ty = match self.interner.kind(callee).clone() {
			TyKind::Fn { params, ret } => {
				if args.len() != params.len() {
					self.emit(
						span,
						TypeError::WrongArgCount {
							expected: params.len(),
							found: args.len(),
						},
					);
				}
				for (arg, pty) in args.iter().zip(&params) {
					self.check_call_arg(&arg.0.value, *pty);
				}
				for arg in args.iter().skip(params.len()) {
					self.infer(&arg.0.value);
				}
				ret
			}
			TyKind::Error => self.interner.error(),
			TyKind::Infer(_) => {
				for arg in args {
					self.infer(&arg.0.value);
				}
				self.fresh()
			}
			_ => {
				self.emit(span, TypeError::NotCallable);
				for arg in args {
					self.infer(&arg.0.value);
				}
				self.interner.error()
			}
		};
		(ty, None)
	}

	fn infer_struct_ctor(&mut self, def: DefId, args: &[Spanned<CallArg>], span: Span) -> Ty {
		let inst = self.instantiate_struct(def);
		self.defer_obligations(span, inst.obligations.iter().cloned());
		let (adt, subst) = (inst.ty, inst.substitution);
		let sig = self.sigs.structs[&def].clone();
		// Construction commits the exact obligations returned with the same
		// substitution used for the fields below. Pattern callers intentionally
		// leave those obligations undeferred.
		let fields: Vec<(EcoString, Ty, Option<crate::DefinitionId>)> = sig
			.fields
			.iter()
			.zip(&sig.field_metadata)
			.map(|((n, t), metadata)| {
				(
					n.clone(),
					self.subst(*t, &subst, None),
					metadata.target.clone(),
				)
			})
			.collect();
		self.check_ctor_args(&fields, args);
		adt
	}

	/// `expected`, when given, is the concrete enum type the caller already knows this
	/// construction must produce (from [`Self::try_check_expected_variant`], which only
	/// reaches here once `expected_enum_variant` has confirmed `enum_def` matches it).
	/// Unifying the fresh instantiation against it *before* substituting field types
	/// means a generic field's expected type (e.g. `Option<Tree>`'s `value: T`) is
	/// already the concrete `Tree` — not a still-unbound `Infer` var — by the time
	/// `check_ctor_args` recurses into a nested bare-variant argument, so that nested
	/// argument's own type-directed disambiguation (`try_check_expected_variant` again,
	/// transitively) has something concrete to resolve against. Without this, the
	/// unification that pins the var only happens afterward, in the *outer*
	/// `check_dispatch`'s `subtype(got, expected, ...)` call — too late for the nested
	/// argument, which has already fallen back to the ambiguous global lookup.
	fn infer_variant_ctor(
		&mut self,
		enum_def: DefId,
		variant: usize,
		args: &[Spanned<CallArg>],
		span: Span,
		id: NodeId,
		expected: Option<Ty>,
	) -> Ty {
		let res = self.variant_resolution(enum_def, variant);
		self
			.annotations
			.record_definition_target(id, res.variant_target.as_ref());
		self.annotations.record_variant(id, res);
		let inst = self.instantiate_enum(enum_def);
		self.defer_obligations(span, inst.obligations.iter().cloned());
		let (adt, subst) = (inst.ty, inst.substitution);
		if let Some(expected) = expected {
			self.unify(adt, expected, span);
		}
		let sig = self.sigs.enums[&enum_def].clone();
		// Same reasoning as `infer_struct_ctor` above, for the enum's own bounds.
		let vsig = sig.variants[variant].clone();
		let fields: Vec<(EcoString, Ty, Option<crate::DefinitionId>)> = vsig
			.fields
			.iter()
			.zip(&vsig.field_metadata)
			.map(|((n, t), metadata)| {
				(
					n.clone(),
					self.subst(*t, &subst, None),
					metadata.target.clone(),
				)
			})
			.collect();
		self.check_ctor_args(&fields, args);
		adt
	}

	/// Check constructor arguments against declared fields, by label when present
	/// else positionally.
	fn check_ctor_args(
		&mut self,
		fields: &[(EcoString, Ty, Option<crate::DefinitionId>)],
		args: &[Spanned<CallArg>],
	) {
		for (i, arg) in args.iter().enumerate() {
			let call = &arg.0;
			let target = if let Some(label) = &call.name {
				match fields.iter().find(|(n, ..)| n == &label.0) {
					Some((_, ty, definition)) => {
						self
							.annotations
							.record_source_definition_target(label.1, definition.as_ref());
						Some(*ty)
					}
					None => {
						self.emit(
							label.1,
							TypeError::UnknownField {
								field: label.0.clone(),
							},
						);
						None
					}
				}
			} else {
				match fields.get(i) {
					Some((_, ty, _)) => Some(*ty),
					None => {
						self.emit(call.value.span, TypeError::TooManyFields);
						None
					}
				}
			};
			match target {
				Some(ty) => {
					self.resolve_anon(&call.value, Some(ty));
					self.check(&call.value, ty);
				}
				None => {
					self.resolve_anon(&call.value, None);
					self.infer(&call.value);
				}
			}
		}
	}

	// ── Member access ────────────────────────────────────────────────────────
	fn infer_member_with_resolution(
		&mut self,
		parent: &Expr,
		member: &str,
		span: Span,
		id: NodeId,
	) -> (Ty, Option<Resolution>) {
		if let ExprKind::Identifier(name) = &parent.kind
			&& self.lookup_local(&name.0).is_none()
			&& let Some(definition) = self.defs.get(&name.0)
			&& matches!(
				self.defs.data(definition).kind,
				DefKind::Namespace | DefKind::Struct | DefKind::Enum | DefKind::TypeAlias
			) {
			return (self.infer_member(parent, member, span, id), None);
		}
		let parent_ty = self.infer(parent);
		self.record_member_completion_facts(parent, Some(parent_ty), span);
		if matches!(self.interner.kind(parent_ty), TyKind::Never) {
			return (self.interner.never(), None);
		}
		let resolved_parent = self.shallow_resolve(parent_ty);
		let nominal = self.strip_mut(resolved_parent);
		let has_field = match self.interner.kind(nominal) {
			TyKind::Adt(definition, _) if matches!(self.defs.data(*definition).kind, DefKind::Struct) => {
				self.sigs.structs[definition]
					.fields
					.iter()
					.any(|(name, _)| name == member)
			}
			_ => false,
		};
		let method_receiver = if (!expr_is_place(parent) || self.custom_index_value(parent))
			&& !matches!(self.interner.kind(resolved_parent), TyKind::Mut(_))
		{
			self.interner.mk_mut(resolved_parent)
		} else {
			parent_ty
		};
		if !has_field && let Some(resolution) = self.resolve_method_value(method_receiver, member, span)
		{
			self
				.annotations
				.record_generic_call_arguments(id, resolution.type_arguments.clone());
			let ty = self
				.interner
				.mk_fn(resolution.params.clone(), resolution.ty);
			let dispatch = dispatch_kind_for_method_call(&resolution);
			return (
				ty,
				Some(Resolution {
					method: member.into(),
					dispatch,
					target: resolution.target,
					implementation: resolution.implementation,
					resolved_target: resolution.resolved_target,
				}),
			);
		}
		(self.member_ty_of(parent_ty, member, span, Some(id)), None)
	}

	/// Capture completion at the point where all lexical generic bounds and place
	/// mutability are live. Tooling consumes these immutable facts and never
	/// reconstructs solver decisions.
	fn record_member_completion_facts(&mut self, parent: &Expr, inferred: Option<Ty>, span: Span) {
		use crate::{MemberCompletion, MemberCompletionKind};
		let mut out = Vec::new();
		if let ExprKind::Identifier(name) = &parent.kind
			&& self.lookup_local(&name.0).is_none()
			&& let Some(param) = self.lookup_param(&name.0)
		{
			let mut names = self
				.param_interface_bounds(param)
				.into_iter()
				.filter_map(|(definition, _)| self.interfaces.get(&definition))
				.flat_map(|interface| interface.methods.keys().cloned())
				.collect::<Vec<_>>();
			names.sort();
			names.dedup();
			for candidate_name in names {
				let checkpoint = self.table.snapshot();
				let diagnostics = self.diags.len();
				let pending_bounds = self.pending_bounds.len();
				let pending_operators = self.pending_operators.len();
				let pending_bound_arg_mut = self.pending_bound_arg_mut.clone();
				let synthetic_params = self.synthetic_params;
				let synthetic_bounds = self.synthetic_bounds.clone();
				let synthetic_bound_details = self.synthetic_bound_details.clone();
				let annotations = self.annotations.clone();
				let resolved = self.resolve_param_namespaced_value(param, &candidate_name, span);
				let detail = resolved.map(|(params, ret)| {
					let ty = self.interner.mk_fn(params, ret);
					self.display(ty)
				});
				self.table.rollback_to(checkpoint);
				self.diags.truncate(diagnostics);
				self.pending_bounds.truncate(pending_bounds);
				self.pending_operators.truncate(pending_operators);
				self.pending_bound_arg_mut = pending_bound_arg_mut;
				self.synthetic_params = synthetic_params;
				self.synthetic_bounds = synthetic_bounds;
				self.synthetic_bound_details = synthetic_bound_details;
				self.annotations = annotations;
				if let Some(detail) = detail {
					out.push(MemberCompletion {
						name: candidate_name,
						kind: MemberCompletionKind::Function,
						detail,
					});
				}
			}
			self.annotations.record_member_completions(parent.id, out);
			return;
		}
		if let ExprKind::Identifier(name) = &parent.kind
			&& self.lookup_local(&name.0).is_none()
			&& let Some(def) = self.defs.get(&name.0)
		{
			let checkpoint = self.table.snapshot();
			let diagnostics = self.diags.len();
			let pending_bounds = self.pending_bounds.len();
			let pending_operators = self.pending_operators.len();
			let pending_bound_arg_mut = self.pending_bound_arg_mut.clone();
			let synthetic_params = self.synthetic_params;
			let synthetic_bounds = self.synthetic_bounds.clone();
			let synthetic_bound_details = self.synthetic_bound_details.clone();
			let annotations = self.annotations.clone();
			match self.defs.data(def).kind {
				DefKind::Namespace => {
					if let Some(namespace) = self.sigs.namespaces.get(&def).cloned() {
						for (name, member) in namespace.members {
							let (kind, detail) = match member {
								NamespaceMemberSig::Func { sig, .. } => {
									let (ty, _) = self.namespace_func_type(&sig, span);
									(MemberCompletionKind::Function, self.display(ty))
								}
								NamespaceMemberSig::Value { ty, mutable, .. } => {
									let kind = if mutable {
										MemberCompletionKind::Variable
									} else {
										MemberCompletionKind::Value
									};
									(kind, self.display(ty))
								}
							};
							out.push(MemberCompletion { name, kind, detail });
						}
					}
				}
				DefKind::Struct | DefKind::Enum | DefKind::TypeAlias => {
					let owner = if matches!(self.defs.data(def).kind, DefKind::TypeAlias) {
						self
							.sigs
							.aliases
							.get(&def)
							.and_then(|alias| match self.interner.kind(alias.target) {
								TyKind::Adt(owner, _) => Some(*owner),
								_ => None,
							})
					} else {
						Some(def)
					};
					let concrete_owner = if matches!(self.defs.data(def).kind, DefKind::TypeAlias) {
						self.sigs.aliases.get(&def).map(|alias| alias.target)
					} else {
						None
					};
					if let Some(enumeration) = owner.and_then(|owner| self.sigs.enums.get(&owner)).cloned() {
						let result =
							concrete_owner.unwrap_or_else(|| self.instantiate_enum(owner.unwrap_or(def)).ty);
						let subst = match self.interner.kind(result) {
							TyKind::Adt(_, args) => adt_subst(args),
							_ => FxHashMap::default(),
						};
						for variant in enumeration.variants {
							let fields = variant
								.fields
								.into_iter()
								.map(|(name, ty)| (name, self.subst(ty, &subst, Some(result))))
								.collect::<Vec<_>>();
							let detail = if fields.is_empty() {
								self.display(result)
							} else {
								let ty = self
									.interner
									.mk_fn(fields.into_iter().map(|(_, ty)| ty).collect(), result);
								self.display(ty)
							};
							out.push(MemberCompletion {
								name: variant.name,
								kind: MemberCompletionKind::Variant,
								detail,
							});
						}
					}
					let mut names = owner
						.into_iter()
						.flat_map(|owner| self.inherent.candidates(crate::iface::Head::Adt(owner)))
						.flat_map(|index| self.inherent.impls[index].methods.keys().cloned())
						.collect::<Vec<_>>();
					names.sort();
					names.dedup();
					for name in names {
						let checkpoint = self.table.snapshot();
						let diagnostics = self.diags.len();
						let pending_bounds = self.pending_bounds.len();
						let pending_operators = self.pending_operators.len();
						let pending_bound_arg_mut = self.pending_bound_arg_mut.clone();
						let synthetic_params = self.synthetic_params;
						let synthetic_bounds = self.synthetic_bounds.clone();
						let synthetic_bound_details = self.synthetic_bound_details.clone();
						let annotations = self.annotations.clone();
						let mut candidate = None;
						if let Some((params, ret, _, _)) =
							self.resolve_namespaced_value_on(owner.unwrap_or(def), concrete_owner, &name, span)
							&& self.diags.len() == diagnostics
							&& !matches!(self.interner.kind(ret), TyKind::Error)
						{
							let ty = self.interner.mk_fn(params, ret);
							candidate = Some(self.display(ty));
						}
						self.diags.truncate(diagnostics);
						self.table.rollback_to(checkpoint);
						self.pending_bounds.truncate(pending_bounds);
						self.pending_operators.truncate(pending_operators);
						self.pending_bound_arg_mut = pending_bound_arg_mut;
						self.synthetic_params = synthetic_params;
						self.synthetic_bounds = synthetic_bounds;
						self.synthetic_bound_details = synthetic_bound_details;
						self.annotations = annotations;
						if let Some(detail) = candidate {
							out.push(MemberCompletion {
								name,
								kind: MemberCompletionKind::Function,
								detail,
							});
						}
					}
				}
				_ => {}
			}
			self.diags.truncate(diagnostics);
			self.table.rollback_to(checkpoint);
			self.pending_bounds.truncate(pending_bounds);
			self.pending_operators.truncate(pending_operators);
			self.pending_bound_arg_mut = pending_bound_arg_mut;
			self.synthetic_params = synthetic_params;
			self.synthetic_bounds = synthetic_bounds;
			self.synthetic_bound_details = synthetic_bound_details;
			self.annotations = annotations;
			self.annotations.record_member_completions(parent.id, out);
			return;
		}

		let Some(recv) = inferred else {
			self.annotations.record_member_completions(parent.id, out);
			return;
		};
		let resolved = self.resolve_deep(recv);
		if matches!(self.interner.kind(resolved), TyKind::Error) {
			self.annotations.record_member_completions(parent.id, out);
			return;
		}
		let temporary_receiver = !expr_is_place(parent) || self.custom_index_value(parent);
		if self.has_infer(resolved) {
			self
				.pending_member_completions
				.push(PendingMemberCompletion {
					receiver: parent.id,
					ty: resolved,
					span,
					temporary_receiver,
					param_bounds: self.param_bounds.clone(),
					param_bound_details: self.param_bound_details.clone(),
					checking_interface_default: self.checking_interface_default,
				});
			self.annotations.record_member_completions(parent.id, out);
			return;
		}
		self.record_value_member_completion_facts(parent.id, resolved, span, temporary_receiver);
	}

	fn record_value_member_completion_facts(
		&mut self,
		receiver: NodeId,
		recv: Ty,
		span: Span,
		temporary_receiver: bool,
	) {
		use crate::{MemberCompletion, MemberCompletionKind};
		let mut out = Vec::new();
		let dispatch = if temporary_receiver && !matches!(self.interner.kind(recv), TyKind::Mut(_)) {
			self.interner.mk_mut(recv)
		} else {
			recv
		};
		let nominal = self.strip_mut(recv);
		if let TyKind::Adt(def, args) = self.interner.kind(nominal).clone()
			&& let Some(sig) = self.sigs.structs.get(&def).cloned()
		{
			let subst = adt_subst(&args);
			for (name, ty) in sig.fields {
				let ty = self.subst(ty, &subst, Some(nominal));
				out.push(MemberCompletion {
					name,
					kind: MemberCompletionKind::Field,
					detail: self.display(ty),
				});
			}
		}
		let mut names = self
			.inherent
			.impls
			.iter()
			.flat_map(|i| i.methods.keys().cloned())
			.chain(
				self
					.interfaces
					.values()
					.flat_map(|i| i.methods.keys().cloned()),
			)
			.collect::<Vec<_>>();
		names.sort();
		names.dedup();
		for name in names {
			if out.iter().any(|candidate| candidate.name == name) {
				continue;
			}
			let checkpoint = self.table.snapshot();
			let diagnostics = self.diags.len();
			let pending_bounds = self.pending_bounds.len();
			let pending_operators = self.pending_operators.len();
			let pending_bound_arg_mut = self.pending_bound_arg_mut.clone();
			let synthetic_params = self.synthetic_params;
			let synthetic_bounds = self.synthetic_bounds.clone();
			let synthetic_bound_details = self.synthetic_bound_details.clone();
			let annotations = self.annotations.clone();
			if let Some(method) = self.resolve_method_value(dispatch, &name, span)
				&& self.diags.len() == diagnostics
				&& !matches!(self.interner.kind(method.ty), TyKind::Error)
			{
				let ty = self.interner.mk_fn(method.params, method.ty);
				let detail = self.display(ty);
				out.push(MemberCompletion {
					name,
					kind: MemberCompletionKind::Method,
					detail,
				});
			}
			self.diags.truncate(diagnostics);
			self.table.rollback_to(checkpoint);
			self.pending_bounds.truncate(pending_bounds);
			self.pending_operators.truncate(pending_operators);
			self.pending_bound_arg_mut = pending_bound_arg_mut;
			self.synthetic_params = synthetic_params;
			self.synthetic_bounds = synthetic_bounds;
			self.synthetic_bound_details = synthetic_bound_details;
			self.annotations = annotations;
		}
		self.annotations.record_member_completions(receiver, out);
	}

	pub(crate) fn finalize_pending_member_completions(&mut self) {
		for pending in std::mem::take(&mut self.pending_member_completions) {
			let previous_bounds = std::mem::replace(&mut self.param_bounds, pending.param_bounds);
			let previous_bound_details =
				std::mem::replace(&mut self.param_bound_details, pending.param_bound_details);
			let previous_default = std::mem::replace(
				&mut self.checking_interface_default,
				pending.checking_interface_default,
			);
			let receiver = self.resolve_deep(pending.ty);
			if !self.has_infer(receiver) && !matches!(self.interner.kind(receiver), TyKind::Error) {
				self.record_value_member_completion_facts(
					pending.receiver,
					receiver,
					pending.span,
					pending.temporary_receiver,
				);
			}
			self.param_bounds = previous_bounds;
			self.param_bound_details = previous_bound_details;
			self.checking_interface_default = previous_default;
		}
	}

	fn infer_member(&mut self, parent: &Expr, member: &str, span: Span, id: NodeId) -> Ty {
		if let ExprKind::Identifier(name) = &parent.kind
			&& self.lookup_local(&name.0).is_none()
			&& let Some(definition) = self.defs.get(&name.0)
			&& matches!(
				self.defs.data(definition).kind,
				DefKind::Namespace | DefKind::Struct | DefKind::Enum | DefKind::TypeAlias
			) {
			self.record_member_completion_facts(parent, None, span);
		}
		if let ExprKind::Identifier(name) = &parent.kind
			&& self.lookup_local(&name.0).is_none()
			&& let Some(def) = self.defs.get(&name.0)
			&& matches!(
				self.defs.data(def).kind,
				DefKind::Namespace | DefKind::Struct | DefKind::Enum | DefKind::TypeAlias
			) {
			self
				.annotations
				.record_definition_target(parent.id, self.defs.stable(def));
		}
		if let ExprKind::Identifier(name) = &parent.kind
			&& self.lookup_local(&name.0).is_none()
			&& let Some(def) = self.defs.get(&name.0)
			&& matches!(self.defs.data(def).kind, DefKind::Namespace)
		{
			let module = match (&self.defs.data(def).origin, self.defs.stable(def)) {
				(crate::DefOrigin::Imported { module }, None) => Some(module.clone()),
				(crate::DefOrigin::Local { .. }, _) => None,
				(crate::DefOrigin::Imported { .. }, Some(_)) => None,
			};
			self
				.annotations
				.record_module_target(parent.id, module.as_ref());
			return match self
				.sigs
				.namespaces
				.get(&def)
				.and_then(|namespace| namespace.members.get(member))
				.cloned()
			{
				Some(NamespaceMemberSig::Value { target, ty, .. }) => {
					self.annotations.record_direct_namespace_member(id);
					self
						.annotations
						.record_definition_target(id, target.as_ref());
					ty
				}
				Some(NamespaceMemberSig::Func { target, sig }) => {
					self.annotations.record_direct_namespace_member(id);
					self
						.annotations
						.record_definition_target(id, target.as_ref());
					let (ty, type_arguments) = self.namespace_func_type(&sig, span);
					self
						.annotations
						.record_generic_call_arguments(id, type_arguments);
					ty
				}
				None => {
					if let crate::DefOrigin::Imported { module } = &self.defs.data(def).origin {
						self.annotations.record_unresolved_qualified_access(
							module.clone(),
							member.into(),
							span,
						);
						return self.interner.error();
					}
					self.emit(
						span,
						TypeError::NoField {
							field: member.into(),
							ty: name.0.to_string(),
						},
					);
					self.interner.error()
				}
			};
		}
		if let ExprKind::Identifier(type_name) = &parent.kind
			&& self.lookup_local(&type_name.0).is_none()
			&& let Some(definition) = self.defs.get(&type_name.0)
			&& matches!(self.defs.data(definition).kind, DefKind::Struct)
		{
			if let Some((parameters, return_type, target, type_arguments)) =
				self.resolve_namespaced_value(definition, member, span)
			{
				self.annotations.record_direct_namespace_member(id);
				self
					.annotations
					.record_definition_target(id, target.as_ref());
				self
					.annotations
					.record_generic_call_arguments(id, type_arguments);
				return self.interner.mk_fn(parameters, return_type);
			}
			self.emit(
				span,
				TypeError::NoNamespacedFn {
					ty: type_name.0.clone(),
					name: member.into(),
				},
			);
			return self.interner.error();
		}
		if let ExprKind::Identifier(type_name) = &parent.kind
			&& self.lookup_local(&type_name.0).is_none()
			&& let Some(alias_def) = self.defs.get(&type_name.0)
			&& matches!(self.defs.data(alias_def).kind, DefKind::TypeAlias)
			&& let Some(alias) = self.sigs.aliases.get(&alias_def).cloned()
			&& let TyKind::Adt(owner, _) = self.interner.kind(alias.target).clone()
		{
			if let Some(variant) = self
				.sigs
				.enums
				.get(&owner)
				.and_then(|enumeration| enumeration.variants.iter().position(|v| v.name == member))
			{
				let result = self.variant_value(owner, variant, id, span);
				self.unify(result, alias.target, span);
				return alias.target;
			}
			if let Some((parameters, return_type, target, type_arguments)) =
				self.resolve_namespaced_value_on(owner, Some(alias.target), member, span)
			{
				self.annotations.record_direct_namespace_member(id);
				self
					.annotations
					.record_definition_target(id, target.as_ref());
				self
					.annotations
					.record_generic_call_arguments(id, type_arguments);
				return self.interner.mk_fn(parameters, return_type);
			}
			self.emit(
				span,
				TypeError::NoNamespacedFn {
					ty: type_name.0.clone(),
					name: member.into(),
				},
			);
			return self.interner.error();
		}
		// `EnumName.Variant` — a variant referenced through its type.
		if let ExprKind::Identifier(tname) = &parent.kind
			&& self.lookup_local(&tname.0).is_none()
			&& let Some(def) = self.defs.get(&tname.0)
			&& let DefKind::Enum = self.defs.data(def).kind
		{
			let variants = &self.sigs.enums[&def].variants;
			if let Some(vidx) = variants.iter().position(|v| v.name == member) {
				return self.variant_value(def, vidx, id, span);
			}
			if let Some((parameters, return_type, target, type_arguments)) =
				self.resolve_namespaced_value(def, member, span)
			{
				self.annotations.record_direct_namespace_member(id);
				self
					.annotations
					.record_definition_target(id, target.as_ref());
				self
					.annotations
					.record_generic_call_arguments(id, type_arguments);
				return self.interner.mk_fn(parameters, return_type);
			}
			self.emit(
				span,
				TypeError::NoVariantOrNamespacedFn {
					ty: tname.0.clone(),
					name: member.into(),
				},
			);
			return self.interner.error();
		}

		let parent_ty = self.infer(parent);
		self.record_member_completion_facts(parent, Some(parent_ty), span);
		self.member_ty_of(parent_ty, member, span, Some(id))
	}

	fn namespace_func_type(&mut self, sig: &FuncSig, span: Span) -> (Ty, Vec<Ty>) {
		let inst = self.instantiate(
			sig.ret,
			&sig.bounds,
			(0..sig.generics.len()).map(|index| ParamIdx(index as u32)),
			FxHashMap::default(),
			None,
		);
		self.defer_obligations(span, inst.obligations.iter().cloned());
		let subst = inst.substitution;
		let params = sig
			.params
			.iter()
			.map(|param| self.subst(param.ty, &subst, None))
			.collect();
		let ret = self.subst(sig.ret, &subst, None);
		let arguments = (0..sig.generics.len())
			.map(|index| subst[&ParamIdx(index as u32)])
			.collect();
		(self.interner.mk_fn(params, ret), arguments)
	}

	fn check_direct_call(&mut self, callee: Ty, args: &[Spanned<CallArg>], span: Span) -> Ty {
		let TyKind::Fn { params, ret } = self.interner.kind(callee).clone() else {
			return self.interner.error();
		};
		if args.len() != params.len() {
			self.emit(
				span,
				TypeError::WrongArgCount {
					expected: params.len(),
					found: args.len(),
				},
			);
		}
		for (argument, parameter) in args.iter().zip(&params) {
			self.check_call_arg(&argument.0.value, *parameter);
		}
		for argument in args.iter().skip(params.len()) {
			self.infer(&argument.0.value);
		}
		ret
	}

	/// The type of `member` accessed on a value of type `parent_ty`. Split out of
	/// [`Self::infer_member`] so a `mut Adt` receiver (e.g. `this` inside a `mut
	/// func`) can re-dispatch field lookup on the peeled inner type without
	/// re-inferring (and so re-recording) the parent expression.
	fn member_ty_of(&mut self, parent_ty: Ty, member: &str, span: Span, id: Option<NodeId>) -> Ty {
		let parent_ty = self.shallow_resolve(parent_ty);
		match self.interner.kind(parent_ty).clone() {
			TyKind::Adt(def, args) => {
				if matches!(self.defs.data(def).kind, DefKind::Struct) {
					let sig = self.sigs.structs[&def].clone();
					let subst = adt_subst(&args);
					if let Some((field_index, (_, fty))) = sig
						.fields
						.iter()
						.enumerate()
						.find(|(_, (n, _))| n == member)
					{
						if let Some(id) = id {
							self.annotations.record_definition_target(
								id,
								sig
									.field_metadata
									.get(field_index)
									.and_then(|metadata| metadata.target.as_ref()),
							);
						}
						return self.subst(*fty, &subst, Some(parent_ty));
					}
					let owner = self.defs.data(def).name.clone();
					self.emit(
						span,
						TypeError::NoField {
							field: member.into(),
							ty: owner.to_string(),
						},
					);
					return self.interner.error();
				}
				self.emit(span, TypeError::MethodCallsUnsupported);
				self.interner.error()
			}
			// A `mut Struct` still has the struct's fields — re-dispatch on the peeled
			// inner type, then re-wrap the field in `mut`: projecting a field out of a
			// mutable receiver yields a mutable *place*, so a `mut func` may be called on
			// it (e.g. an iterator adapter's `this.source.next()`) and it may be
			// reassigned. Reads coerce `mut T` → `T` as usual (`coerce.rs`), so this
			// doesn't disturb ordinary field reads. `mk_mut` is idempotent, so a field
			// whose declared type is already `mut` isn't double-wrapped; `Error` stays
			// `Error`.
			TyKind::Mut(inner) => {
				let fty = self.member_ty_of(inner, member, span, id);
				match self.interner.kind(fty) {
					TyKind::Error => fty,
					_ => self.interner.mk_mut(fty),
				}
			}
			TyKind::Error => self.interner.error(),
			TyKind::Infer(_) => self.fresh(),
			_ => {
				let rendered = self.display(parent_ty);
				self.emit(
					span,
					TypeError::CannotAccess {
						member: member.into(),
						ty: rendered,
					},
				);
				self.interner.error()
			}
		}
	}

	// ── Closures ─────────────────────────────────────────────────────────────
	fn infer_closure(&mut self, expr: &Expr) -> Ty {
		let ExprKind::Closure {
			params,
			generics,
			return_type,
			body,
			label,
		} = &expr.kind
		else {
			unreachable!("guarded by caller");
		};
		self.push_params(build_param_scope(generics));
		self.push_scope();
		let mut param_tys = Vec::new();
		for param in params {
			let ty = match &param.0.type_ {
				Some(annot) => self.lower_type(annot),
				None => self.fresh(),
			};
			self.bind_pattern(&param.0.name, ty, param.0.mutable);
			param_tys.push(ty);
		}
		// An explicit closure's own body is itself a hard `$N` boundary: it does
		// NOT let an inner `$N` escape out to some OUTER enclosing slot (see
		// `resolve_anon`'s doc comment on why every closure-slot call site,
		// this one included, must scan its own slot before checking/inferring
		// it).
		let outer_loops = std::mem::take(&mut self.loop_controls);
		let outer_labels = std::mem::take(&mut self.control_labels);
		let closure_ret = return_type
			.as_ref()
			.map(|annot| self.lower_type(annot))
			.unwrap_or_else(|| self.fresh());
		self.push_control_label(
			label.as_ref(),
			expr.id,
			ControlLabelKind::Callable,
			None,
			Some(closure_ret),
		);
		let ret = match return_type {
			Some(_) => {
				let rt = closure_ret;
				let outer_ret = self.ret_ty.replace(rt);
				self.resolve_anon(body, Some(rt));
				self.check(body, rt);
				self.ret_ty = outer_ret;
				rt
			}
			None => {
				let rt = closure_ret;
				let outer_ret = self.ret_ty.replace(rt);
				self.resolve_anon(body, Some(rt));
				self.check(body, rt);
				self.ret_ty = outer_ret;
				rt
			}
		};
		self.loop_controls = outer_loops;
		self.control_labels = outer_labels;
		self.pop_scope();
		self.pop_params();
		self.interner.mk_fn(param_tys, ret)
	}

	fn check_closure(&mut self, expr: &Expr, expected: Ty) {
		let expected = self.strip_mut(expected);
		let ExprKind::Closure {
			params,
			generics,
			return_type,
			body,
			label,
		} = &expr.kind
		else {
			unreachable!("guarded by caller");
		};

		let outer_loops = std::mem::take(&mut self.loop_controls);
		let outer_labels = std::mem::take(&mut self.control_labels);
		// Pull expected parameter/return types out of an expected function type.
		let (exp_params, exp_ret) = match self.interner.kind(expected).clone() {
			TyKind::Fn { params, ret } => (Some(params), Some(ret)),
			_ => (None, None),
		};
		self.push_control_label(
			label.as_ref(),
			expr.id,
			ControlLabelKind::Callable,
			None,
			exp_ret,
		);

		self.push_params(build_param_scope(generics));
		self.push_scope();
		let mut param_tys = Vec::new();
		for (i, param) in params.iter().enumerate() {
			let ty = match &param.0.type_ {
				Some(annot) => self.lower_type(annot),
				None => exp_params
					.as_ref()
					.and_then(|ps| ps.get(i).copied())
					.unwrap_or_else(|| self.fresh()),
			};
			self.bind_pattern(&param.0.name, ty, param.0.mutable);
			param_tys.push(ty);
		}
		// See `infer_closure`'s matching comment: an explicit closure's body is
		// its own hard `$N` boundary.
		let ret = match (return_type, exp_ret) {
			(Some(annot), _) => {
				let rt = self.lower_type(annot);
				let outer_ret = self.ret_ty.replace(rt);
				self.resolve_anon(body, Some(rt));
				self.check(body, rt);
				self.ret_ty = outer_ret;
				rt
			}
			(None, Some(rt)) => {
				let outer_ret = self.ret_ty.replace(rt);
				self.resolve_anon(body, Some(rt));
				self.check(body, rt);
				self.ret_ty = outer_ret;
				rt
			}
			(None, None) => {
				let rt = self.fresh();
				let outer_ret = self.ret_ty.replace(rt);
				self.resolve_anon(body, Some(rt));
				self.check(body, rt);
				self.ret_ty = outer_ret;
				rt
			}
		};
		self.pop_scope();
		self.pop_params();
		self.loop_controls = outer_loops;
		self.control_labels = outer_labels;
		let got = self.interner.mk_fn(param_tys, ret);
		self.subtype(got, expected, expr.span);
	}

	// ── Operators ─────────────────────────────────────────────────────────────
	/// `!true`/negation on a primitive is built in; otherwise `-`/`~` desugar to the
	/// interface method (`negate`/`bit_not`). Also decides, per U2 of the Slice 4C-a
	/// plan, the operator's [`Resolution`] (mirroring `infer_binary`'s D3 table),
	/// returned alongside the type so `infer`'s `PrefixOp` interception can record
	/// both once the node exists in the annotation table. The third element of the
	/// tuple is `Some(ty)` only for `Negate`/`BitNot`'s still-unresolved-inference-
	/// variable case: the caller enqueues `(node, span, ty, PrefixOp(op))` for
	/// `finalize_pending_operators` to retry once the current body has been checked.
	/// `BoolNot` never defers — a primitive-or-`Infer` operand eagerly unifies with
	/// `boolean` (unchanged from before this slice), so it never reaches a state
	/// where finalization could still help.
	fn infer_prefix(
		&mut self,
		op: nymph_ast::ops::PrefixOperator,
		value: &Expr,
		span: Span,
	) -> (Ty, Option<Resolution>, Option<Ty>) {
		use nymph_ast::ops::PrefixOperator::*;
		let operand = self.infer(value);
		if matches!(self.interner.kind(operand), TyKind::Never) {
			return (self.interner.never(), None, None);
		}
		// See the matching comment in `infer_binary`: an operator's operand type
		// is used mut-transparently throughout this function.
		let operand = self.strip_mut(operand);
		match op {
			BoolNot => {
				// `!` defaults to `boolean`: a primitive or a still-unresolved operand
				// (e.g. a method whose omitted return type isn't inferred yet) is taken as
				// `boolean`; only a concrete ADT routes to a `Not` overload.
				let resolved = self.shallow_resolve(operand);
				if self.prim_kind(resolved).is_some()
					|| matches!(self.interner.kind(resolved), TyKind::Infer(_))
				{
					let boolean = self.interner.boolean();
					self.unify(operand, boolean, span);
					(
						boolean,
						Some(Resolution {
							method: "not".into(),
							dispatch: DispatchKind::BuiltinEager,
							target: None,
							implementation: None,
							resolved_target: None,
						}),
						None,
					)
				} else {
					let (ty, dispatch, target, implementation, resolved_target) =
						self.dispatch_operator(operand, "not", &[], span);
					(
						ty,
						Some(Resolution {
							method: "not".into(),
							dispatch,
							target,
							implementation,
							resolved_target,
						}),
						None,
					)
				}
			}
			Negate | BitNot => {
				let method = prefix_method(op);
				if self.prim_kind(operand).is_some() {
					(
						operand,
						Some(Resolution {
							method: method.into(),
							dispatch: DispatchKind::BuiltinEager,
							target: None,
							implementation: None,
							resolved_target: None,
						}),
						None,
					)
				} else if self.is_adt(operand) || {
					let resolved = self.shallow_resolve(operand);
					matches!(self.interner.kind(resolved), TyKind::Param(_))
				} {
					// A resolved ADT or generic-parameter operand dispatches through the
					// solver immediately, exactly like `infer_binary`'s equivalent branch.
					let (ty, dispatch, target, implementation, resolved_target) =
						self.dispatch_operator(operand, method, &[], span);
					(
						ty,
						Some(Resolution {
							method: method.into(),
							dispatch,
							target,
							implementation,
							resolved_target,
						}),
						None,
					)
				} else {
					match self.resolve_fallback_prefix_operand(op, operand, span) {
						Some((ty, res)) => (ty, Some(res), None),
						// Still an unresolved inference variable (or `Error`, from an
						// already-diagnosed upstream mistake): defer to the end-of-body
						// finalization pass rather than guessing or panicking now.
						None => (operand, None, Some(operand)),
					}
				}
			}
		}
	}

	/// Mirrors [`Self::resolve_fallback_operand`] for a prefix (`Negate`/`BitNot`)
	/// operator: attempt to resolve once the operand's final type is known. Returns
	/// `None` only when the operand is still an unresolved inference variable (or
	/// `Error`) — the caller either defers to `finalize_pending_operators` (during a
	/// body) or, there, reports [`TypeError::CannotInferOperandType`].
	fn resolve_fallback_prefix_operand(
		&mut self,
		op: nymph_ast::ops::PrefixOperator,
		ty: Ty,
		span: Span,
	) -> Option<(Ty, Resolution)> {
		let method = prefix_method(op);
		if self.prim_kind(ty).is_some() {
			return Some((
				ty,
				Resolution {
					method: method.into(),
					dispatch: DispatchKind::BuiltinEager,
					target: None,
					implementation: None,
					resolved_target: None,
				},
			));
		}
		let resolved_ty = self.shallow_resolve(ty);
		if matches!(
			self.interner.kind(resolved_ty),
			TyKind::Infer(_) | TyKind::Error
		) {
			return None;
		}
		let (result_ty, dispatch, target, implementation, resolved_target) =
			self.dispatch_operator(ty, method, &[], span);
		Some((
			result_ty,
			Resolution {
				method: method.into(),
				dispatch,
				target,
				implementation,
				resolved_target,
			},
		))
	}

	/// Operators desugar to interface method calls. Primitives keep built-in
	/// fast-paths (so basic arithmetic needs no impls in scope); everything else —
	/// including mixed-primitive arithmetic like `int + float` — routes through the
	/// solver, where the method's return type *is* the operator's result type.
	///
	/// Also decides, per D3 of the Slice 4B plan, the operator's [`Resolution`] —
	/// how codegen must compile this exact node — returned alongside the type so
	/// `infer` can record both against the `BinaryOp` node once it exists in the
	/// annotation table. The third element of the tuple is `Some(ty)` only for the
	/// arithmetic fallback's still-unresolved-inference-variable case (Finding 2): the
	/// caller enqueues `(node, op, span, ty)` for `finalize_pending_operators` to
	/// retry once the current body has been checked, rather than giving up. A plain
	/// A plain `None` resolution (with no pending ty) marks `|>`, whose
	/// `Call`-shaped lowering (Slice 4I, D1) needs no `Resolution` at all — every
	/// other operator, including `??`/`in`/`!in` since Slice 4I, always records one
	/// (or reports a diagnostic and never reaches lowering).
	fn infer_binary(
		&mut self,
		lhs: &Expr,
		op: BinaryOperator,
		rhs: &Expr,
		span: Span,
	) -> (Ty, Option<Resolution>, Option<Ty>) {
		use BinaryOperator::*;

		// `|>` is application, not a method. D3: `lower_binop` already panics on
		// `Pipe` before any dispatch question arises, so no resolution is needed.
		//
		// `x |> f` lowers structurally to `f(x)` (DD1), so its LHS must be typed the
		// same way a direct call types its sole argument: `check`-ed against the
		// callee's known parameter type (letting an int literal widen to
		// `float`/`uint`, exactly like `infer_call`'s `TyKind::Fn` arm), not
		// `infer`-ed up front as a concrete type and then unified. Only fall back to
		// `infer` + `apply`'s unification-based typing when the callee isn't (yet)
		// known to be a plain function type — an unresolved inference variable, or
		// an already-diagnosed `Error` — where there's no parameter type to check
		// against anyway.
		if op == Pipe {
			let callee = self.infer(rhs);
			// A `mut`-bound closure pipes exactly like a plain one — see the
			// matching comment above `l`/`r`'s strip a few lines down.
			let resolved_callee = self.strip_mut(callee);
			return match self.interner.kind(resolved_callee).clone() {
				TyKind::Fn { params, ret } if params.len() == 1 => {
					self.check(lhs, params[0]);
					(ret, None, None)
				}
				_ => {
					let arg = self.infer(lhs);
					(self.apply(callee, vec![arg], span), None, None)
				}
			};
		}

		let l = self.infer(lhs);
		let r = self.infer(rhs);
		if matches!(self.interner.kind(l), TyKind::Never)
			|| (!matches!(op, BoolAnd | BoolOr) && matches!(self.interner.kind(r), TyKind::Never))
		{
			return (self.interner.never(), None, None);
		}
		// Operators never produce (or require) a `mut` operand — arithmetic on a
		// `mut int` local reads through exactly like on a plain `int` (peeling
		// mirrors `prim_kind`/`is_adt` above; stripping it here too, once, keeps
		// every later `self.unify(l, r, ..)`/`dispatch_operator(l, ..)` call and
		// the returned result type consistent, instead of comparing a peeled
		// discriminant against still-`mut`-wrapped operand handles).
		let l = self.strip_mut(l);
		let r = self.strip_mut(r);
		let boolean = self.interner.boolean();
		let eager = |method: &str| {
			Some(Resolution {
				method: method.into(),
				dispatch: DispatchKind::BuiltinEager,
				target: None,
				implementation: None,
				resolved_target: None,
			})
		};

		match op {
			Power => {
				let (ty, dispatch, target, implementation, resolved_target) =
					self.dispatch_operator(l, binary_method(op), &[r], span);
				(
					ty,
					Some(Resolution {
						method: binary_method(op).into(),
						dispatch,
						target,
						implementation,
						resolved_target,
					}),
					None,
				)
			}
			Plus | Minus | Times | Divide | Remainder | BitAnd | BitOr | BitXor | LeftShift
			| RightShift => match (self.prim_kind(l), self.prim_kind(r)) {
				// Same primitive → built-in. Division of integral operands produces a
				// float; every other result keeps the operand type. Boolean is the other
				// exception,
				// which has no native JS arithmetic/bitwise semantics to reuse: JS
				// coerces booleans to numbers (`true & false` → 0, not `false`). A
				// boolean's only real binary operators are the stdlib's
				// BitAnd/BitOr/BitXor (`&`/`|`/`^`), so route every boolean binary op
				// through the solver — it resolves those to the (materializable)
				// prelude impls and reports the rest (`true + false`, `true << false`)
				// as `NotImplemented`, never emitting silently-wrong native JS.
				(Some(a), Some(b)) if a == b => {
					self.unify(l, r, span);
					if matches!(a, TyKind::Boolean) {
						let (ty, dispatch, target, implementation, resolved_target) =
							self.dispatch_operator(l, binary_method(op), &[r], span);
						(
							ty,
							Some(Resolution {
								method: binary_method(op).into(),
								dispatch,
								target,
								implementation,
								resolved_target,
							}),
							None,
						)
					} else {
						let result = if op == Divide && matches!(a, TyKind::Int | TyKind::UInt) {
							self.interner.float()
						} else {
							l
						};
						(result, eager(binary_method(op)), None)
					}
				}
				// Different concrete primitives: an `int` literal against a `float`/`uint`
				// widens (so `1.5 * 2` is a `float` with no impl needed); otherwise this is
				// a genuine mixed-type operator that must be overloaded (e.g. `x + y` with
				// `x: float`, `y: int`). Both sub-cases still compile to a native JS
				// operator (D3: literal widening never dispatches; the dispatched case is
				// "impl self-type is a primitive" — stdlib isn't linked until Slice 5, so
				// its JS numeric semantics already match).
				(Some(_), Some(_)) => {
					if matches!(rhs.kind, ExprKind::Int(_)) && self.int_literal_coerces_to(l) {
						(l, eager(binary_method(op)), None)
					} else if matches!(lhs.kind, ExprKind::Int(_)) && self.int_literal_coerces_to(r) {
						(r, eager(binary_method(op)), None)
					} else {
						let (ty, _, _, _, _) = self.dispatch_operator(l, binary_method(op), &[r], span);
						(ty, eager(binary_method(op)), None)
					}
				}
				// A non-primitive operand: an ADT or generic-parameter receiver dispatches
				// through the solver (Finding 2 routes `Param` receivers here too, rather
				// than silently accepting them below — a bounded parameter resolves through
				// its bound, an unbounded one gets a proper `NotImplemented` diagnostic from
				// `dispatch_operator`, never a lowering-time ICE on a type-checked program).
				// An inference variable falls to the `unify`-then-recheck fallback, which
				// covers `xs[0] + 1` (resolved by this very unify) and `xs[0] + xs[0]`
				// (resolved only later, via a `check`-mode subtype after this node is
				// recorded) by deferring to `finalize_pending_operators`.
				_ if self.is_adt(l) || {
					let resolved = self.shallow_resolve(l);
					matches!(self.interner.kind(resolved), TyKind::Param(_))
				} =>
				{
					let (ty, dispatch, target, implementation, resolved_target) =
						self.dispatch_operator(l, binary_method(op), &[r], span);
					(
						ty,
						Some(Resolution {
							method: binary_method(op).into(),
							dispatch,
							target,
							implementation,
							resolved_target,
						}),
						None,
					)
				}
				_ => {
					self.unify(l, r, span);
					match self.resolve_fallback_operand(op, l, span) {
						Some((ty, res)) => (ty, Some(res), None),
						// Still an unresolved inference variable (or `Error`, from an
						// already-diagnosed upstream mistake): defer to the end-of-module
						// finalization pass rather than guessing or panicking now.
						None => {
							let result = if op == Divide { self.fresh() } else { l };
							(result, None, Some(l))
						}
					}
				}
			},
			Equals | NotEquals => {
				let method = if op == Equals { "equals" } else { "not_equals" };
				match (self.prim_kind(l), self.prim_kind(r)) {
					// Same primitive: unify the two sides (letting an int literal widen),
					// native JS `===`/`!==`.
					(Some(a), Some(b)) if a == b => {
						self.unify_operands(lhs, l, rhs, r, span);
						(boolean, eager(method), None)
					}
					// Two different primitives (e.g. int/uint): an `int` literal operand
					// widens; otherwise a cross-type `Equals<Other = …>` impl must
					// authorize the comparison (erroring on a genuine mismatch such as
					// `boolVal == intVal`). Two primitives always compare via a native JS
					// `===`/`!==`, so the impl only authorizes — it is not dispatched to.
					(Some(_), Some(_)) => {
						let literal_widens = (matches!(lhs.kind, ExprKind::Int(_))
							&& self.int_literal_coerces_to(r))
							|| (matches!(rhs.kind, ExprKind::Int(_)) && self.int_literal_coerces_to(l));
						if !literal_widens {
							self.dispatch_operator(l, method, &[r], span);
						}
						(boolean, eager(method), None)
					}
					_ => {
						let resolved = self.shallow_resolve(l);
						if matches!(self.interner.kind(resolved), TyKind::Infer(_)) {
							self.unify_operands(lhs, l, rhs, r, span);
							let resolved = self.shallow_resolve(l);
							if matches!(self.interner.kind(resolved), TyKind::Infer(_)) {
								return (boolean, None, Some(l));
							}
							if self.prim_kind(l).is_some() {
								return (boolean, eager(method), None);
							}
						}
						if matches!(self.interner.kind(resolved), TyKind::Error) {
							return (boolean, eager(method), None);
						}
						let (_, dispatch, target, implementation, resolved_target) =
							self.dispatch_operator(l, method, &[r], span);
						(
							boolean,
							Some(Resolution {
								method: method.into(),
								dispatch,
								target,
								implementation,
								resolved_target,
							}),
							None,
						)
					}
				}
			}
			// W1 (Slice 4C-c): comparison operators now mirror the arithmetic arm's
			// dispatch table exactly, just with a result type fixed at `boolean`
			// throughout (rather than the operand/`Output` type arithmetic uses) — a
			// concrete primitive pair stays a native comparison; an ADT or
			// generic-parameter receiver dispatches through `dispatch_operator`
			// (bound → `UserImplDefaultMethod`, unbound → `NotImplemented`); a still-
			// unresolved inference variable defers to the per-body pending queue
			// rather than guessing `BuiltinEager` immediately, which is exactly the
			// silent-miscompile gap this slice closes (see the 4C-c investigation
			// brief's `late_pinned_adt_comparison` probe).
			LessThan | LessThanEquals | GreaterThan | GreaterThanEquals => {
				let method = comparison_method(op);
				match (self.prim_kind(l), self.prim_kind(r)) {
					(Some(a), Some(b)) if a == b => {
						self.unify(l, r, span);
						(boolean, eager(method), None)
					}
					// Unlike the arithmetic arm, a comparison's result is always
					// `boolean` regardless of which side (if either) is the widening
					// int literal, so both literal-widening cases collapse into one
					// condition here (the arithmetic arm keeps them separate because it
					// must pick *which* operand's type becomes the result).
					(Some(_), Some(_)) => {
						let literal_widens = (matches!(rhs.kind, ExprKind::Int(_))
							&& self.int_literal_coerces_to(l))
							|| (matches!(lhs.kind, ExprKind::Int(_)) && self.int_literal_coerces_to(r));
						if literal_widens {
							(boolean, eager(method), None)
						} else {
							// Validate the operator is implemented for this primitive pair
							// (e.g. the cross-type `Comparable<Other = uint> for int`), but a
							// comparison between two primitives always lowers to a native JS
							// operator — the impl authorizes the check, it is not routed
							// through its non-materialized `less_than` default body.
							self.dispatch_operator(l, method, &[r], span);
							(boolean, eager(method), None)
						}
					}
					_ if self.is_adt(l) || {
						let resolved = self.shallow_resolve(l);
						matches!(self.interner.kind(resolved), TyKind::Param(_))
					} =>
					{
						let (_, dispatch, target, implementation, resolved_target) =
							self.dispatch_operator(l, method, &[r], span);
						(
							boolean,
							Some(Resolution {
								method: method.into(),
								dispatch,
								target,
								implementation,
								resolved_target,
							}),
							None,
						)
					}
					_ => {
						self.unify_operands(lhs, l, rhs, r, span);
						match self.resolve_fallback_operand(op, l, span) {
							Some((_, res)) => (boolean, Some(res), None),
							None => (boolean, None, Some(l)),
						}
					}
				}
			}
			// `&&`/`||` are NOT overloadable — mirroring Rust's design, whether `b`
			// evaluates in `a && b` must never depend on operand types. Both operands
			// therefore always unify with `boolean` (never dispatch through an
			// interface, even for an ADT receiver) and the built-in always
			// short-circuits at codegen (`a ? b : false` / `a ? true : b`). A
			// non-boolean operand is diagnosed with a dedicated variant (rather than
			// plain `unify`'s generic `MismatchedTypes`) so the message can carry a
			// help hint explaining this is by design, not a missing overload.
			BoolAnd | BoolOr => {
				let method = if op == BoolAnd { "and" } else { "or" };
				self.check_logical_operand(l, span);
				self.check_logical_operand(r, span);
				(
					boolean,
					Some(Resolution {
						method: method.into(),
						dispatch: DispatchKind::BuiltinShortCircuit,
						target: None,
						implementation: None,
						resolved_target: None,
					}),
					None,
				)
			}
			In | NotIn => {
				// `a in c` ≡ `c.contains(a)` — receiver is the RHS (the collection), with
				// the LHS (the searched-for item) passed as the sole argument, so operand
				// order is swapped relative to every other binary operator (Slice 4I, D2).
				// Unlike the pre-4I code, this now dispatches unconditionally rather than
				// only when `is_adt(r)`: a primitive/string RHS with no `Contains` impl used
				// to type-check silently (zero diagnostics) and only panic later in
				// lowering; routing every concrete/`Param` receiver through
				// `dispatch_operator` gets it a proper `NotImplemented` diagnostic instead,
				// mirroring the arithmetic/comparison arms. There is no built-in/native `in`
				// row — JS's `in` is key-membership, wrong for lists — so an unresolvable
				// receiver must always diagnose, never silently resolve to `boolean`.
				// (A still-unresolved inference-variable RHS is not specially deferred via
				// `pending_operators` here, unlike the lhs-receiver-shaped arithmetic arms:
				// that queue is shaped for a lhs receiver, and reusing it for a rhs-receiver
				// operator is left for a future slice; such a RHS reaches
				// `dispatch_operator` directly and gets whatever diagnostic that produces.)
				let method = if op == In { "contains" } else { "not_contains" };
				let (_, dispatch, target, implementation, resolved_target) =
					self.dispatch_operator(r, method, &[l], span);
				(
					boolean,
					Some(Resolution {
						method: method.into(),
						dispatch,
						target,
						implementation,
						resolved_target,
					}),
					None,
				)
			}
			// `??` is overloadable via the `Unwrap` interface. Nymph has no optional
			// type today (no `T?` surface syntax, no `TyKind::Optional`, no builtin
			// `Option`/`Result`) — every `??` dispatch is an ordinary, eager user-method
			// call (`recv.unwrap(fallback)`), never a short-circuiting builtin default;
			// an unresolvable receiver is a `NotImplemented` diagnostic, exactly like any
			// other operator with no matching impl (`dispatch_operator`'s `None` arm).
			Unwrap => {
				let (ty, dispatch, target, implementation, resolved_target) =
					self.dispatch_operator(l, "unwrap", &[r], span);
				(
					ty,
					Some(Resolution {
						method: "unwrap".into(),
						dispatch,
						target,
						implementation,
						resolved_target,
					}),
					None,
				)
			}
			Pipe => unreachable!("handled above"),
		}
	}

	/// Attempt to resolve an arithmetic operator's `Resolution` once both operands
	/// are known to be the same type `ty` (the fallback arm unifies them first).
	/// Returns `None` only when `ty` is still an unresolved inference variable (or
	/// `Error`, from an already-diagnosed mistake) — the caller either defers to
	/// `finalize_pending_operators` (during a body) or, there, reports
	/// [`TypeError::CannotInferOperandType`] (after the whole module is checked and
	/// no more information is coming).
	///
	/// Every other resolved type — including an ADT/generic parameter *and* any
	/// other concrete shape with no operator support at all (e.g. a first-class
	/// function value) — dispatches through `dispatch_operator`, which reports a
	/// `NotImplemented` diagnostic when no impl provides the method. Finding 2: the
	/// old code only routed ADT/`Param` receivers there, so a resolved-but-
	/// unsupported type (a function value being the concrete case found) fell
	/// through with neither a `Resolution` nor a diagnostic, and still reached
	/// lowering's `None => panic!(..)` on an otherwise zero-diagnostic program.
	///
	/// W1 (Slice 4C-c) reuses this same fallback for a deferred *comparison or equality*
	/// operator (`PendingOperatorKind::BinaryOp` doesn't distinguish the two
	/// families) — `binary_method` would `unreachable!()` on a comparison
	/// or equality operator, and the arithmetic result type would clobber the
	/// node's `boolean` type (both families always produce `boolean`, never the operand type), so
	/// both the method-name lookup and the returned result type are op-class
	/// aware here.
	fn resolve_fallback_operand(
		&mut self,
		op: BinaryOperator,
		ty: Ty,
		span: Span,
	) -> Option<(Ty, Resolution)> {
		let is_comparison = is_comparison_op(op);
		let is_equality = matches!(op, BinaryOperator::Equals | BinaryOperator::NotEquals);
		let method = if is_comparison {
			comparison_method(op)
		} else if is_equality {
			if op == BinaryOperator::Equals {
				"equals"
			} else {
				"not_equals"
			}
		} else {
			binary_method(op)
		};
		if let Some(kind) = self.prim_kind(ty) {
			let result_ty = if is_comparison || is_equality {
				self.interner.boolean()
			} else if op == BinaryOperator::Divide && matches!(kind, TyKind::Int | TyKind::UInt) {
				self.interner.float()
			} else {
				ty
			};
			return Some((
				result_ty,
				Resolution {
					method: method.into(),
					dispatch: DispatchKind::BuiltinEager,
					target: None,
					implementation: None,
					resolved_target: None,
				},
			));
		}
		let resolved_ty = self.shallow_resolve(ty);
		if matches!(
			self.interner.kind(resolved_ty),
			TyKind::Infer(_) | TyKind::Error
		) {
			return None;
		}
		let (dispatch_ty, dispatch, target, implementation, resolved_target) =
			self.dispatch_operator(ty, method, &[ty], span);
		let result_ty = if is_comparison || is_equality {
			self.interner.boolean()
		} else {
			dispatch_ty
		};
		Some((
			result_ty,
			Resolution {
				method: method.into(),
				dispatch,
				target,
				implementation,
				resolved_target,
			},
		))
	}

	/// Whether an `int` literal is allowed to implicitly become `expected`. Integer
	/// literals widen to `float` or `uint` on demand (codegen emits the `1f`/`1u` form);
	/// every other expected type leaves them as `int`.
	fn int_literal_coerces_to(&mut self, expected: Ty) -> bool {
		let expected = self.shallow_resolve(expected);
		matches!(self.interner.kind(expected), TyKind::Float | TyKind::UInt)
	}

	/// Unify two operand types, but let an `int` *literal* operand widen to a `float`/
	/// `uint` sibling instead of clashing (so `someFloat > 0` and `someUint == 0` type).
	fn unify_operands(&mut self, lhs: &Expr, l: Ty, rhs: &Expr, r: Ty, span: Span) {
		if (matches!(lhs.kind, ExprKind::Int(_)) && self.int_literal_coerces_to(r))
			|| (matches!(rhs.kind, ExprKind::Int(_)) && self.int_literal_coerces_to(l))
		{
			return;
		}
		self.unify(l, r, span);
	}

	/// Check a `&&`/`||` operand against `boolean`. Unlike every other binary
	/// operator, `&&`/`||` are never overloadable (see the `BoolAnd | BoolOr` arm
	/// above): a still-unresolved inference variable or an already-diagnosed
	/// `Error` type unifies with `boolean` as usual (binding the var, or silently
	/// continuing), but a concrete non-boolean type — including any ADT — is
	/// reported with the dedicated [`TypeError::LogicalOperandNotBoolean`] instead
	/// of `unify`'s generic `MismatchedTypes`, so the diagnostic can carry a help
	/// hint stating that logical operators aren't overloadable, rather than
	/// reading like a missing-overload bug.
	fn check_logical_operand(&mut self, ty: Ty, span: Span) {
		let boolean = self.interner.boolean();
		let resolved = self.shallow_resolve(ty);
		match self.interner.kind(resolved) {
			TyKind::Never => {}
			TyKind::Boolean | TyKind::Infer(_) | TyKind::Error => self.unify(ty, boolean, span),
			_ => {
				let found = self.display(ty);
				self.emit(span, TypeError::LogicalOperandNotBoolean { found });
			}
		}
	}

	/// Resolve an operator's method call, reporting an error if no impl provides it.
	/// The paired [`DispatchKind`] tells the binary-operator caller (Slice 4B)
	/// whether the matched method is a real, directly-callable method (`UserImpl` —
	/// covers an inherent method, an impl-direct method, and (Slice 4C-b) an
	/// interface default method, since lowering now materializes un-overridden
	/// defaults onto the implementing class) or only reachable through a
	/// still-generic receiver codegen can't dispatch at compile time
	/// (`UserImplDefaultMethod`, from `MethodSource::GenericBound`).
	/// Unary callers (`infer_prefix`, Slice 4C-a) use the same `DispatchKind` the
	/// same way binary callers do — the two now share one lowering mechanism.
	fn dispatch_operator(
		&mut self,
		recv: Ty,
		method: &str,
		args: &[Ty],
		span: Span,
	) -> (
		Ty,
		DispatchKind,
		Option<crate::DefinitionId>,
		Option<crate::DefinitionId>,
		Option<crate::annotate::ResolvedMethodTarget>,
	) {
		// Operator operands are already typed; literal widening on them is handled on the
		// primitive fast-paths, so no argument is flagged as a coercible literal here.
		let lits = vec![false; args.len()];
		match self.resolve_method(recv, method, args, &lits, span) {
			Some(res) => {
				// `GenericBound` *is* reachable here: the arithmetic-operator arm above
				// routes a `Param`-typed receiver through `dispatch_operator` too (see the
				// `TyKind::Param` check alongside `is_adt` at its call sites), and a
				// default body checked with `this` bound to a rigid synthetic `Param`
				// (`check_interface_default_bodies`) hits this same path for a
				// Self-dependent arithmetic/bitwise operator. The concrete impl is only
				// known once the parameter is instantiated, which this
				// type-erased-at-lowering compiler does not track, so it stays a loud
				// lowering deferral rather than a silent miscompile —
				// `dispatch_kind_for_operator` maps it (and a prelude-origin impl) to
				// `UserImplDefaultMethod` too.
				let dispatch = dispatch_kind_for_operator(&res);
				(
					res.ty,
					dispatch,
					res.target,
					res.implementation,
					res.resolved_target,
				)
			}
			None => {
				let lhs = self.display(recv);
				// Phrase the failure in terms of the operator the user wrote — its symbol,
				// both operands, and the interface to implement — instead of the internal
				// desugared method name and only the receiver type. Falls back to the plain
				// method-not-implemented message for any non-operator method.
				match operator_symbol_and_interface(method) {
					Some((operator, interface)) => {
						let rhs = args.first().map(|&t| self.display(t));
						self.emit(
							span,
							TypeError::OperatorNotImplemented {
								operator: operator.into(),
								interface: interface.into(),
								lhs,
								rhs,
							},
						);
					}
					None => {
						self.emit(
							span,
							TypeError::NotImplemented {
								method: method.into(),
								ty: lhs,
							},
						);
					}
				}
				// An error path (diagnostics already emitted): lowering never runs when
				// `Checked::diags` has errors, so this `DispatchKind` is never consumed.
				(
					self.interner.error(),
					DispatchKind::UserImpl,
					None,
					None,
					None,
				)
			}
		}
	}

	/// Infer a `value as Target` cast (Slice 4K) and decide its `Resolution` —
	/// mirrors `infer_binary`'s two-purposes-at-once shape (Slice 4B): `infer`
	/// needs the node's type recorded before a resolution can be attached to it
	/// (see the `TypeOp` special case in `infer` above), so this splits the same
	/// way the operator-inferring methods do rather than living entirely inside
	/// `check_cast`.
	fn infer_cast(
		&mut self,
		lhs: &Expr,
		rhs: &Spanned<Type>,
		span: Span,
	) -> (Ty, Option<Resolution>) {
		let src = self.infer(lhs);
		let target = self.lower_type(rhs);
		if matches!(self.interner.kind(src), TyKind::Never) {
			return (self.interner.never(), None);
		}
		let target_r = self.strip_mut(target);
		if matches!(self.interner.kind(target_r), TyKind::Char)
			&& self.numeric_literal_value(lhs).is_some_and(|value| {
				let value = value.trunc();
				value < 0.0 || value > 0x10_FFFF as f64 || (0xD800 as f64..=0xDFFF as f64).contains(&value)
			}) {
			self.emit(span, TypeError::InvalidCharCastLiteral);
			return (target, None);
		}
		let resolution = self.check_cast(src, target, span);
		(target, resolution)
	}

	fn numeric_literal_value(&self, expr: &Expr) -> Option<f64> {
		match &expr.kind {
			ExprKind::Int(value) => Some(value.0 as f64),
			ExprKind::UInt(value) => Some(value.0 as f64),
			ExprKind::Float(value) => Some(value.0.into_inner()),
			ExprKind::PrefixOp {
				op: nymph_ast::ops::PrefixOperator::Negate,
				value,
			} => self.numeric_literal_value(value).map(|value| -value),
			_ => None,
		}
	}

	/// Check a `value as Target` cast, returning the `Resolution` lowering needs to
	/// compile it. An identity cast and conversions among the scalar numeric/`char`
	/// types are built in (`DispatchKind::BuiltinEager` — lowering picks the exact JS
	/// mapping itself from the recorded operand/target types); every other cast
	/// requires the source type to implement `Into<Other = Target>`
	/// (`DispatchKind::UserImpl`, dispatched to its `into` method). When no `Into`
	/// interface is even in scope, the cast used to be left completely unchecked
	/// (silently type-checking a program that would panic in lowering); it now
	/// reports [`TypeError::CastRequiresInto`] instead, distinct from
	/// [`TypeError::CannotCast`] (which fires when `Into` *is* in scope but no impl
	/// satisfies it).
	fn check_cast(&mut self, src: Ty, target: Ty, span: Span) -> Option<Resolution> {
		// `mut` is transparent to casting — `mut int as int` is the same identity
		// cast as `int as int`. Peel it off both sides (a common `let mut`/`mut`
		// param/field operand) before the built-in-path check, else the cast falls
		// through to a bogus "no `Into` impl" diagnostic.
		let src = self.strip_mut(src);
		let target_r = self.strip_mut(target);
		// Don't pile diagnostics onto a poisoned or still-unknown operand, and don't
		// record a resolution lowering could act on either — an `Error`/`Infer` type
		// already has (or will have) its own diagnostic; lowering never runs on a
		// program with any diagnostic at all.
		if self.is_error_or_infer(src) || self.is_error_or_infer(target_r) {
			return None;
		}
		// Identity and scalar numeric/char conversions need no `Into` impl.
		if src == target_r || (self.is_scalar_cast_ty(src) && self.is_scalar_cast_ty(target_r)) {
			return Some(Resolution {
				method: "as".into(),
				dispatch: DispatchKind::BuiltinEager,
				target: None,
				implementation: None,
				resolved_target: None,
			});
		}
		let Some(into) = self.defs.get("Into").filter(|&d| self.is_interface(d)) else {
			let s = self.display(src);
			let t = self.display(target);
			self.emit(span, TypeError::CastRequiresInto { from: s, to: t });
			return None;
		};
		let known: Vec<(EcoString, Ty)> = self
			.interfaces
			.get(&into)
			.and_then(|i| i.generics.first().cloned())
			.map(|name| (name, target))
			.into_iter()
			.collect();
		if self.holds(src, into, &known, 0) {
			// `into` is only the stdlib `Into`'s conventional method name — `into` is
			// looked up purely BY NAME (`self.defs.get("Into")` above), so a local
			// interface literally called `Into` whose sole method isn't named `into`
			// (e.g. `func convert(): Other`) is a legal shape `holds` alone can't
			// rule out (it only checks the interface's generic args, never method
			// names). Read the actual dispatched name back off the interface's own
			// declared methods instead of assuming "into" — the exact zero-arg
			// method the `Into` shape declares — rather than emitting a call to a
			// name that may not exist on the class at all (a silent-miscompile
			// bug this fixes; see `TypeError::IntoInterfaceMalformed`'s doc for the
			// ambiguous/malformed fallback). `Into` declares no default body
			// (`ops/mod.nym:91`), so any impl that satisfies `holds` necessarily
			// defines that method directly.
			//
			// The actual dispatch is decided by re-resolving `method` through the
			// solver (`resolve_method`, exactly `dispatch_kind_for_method_call`'s
			// method-call path) rather than assuming `UserImpl` outright: `holds`
			// only proves *some* impl provides `method`, not that the impl lives
			// in the user's own module — a cast whose only `Into` impl is one of
			// the stdlib prelude's own (e.g. `impl Into<string> for boolean`, a
			// prelude-origin `ImplDirect`) used to still get `UserImpl` here,
			// which lowering trusted unconditionally and compiled straight to
			// `operand.into()` — a silent `TypeError: operand.into is not a
			// function` under Node for a JS primitive with no such method,
			// confirmed by probe. Re-resolving gets the exact same
			// `UserImplDefaultMethod` deferral (or, once materializable, the
			// mangled-function dispatch) a plain method call on the same impl
			// would get.
			let zero_arg_methods: Vec<EcoString> = self
				.interfaces
				.get(&into)
				.map(|def| {
					def
						.methods
						.iter()
						.filter(|(_, m)| m.params.is_empty())
						.map(|(name, _)| name.clone())
						.collect()
				})
				.unwrap_or_default();
			match <[EcoString; 1]>::try_from(zero_arg_methods) {
				Ok([method]) => match self.resolve_method(src, &method, &[], &[], span) {
					Some(res) => Some(Resolution {
						method,
						dispatch: dispatch_kind_for_method_call(&res),
						target: res.target.clone(),
						implementation: res.implementation.clone(),
						resolved_target: res.resolved_target.clone(),
					}),
					// `holds` already proved an impl exists; `resolve_method` failing
					// here would mean the two solver entry points disagree — kept
					// total (falls to `CannotCast`) rather than `unreachable!()` so a
					// future divergence between them fails loudly via a wrong-but-safe
					// diagnostic instead of a panic mid-typecheck.
					None => {
						let s = self.display(src);
						let t = self.display(target);
						self.emit(span, TypeError::CannotCast { from: s, to: t });
						None
					}
				},
				Err(_) => {
					let s = self.display(src);
					let t = self.display(target);
					self.emit(span, TypeError::IntoInterfaceMalformed { from: s, to: t });
					None
				}
			}
		} else {
			let s = self.display(src);
			let t = self.display(target);
			self.emit(span, TypeError::CannotCast { from: s, to: t });
			None
		}
	}

	/// Whether a (shallow-resolved) type is `Error` or an unbound inference variable.
	fn is_error_or_infer(&self, ty: Ty) -> bool {
		matches!(self.interner.kind(ty), TyKind::Error | TyKind::Infer(_))
	}

	/// Whether a type is one of the scalar kinds that participate in built-in `as`
	/// conversions (the numeric types and `char`).
	fn is_scalar_cast_ty(&self, ty: Ty) -> bool {
		matches!(
			self.interner.kind(ty),
			TyKind::Int | TyKind::UInt | TyKind::Float | TyKind::Char
		)
	}

	/// The primitive kind of a (resolved) type, if it is one.
	fn prim_kind(&mut self, ty: Ty) -> Option<TyKind> {
		// A `mut` primitive is still that primitive for every dispatch purpose —
		// `mut` is a compile-time-only view, transparent here exactly like
		// `head_of` treats it for method/impl dispatch.
		let ty = self.strip_mut(ty);
		match self.interner.kind(ty) {
			k @ (TyKind::Int
			| TyKind::UInt
			| TyKind::Float
			| TyKind::Char
			| TyKind::String
			| TyKind::Boolean) => Some(k.clone()),
			_ => None,
		}
	}

	/// Whether a (resolved) type has a nominal head an impl could be keyed on.
	/// Peels `mut` first, same rationale as [`Self::prim_kind`].
	fn is_adt(&mut self, ty: Ty) -> bool {
		let ty = self.strip_mut(ty);
		matches!(
			self.interner.kind(ty),
			TyKind::Adt(..) | TyKind::List(_) | TyKind::Tuple(_) | TyKind::Map(..)
		)
	}

	/// Apply a callee type to argument types via unification (used by `|>`).
	fn apply(&mut self, callee: Ty, arg_tys: Vec<Ty>, span: Span) -> Ty {
		let callee = self.strip_mut(callee);
		if matches!(self.interner.kind(callee), TyKind::Error) {
			return self.interner.error();
		}
		let ret = self.fresh();
		let expected = self.interner.mk_fn(arg_tys, ret);
		self.unify(callee, expected, span);
		ret
	}

	/// Type an assignment `place = value` or a compound `place op= value`. A compound
	/// assignment reads as `place = place <op> value`, so its value type comes from the
	/// underlying binary operator; a plain `=` checks the value against the place type
	/// (letting an `int` literal widen, etc.).
	///
	/// Returns the assignment's (void) type, plus — for a compound assignment — the
	/// underlying operator's `Resolution`, mirroring `infer_binary`. The desugared
	/// `place op value` has no `BinaryOp` AST node of its own; the `AssignOp` node
	/// itself carries the id the resolution is recorded against, in `infer`'s
	/// `AssignOp` special case (Finding 1). The third element mirrors
	/// `infer_binary`'s pending-operand slot for Finding 2's late finalization,
	/// paired with the operator so `finalize_pending_operators` knows which method to
	/// retry.
	fn infer_assign(
		&mut self,
		lhs: &Expr,
		op: AssignOperator,
		rhs: &Expr,
		span: Span,
	) -> (Ty, Option<Resolution>, Option<(BinaryOperator, Ty, Ty)>) {
		// Resolve the assignable place, reporting non-places and immutable targets.
		let place_ty = match &lhs.kind {
			ExprKind::Identifier(name) => match self.lookup_local(&name.0).map(|b| (b.ty, b.mutable)) {
				Some((ty, mutable)) => {
					if !mutable {
						self.emit(
							lhs.span,
							TypeError::AssignToImmutable {
								name: name.0.clone(),
							},
						);
					}
					ty
				}
				None
					if self.allow_imported_assignment
						&& self
							.defs
							.get(&name.0)
							.is_some_and(|definition| self.mutable_imports.contains(&definition)) =>
				{
					let definition = self.defs.get(&name.0).expect("checked imported definition");
					self
						.annotations
						.record_definition_target(lhs.id, self.defs.stable(definition));
					self
						.sigs
						.lets
						.get(&definition)
						.map(|signature| signature.ty)
						.unwrap_or_else(|| self.fresh())
				}
				None => {
					self.emit(
						lhs.span,
						TypeError::CannotAssign {
							name: name.0.clone(),
						},
					);
					self.infer(rhs);
					return (self.interner.void(), None, None);
				}
			},
			// A field-slot target (`p.field`): gated on a `mut` receiver — the
			// headline mutable-types enforcement. `xs[i]` index targets are left
			// ungated in MT1 (a separate question the plan defers).
			ExprKind::MemberAccess { parent, member, .. } => {
				let parent_ty = self.infer(parent);
				let resolved = self.shallow_resolve(parent_ty);
				match self.interner.kind(resolved) {
					// A prior error/unresolved var: don't cascade a second diagnostic.
					TyKind::Mut(_) | TyKind::Error | TyKind::Infer(_) => {}
					_ => {
						let ty = self.display(resolved);
						self.emit(
							lhs.span,
							TypeError::AssignFieldThroughImmutable {
								field: member.0.clone(),
								ty,
							},
						);
					}
				}
				self.member_ty_of(parent_ty, &member.0, member.1, None)
			}
			ExprKind::IndexAccess { parent, .. } => {
				let place_ty = self.infer(lhs);
				let parent_ty = self
					.annotations
					.get(parent.id)
					.map(|info| self.strip_mut(info.ty));
				let stripped_place = self.strip_mut(place_ty);
				let place_is_error = matches!(self.interner.kind(stripped_place), TyKind::Error);
				if !place_is_error
					&& !parent_ty.is_some_and(|ty| {
						matches!(
							self.interner.kind(ty),
							TyKind::List(_) | TyKind::Tuple(_) | TyKind::Map(..) | TyKind::Error
						)
					}) {
					self.emit(
						lhs.span,
						TypeError::CannotAssign {
							name: "custom index access".into(),
						},
					);
				}
				place_ty
			}
			_ => self.infer(lhs),
		};

		// Fitting a value back into a place is about the STORED value's type, not
		// the place's own `mut`-ness (a characteristic of the binding/field slot,
		// already gated above for a field target) — strip it here so storing an
		// ordinary `T` into a `mut T` place (e.g. reassigning a `let mut` local)
		// doesn't spuriously demand the value itself be `mut`-typed too.
		let expected = self.strip_mut(place_ty);

		let mut resolution = None;
		let mut pending = None;
		match binary_of_assign(op) {
			// `place op= value` ≡ `place = place op value`: the operator's result type
			// must be assignable back into the place.
			Some(binop) => {
				let (result, res, pend) = self.infer_binary(lhs, binop, rhs, span);
				if !matches!(self.interner.kind(result), TyKind::Never) {
					self.unify(result, expected, span);
				}
				resolution = res;
				pending = pend.map(|ty| (binop, ty, result));
			}
			// Plain `=`.
			None => self.check(rhs, expected),
		}
		(self.interner.void(), resolution, pending)
	}

	// ── Blocks ───────────────────────────────────────────────────────────────
	fn infer_block(&mut self, body: &[Spanned<Statement>], expected: Option<Ty>) -> Ty {
		self.push_scope();
		let void = self.interner.void();
		let mut result = void;
		let last = body.len().saturating_sub(1);
		for (i, stmt) in body.iter().enumerate() {
			match &stmt.0 {
				Statement::Let { meta, value } => {
					self.check_let_statement(meta, value);
					result = void;
				}
				Statement::Expr(expr) => {
					if i == last {
						result = match expected {
							Some(exp) => {
								self.check(expr, exp);
								exp
							}
							None => self.infer(expr),
						};
					} else {
						self.infer(expr);
					}
				}
			}
		}
		self.pop_scope();
		result
	}

	fn check_let_statement(&mut self, meta: &nymph_ast::decl::LetDeclaration, value: &Expr) {
		let has_annot = meta.type_.is_some();
		let ty = match &meta.type_ {
			Some(annot) => {
				let declared = self.lower_type(annot);
				// Check the initializer against the declared type with any `mut` the
				// annotation itself carries peeled off first: initializing a
				// `mut T`-annotated binding only needs a plain `T`-compatible value —
				// `mut` here is a capability layer the BINDING gains, not a runtime
				// distinction the initializer must already carry (mirrors the
				// un-annotated `let mut` form below, which already accepts a plain
				// value and wraps it in `mut` after the fact). Without this peel,
				// `subtype` (one-way `mut T <: T`, never the reverse) would reject
				// every plain-typed initializer against an explicit `mut T`
				// annotation, e.g. `let mut c: mut Counter = Counter(n = 0)`.
				let expected = self.strip_mut(declared);
				self.check(value, expected);
				declared
			}
			None => self.infer(value),
		};
		// `let mut x = v` binds `x` at `mut <ty(v)>` (one of the two mutability
		// cancel points: dropping into a `let mut` always gains `mut`, whatever
		// `v`'s own mutability was — `mk_mut` is idempotent, so this never nests).
		// A plain `let x = v` WITHOUT an explicit annotation instead drops any
		// `mut` `v` had (the other cancel point): the binding is immutable, so its
		// type must be too. But a plain `let x: mut T = v` — an explicit `mut`
		// annotation is its own, separate authority (NN2), independent of the
		// `let mut` keyword (NN4) — must keep the `mut` the user wrote instead of
		// silently stripping it.
		let ty = if meta.is_mutable() {
			self.interner.mk_mut(ty)
		} else if has_annot {
			ty
		} else {
			self.strip_mut(ty)
		};
		self.bind_pattern(&meta.name, ty, meta.is_mutable());
	}

	// ── Iteration ────────────────────────────────────────────────────────────
	fn infer_iterable_element(&mut self, iterable: &Expr) -> Ty {
		let ty = self.infer(iterable);
		// `mut` is transparent to iteration: `for x in xs` over a `mut #[int]`
		// yields `int` elements, same as an immutable list. Peel it, else the
		// element type falls through to an unconstrained fresh var and the loop
		// body escapes type-checking.
		let stripped = self.strip_mut(ty);
		if let TyKind::List(elem) = self.interner.kind(stripped)
			&& self.runtime_roles.iterable.is_none()
		{
			let elem = *elem;
			self
				.annotations
				.record_iter_mode(iterable.id, IterMode::ViaIter);
			return elem;
		}
		self.resolve_iterable_source(iterable, ty, stripped)
	}

	/// Resolve a non-range `for`-loop source (RR1): prefer
	/// ITERATOR-DIRECT (the source itself implements `Iterator<Item>`) over
	/// ITERABLE-VIA-ITER (the source implements `Iterable<T>`, reached through
	/// `.iter()`) — a type implementing both uses its own `next()` directly
	/// rather than paying for an extra `.iter()` hop. Neither ⇒ `NotIterable`,
	/// replacing what used to be a silent `self.fresh()` accept (the loop
	/// pattern bound to an unconstrained inference variable that let the body
	/// typecheck against garbage, only to panic in lowering).
	///
	/// Item/`T` is read directly off the matched impl's substituted argument
	/// (`resolve_iface_arg`) rather than by typing `iter()`'s return: `Iterator<T>`
	/// in RETURN position lowers through `mint_synthetic_param` (lower.rs) to an
	/// anonymous `impl Trait` param whose interface generic args are discarded, so
	/// a two-hop `iter().next()` would come back unpinned.
	///
	/// Records which mode won (`IterMode`) on the iterable's own node id —
	/// `lower_for` has no solver access of its own and reads this back to know
	/// whether to emit `<src>.iter()` or `<src>` as the desugar's first `let`.
	fn resolve_iterable_source(&mut self, iterable: &Expr, ty: Ty, stripped: Ty) -> Ty {
		if self.is_error_or_infer(stripped) {
			return self.interner.error();
		}
		// Captured the same way `resolve_method`'s own `recv_is_mut` is (BEFORE
		// the `mut` peel above erases it): whether the source, as the caller
		// actually wrote it, is `mut`. Needed so an `Iterator`/`Iterable` impl
		// reachable only through the mutable view (`impl A for mut B` / `impl
		// mut A for B`, MT2 OO4/OO5 — the only way such an impl's `next`/`iter`
		// can mutate `this`, since a plain `func` binds `this: Self`, not `mut
		// Self`) is actually reachable, rather than permanently unmatched.
		let resolved = self.shallow_resolve(ty);
		let self_is_mut = matches!(self.interner.kind(resolved), TyKind::Mut(_));
		if let TyKind::Param(idx) = self.interner.kind(stripped) {
			let idx = *idx;
			// A generic parameter's iterability can't be found through the impl registry
			// (`head_of` maps `Param` to `None`), but it CAN be found through the
			// parameter's own interface bound (`T: Iterator<Item>`) — resolve `next`
			// against that bound and read the element type off its `Option<Item>` return,
			// exactly the way an ambient `Iterator` default method (`fold`/`to_list`/…)
			// iterates its own `this`. Records `IterMode::Direct` so lowering emits the
			// `.next()` protocol rather than the native-list index fast path.
			if let Some((iterator, next)) = self.runtime_roles.iterator.clone()
				&& let Some((ret, _)) = self.resolve_param_exact_method(idx, iterator, &next, iterable.span)
				&& let Some(item) = self.option_element(ret)
			{
				let Some(next_name) = self.runtime_role_member_name(iterator, &next) else {
					return self.interner.error();
				};
				self.gate_mutating(iterator, &next_name, self_is_mut, iterable.span);
				if let (Some(interface), Some(interface_member)) =
					(self.defs.stable(iterator).cloned(), Some(next.clone()))
				{
					self.annotations.record_iteration_next_resolution(
						iterable.id,
						Resolution {
							method: next_name,
							dispatch: DispatchKind::UserImpl,
							target: Some(interface_member.clone()),
							implementation: None,
							resolved_target: Some(crate::annotate::ResolvedMethodTarget::GenericBound {
								interface,
								interface_member,
							}),
						},
					);
				}
				self
					.annotations
					.record_iter_mode(iterable.id, IterMode::Direct);
				return item;
			}
			if let Some((iface, iter)) = self.runtime_roles.iterable.clone()
				&& let Some(item_name) = self
					.interfaces
					.get(&iface)
					.and_then(|i| i.generics.first().cloned())
				&& let Some(item) = self.resolve_param_iface_arg(idx, iface, &item_name)
				&& let Some((_ret, _)) = self.resolve_param_exact_method(idx, iface, &iter, iterable.span)
			{
				let Some(iter_name) = self.runtime_role_member_name(iface, &iter) else {
					return self.interner.error();
				};
				self.gate_mutating(iface, &iter_name, self_is_mut, iterable.span);
				if let (Some(interface), Some(interface_member)) =
					(self.defs.stable(iface).cloned(), Some(iter.clone()))
				{
					self.annotations.record_iter_resolution(
						iterable.id,
						Resolution {
							method: "iter".into(),
							dispatch: DispatchKind::UserImpl,
							target: Some(interface_member.clone()),
							implementation: None,
							resolved_target: Some(crate::annotate::ResolvedMethodTarget::GenericBound {
								interface,
								interface_member,
							}),
						},
					);
				}
				self
					.annotations
					.record_iter_mode(iterable.id, IterMode::ViaIter);
				return item;
			}
		}
		if let Some((iterator, next)) = self.runtime_roles.iterator.clone()
			&& let Some(item_name) = self
				.interfaces
				.get(&iterator)
				.and_then(|i| i.generics.first().cloned())
			&& let Some((item, implementation_index)) =
				self.resolve_iface_arg_with_implementation(stripped, self_is_mut, iterator, &item_name, 0)
		{
			// The desugar (`lower_for_protocol`) invokes `next()` on this exact
			// source directly (`IterMode::Direct`: `let $it = <src>`) — gate it
			// exactly like an explicit `<src>.next()` call would be via
			// `resolve_method`, or a non-`mut` receiver's fields get mutated
			// through the loop with no diagnostic at all (MutMethodNeedsMutReceiver
			// bypassed).
			let Some(next_name) = self.runtime_role_member_name(iterator, &next) else {
				return self.interner.error();
			};
			self.gate_mutating(iterator, &next_name, self_is_mut, iterable.span);
			let resolution = self.commit_method(
				implementation_index,
				stripped,
				&next_name,
				Some((&[], &[])),
				iterable.span,
			);
			self
				.annotations
				.record_iteration_next_resolution(iterable.id, method_resolution(next_name, &resolution));
			self
				.annotations
				.record_iter_mode(iterable.id, IterMode::Direct);
			return item;
		}
		if let Some((iface, iter)) = self.runtime_roles.iterable.clone()
			&& let Some(t_name) = self
				.interfaces
				.get(&iface)
				.and_then(|i| i.generics.first().cloned())
			&& let Some((elem, implementation_index)) =
				self.resolve_iface_arg_with_implementation(stripped, self_is_mut, iface, &t_name, 0)
		{
			// Same reasoning as the `Direct` gate above, but for the `.iter()` hop
			// the desugar calls on this source (`IterMode::ViaIter`): gate it
			// against `Iterable::iter`'s own declared mutability, not `Iterator::next`'s
			// (the iterator `iter()` returns is a distinct value, resolved and
			// gated separately were it ever user-callable — out of scope here).
			let Some(iter_name) = self.runtime_role_member_name(iface, &iter) else {
				return self.interner.error();
			};
			self.gate_mutating(iface, &iter_name, self_is_mut, iterable.span);
			let iter_resolution = self.commit_method(
				implementation_index,
				stripped,
				&iter_name,
				Some((&[], &[])),
				iterable.span,
			);
			let iterator_ty = self.strip_mut(iter_resolution.ty);
			if let Some((iterator, next)) = self.runtime_roles.iterator.clone()
				&& let Some(next_name) = self.runtime_role_member_name(iterator, &next)
				&& let Some(item_name) = self
					.interfaces
					.get(&iterator)
					.and_then(|interface| interface.generics.first().cloned())
				&& let Some((_, next_implementation_index)) =
					self.resolve_iface_arg_with_implementation(iterator_ty, true, iterator, &item_name, 0)
			{
				let next_resolution = self.commit_method(
					next_implementation_index,
					iterator_ty,
					&next_name,
					Some((&[], &[])),
					iterable.span,
				);
				self.annotations.record_iteration_next_resolution(
					iterable.id,
					method_resolution(next_name, &next_resolution),
				);
			}
			self
				.annotations
				.record_iter_resolution(iterable.id, method_resolution(iter_name, &iter_resolution));
			self
				.annotations
				.record_iter_mode(iterable.id, IterMode::ViaIter);
			return elem;
		}
		let s = self.display(ty);
		self.emit(iterable.span, TypeError::NotIterable { ty: s });
		self.interner.error()
	}

	/// If `ty` is `Option<T>` (the return of `Iterator::next`), return `T`.
	fn option_element(&mut self, ty: Ty) -> Option<Ty> {
		let ty = self.shallow_resolve(ty);
		let (def, first) = match self.interner.kind(ty) {
			TyKind::Adt(def, args) => (*def, args.positional.first().copied()),
			_ => return None,
		};
		if self.runtime_roles.option == Some(def) {
			first
		} else {
			None
		}
	}

	fn runtime_role_member_name(
		&self,
		interface: DefId,
		member: &crate::DefinitionId,
	) -> Option<EcoString> {
		self
			.interfaces
			.get(&interface)?
			.methods
			.iter()
			.find_map(|(name, method)| (method.definition.as_ref() == Some(member)).then(|| name.clone()))
	}

	fn infer_range_element(&mut self, kind: &RangeKind) -> Ty {
		let elem = self.fresh();
		let bound = |checker: &mut Self, e: &Expr| checker.check(e, elem);
		match kind {
			RangeKind::From(a) | RangeKind::To(a) | RangeKind::ToInclusive(a) => {
				bound(self, a);
			}
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				bound(self, min);
				bound(self, max);
			}
		}
		elem
	}

	// ── Late operator finalization (Finding 2) ────────────────────────────────
	/// Retry every operator node `infer_binary`'s fallback arm deferred (its operand
	/// was still an unresolved inference variable at the moment it was recorded).
	/// Called at the end of each body's own checking (`check_func_body`,
	/// `check_let_body`, `check_method_body`, `check_interface_impl_members`), while
	/// that body's `param_bounds` and the unify table are still alive — an operand
	/// left unbound at record time may since have been pinned down by a
	/// `check`-mode subtype applied *later in the same body* (e.g. the function's
	/// declared return type). This must run per body, not once at module end:
	/// inference variables are body-local, so nothing outside the body can pin them
	/// down later, and `param_bounds` itself is cleared and rebuilt per body — a
	/// module-end pass would resolve every pending operator against whichever
	/// body's bounds happened to be checked last. Every zero-diagnostic program
	/// must leave this method with a `Resolution` recorded on every operator node
	/// it drains, or lowering's `None` panic is a real bug, not an expected gap.
	pub(crate) fn finalize_pending_operators(&mut self) {
		use crate::check::PendingOperatorKind;

		let pending = std::mem::take(&mut self.pending_operators);
		for (id, span, ty, pending_result, kind) in pending {
			let resolved = match kind {
				PendingOperatorKind::BinaryOp(op) | PendingOperatorKind::AssignOp(op) => {
					self.resolve_fallback_operand(op, ty, span)
				}
				PendingOperatorKind::PrefixOp(op) => self.resolve_fallback_prefix_operand(op, ty, span),
			};
			match resolved {
				Some((result_ty, resolution)) => {
					self.unify(result_ty, pending_result, span);
					match kind {
						// A `BinaryOp`/`PrefixOp` node's initially-recorded type was only the
						// (possibly still-unbound) operand placeholder; overwrite it with the
						// now-final result type, same as the immediately-resolved path would
						// have (a `PrefixOp`'s placeholder is the operand type itself, per
						// `infer_prefix`, exactly mirroring the arithmetic fallback's
						// operand-as-placeholder shape).
						PendingOperatorKind::BinaryOp(_) | PendingOperatorKind::PrefixOp(_) => {
							self.record(id, result_ty, Some(resolution))
						}
						// Finding 1: an `AssignOp` node's own type is always `Void` — set
						// immediately in `infer`'s `AssignOp` special case — and must stay
						// that way. `record` overwrites the whole `ExprInfo` (including
						// `ty`), so only `record_resolution` (which touches just the
						// `resolution` field) is safe here; it must never regress to
						// clobbering `Void` with the operator's operand/result type the way
						// the immediately-resolved compound-assign path never does.
						PendingOperatorKind::AssignOp(_) => self.annotations.record_resolution(id, resolution),
					}
				}
				None => {
					// Still unbound (a genuinely under-determined program) or `Error` (an
					// already-diagnosed upstream mistake, where piling on a second
					// diagnostic would be noise): only the former gets a fresh diagnostic.
					let resolved = self.shallow_resolve(ty);
					if matches!(self.interner.kind(resolved), TyKind::Infer(_)) {
						self.emit(span, TypeError::CannotInferOperandType);
					}
				}
			}
		}
	}

	/// Drain this body's `pending_bounds` obligations (Slice 4G), mirroring
	/// `finalize_pending_operators` exactly: called from every per-body driver
	/// while that body's `param_bounds`/`synthetic_bounds` and the unify table
	/// are still live, and truncated (not drained) by `infer_inherent_return`'s
	/// discarded trial run, for the same reasons documented on `pending_operators`.
	///
	/// For each obligation, shallow-resolve the minted variable:
	/// - Still `Infer` (never pinned to a concrete argument — reachable from a
	///   handful of zero-diagnostic programs, e.g. an unapplied function value,
	///   or a generic mentioned in no parameter): skip silently, matching
	///   `finalize_pending_operators`'s treatment of a genuinely
	///   under-determined var (there is no concrete type yet to check, and no
	///   runtime value of it ever exists in these shapes either).
	/// - `Error`: skip (an upstream mistake already diagnosed elsewhere).
	/// - A rigid `Param` (the classic generic-to-generic forwarding case, e.g.
	///   `outer<T: Area>(x: T) = measure(x)`): satisfied if the *caller's own*
	///   `param_bounds`/`synthetic_bounds` already record this interface for
	///   that `Param` — those maps are live because this is still that caller's
	///   own body. `holds` cannot see them at all (a rigid `Param` has no
	///   `head_of`, so only the interface's blanket bucket is even considered),
	///   so it is consulted only as a fallback, to still accept an
	///   *unconstrained* blanket impl (e.g. stdlib's
	///   `impl<T> Comparable<Other = T> for T`).
	/// - Any other concrete type: satisfied iff `holds` finds a matching impl
	///   (with the bound's own, call-site-substituted arguments, giving full
	///   fidelity for an argful declared bound like `Comparable<Other = T>`).
	pub(crate) fn finalize_pending_bounds(&mut self) {
		let pending = std::mem::take(&mut self.pending_bounds);
		let arg_mut = std::mem::take(&mut self.pending_bound_arg_mut);
		for obligation in pending {
			let PendingBound {
				site: span,
				obligation,
			} = obligation;
			let InstantiatedObligation {
				ty,
				interface,
				args,
			} = obligation;
			let resolved = self.shallow_resolve(ty);
			let disposition = match self.interner.kind(resolved).clone() {
				TyKind::Infer(_) => BoundFinalizationDisposition::Underdetermined,
				TyKind::Error => BoundFinalizationDisposition::Poisoned,
				kind => BoundFinalizationDisposition::Check(kind),
			};
			let satisfied = match disposition {
				BoundFinalizationDisposition::Underdetermined | BoundFinalizationDisposition::Poisoned => {
					true
				}
				BoundFinalizationDisposition::Check(TyKind::Param(p)) => {
					let bounded = self
						.param_bounds
						.get(&p)
						.is_some_and(|is| is.contains(&interface))
						|| self
							.synthetic_bounds
							.get(&p)
							.is_some_and(|is| is.contains(&interface));
					bounded || self.holds(resolved, interface, &args, 0)
				}
				// MT2 OO4: `resolved` here has already had any `mut` cancelled by
				// `subtype`'s one-way `mut T <: T` (`check_call_arg` is what binds
				// this obligation's variable) — `pending_bound_arg_mut`, keyed by
				// the SAME un-resolved `ty` `fn_type_of` pushed this obligation
				// with, is the side channel that survived that cancellation.
				BoundFinalizationDisposition::Check(_) => match arg_mut.get(&ty).copied() {
					// One contributing argument was `mut`, another wasn't. This is
					// NOT automatically an error: if the bound is satisfied by the
					// PLAIN type (an ordinary `impl A for B`, no mut-only impl), both
					// a `mut` and a plain argument satisfy it — the mixed-ness is
					// harmless. Only when the plain type FAILS the bound (i.e. A is
					// implemented only for `mut B`) does this reject, and then the
					// general `!satisfied` block below emits `BoundSatisfiedOnlyByMut`
					// — the precise "B doesn't fit; A is implemented for `mut B`"
					// message — rather than a vaguer mixed-arguments one. So the
					// mixed case is just the ordinary plain-type check.
					Some((true, true)) => self.holds(resolved, interface, &args, 0),
					// Every contributing argument was `mut`: a `Mut(B)`-only impl
					// additionally matches (`holds_self`, mut-aware), on top of the
					// ordinary plain-type check every argument (mut or not) already
					// gets — a `mut` argument still satisfies an unrelated, non-mut
					// bound one-way, same as the `mut T <: T` subtype rule elsewhere.
					Some((true, false)) => {
						self.holds(resolved, interface, &args, 0)
							|| self.holds_self(resolved, true, interface, &args, 0)
					}
					_ => self.holds(resolved, interface, &args, 0),
				},
			};
			if !satisfied {
				// The plain type failed the bound — if its `mut` version WOULD
				// satisfy it (`impl A for mut ty` / `impl mut A for ty`), say so
				// directly rather than a bare "does not implement". `holds_self`
				// (not a `mk_mut`-wrapped `holds`) so a `Mut` impl self type is
				// peeled correctly rather than compared against a doubly-`Mut`
				// `self_ty` (see `holds_self`'s doc comment).
				if self.holds_self(resolved, true, interface, &args, 0) {
					let ty = self.display(resolved);
					let interface = self.defs.data(interface).name.clone();
					self.emit(span, TypeError::BoundSatisfiedOnlyByMut { ty, interface });
				} else {
					let ty = self.display(resolved);
					let interface = self.defs.data(interface).name.clone();
					self.emit(span, TypeError::BoundNotSatisfied { ty, interface });
				}
			}
		}
	}

	/// Infer a `receiver.method(args…)` call argument's shape for `resolve_method`.
	/// Closure bodies are deferred until the selected method supplies their exact
	/// contextual function type; collection literals retain the existing owned-`Mut`
	/// coercion described below.
	///
	/// Wraps a fresh `#{…}`/`#[…]` literal argument's inferred type in `Mut` so
	/// it can satisfy a `mut`-typed method parameter (Confirmed defect 2: unlike
	/// free-function calls — `check_call_arg` — and ctor/block/if/match positions
	/// — `check_dispatch`'s own hook — a method call has no parameter type to
	/// `check` the argument against up front; the candidate's params only become
	/// known *after* `resolve_method` has already committed to one). This is
	/// exactly [`Checker::try_coerce_owned_literal_to_mut`]'s own rationale
	/// (a collection literal is a uniquely-owned temporary with no other alias,
	/// so it may stand in for `mut T`), applied at the type level instead: wrapping
	/// unconditionally is safe because `unify_arg` (`coerce.rs`) already peels a
	/// `mut`-typed ARGUMENT one-way — `(_, Mut(a)) => unify_arg(param, a, …)` —
	/// whenever the matched parameter isn't itself `mut`, so this can never leak a
	/// spurious `Mut` into a non-`mut`/generic parameter's binding; it only ever
	/// unlocks the one case `unify_arg` couldn't reach before: a genuinely
	/// `mut`-typed parameter matched against this now-`Mut`-typed literal argument.
	/// A NAMED binding (`ExprKind::Identifier`, never wrapped here) is untouched,
	/// so the existing one-way `mut T <: T` invariant — a plain-typed named
	/// argument still can't satisfy a `mut` parameter — is unaffected.
	fn infer_method_call_arg(&mut self, expr: &Expr) -> Ty {
		let ty = if let ExprKind::Closure {
			params,
			generics,
			return_type,
			..
		} = &expr.kind
		{
			self.push_params(build_param_scope(generics));
			let params = params
				.iter()
				.map(|parameter| match &parameter.0.type_ {
					Some(annotation) => self.lower_type(annotation),
					None => self.fresh(),
				})
				.collect();
			let return_type = match return_type {
				Some(annotation) => self.lower_type(annotation),
				None => self.fresh(),
			};
			self.pop_params();
			self.interner.mk_fn(params, return_type)
		} else {
			self.infer(expr)
		};
		if matches!(expr.kind, ExprKind::Map(_) | ExprKind::List(_)) {
			self.interner.mk_mut(ty)
		} else {
			ty
		}
	}

	/// Check a free-function call argument against its (possibly still a fresh
	/// generic-parameter variable) parameter type, additionally recording the
	/// argument's ACTUAL, pre-cancellation mutability against that parameter
	/// type (MT2 OO4) — mirrors [`Self::check`] exactly, branch for branch, so
	/// behavior is unchanged; the one addition is the record in the generic
	/// fallback arm, made before `subtype` (via the fallback's call) cancels a
	/// `mut` argument's tag. See [`Checker::pending_bound_arg_mut`]'s doc
	/// comment for why this can't instead hook `subtype` itself (it also
	/// checks return values, `let` bindings, etc. — this call site is the one
	/// `fn_type_of`'s `pending_bounds` obligations are actually about).
	fn check_call_arg(&mut self, expr: &Expr, pty: Ty) {
		self.resolve_anon(expr, Some(pty));
		match &expr.kind {
			ExprKind::Closure { .. }
			| ExprKind::Block { .. }
			| ExprKind::If { .. }
			| ExprKind::Match { .. }
			| ExprKind::Grouped(_) => self.check(expr, pty),
			// An int literal implicitly widening to a `float`/`uint` PARAMETER must
			// record the coerced type on its node, exactly like `check`'s own arm
			// (see line ~256) — uniform value boxing (slice #2) reads this back in
			// lowering to box the literal as `NFloat`/`NUint` rather than `NInt`.
			// `check_call_arg` is the sole path for free-function call args, so
			// without this record a coerced literal arg (`g(5)` where `g(x: float)`)
			// falls back to the syntactic `int` kind and misboxes as `NInt`.
			ExprKind::Int(_) if self.int_literal_coerces_to(pty) => {
				let coerced = self.shallow_resolve(pty);
				self.record(expr.id, coerced, None);
			}
			// Owned-literal → `mut` coercion, mirroring `check_dispatch`'s own hook:
			// a `#{…}`/`#[…]` literal argument may satisfy a `mut` parameter
			// directly. On guard failure (`pty`'s own top level isn't `mut`), routes
			// through `self.check(expr, pty)` — NOT the `_` arm's blind
			// `self.infer(expr)` — so `check_dispatch`'s `List`/`Map` arms still
			// propagate a concrete (possibly nested-`mut`) element/value type down
			// into this literal's own items/entries, letting a NESTED literal reach
			// `try_coerce_owned_literal_to_mut` in turn (Confirmed defect 1: a
			// blind `infer()` here left nested elements with no expected type at
			// all, so a `mut`-expected nested item could never win the coercion).
			// `check_call_arg`'s extra `pending_bound_arg_mut` tracking (below, in
			// the `_` arm) is for a NAMED argument's own recorded mutability
			// feeding a later generic-bound check; a `Map`/`List` literal is never
			// itself typed `mut` by `infer`, so skipping that tracking here changes
			// nothing observable — `is_mut` in the `_` arm would always have read
			// `false` for these two expression kinds anyway.
			ExprKind::Map(_) | ExprKind::List(_) => self.check(expr, pty),
			_ => {
				let got = self.infer(expr);
				let got_resolved = self.shallow_resolve(got);
				let is_mut = matches!(self.interner.kind(got_resolved), TyKind::Mut(_));
				let entry = self
					.pending_bound_arg_mut
					.entry(pty)
					.or_insert((false, false));
				if is_mut {
					entry.0 = true;
				} else {
					entry.1 = true;
				}
				self.subtype(got, pty, expr.span);
			}
		}
	}
}

/// The interface method a binary arithmetic/bitwise operator desugars to.
/// For an operator's desugared interface-method name (`binary_method` /
/// `comparison_method` / `prefix_method` / `equals` / `contains` / `unwrap`), the
/// user-facing operator symbol and the interface that backs it. Lets the
/// operator-resolution-failure diagnostic speak in terms the user actually wrote
/// (`**` / `Power`) instead of the internal method (`power`). `None` for a
/// non-operator method, so the caller falls back to the plain `NotImplemented` message.
fn operator_symbol_and_interface(method: &str) -> Option<(&'static str, &'static str)> {
	Some(match method {
		"plus" => ("+", "Plus"),
		"minus" => ("-", "Minus"),
		"times" => ("*", "Times"),
		"divide" => ("/", "Divide"),
		"remainder" => ("%", "Remainder"),
		"power" => ("**", "Power"),
		"bit_and" => ("&", "BitAnd"),
		"bit_or" => ("|", "BitOr"),
		"bit_xor" => ("^", "BitXor"),
		"shl" => ("<<", "LeftShift"),
		"shr" => (">>", "RightShift"),
		"less_than" => ("<", "Comparable"),
		"less_than_eq" => ("<=", "Comparable"),
		"greater_than" => (">", "Comparable"),
		"greater_than_eq" => (">=", "Comparable"),
		"equals" => ("==", "Equals"),
		"not_equals" => ("!=", "Equals"),
		"contains" => ("in", "Contains"),
		"not_contains" => ("!in", "Contains"),
		"unwrap" => ("??", "Unwrap"),
		"negate" => ("-", "Negate"),
		"not" => ("!", "Not"),
		"bit_not" => ("~", "BitNot"),
		_ => return None,
	})
}

fn binary_method(op: BinaryOperator) -> &'static str {
	use BinaryOperator::*;
	match op {
		Plus => "plus",
		Minus => "minus",
		Times => "times",
		Divide => "divide",
		Remainder => "remainder",
		Power => "power",
		BitAnd => "bit_and",
		BitOr => "bit_or",
		BitXor => "bit_xor",
		LeftShift => "shl",
		RightShift => "shr",
		other => unreachable!("not an arithmetic operator: {other:?}"),
	}
}

/// Whether `op` is one of the four comparison operators (`<`, `<=`, `>`, `>=`) —
/// used by `resolve_fallback_operand` to pick `comparison_method` over
/// `binary_method` and to force a `boolean` result type for a deferred
/// comparison (Slice 4C-c, W1).
fn is_comparison_op(op: BinaryOperator) -> bool {
	use BinaryOperator::*;
	matches!(
		op,
		LessThan | LessThanEquals | GreaterThan | GreaterThanEquals
	)
}

/// The `Comparable` method a comparison operator desugars to.
fn comparison_method(op: BinaryOperator) -> &'static str {
	use BinaryOperator::*;
	match op {
		LessThan => "less_than",
		LessThanEquals => "less_than_eq",
		GreaterThan => "greater_than",
		GreaterThanEquals => "greater_than_eq",
		other => unreachable!("not a comparison operator: {other:?}"),
	}
}

/// The interface method a deferrable prefix operator (`Negate`/`BitNot`) desugars
/// to. `BoolNot` never defers (see `infer_prefix`), so it has no entry here.
fn prefix_method(op: PrefixOperator) -> &'static str {
	match op {
		PrefixOperator::Negate => "negate",
		PrefixOperator::BitNot => "bit_not",
		PrefixOperator::BoolNot => unreachable!("BoolNot never defers to the fallback path"),
	}
}

/// Build a `ParamIdx → arg` substitution from an ADT's positional generic
/// arguments, for reading a field type in terms of the receiver's arguments.
impl Checker<'_> {
	fn loop_result_type(&mut self, kind: Option<LoopBreakKind>) -> Ty {
		let Some(kind) = kind else {
			return self.interner.void();
		};
		let element = match kind {
			LoopBreakKind::None => return self.interner.void(),
			LoopBreakKind::Bare => self.interner.mk_tuple(vec![]),
			LoopBreakKind::Valued(ty) => ty,
		};
		let element = self.shallow_resolve(element);
		let unresolved = match self.interner.kind(element) {
			TyKind::Infer(var) => Some(*var),
			_ => None,
		};
		let element = if let Some(var) = unresolved {
			// A syntactically valued break whose value always transfers control
			// (for example `break (break@outer 1)`) never constrains its loop's
			// element variable. The loop still has the valued Option shape required
			// by the syntax scan; complete that otherwise-unsolved type as `never`.
			let never = self.interner.never();
			self.table.assign(var, never);
			never
		} else {
			element
		};
		let Some(option) = self.runtime_roles.option else {
			return self.interner.error();
		};
		self
			.interner
			.mk_adt(option, GenericArgs::new(vec![element], vec![]))
	}

	/// Finds breaks targeting the immediately enclosing loop. `Some(true)` means
	/// at least one valued break; nested loops and every callable body are boundaries.
	fn targeting_break_kind(
		&self,
		expr: &Expr,
		target_label: Option<&nymph_ast::Ident>,
	) -> Option<bool> {
		fn merge(a: Option<bool>, b: Option<bool>) -> Option<bool> {
			match (a, b) {
				(None, b) => b,
				(a, None) => a,
				(Some(a), Some(b)) => Some(a || b),
			}
		}
		fn walk(
			checker: &Checker<'_>,
			expr: &Expr,
			target: (Option<&nymph_ast::Ident>, bool),
		) -> Option<bool> {
			if checker.annotations.anon_boundary_arity(expr.id).is_some() {
				return None;
			}
			let many = |items: Vec<&Expr>| {
				items.into_iter().fold(None, |found, item| {
					merge(found, walk(checker, item, target))
				})
			};
			match &expr.kind {
				ExprKind::Break { value, label } => {
					let nested = value
						.as_deref()
						.and_then(|value| walk(checker, value, target));
					if match (label, target.0) {
						(None, _) => target.1,
						(Some(a), Some(b)) => a.0 == b.0,
						_ => false,
					} {
						merge(Some(value.is_some()), nested)
					} else {
						nested
					}
				}
				ExprKind::Closure { .. } => None,
				ExprKind::While {
					condition,
					body,
					label,
				} => merge(
					walk(checker, condition, target),
					if target.0.is_none()
						|| label
							.as_ref()
							.zip(target.0)
							.is_some_and(|(a, b)| a.0 == b.0)
					{
						None
					} else {
						walk(checker, body, (target.0, false))
					},
				),
				ExprKind::For {
					iterable,
					body,
					label,
					..
				} => merge(
					walk(checker, iterable, target),
					if target.0.is_none()
						|| label
							.as_ref()
							.zip(target.0)
							.is_some_and(|(a, b)| a.0 == b.0)
					{
						None
					} else {
						walk(checker, body, (target.0, false))
					},
				),
				ExprKind::String(parts) => many(
					parts
						.iter()
						.filter_map(|part| match &part.0 {
							StringPart::InterpolatedExpr(expr) => Some(expr),
							_ => None,
						})
						.collect(),
				),
				ExprKind::List(items) | ExprKind::Tuple(items) => many(
					items
						.iter()
						.map(|item| match &item.0 {
							ListItem::Expr(expr) | ListItem::Spread(expr) => expr,
						})
						.collect(),
				),
				ExprKind::Map(entries) => entries.iter().fold(None, |found, entry| {
					let nested = match &entry.0 {
						MapEntry::Entry(key, value) => {
							merge(walk(checker, key, target), walk(checker, value, target))
						}
						MapEntry::Spread(expr) => walk(checker, expr, target),
					};
					merge(found, nested)
				}),
				ExprKind::Range(range) => match range {
					RangeKind::From(expr) | RangeKind::To(expr) | RangeKind::ToInclusive(expr) => {
						walk(checker, expr, target)
					}
					RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
						merge(walk(checker, min, target), walk(checker, max, target))
					}
				},
				ExprKind::Call { func, args, .. } => merge(
					walk(checker, func, target),
					many(args.iter().map(|arg| &arg.0.value).collect()),
				),
				ExprKind::MemberAccess { parent, .. } => walk(checker, parent, target),
				ExprKind::IndexAccess { parent, index, .. } => {
					merge(walk(checker, parent, target), walk(checker, index, target))
				}
				ExprKind::PrefixOp { value, .. }
				| ExprKind::PostfixOp { value, .. }
				| ExprKind::Grouped(value)
				| ExprKind::TypeOp { lhs: value, .. }
				| ExprKind::PatternOp { lhs: value, .. } => walk(checker, value, target),
				ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
					merge(walk(checker, lhs, target), walk(checker, rhs, target))
				}
				ExprKind::Return { value, .. } => value
					.as_deref()
					.and_then(|value| walk(checker, value, target)),
				ExprKind::If {
					condition,
					then,
					otherwise,
				} => merge(
					walk(checker, condition, target),
					merge(
						walk(checker, then, target),
						otherwise
							.as_deref()
							.and_then(|value| walk(checker, value, target)),
					),
				),
				ExprKind::Match { value, arms } => {
					arms
						.iter()
						.fold(walk(checker, value, target), |found, arm| {
							merge(
								found,
								merge(
									arm
										.guard
										.as_ref()
										.and_then(|guard| walk(checker, guard, target)),
									walk(checker, &arm.body, target),
								),
							)
						})
				}
				ExprKind::Block { body, .. } => body.iter().fold(None, |found, statement| {
					let expr = match &statement.0 {
						Statement::Expr(expr) => expr,
						Statement::Let { value, .. } => value,
					};
					merge(found, walk(checker, expr, target))
				}),
				_ => None,
			}
		}
		walk(self, expr, (target_label, true))
	}
}

fn adt_subst(args: &GenericArgs) -> FxHashMap<ParamIdx, Ty> {
	args
		.positional
		.iter()
		.enumerate()
		.map(|(i, &ty)| (ParamIdx(i as u32), ty))
		.collect()
}

/// Which call arguments are bare integer literals — these may widen to a `float`/`uint`
/// parameter in argument position (mirroring the check-mode literal rule).
fn arg_int_lits(args: &[Spanned<CallArg>]) -> Vec<bool> {
	args
		.iter()
		.map(|a| matches!(a.0.value.kind, ExprKind::Int(_)))
		.collect()
}

/// The binary operator underlying a compound assignment (`+=` → `+`), or `None` for a
/// plain `=`.
fn binary_of_assign(op: AssignOperator) -> Option<BinaryOperator> {
	use AssignOperator::*;
	use BinaryOperator as B;
	Some(match op {
		Assign => return None,
		PlusAssign => B::Plus,
		MinusAssign => B::Minus,
		TimesAssign => B::Times,
		DivideAssign => B::Divide,
		RemainderAssign => B::Remainder,
		PowerAssign => B::Power,
		LeftShiftAssign => B::LeftShift,
		RightShiftAssign => B::RightShift,
		BitAndAssign => B::BitAnd,
		BitXorAssign => B::BitXor,
		BitOrAssign => B::BitOr,
		BoolAndAssign => B::BoolAnd,
		BoolOrAssign => B::BoolOr,
		// `~=` has no binary form; treat it as a plain re-assignment.
		BitNotAssign => return None,
	})
}
