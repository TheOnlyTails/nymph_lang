//! Integration tests for the parser, exercising the refined Nymph syntax end to end
//! (lex → parse).

use nymph_ast::{
	Spanned,
	decl::Declaration,
	expr::{Expr, RangeKind},
	ops::BinaryOperator,
};
use nymph_syntax::{parse_expression, parse_module};

fn expr(src: &str) -> Spanned<Expr> {
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
	assert!(matches!(expr("42").0, Expr::Int(_)));
	assert!(matches!(expr("3.14").0, Expr::Float(_)));
	assert!(matches!(expr("true").0, Expr::Boolean(_)));
	assert!(matches!(expr("'a'").0, Expr::Char(_)));
	assert!(matches!(expr("foo").0, Expr::Identifier(_)));
	assert!(matches!(expr("this").0, Expr::This));
}

#[test]
fn arithmetic_precedence() {
	// 1 + 2 * 3 parses as 1 + (2 * 3)
	let Expr::BinaryOp { op, rhs, .. } = expr("1 + 2 * 3").0 else {
		panic!("expected a binary op");
	};
	assert_eq!(op, BinaryOperator::Plus);
	assert!(matches!(
		rhs.0,
		Expr::BinaryOp {
			op: BinaryOperator::Times,
			..
		}
	));
}

#[test]
fn power_is_right_associative() {
	// 2 ** 3 ** 2 parses as 2 ** (3 ** 2)
	let Expr::BinaryOp { op, rhs, .. } = expr("2 ** 3 ** 2").0 else {
		panic!("expected a binary op");
	};
	assert_eq!(op, BinaryOperator::Power);
	assert!(matches!(
		rhs.0,
		Expr::BinaryOp {
			op: BinaryOperator::Power,
			..
		}
	));
}

#[test]
fn shift_recombination() {
	let Expr::BinaryOp { op, .. } = expr("a << b").0 else {
		panic!("expected a binary op");
	};
	assert_eq!(op, BinaryOperator::LeftShift);
}

#[test]
fn collections_and_ranges() {
	assert!(matches!(expr("#[1, 2, 3]").0, Expr::List(_)));
	assert!(matches!(expr("#(1, true, 'a')").0, Expr::Tuple(_)));
	assert!(matches!(expr(r#"#{"a": 1}"#).0, Expr::Map(_)));
	assert!(matches!(
		expr("1..10").0,
		Expr::Range(RangeKind::Exclusive { .. })
	));
	assert!(matches!(
		expr("1..=10").0,
		Expr::Range(RangeKind::Inclusive { .. })
	));
	assert!(matches!(expr("1..").0, Expr::Range(RangeKind::From(_))));
	assert!(matches!(expr("..10").0, Expr::Range(RangeKind::To(_))));
	assert!(matches!(
		expr("..=10").0,
		Expr::Range(RangeKind::ToInclusive(_))
	));
}

#[test]
fn calls_and_member_access() {
	// foo.bar(1).baz chains member/call postfix operators.
	assert!(matches!(
		expr("foo.bar(1).baz").0,
		Expr::MemberAccess { .. }
	));
	assert!(matches!(
		expr("list?.first").0,
		Expr::MemberAccess { optional: true, .. }
	));
	assert!(matches!(expr("value?").0, Expr::PostfixOp { .. }));
}

#[test]
fn closures() {
	assert!(matches!(expr("x -> x + 1").0, Expr::Closure { .. }));
	assert!(matches!(expr("(a, b) -> a + b").0, Expr::Closure { .. }));
	assert!(matches!(expr("() -> 0").0, Expr::Closure { .. }));
	// A grouped expression, not a closure.
	assert!(matches!(expr("(1 + 2)").0, Expr::Grouped(_)));
}

#[test]
fn control_flow_expressions() {
	assert!(matches!(expr("if (x > 0) x else -x").0, Expr::If { .. }));
	assert!(matches!(
		expr("match (n) { 0 -> true, _ -> false }").0,
		Expr::Match { .. }
	));
	assert!(matches!(expr("{ let x = 1 x + 1 }").0, Expr::Block { .. }));
}

#[test]
fn string_interpolation() {
	let e = expr(r#""Hello, ${name}!""#);
	let Expr::String(parts) = e.0 else {
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
	assert!(matches!(expr("x is Some(value)").0, Expr::PatternOp { .. }));
	assert!(matches!(expr("x !is None").0, Expr::PatternOp { .. }));
	assert!(matches!(expr("x as float").0, Expr::TypeOp { .. }));
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
