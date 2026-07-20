//! Integration tests for the parser, exercising the refined Nymph syntax end to end
//! (lex → parse).

use nymph_ast::{
	decl::{Declaration, FuncDeclaration, FuncKind, ImplMember, LetKind},
	expr::{Expr, ExprKind, RangeKind},
	ops::BinaryOperator,
	ty::Type,
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
fn mut_type_parses_as_a_distinct_type_node() {
	// `mut T` — distinct from the pre-existing binding-position `mut` (`let
	// mut`, `mut` params), which is consumed before a type is ever parsed (see
	// `namespace_let_mut_is_diagnosed` etc. below for that unrelated form).
	let members = module_ok("func f(x: mut int): void = {}");
	let Declaration::Func { meta, .. } = &members[0] else {
		panic!("expected a func declaration");
	};
	let param_ty = &meta.params[0].0.type_.0;
	assert!(
		matches!(param_ty, Type::Mut(inner) if matches!(inner.0, Type::Int)),
		"expected `mut int`, got {param_ty:?}"
	);
}

#[test]
fn mut_type_nests_inside_a_list_type() {
	// `mut` binds at PRIMARY-type precedence, so `mut #[int]` wraps the whole
	// list (not just the first slot of some larger construct).
	let members = module_ok("func f(x: mut #[int]): void = {}");
	let Declaration::Func { meta, .. } = &members[0] else {
		panic!("expected a func declaration");
	};
	let param_ty = &meta.params[0].0.type_.0;
	let Type::Mut(inner) = param_ty else {
		panic!("expected `mut #[int]`, got {param_ty:?}");
	};
	assert!(
		matches!(&inner.0, Type::List(elem) if matches!(elem.0, Type::Int)),
		"expected the `mut` to wrap the whole list, got {:?}",
		inner.0
	);
}

#[test]
fn mut_type_and_binding_position_mut_compose() {
	// `func f(mut x: mut T)`: the first `mut` (binding position, consumed
	// before the `:`) sets `FuncParam.mutable`; the second (type position,
	// after the `:`) is `Type::Mut` — no ambiguity between the two forms.
	let members = module_ok("func f(mut x: mut int): void = {}");
	let Declaration::Func { meta, .. } = &members[0] else {
		panic!("expected a func declaration");
	};
	let param = &meta.params[0].0;
	assert!(
		param.mutable,
		"expected the binding-position `mut` to be set"
	);
	assert!(
		matches!(param.type_.0, Type::Mut(_)),
		"expected the type-position `mut`, got {:?}",
		param.type_.0
	);
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

#[test]
fn import_bare() {
	let members = module_ok("import @/math");
	let Declaration::Import {
		path,
		alias,
		idents,
		..
	} = &members[0]
	else {
		panic!("expected import");
	};
	assert_eq!(path.len(), 1);
	assert_eq!(path[0].0, "math");
	assert!(alias.is_none());
	assert!(idents.is_none());
}

#[test]
fn import_with_alias() {
	let members = module_ok("import @/math as m");
	let Declaration::Import {
		path,
		alias,
		idents,
		..
	} = &members[0]
	else {
		panic!("expected import");
	};
	assert_eq!(path.len(), 1);
	assert_eq!(alias.as_ref().unwrap().0, "m");
	assert!(idents.is_none());
}

#[test]
fn import_with_idents_only() {
	let members = module_ok("import @/math with (sin, cos as c)");
	let Declaration::Import { alias, idents, .. } = &members[0] else {
		panic!("expected import");
	};
	assert!(alias.is_none());
	let idents = idents.as_ref().unwrap();
	assert_eq!(idents.len(), 2);
	assert_eq!(idents[0].0.0, "sin");
	assert!(idents[0].1.is_none());
	assert_eq!(idents[1].0.0, "cos");
	assert_eq!(idents[1].1.as_ref().unwrap().0, "c");
}

#[test]
fn import_alias_and_idents_combined() {
	let members = module_ok("import @/math as m with (sin as sine, cos)");
	let Declaration::Import {
		path,
		alias,
		idents,
		..
	} = &members[0]
	else {
		panic!("expected import");
	};
	assert_eq!(path.len(), 1);
	assert_eq!(alias.as_ref().unwrap().0, "m");
	let idents = idents.as_ref().unwrap();
	assert_eq!(idents.len(), 2);
	assert_eq!(idents[0].1.as_ref().unwrap().0, "sine");
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

// ── `mut func` / `namespace func` kinds and `namespace Name { … }` ──────────

/// Every member `func` kind that a struct/enum body carries, in order.
fn struct_member_kinds(decl: &Declaration) -> Vec<FuncKind> {
	let Declaration::Struct { members, .. } = decl else {
		panic!("expected a struct, got {decl:?}");
	};
	members
		.iter()
		.filter_map(|m| match &m.0 {
			ImplMember::Func { meta, .. } => Some(meta.kind.clone()),
			_ => None,
		})
		.collect()
}

#[test]
fn struct_body_carries_func_kinds_and_splits_out_impls() {
	// `func` / `mut func` / `namespace func` all land in the flat `members`
	// list with the right kind; a nested `impl` lands in the separate `impls`
	// list, not among the members.
	let members = module_ok(
		"struct Counter(n: int) {
		   func get(): int = this.n
		   mut func bump(): void = { this.n = this.n + 1 }
		   namespace func zero(): Counter = Counter(n = 0)
		   impl Default { func default(): Counter = Counter(n = 0) }
		 }",
	);
	assert_eq!(
		struct_member_kinds(&members[0]),
		vec![FuncKind::Instance, FuncKind::Mut, FuncKind::Namespace],
	);
	let Declaration::Struct { impls, .. } = &members[0] else {
		unreachable!();
	};
	assert_eq!(impls.len(), 1, "the nested impl is not a flat member");
}

#[test]
fn top_level_namespace_block_still_parses() {
	// A named `namespace Name { … }` remains a top-level declaration, holding
	// regular funcs.
	let members = module_ok("namespace Math { func double(x: int): int = x * 2 }");
	assert!(
		matches!(&members[0], Declaration::Namespace { name, .. } if name.0 == "Math"),
		"expected a namespace declaration, got {:?}",
		members[0],
	);
}

#[test]
fn top_level_mut_func_is_diagnosed() {
	let result = parse_module("mut func f(): int = 1", "test");
	assert!(
		result
			.diagnostics
			.iter()
			.any(|d| d.message.contains("only valid inside")),
		"expected an outside-a-type diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn top_level_namespace_func_is_diagnosed() {
	let result = parse_module("namespace func f(): int = 1", "test");
	assert!(
		result
			.diagnostics
			.iter()
			.any(|d| d.message.contains("only valid inside")),
		"expected an outside-a-type diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn namespace_block_rejects_a_mut_func() {
	// A namespace has no `this` to mutate, so a `mut func` inside one is an error.
	let result = parse_module("namespace N { mut func f(): int = 1 }", "test");
	assert!(
		result
			.diagnostics
			.iter()
			.any(|d| d.message.contains("only contain regular")),
		"expected a namespace-only-regular-funcs diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn external_funcs_carry_their_kind() {
	let kind_of = |src: &str| -> FuncKind {
		let members = module_ok(src);
		match &members[0] {
			Declaration::ExternalFunc(_, _, FuncDeclaration { kind, .. }) => kind.clone(),
			other => panic!("expected an external func, got {other:?}"),
		}
	};
	assert_eq!(kind_of("external(js_f) func f(): int"), FuncKind::Instance);
	assert_eq!(kind_of("external(js_g) mut func g(): int"), FuncKind::Mut);
	assert_eq!(
		kind_of("external(js_h) namespace func h(): int"),
		FuncKind::Namespace
	);
}

#[test]
fn namespace_let_is_a_static_member_binding() {
	let members = module_ok(
		"struct Config(v: int) {
		   namespace let DEFAULT_V: int = 42
		   let plain: int = 1
		 }",
	);
	let Declaration::Struct { members, .. } = &members[0] else {
		panic!("expected a struct, got {:?}", members[0]);
	};
	let kinds: Vec<LetKind> = members
		.iter()
		.filter_map(|m| match &m.0 {
			ImplMember::Let { meta, .. } => Some(meta.kind),
			_ => None,
		})
		.collect();
	assert_eq!(kinds, vec![LetKind::Namespace, LetKind::Instance]);
}

#[test]
fn namespace_let_mut_is_diagnosed() {
	// A static binding can never be mutable — `namespace let mut` is rejected.
	let result = parse_module("struct S(v: int) { namespace let mut x: int = 1 }", "test");
	assert!(
		result
			.diagnostics
			.iter()
			.any(|d| d.message.contains("cannot be `mut`")),
		"expected a mutable-namespace-let diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn top_level_namespace_let_is_diagnosed() {
	let result = parse_module("namespace let x: int = 1", "test");
	assert!(
		result
			.diagnostics
			.iter()
			.any(|d| d.message.contains("only valid inside")),
		"expected an outside-a-type diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn namespace_block_rejects_a_namespace_let() {
	let result = parse_module("namespace N { namespace let x: int = 1 }", "test");
	assert!(
		result
			.diagnostics
			.iter()
			.any(|d| d.message.contains("only contain regular")),
		"expected a namespace-only-regular diagnostic, got {:?}",
		result.diagnostics,
	);
}

// ── `mut` as a struct/enum-variant field modifier is rejected with one clean
// diagnostic (field mutability is expressed on the field's type, `n: mut int`,
// not as a modifier on the field itself) — never the old cascade of spurious
// "cannot find type"/"no field" errors. ──────────────────────────────────────

#[test]
fn struct_field_mut_modifier_is_one_clean_diagnostic() {
	let result = parse_module("struct P(mut n: int) {}", "test");
	let errors: Vec<_> = result.diagnostics.iter().filter(|d| d.is_error()).collect();
	assert_eq!(
		errors.len(),
		1,
		"expected exactly one diagnostic, got {:?}",
		result.diagnostics,
	);
	assert!(
		errors[0].message.contains("not as a `mut` field modifier"),
		"expected a mut-field-modifier diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn enum_variant_field_mut_modifier_is_one_clean_diagnostic() {
	let result = parse_module("enum E { V(mut n: int) }", "test");
	let errors: Vec<_> = result.diagnostics.iter().filter(|d| d.is_error()).collect();
	assert_eq!(
		errors.len(),
		1,
		"expected exactly one diagnostic, got {:?}",
		result.diagnostics,
	);
	assert!(
		errors[0].message.contains("not as a `mut` field modifier"),
		"expected a mut-field-modifier diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn pub_mut_struct_field_is_one_clean_diagnostic() {
	let result = parse_module("struct P(public mut n: int) {}", "test");
	let errors: Vec<_> = result.diagnostics.iter().filter(|d| d.is_error()).collect();
	assert_eq!(
		errors.len(),
		1,
		"expected exactly one diagnostic, got {:?}",
		result.diagnostics,
	);
	assert!(
		errors[0].message.contains("not as a `mut` field modifier"),
		"expected a mut-field-modifier diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn struct_field_type_mut_is_the_working_spelling() {
	// The type-position spelling (`n: mut int`) is the actual mechanism field
	// mutability is expressed through, and parses cleanly.
	module_ok("struct P(n: mut int) {}");
}
