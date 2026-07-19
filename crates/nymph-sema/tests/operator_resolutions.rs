//! Slice 4B, Task 1: integration tests pinning the `Resolution` (method name +
//! [`DispatchKind`]) the checker records on a `BinaryOp` node, per the D3 dispatch
//! table in the Slice 4B plan.
//!
//! Mirrors `tests/solve.rs`'s parse+check helper shape, plus a small local AST walk
//! (there is no other way to get from a test source string to the `NodeId` whose
//! `Resolution` we want to inspect).

use nymph_ast::{
	NodeId,
	decl::Declaration,
	expr::{Expr, ExprKind, Statement},
};
use nymph_sema::{DispatchKind, Resolution, check_module};
use nymph_syntax::parse_module;

const PLUS: &str = "interface Plus<Other, Output> { func plus(other: Other): Output }\n";

/// Minimal recursive descent collecting every `BinaryOp` node's [`NodeId`] found in
/// `expr`. Only descends through the expression shapes the fixtures below actually
/// produce — this is not a general-purpose AST visitor.
fn collect_binary_ops(expr: &Expr, out: &mut Vec<NodeId>) {
	if let ExprKind::BinaryOp { lhs, rhs, .. } = &expr.kind {
		out.push(expr.id);
		collect_binary_ops(lhs, out);
		collect_binary_ops(rhs, out);
		return;
	}
	match &expr.kind {
		ExprKind::Grouped(inner) => collect_binary_ops(inner, out),
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			collect_binary_ops(condition, out);
			collect_binary_ops(then, out);
			if let Some(otherwise) = otherwise {
				collect_binary_ops(otherwise, out);
			}
		}
		ExprKind::Block { body, .. } => {
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) => collect_binary_ops(e, out),
					Statement::Let { value, .. } => collect_binary_ops(value, out),
				}
			}
		}
		ExprKind::Call { func, args, .. } => {
			collect_binary_ops(func, out);
			for arg in args {
				collect_binary_ops(&arg.0.value, out);
			}
		}
		ExprKind::MemberAccess { parent, .. } => collect_binary_ops(parent, out),
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
			collect_binary_ops(value, out);
		}
		ExprKind::AssignOp { lhs, rhs, .. } => {
			collect_binary_ops(lhs, out);
			collect_binary_ops(rhs, out);
		}
		_ => {}
	}
}

/// Parse+check `source` (asserting zero diagnostics), find the single `BinaryOp`
/// node inside the named top-level `func`'s body, and return the `Resolution` the
/// checker recorded for it.
fn resolution_for(source: &str, func_name: &str) -> Resolution {
	let parsed = parse_module(source, "test");
	let parse_errors: Vec<_> = parsed
		.diagnostics
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		parse_errors.is_empty(),
		"source failed to parse: {parse_errors:?}\n---\n{source}"
	);

	let checked = check_module(&parsed.tree);
	let check_errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		check_errors.is_empty(),
		"expected no errors, got: {check_errors:?}\n---\n{source}"
	);

	let body = parsed
		.tree
		.members
		.iter()
		.find_map(|member| match member {
			Declaration::Func { meta, body, .. } if meta.name.0 == func_name => Some(body),
			_ => None,
		})
		.unwrap_or_else(|| panic!("no func named `{func_name}` in module:\n{source}"));

	let mut ops = Vec::new();
	collect_binary_ops(body, &mut ops);
	assert_eq!(
		ops.len(),
		1,
		"expected exactly one BinaryOp node in `{func_name}`'s body, found {}",
		ops.len()
	);

	checked
		.annotations
		.resolution_of(ops[0])
		.unwrap_or_else(|| panic!("no Resolution recorded for the BinaryOp node in `{func_name}`"))
		.clone()
}

#[test]
fn int_plus_int_is_builtin_eager() {
	// Same-primitive fast path: no `Plus` impl in scope, native JS `+`.
	let res = resolution_for("func f(a: int, b: int): int = a + b", "f");
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "plus");
}

#[test]
fn boolean_and_is_builtin_short_circuit() {
	// The built-in `boolean` default for `&&` short-circuits at codegen rather than
	// compiling to an eager method call.
	let res = resolution_for("func f(a: boolean, b: boolean): boolean = a && b", "f");
	assert_eq!(res.dispatch, DispatchKind::BuiltinShortCircuit);
	assert_eq!(res.method, "and");
}

#[test]
fn mixed_primitive_arithmetic_is_builtin_eager() {
	// `int + float` with non-literal operands: the checker resolves this through the
	// `Plus<Other = float, Output = float> for int` impl, but because the impl's
	// self-type is a primitive its JS numeric semantics already match a native `+`,
	// so codegen still gets `BuiltinEager` rather than a dispatched method call.
	let res = resolution_for(
		&format!(
			"{PLUS}
			 impl Plus<Other = float, Output = float> for int {{
			   func plus(other: float): float = other
			 }}
			 func f(a: int, b: float): float = a + b",
		),
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "plus");
}

#[test]
fn user_struct_plus_is_user_impl() {
	// `Vec2 + Vec2` resolves through a direct user impl method: `UserImpl`, compiled
	// as `lhs.plus(rhs)`.
	let res = resolution_for(
		&format!(
			"{PLUS}
			 struct Vec2(x: int, y: int)
			 impl Plus<Other = Vec2, Output = Vec2> for Vec2 {{
			   func plus(other: Vec2): Vec2 = other
			 }}
			 func add(a: Vec2, b: Vec2): Vec2 = a + b",
		),
		"add",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "plus");
}

#[test]
fn comparable_less_than_is_materialized_user_impl() {
	// `v1 < v2` desugars to `Comparable::less_than`, which `Vec2` never defines
	// directly — only `compare_to`. `less_than` is only reachable as the
	// interface's *default* body; Slice 4C-b materializes un-overridden defaults
	// onto the implementing class, so codegen can dispatch to it directly and
	// this now resolves as `UserImpl` (was `UserImplDefaultMethod` pre-4C-b).
	let res = resolution_for(
		"interface Comparable<Other> {
		   func compare_to(other: Other): int
		   func less_than(other: Other): boolean = this.compare_to(other) < 0
		 }
		 struct Vec2(x: int, y: int)
		 impl Comparable<Other = Vec2> for Vec2 {
		   func compare_to(other: Vec2): int = 0
		 }
		 func f(v1: Vec2, v2: Vec2): boolean = v1 < v2",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "less_than");
}

// ── Slice 4C-c, Task 1: comparison-arm parity with the arithmetic arm (W1) ──

#[test]
fn bounded_generic_less_than_dispatches_through_bound() {
	// A bounded generic parameter's `a < b` resolves through its `Comparable`
	// bound (W1) — mirrors the arithmetic arm's `GenericBound` →
	// `UserImplDefaultMethod` mapping. Before this slice the comparison arm never
	// routed a `Param` receiver through `dispatch_operator` at all, so this used
	// to record `BuiltinEager` and silently emit a native JS `<` on the
	// still-generic operands.
	let res = resolution_for(
		"interface Comparable<Other> { func less_than(other: Other): boolean }
		 func f<T: Comparable<Other = T>>(a: T, b: T): boolean = a < b",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImplDefaultMethod);
	assert_eq!(res.method, "less_than");
}

#[test]
fn late_resolved_infer_var_less_than_is_builtin_eager() {
	// Mirrors `late_resolved_infer_var_operand_is_builtin_eager` for `<`: `xs`'s
	// element type is a genuinely unconstrained inference variable at the moment
	// this `BinaryOp` node is recorded, pinned to `int` only afterward. W1 routes
	// this through the pending-operator queue (comparisons never used to defer at
	// all), finalized once `f`'s body is fully checked.
	let res = resolution_for(
		"func f(): boolean = {
		   let xs = #[]
		   let c = xs[0] < xs[0]
		   let pin: int = xs[0]
		   c
		 }",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "less_than");
}

#[test]
fn late_pinned_adt_less_than_dispatches_to_user_impl() {
	// The headline silent-miscompile probe from the 4C-c investigation: `xs`'s
	// element type is still an inference variable when `xs[0] < xs[0]` is
	// recorded, and is only pinned to `Vec2` afterward via the `#[Vec2]`
	// annotation. Before W1 this recorded `BuiltinEager` with zero diagnostics —
	// native JS `<` on `Vec2` objects. W1's pending-queue deferral re-resolves it
	// against the now-known `Vec2` element type, finding the direct `less_than`
	// impl (`UserImpl`).
	let res = resolution_for(
		"interface Comparable<Other> { func less_than(other: Other): boolean }
		 struct Vec2(x: int)
		 impl Comparable<Other = Vec2> for Vec2 {
		   func less_than(other: Vec2): boolean = true
		 }
		 func f(): boolean = {
		   let xs = #[]
		   let c = xs[0] < xs[0]
		   let pin: #[Vec2] = xs
		   c
		 }",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "less_than");
}

#[test]
fn unbounded_generic_equals_is_builtin_eager() {
	// W2: equality stays `BuiltinEager` for every operand kind, including a
	// generic parameter with no bound at all — `==`/`!=` is always native
	// reference equality, never dispatched to a user `Equals` impl (D3, unchanged
	// by this slice).
	let res = resolution_for("func f<T>(a: T, b: T): boolean = a == b", "f");
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "equals");
}

/// Like [`collect_binary_ops`], but collects `AssignOp` nodes instead — Finding 1
/// records the compound-assign operator's `Resolution` on the `AssignOp` node
/// itself (there's no separate desugared `BinaryOp` AST node to hang it on).
fn collect_assign_ops(expr: &Expr, out: &mut Vec<NodeId>) {
	if let ExprKind::AssignOp { lhs, rhs, .. } = &expr.kind {
		out.push(expr.id);
		collect_assign_ops(lhs, out);
		collect_assign_ops(rhs, out);
		return;
	}
	match &expr.kind {
		ExprKind::Grouped(inner) => collect_assign_ops(inner, out),
		ExprKind::Block { body, .. } => {
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) => collect_assign_ops(e, out),
					Statement::Let { value, .. } => collect_assign_ops(value, out),
				}
			}
		}
		ExprKind::While { body, .. } => collect_assign_ops(body, out),
		_ => {}
	}
}

/// Parse+check `source` (asserting zero diagnostics), find the single `AssignOp`
/// node inside the named top-level `func`'s body, and return the `Resolution` the
/// checker recorded for it.
fn assign_resolution_for(source: &str, func_name: &str) -> Resolution {
	let parsed = parse_module(source, "test");
	let parse_errors: Vec<_> = parsed
		.diagnostics
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		parse_errors.is_empty(),
		"source failed to parse: {parse_errors:?}\n---\n{source}"
	);

	let checked = check_module(&parsed.tree);
	let check_errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		check_errors.is_empty(),
		"expected no errors, got: {check_errors:?}\n---\n{source}"
	);

	let body = parsed
		.tree
		.members
		.iter()
		.find_map(|member| match member {
			Declaration::Func { meta, body, .. } if meta.name.0 == func_name => Some(body),
			_ => None,
		})
		.unwrap_or_else(|| panic!("no func named `{func_name}` in module:\n{source}"));

	let mut ops = Vec::new();
	collect_assign_ops(body, &mut ops);
	assert_eq!(
		ops.len(),
		1,
		"expected exactly one AssignOp node in `{func_name}`'s body, found {}",
		ops.len()
	);

	checked
		.annotations
		.resolution_of(ops[0])
		.unwrap_or_else(|| panic!("no Resolution recorded for the AssignOp node in `{func_name}`"))
		.clone()
}

#[test]
fn compound_assign_user_struct_plus_is_user_impl() {
	// `v1 += v2` on a struct with a directly-defined `Plus.plus` impl resolves
	// through a direct user impl method, same as `v1 + v2` would (Finding 1): the
	// `AssignOp` node itself carries the `Resolution`, not a separate `BinaryOp`.
	let res = assign_resolution_for(
		&format!(
			"{PLUS}
			 struct Vec2(x: int, y: int)
			 impl Plus<Other = Vec2, Output = Vec2> for Vec2 {{
			   func plus(other: Vec2): Vec2 = other
			 }}
			 func add(a: Vec2, b: Vec2): Vec2 = {{
			   let mut v1 = a
			   v1 += b
			   v1
			 }}",
		),
		"add",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "plus");
}

#[test]
fn compound_assign_int_plus_is_builtin_eager() {
	// `x += 1` on a plain `int` stays a native builtin, same as `x + 1` would.
	let res = assign_resolution_for(
		"func f(): int = {
		   let mut x = 1
		   x += 1
		   x
		 }",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "plus");
}

#[test]
fn deferred_compound_assign_keeps_its_own_type_as_void() {
	// Finding 1: `finalize_pending_operators` used to overwrite the `AssignOp`
	// node's own recorded type with the operator's resolved operand/result type
	// whenever the resolution was *deferred* (the operand still an unresolved
	// inference variable at the moment `infer_binary`'s fallback ran, later pinned
	// down by a `check`-mode subtype applied elsewhere in the body -- here, the
	// function's declared `int` return type). The immediately-resolved compound-
	// assign path (`compound_assign_int_plus_is_builtin_eager` above) never
	// clobbers `ty` this way -- it only ever calls `record_resolution`, leaving
	// `ty` at the `Void` that `infer`'s `AssignOp` special case sets up front. The
	// two paths must agree on the stored `ExprInfo.ty` for the same AST shape.
	let source = "func f(): int = {
	   let mut xs = #[]
	   let mut x = xs[0]
	   x += xs[0]
	   x
	 }";
	let parsed = parse_module(source, "test");
	let parse_errors: Vec<_> = parsed
		.diagnostics
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		parse_errors.is_empty(),
		"source failed to parse: {parse_errors:?}\n---\n{source}"
	);

	let checked = check_module(&parsed.tree);
	let check_errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		check_errors.is_empty(),
		"expected no errors, got: {check_errors:?}\n---\n{source}"
	);

	let body = parsed
		.tree
		.members
		.iter()
		.find_map(|member| match member {
			Declaration::Func { meta, body, .. } if meta.name.0 == "f" => Some(body),
			_ => None,
		})
		.expect("no func named `f` in module");

	let mut ops = Vec::new();
	collect_assign_ops(body, &mut ops);
	assert_eq!(
		ops.len(),
		1,
		"expected exactly one AssignOp node in `f`'s body, found {}",
		ops.len()
	);

	let info = checked
		.annotations
		.get(ops[0])
		.unwrap_or_else(|| panic!("no ExprInfo recorded for the AssignOp node in `f`"));
	assert_eq!(
		info.ty,
		checked.interner.void(),
		"the AssignOp node's own recorded type must stay Void even when its operator \
		 Resolution was deferred to finalize_pending_operators"
	);

	// Sanity: the deferred `Resolution` itself is still attached correctly.
	let res = checked
		.annotations
		.resolution_of(ops[0])
		.unwrap_or_else(|| panic!("no Resolution recorded for the AssignOp node in `f`"));
	assert_eq!(res.method, "plus");
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
}

#[test]
fn late_resolved_infer_var_operand_is_builtin_eager() {
	// Finding 2: `xs[0] + xs[0]` — `xs`'s element type is a genuinely unconstrained
	// inference variable at the moment this `BinaryOp` node is recorded (the
	// fallback's own `unify(l, r)` is a no-op here, since both operands are already
	// the same still-unbound variable). It only gets pinned to `int` *afterward*,
	// when `f`'s body is checked against its declared `int` return type. Zero
	// diagnostics, so `finalize_pending_operators` must retry this node once the
	// whole module is checked, rather than leaving lowering to panic on it.
	let res = resolution_for(
		"func f(): int = {
		   let xs = #[]
		   xs[0] + xs[0]
		 }",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "plus");
}

#[test]
fn user_struct_equals_is_builtin_eager() {
	// D3 defers `equals` dispatch to the stdlib slice: even with a user `Equals`
	// impl in scope (here via the blanket `impl<T> Equals<Other = T> for T`),
	// codegen still emits `===`, so this stays `BuiltinEager` rather than
	// `UserImpl`.
	let res = resolution_for(
		"interface Equals<Other> { func equals(other: Other): boolean }
		 impl<T> Equals<Other = T> for T { func equals(other: T): boolean = true }
		 struct P(x: int)
		 func same(a: P, b: P): boolean = a == b",
		"same",
	);
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "equals");
}

// ── Slice 4C-a: prefix (unary) operator resolutions ─────────────────────────

/// Like [`collect_binary_ops`], but collects `PrefixOp` nodes instead.
fn collect_prefix_ops(expr: &Expr, out: &mut Vec<NodeId>) {
	if let ExprKind::PrefixOp { value, .. } = &expr.kind {
		out.push(expr.id);
		collect_prefix_ops(value, out);
		return;
	}
	match &expr.kind {
		ExprKind::Grouped(inner) => collect_prefix_ops(inner, out),
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			collect_prefix_ops(condition, out);
			collect_prefix_ops(then, out);
			if let Some(otherwise) = otherwise {
				collect_prefix_ops(otherwise, out);
			}
		}
		ExprKind::Block { body, .. } => {
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) => collect_prefix_ops(e, out),
					Statement::Let { value, .. } => collect_prefix_ops(value, out),
				}
			}
		}
		ExprKind::Call { func, args, .. } => {
			collect_prefix_ops(func, out);
			for arg in args {
				collect_prefix_ops(&arg.0.value, out);
			}
		}
		ExprKind::MemberAccess { parent, .. } => collect_prefix_ops(parent, out),
		ExprKind::IndexAccess { parent, index, .. } => {
			collect_prefix_ops(parent, out);
			collect_prefix_ops(index, out);
		}
		ExprKind::PostfixOp { value, .. } => collect_prefix_ops(value, out),
		ExprKind::AssignOp { lhs, rhs, .. } | ExprKind::BinaryOp { lhs, rhs, .. } => {
			collect_prefix_ops(lhs, out);
			collect_prefix_ops(rhs, out);
		}
		_ => {}
	}
}

/// Parse+check `source` (asserting zero diagnostics), find the single `PrefixOp`
/// node inside the named top-level `func`'s body, and return the `Resolution` the
/// checker recorded for it.
fn prefix_resolution_for(source: &str, func_name: &str) -> Resolution {
	let parsed = parse_module(source, "test");
	let parse_errors: Vec<_> = parsed
		.diagnostics
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		parse_errors.is_empty(),
		"source failed to parse: {parse_errors:?}\n---\n{source}"
	);

	let checked = check_module(&parsed.tree);
	let check_errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		check_errors.is_empty(),
		"expected no errors, got: {check_errors:?}\n---\n{source}"
	);

	let body = parsed
		.tree
		.members
		.iter()
		.find_map(|member| match member {
			Declaration::Func { meta, body, .. } if meta.name.0 == func_name => Some(body),
			_ => None,
		})
		.unwrap_or_else(|| panic!("no func named `{func_name}` in module:\n{source}"));

	let mut ops = Vec::new();
	collect_prefix_ops(body, &mut ops);
	assert_eq!(
		ops.len(),
		1,
		"expected exactly one PrefixOp node in `{func_name}`'s body, found {}",
		ops.len()
	);

	checked
		.annotations
		.resolution_of(ops[0])
		.unwrap_or_else(|| panic!("no Resolution recorded for the PrefixOp node in `{func_name}`"))
		.clone()
}

#[test]
fn negate_int_is_builtin_eager() {
	let res = prefix_resolution_for("func f(a: int): int = -a", "f");
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "negate");
}

#[test]
fn bool_not_on_boolean_is_builtin_eager() {
	let res = prefix_resolution_for("func f(a: boolean): boolean = !a", "f");
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "not");
}

#[test]
fn bit_not_int_is_builtin_eager() {
	let res = prefix_resolution_for("func f(a: int): int = ~a", "f");
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "bit_not");
}

#[test]
fn negate_user_struct_direct_impl_is_user_impl() {
	// `-v` on a struct with a directly-defined `Negate.negate` impl: `UserImpl`,
	// compiled as `v.negate()`.
	let res = prefix_resolution_for(
		"interface Negate<Output> { func negate(): Output }
		 struct Vec2(x: int, y: int)
		 impl Negate<Output = Vec2> for Vec2 {
		   func negate(): Vec2 = this
		 }
		 func f(v: Vec2): Vec2 = -v",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "negate");
}

#[test]
fn negate_interface_default_method_is_materialized_user_impl() {
	// `-v` resolves through `Negate`'s interface *default* body (`negate`, provided
	// in terms of `base`), which `Vec2`'s impl never defines directly — only
	// `base`. Slice 4C-b materializes the un-overridden default onto the class, so
	// this now resolves as `UserImpl` (was `UserImplDefaultMethod` pre-4C-b).
	let res = prefix_resolution_for(
		"interface Negate<Output> {
		   func base(): Output
		   func negate(): Output = this.base()
		 }
		 struct Vec2(x: int, y: int)
		 impl Negate<Output = Vec2> for Vec2 {
		   func base(): Vec2 = this
		 }
		 func f(v: Vec2): Vec2 = -v",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "negate");
}

#[test]
fn late_resolved_infer_var_negate_operand_is_builtin_eager() {
	// Mirrors `late_resolved_infer_var_operand_is_builtin_eager` for the binary
	// case: `xs[0]`'s element type is a genuinely unconstrained inference variable
	// at the moment this `PrefixOp` node is recorded, pinned to `int` only
	// afterward via `f`'s declared return type. Zero diagnostics, so
	// `finalize_pending_operators` must retry this node.
	//
	// NB: a leading `-` on its own statement line continuing a previous expression
	// parses as *binary* minus, not a prefix negate — the fixture binds the negation
	// to its own `let` to avoid that trap.
	let res = prefix_resolution_for(
		"func f(): int = {
		   let xs = #[]
		   let y = -xs[0]
		   y
		 }",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "negate");
}

#[test]
fn bounded_generic_negate_dispatches_through_bound() {
	// A bounded generic parameter's `-t` resolves through its `Negate` bound —
	// mirrors the binary case's `GenericBound` → `UserImplDefaultMethod` mapping
	// (no direct-call binary-operator call site reaches `GenericBound` either; both
	// share the same "never miscompile silently" deferral).
	let res = prefix_resolution_for(
		"interface Negate<Output> { func negate(): Output }
		 func f<T: Negate<Output = T>>(t: T): T = -t",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImplDefaultMethod);
	assert_eq!(res.method, "negate");
}

#[test]
fn impl_trait_parameter_negate_dispatches_through_bound() {
	// Z3 (Slice 4F): `impl Trait` param sugar (`t: Negate<Output = int>`, sugar
	// for a synthetic generic parameter bounded by `Negate`) resolves `-t`
	// through that bound exactly like the declared-generic spelling above —
	// still `GenericBound` → `UserImplDefaultMethod`, unaffected by this
	// slice's call-site instantiation fix (that fix only touches how the
	// function's *type* is instantiated at a use site, not how operators
	// dispatch on the param inside the body being checked).
	let res = prefix_resolution_for(
		"interface Negate<Output> { func negate(): Output }
		 func f(t: Negate<Output = int>): int = -t",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImplDefaultMethod);
	assert_eq!(res.method, "negate");
}

#[test]
fn unbounded_generic_negate_is_not_implemented() {
	// An unbounded generic parameter has no `Negate` impl to dispatch to — a
	// `NotImplemented` diagnostic, not a lowering-time ICE.
	let parsed = parse_module("func f<T>(t: T): T = -t", "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed"
	);
	let checked = check_module(&parsed.tree);
	let messages: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert_eq!(
		messages.len(),
		1,
		"expected exactly one error: {messages:?}"
	);
	assert!(
		messages[0].contains('-')
			&& messages[0].contains("Negate")
			&& messages[0].contains("not implemented"),
		"unexpected message: {messages:?}"
	);
}

// ── Slice 4I, Task 1: `in`/`!in`/`??` resolutions, `|>` typing ──────────────

#[test]
fn user_contains_impl_in_is_user_impl() {
	// `a in c` ≡ `c.contains(a)` — the RHS (collection) is the receiver.
	let res = resolution_for(
		"interface Contains<Item> { func contains(item: Item): boolean }
		 struct Bag(n: int)
		 impl Contains<Item = int> for Bag {
		   func contains(item: int): boolean = true
		 }
		 func f(b: Bag, x: int): boolean = x in b",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "contains");
}

#[test]
fn user_contains_impl_not_in_dispatches_not_contains() {
	// `!in` resolves the separate `not_contains` method name.
	let res = resolution_for(
		"interface Contains<Item> {
		   func contains(item: Item): boolean
		   func not_contains(item: Item): boolean
		 }
		 struct Bag(n: int)
		 impl Contains<Item = int> for Bag {
		   func contains(item: int): boolean = true
		   func not_contains(item: int): boolean = false
		 }
		 func f(b: Bag, x: int): boolean = x !in b",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "not_contains");
}

#[test]
fn primitive_rhs_in_is_not_implemented() {
	// D2/Task 1 gap closed: a primitive RHS with no `Contains` impl used to
	// type-check silently (zero diagnostics, `is_adt` gated dispatch off); it must
	// now report `NotImplemented` rather than reach the lowering panic.
	let parsed = parse_module("func f(x: int, y: int): boolean = x in y", "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed"
	);
	let checked = check_module(&parsed.tree);
	let messages: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert_eq!(
		messages.len(),
		1,
		"expected exactly one error: {messages:?}"
	);
	assert!(
		messages[0].contains("in")
			&& messages[0].contains("Contains")
			&& messages[0].contains("not implemented"),
		"unexpected message: {messages:?}"
	);
}

#[test]
fn user_unwrap_impl_is_user_impl_eager() {
	// `a ?? b` on a struct with a directly-defined `Unwrap.unwrap` impl resolves
	// eagerly to `UserImpl` — `recv.unwrap(fallback)`, not short-circuiting.
	let res = resolution_for(
		"interface Unwrap<Output> { func unwrap(default: Output): Output }
		 struct MaybeInt(present: boolean, value: int)
		 impl Unwrap<Output = int> for MaybeInt {
		   func unwrap(default: int): int = default
		 }
		 func f(m: MaybeInt, d: int): int = m ?? d",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "unwrap");
}

#[test]
fn unwrap_with_no_impl_is_not_implemented() {
	// An int LHS has no `Unwrap` impl — `NotImplemented`, never a silent
	// lowering-time panic.
	let parsed = parse_module("func f(a: int, b: int): int = a ?? b", "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed"
	);
	let checked = check_module(&parsed.tree);
	let messages: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert_eq!(
		messages.len(),
		1,
		"expected exactly one error: {messages:?}"
	);
	assert!(
		messages[0].contains("??")
			&& messages[0].contains("Unwrap")
			&& messages[0].contains("not implemented"),
		"unexpected message: {messages:?}"
	);
}

#[test]
fn bounded_generic_unwrap_dispatches_through_bound() {
	// A bounded generic parameter's `a ?? b` resolves through its `Unwrap`
	// bound — `GenericBound` → `UserImplDefaultMethod`, mirroring every other
	// operator's bound-dispatch mapping.
	let res = resolution_for(
		"interface Unwrap<Output> { func unwrap(default: Output): Output }
		 func f<T: Unwrap<Output = int>>(a: T, b: int): int = a ?? b",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImplDefaultMethod);
	assert_eq!(res.method, "unwrap");
}

#[test]
fn pipe_chain_types_as_left_associative_application() {
	// `10 |> double |> inc` types cleanly and (per D1/DD1) records no
	// `Resolution` at all — `|>` lowers structurally to a `Call`, not a dispatch.
	// This is a smoke check that the checker's existing Pipe handling still
	// agrees with structural-`Call` lowering; `collect_binary_ops` doesn't walk
	// through `Pipe`'s `Call`-shaped AST node so we just assert zero diagnostics.
	let parsed = parse_module(
		"func double(x: int): int = x * 2
		 func inc(x: int): int = x + 1
		 func f(): int = 10 |> double |> inc",
		"test",
	);
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed"
	);
	let checked = check_module(&parsed.tree);
	let messages: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(messages.is_empty(), "expected no errors: {messages:?}");
}

#[test]
fn pipe_widens_int_literal_argument_like_a_direct_call() {
	// `5 |> takes_float` must type-check exactly like `takes_float(5)`: an int
	// literal argument widens to the parameter's `float` type either way, since
	// `|>` lowers structurally to the same `Call` shape (DD1). Regression test for
	// the confirmed Pipe-widening gap: the checker's Pipe arm used to type the
	// piped-in literal via `infer` (concrete `int`) instead of `check` against the
	// callee's parameter type, so the direct call and its pipe-equivalent
	// disagreed.
	let parsed = parse_module(
		"func takes_float(x: float): float = x
		 func f(): float = 5 |> takes_float",
		"test",
	);
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed"
	);
	let checked = check_module(&parsed.tree);
	let messages: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(messages.is_empty(), "expected no errors: {messages:?}");
}

#[test]
fn unresolved_prefix_operand_reports_cannot_infer_operand_type() {
	// A prefix negate whose operand type never gets pinned down by the end of the
	// body reports `CannotInferOperandType` rather than silently leaving lowering
	// to panic on a supposedly zero-diagnostic program.
	let parsed = parse_module(
		"func f(): int = {
		   let xs = #[]
		   let y = -xs[0]
		   0
		 }",
		"test",
	);
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed"
	);
	let checked = check_module(&parsed.tree);
	let messages: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert_eq!(
		messages.len(),
		1,
		"expected exactly one error: {messages:?}"
	);
}

// ── Resolver precedence for `Param` receivers (MM2, prelude-flavored pins) ──
//
// A still-generic `TyKind::Param` receiver used to lose a method name declared
// by the param's OWN bound to an unrelated blanket impl of the same name
// (`resolve_method`, `solve.rs`), because `head_of(Param)` is `None` and phase
// 1's impl-index search only ever finds blanket buckets. These pins exercise
// that precedence against the real stdlib prelude (`stdlib/src/ops/mod.nym`),
// which is exactly where the bug was first reported (the prelude flip's
// `less_than` → `lighter_than` rename worked around it rather than fixing the
// resolver). `tests/solve.rs` pins the same two shapes with local interface
// declarations under bare `check_module`; these mirror them through
// `check_module_with_prelude` and inspect the recorded `Resolution` directly
// rather than relying solely on a return-type mismatch to prove which impl
// won. Neither test calls `lower_hir` — only `check_module_with_prelude` — so
// the "prelude bodies still panic in lowering" caveat (materialization is a
// separate slice's concern) never triggers here.

fn ops_prelude_module() -> Module {
	let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../stdlib/src/ops/mod.nym")
		.canonicalize()
		.unwrap();
	let source = std::fs::read_to_string(path).unwrap();
	let parsed = parse_module(&source, "std/ops");
	assert!(
		parsed.diagnostics.iter().all(|d| !d.is_error()),
		"std/ops failed to parse"
	);
	parsed.tree
}

/// Minimal recursive descent collecting every method-call (`Call` whose `func` is
/// a `MemberAccess`) node's [`NodeId`] found in `expr`. Mirrors
/// `collect_binary_ops` above, but for `receiver.method(args)` shapes.
fn collect_method_calls(expr: &Expr, out: &mut Vec<NodeId>) {
	if let ExprKind::Call { func, args, .. } = &expr.kind {
		if matches!(func.kind, ExprKind::MemberAccess { .. }) {
			out.push(expr.id);
		}
		collect_method_calls(func, out);
		for arg in args {
			collect_method_calls(&arg.0.value, out);
		}
		return;
	}
	match &expr.kind {
		ExprKind::Grouped(inner) => collect_method_calls(inner, out),
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			collect_method_calls(condition, out);
			collect_method_calls(then, out);
			if let Some(otherwise) = otherwise {
				collect_method_calls(otherwise, out);
			}
		}
		ExprKind::Block { body, .. } => {
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) => collect_method_calls(e, out),
					Statement::Let { value, .. } => collect_method_calls(value, out),
				}
			}
		}
		ExprKind::MemberAccess { parent, .. } => collect_method_calls(parent, out),
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
			collect_method_calls(value, out);
		}
		ExprKind::AssignOp { lhs, rhs, .. } => {
			collect_method_calls(lhs, out);
			collect_method_calls(rhs, out);
		}
		_ => {}
	}
}

/// Parse `user` + the real stdlib ops prelude, check them together via
/// `check_module_with_prelude` (asserting zero diagnostics), find the single
/// method-call node inside the named top-level `func`'s body, and return the
/// `Resolution` the checker recorded for it.
fn resolution_for_prelude(user_source: &str, func_name: &str) -> Resolution {
	let parsed = parse_module(user_source, "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"source failed to parse: {:?}\n---\n{user_source}",
		parsed.diagnostics
	);
	let user = parsed.tree;
	let prelude = ops_prelude_module();

	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));
	let errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		errors.is_empty(),
		"expected no errors, got: {errors:?}\n---\n{user_source}"
	);

	let body = user
		.members
		.iter()
		.find_map(|member| match member {
			Declaration::Func { meta, body, .. } if meta.name.0 == func_name => Some(body),
			_ => None,
		})
		.unwrap_or_else(|| panic!("no func named `{func_name}` in module:\n{user_source}"));

	let mut calls = Vec::new();
	collect_method_calls(body, &mut calls);
	assert_eq!(
		calls.len(),
		1,
		"expected exactly one method-call node in `{func_name}`'s body, found {}",
		calls.len()
	);

	checked
		.annotations
		.resolution_of(calls[0])
		.unwrap_or_else(|| panic!("no Resolution recorded for the method-call node in `{func_name}`"))
		.clone()
}

#[test]
fn param_bound_method_beats_prelude_blanket_default() {
	// Collision shape: a user-declared bound `Weighty::less_than` (returns `int`)
	// must win over the prelude's blanket `impl<T> Comparable<Other = T> for T`,
	// whose `less_than` is an interface *default* method (returns `boolean`).
	// Before the MM1 fix, `head_of(Param) == None` forced candidate gathering to
	// that blanket bucket, which matched *any* receiver and got committed before
	// `T`'s own bound was ever consulted — this is precisely the shape the
	// prelude flip migrated away from by renaming to `lighter_than`.
	let res = resolution_for_prelude(
		"interface Weighty<Other> { func less_than(other: Other): int }
		 func f<T: Weighty<Other = T>>(a: T, b: T): int = a.less_than(b)",
		"f",
	);
	assert_eq!(res.method, "less_than");
	// The bound is a plain user-declared interface (not prelude-origin), so this
	// resolves as an ordinary, immediately-lowerable generic-bound dispatch.
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
}

#[test]
fn unconstrained_param_still_dispatches_through_prelude_blanket() {
	// The blanket-dispatch behavior MM1 must preserve: with no bound at all on
	// `T`, `a.equals(b)` has nothing to consult in `resolve_param_method` (no
	// bounds recorded), so control falls through to phase 1's blanket-bucket
	// search, which matches the prelude's blanket `impl<T> Equals<Other = self>
	// for T`. A prior (reverted) fix attempt returned eagerly on a bounds miss
	// here and broke exactly this case (error 2021, "no method `equals`").
	let res = resolution_for_prelude("func same<T>(a: T, b: T): boolean = a.equals(b)", "same");
	assert_eq!(res.method, "equals");
	// The matched impl is the prelude's own blanket impl, so it is
	// prelude-origin/unmaterialized and deferred accordingly.
	assert_eq!(res.dispatch, DispatchKind::UserImplDefaultMethod);
}

/// Like [`resolution_for_prelude`], but for a `BinaryOp` node (not a method call).
fn binary_resolution_for_prelude(user_source: &str, func_name: &str) -> Resolution {
	let parsed = parse_module(user_source, "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"source failed to parse: {:?}\n---\n{user_source}",
		parsed.diagnostics
	);
	let user = parsed.tree;
	let prelude = ops_prelude_module();
	let checked = check_module_with_prelude(&user, std::slice::from_ref(&prelude));
	let errors: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		errors.is_empty(),
		"expected no errors, got: {errors:?}\n---\n{user_source}"
	);
	let body = user
		.members
		.iter()
		.find_map(|member| match member {
			Declaration::Func { meta, body, .. } if meta.name.0 == func_name => Some(body),
			_ => None,
		})
		.unwrap_or_else(|| panic!("no func named `{func_name}` in module:\n{user_source}"));
	let mut ops = Vec::new();
	collect_binary_ops(body, &mut ops);
	assert_eq!(
		ops.len(),
		1,
		"expected exactly one BinaryOp in `{func_name}`"
	);
	checked
		.annotations
		.resolution_of(ops[0])
		.unwrap_or_else(|| panic!("no Resolution recorded for the BinaryOp in `{func_name}`"))
		.clone()
}

// ── Bug 3: a clearer operator-missing diagnostic ────────────────────────────

#[test]
fn missing_binary_operator_impl_names_the_operator_operands_and_interface() {
	// `A + B` with no `Plus` impl for either type: the old bare message leaked
	// the internal method name (`plus`) and named only the LHS type. The new
	// message must name the surface operator, BOTH operand types, and the
	// interface implementing it.
	let parsed = parse_module(
		"interface Plus<Other, Output> { func plus(other: Other): Output }
		 struct A(x: int)
		 struct B(y: int)
		 func f(a: A, b: B): A = a + b",
		"test",
	);
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed"
	);
	let checked = check_module(&parsed.tree);
	let messages: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert_eq!(
		messages.len(),
		1,
		"expected exactly one error: {messages:?}"
	);
	let message = &messages[0];
	assert!(
		message.contains('+')
			&& message.contains('A')
			&& message.contains('B')
			&& message.contains("Plus")
			&& message.contains("not implemented"),
		"unexpected message: {messages:?}"
	);
}

#[test]
fn missing_unary_operator_impl_names_the_operator_operand_and_interface_with_no_rhs() {
	// A unary operator (`-t`, `Negate`) with no impl: the message must still name
	// the operator and the sole operand type, with no dangling "and `..`" for a
	// nonexistent RHS.
	let parsed = parse_module("func f<T>(t: T): T = -t", "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed"
	);
	let checked = check_module(&parsed.tree);
	let messages: Vec<_> = checked
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert_eq!(
		messages.len(),
		1,
		"expected exactly one error: {messages:?}"
	);
	let message = &messages[0];
	assert!(
		message.contains('-') && message.contains('T') && message.contains("Negate"),
		"unexpected message: {messages:?}"
	);
	assert!(
		!message.contains(" and "),
		"unary diagnostic should not mention a second operand: {messages:?}"
	);
}

#[test]
fn boolean_bitwise_operators_dispatch_to_the_prelude_not_native_js() {
	// Booleans have no native JS `&`/`|`/`^` semantics to reuse (JS coerces them
	// to numbers: `true & false` → 0). infer_binary's same-primitive fast path
	// therefore does NOT take `BuiltinEager` for a boolean receiver; it dispatches
	// to the stdlib BitAnd/BitOr/BitXor impls instead, which the prelude provides
	// and lowering can materialize.
	for (src, method) in [
		("func f(a: boolean, b: boolean): boolean = a & b", "bit_and"),
		("func f(a: boolean, b: boolean): boolean = a | b", "bit_or"),
		("func f(a: boolean, b: boolean): boolean = a ^ b", "bit_xor"),
	] {
		let res = binary_resolution_for_prelude(src, "f");
		assert_eq!(res.method, method, "for {src:?}");
		assert_ne!(
			res.dispatch,
			DispatchKind::BuiltinEager,
			"boolean `{method}` must not compile to a native JS operator ({src:?})"
		);
	}
}
