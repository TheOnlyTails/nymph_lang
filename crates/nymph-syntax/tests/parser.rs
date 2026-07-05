//! Integration tests for the parser, exercising the refined Nymph syntax end to end
//! (lex → parse).

use nymph_ast::{
	decl::Declaration,
	expr::{Expr, ExprKind, RangeKind},
	ops::BinaryOperator,
};
use nymph_syntax::{parse_expression, parse_module};

fn expr(src: &str) -> Expr {
	let result = parse_expression(src);
	assert!(
		result.diagnostics.is_empty(),
		"unexpected diagnostics for {src:?}: {:?}",
		result.diagnostics
	);
	result.tree
}

fn module_ok(src: &str) -> Vec<Declaration> {
	let result = parse_module(src, "test");
	assert!(
		result.diagnostics.is_empty(),
		"unexpected diagnostics: {:?}",
		result.diagnostics
	);
	result.tree.members
}

#[test]
fn literals_and_identifiers() {
	assert!(matches!(expr("42").kind, ExprKind::Int(_)));
	assert!(matches!(expr("3.14").kind, ExprKind::Float(_)));
	assert!(matches!(expr("true").kind, ExprKind::Boolean(_)));
	assert!(matches!(expr("'a'").kind, ExprKind::Char(_)));
	assert!(matches!(expr("foo").kind, ExprKind::Identifier(_)));
	assert!(matches!(expr("this").kind, ExprKind::This));
}

#[test]
fn arithmetic_precedence() {
	// 1 + 2 * 3 parses as 1 + (2 * 3)
	let ExprKind::BinaryOp { op, rhs, .. } = expr("1 + 2 * 3").kind else {
		panic!("expected a binary op");
	};
	assert_eq!(op, BinaryOperator::Plus);
	assert!(matches!(
		rhs.kind,
		ExprKind::BinaryOp {
			op: BinaryOperator::Times,
			..
		}
	));
}

#[test]
fn power_is_right_associative() {
	// 2 ** 3 ** 2 parses as 2 ** (3 ** 2)
	let ExprKind::BinaryOp { op, rhs, .. } = expr("2 ** 3 ** 2").kind else {
		panic!("expected a binary op");
	};
	assert_eq!(op, BinaryOperator::Power);
	assert!(matches!(
		rhs.kind,
		ExprKind::BinaryOp {
			op: BinaryOperator::Power,
			..
		}
	));
}

#[test]
fn shift_recombination() {
	let ExprKind::BinaryOp { op, .. } = expr("a << b").kind else {
		panic!("expected a binary op");
	};
	assert_eq!(op, BinaryOperator::LeftShift);
}

#[test]
fn collections_and_ranges() {
	assert!(matches!(expr("#[1, 2, 3]").kind, ExprKind::List(_)));
	assert!(matches!(expr("#(1, true, 'a')").kind, ExprKind::Tuple(_)));
	assert!(matches!(expr(r#"#{"a": 1}"#).kind, ExprKind::Map(_)));
	assert!(matches!(
		expr("1..10").kind,
		ExprKind::Range(RangeKind::Exclusive { .. })
	));
	assert!(matches!(
		expr("1..=10").kind,
		ExprKind::Range(RangeKind::Inclusive { .. })
	));
	assert!(matches!(
		expr("1..").kind,
		ExprKind::Range(RangeKind::From(_))
	));
	assert!(matches!(
		expr("..10").kind,
		ExprKind::Range(RangeKind::To(_))
	));
	assert!(matches!(
		expr("..=10").kind,
		ExprKind::Range(RangeKind::ToInclusive(_))
	));
}

#[test]
fn calls_and_member_access() {
	// foo.bar(1).baz chains member/call postfix operators.
	assert!(matches!(
		expr("foo.bar(1).baz").kind,
		ExprKind::MemberAccess { .. }
	));
	assert!(matches!(
		expr("list?.first").kind,
		ExprKind::MemberAccess { optional: true, .. }
	));
	assert!(matches!(expr("value?").kind, ExprKind::PostfixOp { .. }));
}

#[test]
fn closures() {
	assert!(matches!(expr("x -> x + 1").kind, ExprKind::Closure { .. }));
	assert!(matches!(
		expr("(a, b) -> a + b").kind,
		ExprKind::Closure { .. }
	));
	assert!(matches!(expr("() -> 0").kind, ExprKind::Closure { .. }));
	// A grouped expression, not a closure.
	assert!(matches!(expr("(1 + 2)").kind, ExprKind::Grouped(_)));
}

#[test]
fn control_flow_expressions() {
	assert!(matches!(
		expr("if (x > 0) x else -x").kind,
		ExprKind::If { .. }
	));
	assert!(matches!(
		expr("match (n) { 0 -> true, _ -> false }").kind,
		ExprKind::Match { .. }
	));
	assert!(matches!(
		expr("{ let x = 1 x + 1 }").kind,
		ExprKind::Block { .. }
	));
}

#[test]
fn string_interpolation() {
	let e = expr(r#""Hello, ${name}!""#);
	let ExprKind::String(parts) = e.kind else {
		panic!("expected string");
	};
	assert_eq!(parts.len(), 3);
	assert!(matches!(
		parts[1].0,
		nymph_ast::expr::StringPart::InterpolatedExpr(_)
	));
}

#[test]
fn pattern_operators() {
	assert!(matches!(
		expr("x is Some(value)").kind,
		ExprKind::PatternOp { .. }
	));
	assert!(matches!(
		expr("x !is None").kind,
		ExprKind::PatternOp { .. }
	));
	assert!(matches!(expr("x as float").kind, ExprKind::TypeOp { .. }));
}

#[test]
fn let_and_func_declarations() {
	let members = module_ok("let x = 1\nfunc add(a: int, b: int): int = a + b");
	assert_eq!(members.len(), 2);
	assert!(matches!(members[0], Declaration::Let { .. }));
	assert!(matches!(members[1], Declaration::Func { .. }));
}

#[test]
fn func_with_block_body() {
	// Block body is just `= { ... }` since blocks are expressions.
	let members = module_ok("func main(): void = {\n  let x = 1\n  x\n}");
	assert!(matches!(members[0], Declaration::Func { .. }));
}

#[test]
fn struct_declaration() {
	let members = module_ok(
		"public struct Person(name: string, age: int) {\n  func is_minor(): boolean = this.age < 18\n}",
	);
	assert!(matches!(members[0], Declaration::Struct { .. }));
}

#[test]
fn enum_declaration() {
	let members = module_ok(
		"public enum Option<T> {\n  Some(value: T),\n  None\n\n  func is_some(): boolean = match (this) { Some(...) -> true, None -> false }\n}",
	);
	let Declaration::Enum {
		variants,
		members: enum_members,
		..
	} = &members[0]
	else {
		panic!("expected enum");
	};
	assert_eq!(variants.len(), 2);
	assert_eq!(enum_members.len(), 1);
}

#[test]
fn interface_and_impl() {
	let members = module_ok(
		"interface Plus<Other, Output> {\n  func plus(other: Other): Output\n}\n\nimpl Plus<Other = int, Output = int> for int {\n  external func plus(other: int): int\n}",
	);
	assert!(matches!(members[0], Declaration::Interface { .. }));
	assert!(matches!(members[1], Declaration::ImplFor { .. }));
}

#[test]
fn import_declaration() {
	let members = module_ok("import @/math with (sin as sine, cos)");
	let Declaration::Import { path, idents, .. } = &members[0] else {
		panic!("expected import");
	};
	assert_eq!(path.len(), 1);
	assert_eq!(idents.as_ref().unwrap().len(), 2);
}

/// Recursively push every expression node id reachable from `expr`. Kept minimal:
/// it only needs to reach the nodes produced by
/// `expression_node_ids_are_unique_and_dense`'s source (binary ops, a call, args,
/// literals), not exhaustively every `ExprKind` variant.
fn collect_expr_ids(expr: &Expr, out: &mut Vec<u32>) {
	out.push(expr.id.0);
	match &expr.kind {
		ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
			collect_expr_ids(lhs, out);
			collect_expr_ids(rhs, out);
		}
		ExprKind::Call { func, args, .. } => {
			collect_expr_ids(func, out);
			for arg in args {
				collect_expr_ids(&arg.0.value, out);
			}
		}
		ExprKind::PrefixOp { value, .. }
		| ExprKind::PostfixOp { value, .. }
		| ExprKind::Grouped(value) => {
			collect_expr_ids(value, out);
		}
		_ => {}
	}
}

/// Push every expression node id reachable from a declaration's body/value.
fn collect_decl_expr_ids(decl: &Declaration, out: &mut Vec<u32>) {
	match decl {
		Declaration::Func { body, .. } => collect_expr_ids(body, out),
		Declaration::Let { value, .. } => collect_expr_ids(value, out),
		_ => {}
	}
}

#[test]
fn expression_node_ids_are_unique_and_dense() {
	// A body with several nested expressions: binary ops, a call, a literal.
	let src = "func f() = 1 + g(2) * 3";
	let members = module_ok(src);

	let mut ids = Vec::new();
	for member in &members {
		collect_decl_expr_ids(member, &mut ids);
	}

	let mut sorted = ids.clone();
	sorted.sort_unstable();
	sorted.dedup();
	assert_eq!(sorted.len(), ids.len(), "node ids must be unique");
	assert_ne!(ids.len(), 0, "expected several expression nodes");
	// Dense from 0: the parser numbers in construction order starting at 0.
	assert_eq!(*sorted.first().unwrap(), 0);
	assert_eq!(*sorted.last().unwrap(), (ids.len() as u32) - 1);
}
