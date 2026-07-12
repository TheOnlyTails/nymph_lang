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
fn comparable_less_than_is_interface_default_method() {
	// `v1 < v2` desugars to `Comparable::less_than`, which `Vec2` never defines
	// directly — only `compare_to`. `less_than` is only reachable as the
	// interface's *default* body, so this resolves as `UserImplDefaultMethod`.
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
	assert_eq!(res.dispatch, DispatchKind::UserImplDefaultMethod);
	assert_eq!(res.method, "less_than");
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
