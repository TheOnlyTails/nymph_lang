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
	Span, Spanned,
	decl::Declaration,
	expr::{CallArg, Expr, ExprKind, RangeKind, Statement, StringPart},
	ops::{AssignOperator, BinaryOperator},
};
use rustc_hash::FxHashMap;

use crate::check::Checker;
use crate::def::DefKind;
use crate::errors::TypeError;
use crate::ids::{DefId, ParamIdx};
use crate::lower::build_param_scope;
use crate::ty::{GenericArgs, Ty, TyKind};

impl<'m> Checker<'m> {
	// ── Body driver ──────────────────────────────────────────────────────────
	pub(crate) fn check_bodies(&mut self) {
		let defs: Vec<(DefId, DefKind)> = self
			.defs
			.defs
			.iter()
			.enumerate()
			.map(|(i, d)| (DefId(i as u32), d.kind))
			.collect();
		for (id, kind) in defs {
			match kind {
				DefKind::Func { member } => self.check_func_body(id, member),
				DefKind::Let { member } => self.check_let_body(id, member),
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
		self.record_param_bounds(&meta.generics, 0);
		self.push_params(build_param_scope(&meta.generics));
		self.push_scope();
		for (param, psig) in meta.params.iter().zip(&sig.params) {
			self.bind_pattern(&param.0.name, psig.ty, param.0.mutable);
		}
		let prev = self.ret_ty.replace(sig.ret);
		self.check(body, sig.ret);
		self.ret_ty = prev;
		self.pop_scope();
		self.pop_params();
	}

	fn check_let_body(&mut self, id: DefId, member: usize) {
		let module = self.module;
		let value = match &module.members[member] {
			Declaration::Let { value, .. } => value,
			_ => return, // external lets have no value
		};
		let ty = self.sigs.lets[&id];
		self.push_scope();
		self.check(value, ty);
		self.pop_scope();
	}

	// ── check mode ───────────────────────────────────────────────────────────
	pub(crate) fn check(&mut self, expr: &Expr, expected: Ty) {
		match &expr.kind {
			ExprKind::Closure { .. } => self.check_closure(expr, expected),
			ExprKind::Block { body, .. } => {
				let ty = self.infer_block(body, Some(expected));
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
			ExprKind::Int(_) if self.int_literal_coerces_to(expected) => {}
			_ => {
				let got = self.infer(expr);
				self.subtype(got, expected, expr.span);
			}
		}
	}

	// ── infer mode ───────────────────────────────────────────────────────────
	pub(crate) fn infer(&mut self, expr: &Expr) -> Ty {
		let ty = self.infer_kind(expr);
		// Record the node's resolved type for the lowering pass. Zonking happens
		// inside `record`. Returns the *raw* ty so callers can still unify against it.
		self.record(expr.id, ty, None);
		ty
	}

	fn infer_kind(&mut self, expr: &Expr) -> Ty {
		let span = expr.span;
		match &expr.kind {
			ExprKind::Int(_) => self.interner.int(),
			ExprKind::UInt(_) => self.interner.uint(),
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
			ExprKind::Identifier(name) => self.infer_identifier(&name.0, span),
			ExprKind::AnonymousParam(_) => {
				self.emit(span, TypeError::AnonymousParamUnsupported);
				self.fresh()
			}
			ExprKind::List(items) => {
				let elem = self.fresh();
				for item in items {
					use nymph_ast::expr::ListItem;
					match &item.0 {
						ListItem::Expr(e) => self.check(e, elem),
						ListItem::Spread(e) => {
							let list = self.interner.mk_list(elem);
							self.check(e, list);
						}
					}
				}
				self.interner.mk_list(elem)
			}
			ExprKind::Tuple(items) => {
				use nymph_ast::expr::ListItem;
				// Spreads in tuples are not statically sized in Milestone A; infer
				// non-spread items positionally.
				let mut tys = Vec::new();
				for item in items {
					match &item.0 {
						ListItem::Expr(e) => tys.push(self.infer(e)),
						ListItem::Spread(e) => {
							self.infer(e);
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
						MapEntry::Spread(e) => {
							let map = self.interner.mk_map(key, value);
							self.check(e, map);
						}
					}
				}
				self.interner.mk_map(key, value)
			}
			ExprKind::Range(kind) => {
				self.infer_range_element(kind);
				// A range value's own type (an iterable) is Milestone B; for now it
				// is an opaque hole, while `for` extracts the element directly.
				self.fresh()
			}
			ExprKind::Call { func, args, .. } => self.infer_call(func, args, span),
			ExprKind::MemberAccess { parent, member, .. } => {
				self.infer_member(parent, &member.0, member.1)
			}
			ExprKind::IndexAccess { parent, index, .. } => {
				// `a[i]` ≡ `a.index(i)` through the `Index` interface, but lists/maps/
				// tuples are fast-pathed as built-ins so indexing them type-checks with
				// no `Index` impl in scope.
				let recv = self.infer(parent);
				let key = self.infer(index);
				let recv_r = self.shallow_resolve(recv);
				match self.interner.kind(recv_r).clone() {
					TyKind::List(elem) => {
						let int = self.interner.int();
						self.unify(key, int, span); // list index is an int
						elem
					}
					TyKind::Tuple(elems) => {
						// A tuple index yields a fresh var (heterogeneous; precise typing
						// needs a const index and is deferred).
						let _ = elems;
						self.fresh()
					}
					TyKind::Map(k, v) => {
						self.unify(key, k, span);
						v
					}
					_ => {
						let key_lit = matches!(index.kind, ExprKind::Int(_));
						match self.resolve_method(recv, "index", &[key], &[key_lit], span) {
							Some(ret) => ret,
							None => self.fresh(),
						}
					}
				}
			}
			ExprKind::Closure { .. } => self.infer_closure(expr),
			ExprKind::PrefixOp { op, value } => self.infer_prefix(*op, value, span),
			ExprKind::PostfixOp { value, .. } => {
				// `?` error propagation — Milestone B; unwrap best-effort.
				self.infer(value);
				self.fresh()
			}
			ExprKind::BinaryOp { lhs, op, rhs } => {
				// The operands are recorded by their own `infer` calls inside
				// `infer_binary`. TODO(codegen-slice-4): populate the selected-impl
				// `Resolution` (dispatch kind) here, in the operator-lowering slice that
				// consumes it (built-in fast-path → BuiltinEager, dispatch → UserImpl).
				self.infer_binary(lhs, *op, rhs, span)
			}
			ExprKind::TypeOp { lhs, rhs, .. } => {
				let src = self.infer(lhs);
				let target = self.lower_type(rhs);
				self.check_cast(src, target, span);
				target
			}
			ExprKind::PatternOp { lhs, rhs, .. } => {
				let scrutinee = self.infer(lhs);
				self.push_scope();
				self.check_pattern(rhs, scrutinee);
				self.pop_scope();
				self.interner.boolean()
			}
			ExprKind::AssignOp { lhs, op, rhs } => self.infer_assign(lhs, *op, rhs, span),
			ExprKind::Return { value, .. } => {
				let ret = self.ret_ty;
				if let Some(v) = value {
					match ret {
						Some(rt) => self.check(v, rt),
						None => {
							self.infer(v);
						}
					}
				}
				self.interner.never()
			}
			ExprKind::Break { value, .. } => {
				if let Some(v) = value {
					self.infer(v);
				}
				self.interner.never()
			}
			ExprKind::Continue { .. } => self.interner.never(),
			ExprKind::While {
				condition, body, ..
			} => {
				let boolean = self.interner.boolean();
				self.check(condition, boolean);
				self.push_scope();
				self.infer(body);
				self.pop_scope();
				self.interner.void()
			}
			ExprKind::For {
				variable,
				iterable,
				body,
				..
			} => {
				let elem = self.infer_iterable_element(iterable);
				self.push_scope();
				self.check_pattern(variable, elem);
				self.infer(body);
				self.pop_scope();
				self.interner.void()
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
				for arm in arms {
					self.push_scope();
					self.check_pattern(&arm.pattern, scrutinee);
					if let Some(guard) = &arm.guard {
						let boolean = self.interner.boolean();
						self.check(guard, boolean);
					}
					self.check(&arm.body, result);
					self.pop_scope();
				}
				self.check_exhaustive(scrutinee, arms, span);
				result
			}
			ExprKind::Block { body, .. } => self.infer_block(body, None),
			ExprKind::Grouped(inner) => self.infer(inner),
		}
	}

	// ── Identifiers & definitions ────────────────────────────────────────────
	fn infer_identifier(&mut self, name: &str, span: Span) -> Ty {
		if let Some(binding) = self.lookup_local(name) {
			return binding.ty;
		}
		if let Some(def) = self.defs.get(name) {
			return self.type_of_def(def, span);
		}
		match self.defs.resolve_variant(name) {
			Some(Ok((enum_def, variant))) => return self.variant_value(enum_def, variant),
			Some(Err(())) => {
				self.emit(span, TypeError::AmbiguousVariant { name: name.into() });
				return self.interner.error();
			}
			None => {}
		}
		self.emit(span, TypeError::CannotFind { name: name.into() });
		self.interner.error()
	}

	fn type_of_def(&mut self, def: DefId, span: Span) -> Ty {
		match self.defs.data(def).kind {
			DefKind::Let { .. } => self
				.sigs
				.lets
				.get(&def)
				.copied()
				.unwrap_or_else(|| self.fresh()),
			DefKind::Func { .. } => self.fn_type_of(def),
			DefKind::Variant { enum_def, variant } => self.variant_value(enum_def, variant),
			DefKind::Struct { .. } => {
				self.emit(span, TypeError::StructTypeAsValue);
				self.interner.error()
			}
			DefKind::Enum { .. }
			| DefKind::TypeAlias { .. }
			| DefKind::Namespace { .. }
			| DefKind::Interface { .. } => {
				self.emit(span, TypeError::TypeAsValue);
				self.interner.error()
			}
		}
	}

	/// The instantiated function type of a top-level `func`, with fresh variables
	/// for its generic parameters.
	fn fn_type_of(&mut self, def: DefId) -> Ty {
		let sig = self.sigs.funcs[&def].clone();
		let subst = self.fresh_subst(sig.generics.len());
		let params = sig
			.params
			.iter()
			.map(|p| self.subst(p.ty, &subst, None))
			.collect();
		let ret = self.subst(sig.ret, &subst, None);
		self.interner.mk_fn(params, ret)
	}

	/// The value of an enum variant referenced by name: the enum type itself for a
	/// field-less variant, or a constructor function for one with fields.
	fn variant_value(&mut self, enum_def: DefId, variant: usize) -> Ty {
		let (adt, subst) = self.instantiate_enum(enum_def);
		let vsig = self.sigs.enums[&enum_def].variants[variant].clone();
		if vsig.fields.is_empty() {
			adt
		} else {
			let params = vsig
				.fields
				.iter()
				.map(|(_, t)| self.subst(*t, &subst, None))
				.collect();
			self.interner.mk_fn(params, adt)
		}
	}

	pub(crate) fn instantiate_enum(&mut self, enum_def: DefId) -> (Ty, FxHashMap<ParamIdx, Ty>) {
		let arity = self.sigs.enums[&enum_def].generics.len();
		let subst = self.fresh_subst(arity);
		let positional = (0..arity).map(|i| subst[&ParamIdx(i as u32)]).collect();
		let adt = self
			.interner
			.mk_adt(enum_def, GenericArgs::new(positional, Vec::new()));
		(adt, subst)
	}

	pub(crate) fn instantiate_struct(&mut self, struct_def: DefId) -> (Ty, FxHashMap<ParamIdx, Ty>) {
		let arity = self.sigs.structs[&struct_def].generics.len();
		let subst = self.fresh_subst(arity);
		let positional = (0..arity).map(|i| subst[&ParamIdx(i as u32)]).collect();
		let adt = self
			.interner
			.mk_adt(struct_def, GenericArgs::new(positional, Vec::new()));
		(adt, subst)
	}

	// ── Calls & construction ─────────────────────────────────────────────────
	fn infer_call(&mut self, func: &Expr, args: &[Spanned<CallArg>], span: Span) -> Ty {
		// Constructor calls: `Struct(field = …)` / `Variant(field = …)`.
		if let ExprKind::Identifier(name) = &func.kind {
			if let Some(def) = self.defs.get(&name.0)
				&& let DefKind::Struct { .. } = self.defs.data(def).kind
			{
				return self.infer_struct_ctor(def, args, span);
			}
			match self.defs.resolve_variant(&name.0) {
				Some(Ok((enum_def, variant))) => {
					return self.infer_variant_ctor(enum_def, variant, args, span);
				}
				Some(Err(())) => {
					self.emit(
						span,
						TypeError::AmbiguousVariant {
							name: name.0.clone(),
						},
					);
					return self.interner.error();
				}
				None => {}
			}
		}

		// `Type.variant(…)` construction and `Type.static(…)` calls: the parent is a
		// type name, not a value.
		if let ExprKind::MemberAccess { parent, member, .. } = &func.kind
			&& let ExprKind::Identifier(type_name) = &parent.kind
			&& let Some(def) = self.defs.get(&type_name.0)
		{
			match self.defs.data(def).kind {
				DefKind::Enum { .. } => {
					let variant = self.sigs.enums[&def]
						.variants
						.iter()
						.position(|v| v.name == member.0);
					if let Some(variant) = variant {
						return self.infer_variant_ctor(def, variant, args, member.1);
					}
					let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(&a.0.value)).collect();
					let arg_lits = arg_int_lits(args);
					if let Some(ret) = self.resolve_namespaced(def, &member.0, &arg_tys, &arg_lits, member.1)
					{
						return ret;
					}
					self.emit(
						member.1,
						TypeError::NoVariantOrNamespacedFn {
							ty: type_name.0.clone(),
							name: member.0.clone(),
						},
					);
					return self.interner.error();
				}
				DefKind::Struct { .. } => {
					let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(&a.0.value)).collect();
					let arg_lits = arg_int_lits(args);
					if let Some(ret) = self.resolve_namespaced(def, &member.0, &arg_tys, &arg_lits, member.1)
					{
						return ret;
					}
					self.emit(
						member.1,
						TypeError::NoNamespacedFn {
							ty: type_name.0.clone(),
							name: member.0.clone(),
						},
					);
					return self.interner.error();
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
			let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(&a.0.value)).collect();
			return self.resolve_param_namespaced(pidx, &member.0, &arg_tys, member.1);
		}

		// Method call: `receiver.method(args…)` resolves through the interface solver.
		if let ExprKind::MemberAccess { parent, member, .. } = &func.kind {
			let recv = self.infer(parent);
			let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(&a.0.value)).collect();
			let arg_lits = arg_int_lits(args);
			return match self.resolve_method(recv, &member.0, &arg_tys, &arg_lits, member.1) {
				Some(ret) => ret,
				None => {
					let rendered = self.display(recv);
					self.emit(
						member.1,
						TypeError::NoMethod {
							method: member.0.clone(),
							ty: rendered,
						},
					);
					self.interner.error()
				}
			};
		}

		let callee = self.infer(func);
		let callee = self.shallow_resolve(callee);
		match self.interner.kind(callee).clone() {
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
					self.check(&arg.0.value, *pty);
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
		}
	}

	fn infer_struct_ctor(&mut self, def: DefId, args: &[Spanned<CallArg>], _span: Span) -> Ty {
		let (adt, subst) = self.instantiate_struct(def);
		let sig = self.sigs.structs[&def].clone();
		let fields: Vec<(EcoString, Ty)> = sig
			.fields
			.iter()
			.map(|(n, t)| (n.clone(), self.subst(*t, &subst, None)))
			.collect();
		self.check_ctor_args(&fields, args);
		adt
	}

	fn infer_variant_ctor(
		&mut self,
		enum_def: DefId,
		variant: usize,
		args: &[Spanned<CallArg>],
		_span: Span,
	) -> Ty {
		let (adt, subst) = self.instantiate_enum(enum_def);
		let vsig = self.sigs.enums[&enum_def].variants[variant].clone();
		let fields: Vec<(EcoString, Ty)> = vsig
			.fields
			.iter()
			.map(|(n, t)| (n.clone(), self.subst(*t, &subst, None)))
			.collect();
		self.check_ctor_args(&fields, args);
		adt
	}

	/// Check constructor arguments against declared fields, by label when present
	/// else positionally.
	fn check_ctor_args(&mut self, fields: &[(EcoString, Ty)], args: &[Spanned<CallArg>]) {
		for (i, arg) in args.iter().enumerate() {
			let call = &arg.0;
			let target = if let Some(label) = &call.name {
				match fields.iter().find(|(n, _)| n == &label.0) {
					Some((_, ty)) => Some(*ty),
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
					Some((_, ty)) => Some(*ty),
					None => {
						self.emit(call.value.span, TypeError::TooManyFields);
						None
					}
				}
			};
			match target {
				Some(ty) => self.check(&call.value, ty),
				None => {
					self.infer(&call.value);
				}
			}
		}
	}

	// ── Member access ────────────────────────────────────────────────────────
	fn infer_member(&mut self, parent: &Expr, member: &str, span: Span) -> Ty {
		// `EnumName.Variant` — a variant referenced through its type.
		if let ExprKind::Identifier(tname) = &parent.kind
			&& let Some(def) = self.defs.get(&tname.0)
			&& let DefKind::Enum { .. } = self.defs.data(def).kind
		{
			let variants = &self.sigs.enums[&def].variants;
			if let Some(vidx) = variants.iter().position(|v| v.name == member) {
				return self.variant_value(def, vidx);
			}
			self.emit(
				span,
				TypeError::EnumHasNoVariant {
					enum_name: tname.0.clone(),
					variant: member.into(),
				},
			);
			return self.interner.error();
		}

		let parent_ty = self.infer(parent);
		let parent_ty = self.shallow_resolve(parent_ty);
		match self.interner.kind(parent_ty).clone() {
			TyKind::Adt(def, args) => {
				if matches!(self.defs.data(def).kind, DefKind::Struct { .. }) {
					let sig = self.sigs.structs[&def].clone();
					let subst = adt_subst(&args);
					if let Some((_, fty)) = sig.fields.iter().find(|(n, _)| n == member) {
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
		let ret = match return_type {
			Some(annot) => {
				let rt = self.lower_type(annot);
				self.check(body, rt);
				rt
			}
			None => self.infer(body),
		};
		self.pop_scope();
		self.pop_params();
		self.interner.mk_fn(param_tys, ret)
	}

	fn check_closure(&mut self, expr: &Expr, expected: Ty) {
		let expected = self.shallow_resolve(expected);
		let ExprKind::Closure {
			params,
			generics,
			return_type,
			body,
		} = &expr.kind
		else {
			unreachable!("guarded by caller");
		};

		// Pull expected parameter/return types out of an expected function type.
		let (exp_params, exp_ret) = match self.interner.kind(expected).clone() {
			TyKind::Fn { params, ret } => (Some(params), Some(ret)),
			_ => (None, None),
		};

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
		let ret = match (return_type, exp_ret) {
			(Some(annot), _) => {
				let rt = self.lower_type(annot);
				self.check(body, rt);
				rt
			}
			(None, Some(rt)) => {
				self.check(body, rt);
				rt
			}
			(None, None) => self.infer(body),
		};
		self.pop_scope();
		self.pop_params();
		let got = self.interner.mk_fn(param_tys, ret);
		self.subtype(got, expected, expr.span);
	}

	// ── Operators (built-in only in Milestone A) ─────────────────────────────
	fn infer_prefix(&mut self, op: nymph_ast::ops::PrefixOperator, value: &Expr, span: Span) -> Ty {
		use nymph_ast::ops::PrefixOperator::*;
		// `!true`/negation on a primitive is built in; otherwise desugar to the
		// interface method (`not`/`negate`/`bit_not`).
		let operand = self.infer(value);
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
					boolean
				} else {
					self.dispatch_operator(operand, "not", &[], span)
				}
			}
			Negate => {
				if self.prim_kind(operand).is_some() {
					operand
				} else {
					self.dispatch_operator(operand, "negate", &[], span)
				}
			}
			BitNot => {
				if self.prim_kind(operand).is_some() {
					operand
				} else {
					self.dispatch_operator(operand, "bit_not", &[], span)
				}
			}
		}
	}

	/// Operators desugar to interface method calls. Primitives keep built-in
	/// fast-paths (so basic arithmetic needs no impls in scope); everything else —
	/// including mixed-primitive arithmetic like `int + float` — routes through the
	/// solver, where the method's return type *is* the operator's result type.
	fn infer_binary(&mut self, lhs: &Expr, op: BinaryOperator, rhs: &Expr, span: Span) -> Ty {
		use BinaryOperator::*;

		// `|>` is application, not a method.
		if op == Pipe {
			let arg = self.infer(lhs);
			let callee = self.infer(rhs);
			return self.apply(callee, vec![arg], span);
		}

		let l = self.infer(lhs);
		let r = self.infer(rhs);
		let boolean = self.interner.boolean();

		match op {
			Plus | Minus | Times | Divide | Remainder | Power | BitAnd | BitOr | BitXor | LeftShift
			| RightShift => match (self.prim_kind(l), self.prim_kind(r)) {
				// Same primitive → built-in, result is that type.
				(Some(a), Some(b)) if a == b => {
					self.unify(l, r, span);
					l
				}
				// Different concrete primitives: an `int` literal against a `float`/`uint`
				// widens (so `1.5 * 2` is a `float` with no impl needed); otherwise this is
				// a genuine mixed-type operator that must be overloaded (e.g. `x + y` with
				// `x: float`, `y: int`).
				(Some(_), Some(_)) => {
					if matches!(rhs.kind, ExprKind::Int(_)) && self.int_literal_coerces_to(l) {
						l
					} else if matches!(lhs.kind, ExprKind::Int(_)) && self.int_literal_coerces_to(r) {
						r
					} else {
						self.dispatch_operator(l, binary_method(op), &[r], span)
					}
				}
				// A non-primitive operand: user type → overload; inference var/param →
				// best-effort unify (covers generic `T + T` and not-yet-known types).
				_ => {
					if self.is_adt(l) {
						self.dispatch_operator(l, binary_method(op), &[r], span)
					} else {
						self.unify(l, r, span);
						l
					}
				}
			},
			Equals | NotEquals => {
				let method = if op == Equals { "equals" } else { "not_equals" };
				if self.prim_kind(l).is_some() || !self.is_adt(l) {
					self.unify_operands(lhs, l, rhs, r, span);
				} else {
					self.dispatch_operator(l, method, &[r], span);
				}
				boolean
			}
			LessThan | LessThanEquals | GreaterThan | GreaterThanEquals => {
				if self.prim_kind(l).is_some() || !self.is_adt(l) {
					self.unify_operands(lhs, l, rhs, r, span);
				} else {
					self.dispatch_operator(l, comparison_method(op), &[r], span);
				}
				boolean
			}
			// `&&`/`||` are overloadable via the `And`/`Or` interfaces like any operator.
			// The built-in `boolean` *default* impl is fast-pathed here and short-circuits
			// at codegen (`a ? b : false`); a user overload on an ADT resolves through the
			// interface and lowers to an ordinary (eager) method call. Typing checks both
			// operands regardless of runtime laziness.
			BoolAnd | BoolOr => {
				if self.prim_kind(l).is_some() || !self.is_adt(l) {
					self.unify(l, boolean, span);
					self.unify(r, boolean, span);
					boolean
				} else {
					self.dispatch_operator(l, if op == BoolAnd { "and" } else { "or" }, &[r], span)
				}
			}
			In | NotIn => {
				// `a in c` ≡ `c.contains(a)` — receiver is the collection.
				let method = if op == In { "contains" } else { "not_contains" };
				if self.is_adt(r) {
					self.dispatch_operator(r, method, &[l], span);
				}
				boolean
			}
			// `??` is overloadable via the `Unwrap` interface. Its built-in *default*
			// impls (`Option`/`Result`) short-circuit — codegen lowers those to
			// `match a { Some(v) -> v, _ -> b }`, not evaluating `b` when `a` holds a
			// value — while a user `Unwrap` overload is an ordinary (eager) method call.
			// Typing: `b` and the result are `Output`.
			Unwrap => self.dispatch_operator(l, "unwrap", &[r], span),
			Pipe => unreachable!("handled above"),
		}
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

	/// Resolve an operator's method call, reporting an error if no impl provides it.
	fn dispatch_operator(&mut self, recv: Ty, method: &str, args: &[Ty], span: Span) -> Ty {
		// Operator operands are already typed; literal widening on them is handled on the
		// primitive fast-paths, so no argument is flagged as a coercible literal here.
		let lits = vec![false; args.len()];
		match self.resolve_method(recv, method, args, &lits, span) {
			Some(ret) => ret,
			None => {
				let rendered = self.display(recv);
				self.emit(
					span,
					TypeError::NotImplemented {
						method: method.into(),
						ty: rendered,
					},
				);
				self.interner.error()
			}
		}
	}

	/// Check a `value as Target` cast. An identity cast and conversions among the scalar
	/// numeric/`char` types are built in; every other cast requires the source type to
	/// implement `Into<Other = Target>`. When no `Into` interface is in scope (e.g. a test
	/// snippet without the prelude) the cast is left unchecked.
	fn check_cast(&mut self, src: Ty, target: Ty, span: Span) {
		let src = self.shallow_resolve(src);
		let target_r = self.shallow_resolve(target);
		// Don't pile diagnostics onto a poisoned or still-unknown operand.
		if self.is_error_or_infer(src) || self.is_error_or_infer(target_r) {
			return;
		}
		// Identity and scalar numeric/char conversions need no `Into` impl.
		if src == target_r || (self.is_scalar_cast_ty(src) && self.is_scalar_cast_ty(target_r)) {
			return;
		}
		let Some(into) = self.defs.get("Into").filter(|&d| self.is_interface(d)) else {
			return;
		};
		let known: Vec<(EcoString, Ty)> = self
			.interfaces
			.get(&into)
			.and_then(|i| i.generics.first().cloned())
			.map(|name| (name, target))
			.into_iter()
			.collect();
		if !self.holds(src, into, &known, 0) {
			let s = self.display(src);
			let t = self.display(target);
			self.emit(span, TypeError::CannotCast { from: s, to: t });
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
		let ty = self.shallow_resolve(ty);
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
	fn is_adt(&mut self, ty: Ty) -> bool {
		let ty = self.shallow_resolve(ty);
		matches!(
			self.interner.kind(ty),
			TyKind::Adt(..) | TyKind::List(_) | TyKind::Tuple(_) | TyKind::Map(..)
		)
	}

	/// Apply a callee type to argument types via unification (used by `|>`).
	fn apply(&mut self, callee: Ty, arg_tys: Vec<Ty>, span: Span) -> Ty {
		let callee = self.shallow_resolve(callee);
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
	fn infer_assign(&mut self, lhs: &Expr, op: AssignOperator, rhs: &Expr, span: Span) -> Ty {
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
				None => {
					self.emit(
						lhs.span,
						TypeError::CannotAssign {
							name: name.0.clone(),
						},
					);
					self.infer(rhs);
					return self.interner.void();
				}
			},
			// A field or index target (`this.field`, `xs[i]`): its type is the place type.
			_ => self.infer(lhs),
		};

		match binary_of_assign(op) {
			// `place op= value` ≡ `place = place op value`: the operator's result type
			// must be assignable back into the place.
			Some(binop) => {
				let result = self.infer_binary(lhs, binop, rhs, span);
				self.unify(result, place_ty, span);
			}
			// Plain `=`.
			None => self.check(rhs, place_ty),
		}
		self.interner.void()
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
		let ty = match &meta.type_ {
			Some(annot) => {
				let declared = self.lower_type(annot);
				self.check(value, declared);
				declared
			}
			None => self.infer(value),
		};
		self.bind_pattern(&meta.name, ty, meta.mutable);
	}

	// ── Iteration ────────────────────────────────────────────────────────────
	fn infer_iterable_element(&mut self, iterable: &Expr) -> Ty {
		if let ExprKind::Range(kind) = &iterable.kind {
			return self.infer_range_element(kind);
		}
		let ty = self.infer(iterable);
		let ty = self.shallow_resolve(ty);
		match self.interner.kind(ty).clone() {
			TyKind::List(elem) => elem,
			// Other iterables resolve through the `Iterable` interface (Milestone B).
			_ => self.fresh(),
		}
	}

	fn infer_range_element(&mut self, kind: &RangeKind) -> Ty {
		let elem = self.fresh();
		let bound = |checker: &mut Self, e: &Expr| checker.check(e, elem);
		match kind {
			RangeKind::From(a) | RangeKind::To(a) | RangeKind::ToInclusive(a) => bound(self, a),
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				bound(self, min);
				bound(self, max);
			}
		}
		elem
	}
}

/// The interface method a binary arithmetic/bitwise operator desugars to.
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

/// Build a `ParamIdx → arg` substitution from an ADT's positional generic
/// arguments, for reading a field type in terms of the receiver's arguments.
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
