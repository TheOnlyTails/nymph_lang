//! integration tests pinning the `Resolution` (method name +
//! [`DispatchKind`]) the checker records on a `BinaryOp` node, per the dispatch
//! table in the plan.
//!
//! Mirrors `tests/solve.rs`'s parse+check helper shape, plus a small local AST walk
//! (there is no other way to get from a test source string to the `NodeId` whose
//! `Resolution` we want to inspect).

use nymph_ast::{
	NodeId,
	decl::Declaration,
	expr::{Expr, ExprKind, ListItem, Statement},
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
		ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(expr) | ListItem::Spread(expr) => collect_binary_ops(expr, out),
				}
			}
		}
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
				collect_binary_ops(arg.0.value(), out);
			}
		}
		ExprKind::MemberAccess { parent, .. } => collect_binary_ops(parent, out),
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
			collect_binary_ops(value, out);
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
	// interface's default body. The checker materializes un-overridden defaults
	// onto the implementing class, so codegen dispatches to a `UserImpl`.
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

// ── comparison-arm parity with the arithmetic arm  ──

#[test]
fn bounded_generic_less_than_dispatches_through_bound() {
	// A bounded generic parameter's `a < b` resolves through its `Comparable`
	// bound, mirroring the arithmetic arm's `GenericBound` →
	// `UserImplDefaultMethod` mapping.
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
	// this `BinaryOp` node is recorded and is pinned to `int` only afterward. The
	// pending-operator queue finalizes it once `f`'s body is fully checked.
	let res = resolution_for(
		"func defer<T>(): T = defer()
		 func f(): boolean = {
		   let xs = #[defer()]
		   let c = xs[0u] < xs[0u]
		   let pin: int = xs[0u]
		   c
		 }",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "less_than");
}

#[test]
fn late_pinned_adt_less_than_dispatches_to_user_impl() {
	// `xs`'s element type is an inference variable when `xs[0] < xs[0]` is
	// recorded and is pinned to `Vec2` afterward by the `#[Vec2]` annotation.
	// Pending resolution then selects the direct `less_than` impl (`UserImpl`).
	let res = resolution_for(
		"func defer<T>(): T = defer()
		 interface Comparable<Other> { func less_than(other: Other): boolean }
		 struct Vec2(x: int)
		 impl Comparable<Other = Vec2> for Vec2 {
		   func less_than(other: Vec2): boolean = true
		 }
		 func f(): boolean = {
		   let xs = #[defer()]
		   let c = xs[0u] < xs[0u]
		   let pin: #[Vec2] = xs
		   c
		 }",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "less_than");
}

#[test]
fn late_resolved_infer_var_operand_is_builtin_eager() {
	// `xs[0] + xs[0]` — `xs`'s element type is a genuinely unconstrained
	// inference variable at the moment this `BinaryOp` node is recorded (the
	// fallback's own `unify(l, r)` is a no-op here, since both operands are already
	// the same still-unbound variable). It only gets pinned to `int` *afterward*,
	// when `f`'s body is checked against its declared `int` return type. Zero
	// diagnostics, so `finalize_pending_operators` must retry this node once the
	// whole module is checked, rather than leaving lowering to panic on it.
	let res = resolution_for(
		"func defer<T>(): T = defer()
		 func f(): int = {
		   let xs = #[defer()]
		   xs[0u] + xs[0u]
		 }",
		"f",
	);
	assert_eq!(res.dispatch, DispatchKind::BuiltinEager);
	assert_eq!(res.method, "plus");
}

#[test]
fn user_struct_equals_dispatches_to_the_concrete_impl() {
	let res = resolution_for(
		"interface Equals<Other> { func equals(other: Other): boolean }
		 struct P(x: int)
		 impl Equals<Other = P> for P { func equals(other: P): boolean = true }
		 func same(a: P, b: P): boolean = a == b",
		"same",
	);
	assert_eq!(res.dispatch, DispatchKind::UserImpl);
	assert_eq!(res.method, "equals");
}

// ── prefix (unary) operator resolutions ─────────────────────────

/// Like [`collect_binary_ops`], but collects `PrefixOp` nodes instead.
fn collect_prefix_ops(expr: &Expr, out: &mut Vec<NodeId>) {
	if let ExprKind::PrefixOp { value, .. } = &expr.kind {
		out.push(expr.id);
		collect_prefix_ops(value, out);
		return;
	}
	match &expr.kind {
		ExprKind::Grouped(inner) => collect_prefix_ops(inner, out),
		ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(expr) | ListItem::Spread(expr) => collect_prefix_ops(expr, out),
				}
			}
		}
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
				collect_prefix_ops(arg.0.value(), out);
			}
		}
		ExprKind::MemberAccess { parent, .. } => collect_prefix_ops(parent, out),
		ExprKind::IndexAccess { parent, index, .. } => {
			collect_prefix_ops(parent, out);
			collect_prefix_ops(index, out);
		}
		ExprKind::PostfixOp { value, .. } => collect_prefix_ops(value, out),
		ExprKind::BinaryOp { lhs, rhs, .. } => {
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
fn bool_not_on_a_generic_inherent_method_result_is_builtin_eager() {
	let res = prefix_resolution_for(
		"interface Hash { func hash(): int }
		 impl<K: Hash, V> #{K: V} {
		   external func contains_key(key: K): boolean
		 }
		 func missing<K: Hash, V>(map: #{K: V}, key: K): boolean = !map.contains_key(key)",
		"missing",
	);
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
	// `base`. The checker materializes the un-overridden default onto the class,
	// so this resolves as `UserImpl`.
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
		"func defer<T>(): T = defer()
		 func f(): int = {
		   let xs = #[defer()]
		   let y = -xs[0u]
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
	// `impl Trait` param sugar (`t: Negate<Output = int>`, sugar
	// for a synthetic generic parameter bounded by `Negate`) resolves `-t`
	// through that bound exactly like the declared-generic spelling above —
	// still `GenericBound` → `UserImplDefaultMethod`, unaffected by the checker's
	// call-site instantiation (which only touches how the
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
		messages[0].contains("`-` operator")
			&& messages[0].contains("Negate")
			&& messages[0].contains("not implemented"),
		"unexpected message: {messages:?}"
	);
}

// ── `in`/`!in`/`??` resolutions, `|>` typing ──────────────

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
	// A primitive RHS with no `Contains` impl must not
	// type-check silently (zero diagnostics, `is_adt` gated dispatch off); it must
	// report `NotImplemented` rather than reach the lowering panic.
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
		messages[0].contains("`in` operator")
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
		messages[0].contains("`??` operator")
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
	// `10 |> double |> inc` type-checks and records no
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
	// `|>` lowers structurally to the same `Call` shape. The checker's Pipe arm
	// checks the piped-in literal against the callee's parameter type.
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
		"func defer<T>(): T = defer()
		 func f(): int = {
		   let xs = #[defer()]
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

#[test]
fn user_power_impl_remains_the_fallback_outside_the_builtin_matrix() {
	let source = "interface Power<Other, Output> { func power(other: Other): Output }\n\
		struct Base(value: int)\nstruct Exponent(value: int)\n\
		impl Power<Other = Exponent, Output = int> for Base {\n\
		  func power(other: Exponent): int = this.value + other.value\n}\n\
		func f(a: Base, b: Exponent): int = a ** b";
	let resolution = resolution_for(source, "f");
	assert_eq!(resolution.method, "power");
	assert_eq!(resolution.dispatch, DispatchKind::UserImpl);
}

// ---- Operator-resolution-failure diagnostic (#7) ----
// When an operator has no impl for its operands, the diagnostic names the operator
// symbol, BOTH operands, and the interface to implement — not the internal desugared
// method name and only the receiver type.

#[test]
fn missing_binary_operator_names_operator_operands_and_interface() {
	let parsed = parse_module(
		"struct A(n: int)\n struct B(n: int)\n func f(a: A, b: B): float = a ** b",
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
	assert_eq!(
		messages[0],
		"the `**` operator is not implemented for `A` and `B`; implement `Power` to support it"
	);
}

#[test]
fn missing_unary_operator_names_operator_operand_and_interface() {
	let parsed = parse_module("struct A(n: int)\n func f(a: A): A = -a", "test");
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
	assert_eq!(
		messages[0],
		"the `-` operator is not implemented for `A`; implement `Negate` to support it"
	);
}
