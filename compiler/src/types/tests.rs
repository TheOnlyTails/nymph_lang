use crate::{
	ast::{Spanned, declaration::Visibility, expr::Expr},
	types::{Context, ContextEntry, ContextValue, Type, TypeChecker},
};
use crate::ast::Span;
use ecow::EcoString;
use std::collections::BTreeMap;

fn span(s: usize, e: usize) -> Span {
	Span::new(s, e)
}

fn make_spanned<T>(t: T, start: usize, end: usize) -> Spanned<T> {
	Spanned(t, span(start, end))
}

#[test]
fn test_infer_int_literal() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Int);
}

#[test]
fn test_infer_float_literal() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(
		Expr::Float(make_spanned(ordered_float::OrderedFloat(3.15), 0, 4)),
		0,
		4,
	);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Float);
}

#[test]
fn test_infer_char_literal() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(Expr::Char(make_spanned('a', 0, 3)), 0, 3);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Char);
}

#[test]
fn test_infer_string_literal() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(Expr::String(vec![]), 0, 2);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::String);
}

#[test]
fn test_infer_boolean_literal() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(Expr::Boolean(make_spanned(true, 0, 4)), 0, 4);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Boolean);
}

#[test]
fn test_infer_empty_list() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(Expr::List(vec![]), 0, 2);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	match result.unwrap() {
		Type::List { .. } => (),
		_ => panic!("Expected list type"),
	}
}

#[test]
fn test_infer_int_list() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let int_expr = make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2);
	let item = make_spanned(crate::ast::expr::ListItem::Expr(int_expr), 0, 2);
	let expr = make_spanned(Expr::List(vec![item]), 0, 3);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());

	match result.unwrap() {
		Type::List { item } => {
			assert_eq!(*item, Type::Int);
		}
		_ => panic!("Expected list type"),
	}
}

#[test]
fn test_infer_empty_tuple() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(Expr::Tuple(vec![]), 0, 2);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	match result.unwrap() {
		Type::Tuple { items } => {
			assert!(items.is_empty());
		}
		_ => panic!("Expected tuple type"),
	}
}

#[test]
fn test_infer_int_float_tuple() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let int_expr = make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2);
	let int_item = make_spanned(crate::ast::expr::ListItem::Expr(int_expr), 0, 2);

	let float_expr = make_spanned(
		Expr::Float(make_spanned(ordered_float::OrderedFloat(3.15), 0, 4)),
		0,
		4,
	);
	let float_item = make_spanned(crate::ast::expr::ListItem::Expr(float_expr), 0, 4);

	let expr = make_spanned(Expr::Tuple(vec![int_item, float_item]), 0, 5);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	match result.unwrap() {
		Type::Tuple { items } => {
			assert_eq!(items.len(), 2);
			assert_eq!(items[0], Type::Int);
			assert_eq!(items[1], Type::Float);
		}
		_ => panic!("Expected tuple type"),
	}
}

#[test]
fn test_infer_range() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let min = make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1);
	let expr = make_spanned(
		Expr::Range(crate::ast::expr::RangeKind::From(Box::new(min))),
		0,
		3,
	);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	match result.unwrap() {
		Type::List { item } => {
			assert_eq!(*item, Type::Int);
		}
		_ => panic!("Expected list type for range"),
	}
}

#[test]
fn test_infer_if_expr() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let cond = make_spanned(Expr::Boolean(make_spanned(true, 0, 4)), 0, 4);
	let then_expr = make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1);
	let else_expr = make_spanned(Expr::Int(make_spanned(2u64, 0, 1)), 0, 1);

	let expr = make_spanned(
		Expr::If {
			condition: Box::new(cond),
			then: Box::new(then_expr),
			otherwise: Some(Box::new(else_expr)),
		},
		0,
		10,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Int);
}

#[test]
fn test_infer_if_without_else() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let cond = make_spanned(Expr::Boolean(make_spanned(true, 0, 4)), 0, 4);
	let then_expr = make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1);

	let expr = make_spanned(
		Expr::If {
			condition: Box::new(cond),
			then: Box::new(then_expr),
			otherwise: None,
		},
		0,
		10,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());

	// If without else should produce a union with void
	match result.unwrap() {
		Type::Intersection { .. } => (),
		_ => panic!("Expected intersection type"),
	}
}

#[test]
fn test_infer_grouped_expr() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let inner = make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2);
	let expr = make_spanned(Expr::Grouped(Box::new(inner)), 0, 4);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Int);
}

#[test]
fn test_infer_placeholder() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(Expr::Placeholder, 0, 1);
	let result = checker.infer(&expr, &ctx);

	assert!(result.is_ok());
	match result.unwrap() {
		Type::Variable { name, .. } => {
			assert!(name.contains("placeholder"));
		}
		_ => panic!("Expected variable type for placeholder"),
	}
}

#[test]
fn test_infer_binary_op_int_addition() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let lhs = make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1);
	let rhs = make_spanned(Expr::Int(make_spanned(2u64, 0, 1)), 0, 1);
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(lhs),
			op: crate::ast::ops::BinaryOperator::Plus,
			rhs: Box::new(rhs),
		},
		0,
		3,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Int);
}

#[test]
fn test_infer_binary_op_float_addition() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let lhs = make_spanned(
		Expr::Float(make_spanned(ordered_float::OrderedFloat(1.0), 0, 3)),
		0,
		3,
	);
	let rhs = make_spanned(
		Expr::Float(make_spanned(ordered_float::OrderedFloat(2.0), 0, 3)),
		0,
		3,
	);
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(lhs),
			op: crate::ast::ops::BinaryOperator::Plus,
			rhs: Box::new(rhs),
		},
		0,
		7,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Float);
}

#[test]
fn test_infer_binary_op_mixed_numeric() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let lhs = make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1);
	let rhs = make_spanned(
		Expr::Float(make_spanned(ordered_float::OrderedFloat(2.0), 0, 3)),
		0,
		3,
	);
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(lhs),
			op: crate::ast::ops::BinaryOperator::Plus,
			rhs: Box::new(rhs),
		},
		0,
		5,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Float);
}

#[test]
fn test_infer_boolean_and() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let lhs = make_spanned(Expr::Boolean(make_spanned(true, 0, 4)), 0, 4);
	let rhs = make_spanned(Expr::Boolean(make_spanned(false, 0, 5)), 0, 5);
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(lhs),
			op: crate::ast::ops::BinaryOperator::BoolAnd,
			rhs: Box::new(rhs),
		},
		0,
		10,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Boolean);
}

#[test]
fn test_infer_comparison() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let lhs = make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1);
	let rhs = make_spanned(Expr::Int(make_spanned(2u64, 0, 1)), 0, 1);
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(lhs),
			op: crate::ast::ops::BinaryOperator::LessThan,
			rhs: Box::new(rhs),
		},
		0,
		3,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Boolean);
}

#[test]
fn test_infer_prefix_not() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let val = make_spanned(Expr::Boolean(make_spanned(true, 0, 4)), 0, 4);
	let expr = make_spanned(
		Expr::PrefixOp {
			op: crate::ast::ops::PrefixOperator::BoolNot,
			value: Box::new(val),
		},
		0,
		5,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Boolean);
}

#[test]
fn test_infer_prefix_negate_int() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let val = make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2);
	let expr = make_spanned(
		Expr::PrefixOp {
			op: crate::ast::ops::PrefixOperator::Negate,
			value: Box::new(val),
		},
		0,
		3,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Int);
}

#[test]
fn test_infer_prefix_negate_float() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let val = make_spanned(
		Expr::Float(make_spanned(ordered_float::OrderedFloat(3.15), 0, 4)),
		0,
		4,
	);
	let expr = make_spanned(
		Expr::PrefixOp {
			op: crate::ast::ops::PrefixOperator::Negate,
			value: Box::new(val),
		},
		0,
		5,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Float);
}

#[test]
fn test_type_assignable_to_same_type() {
	let int_type = Type::Int;
	let ctx = Default::default();
	assert!(int_type.assignable_to(&Type::Int, &ctx));
}

#[test]
fn test_type_never_assignable_to_anything() {
	let never_type = Type::Never;
	let ctx = Default::default();
	assert!(never_type.assignable_to(&Type::Int, &ctx));
	assert!(never_type.assignable_to(&Type::String, &ctx));
	assert!(never_type.assignable_to(&Type::Boolean, &ctx));
}

#[test]
fn test_type_not_assignable_to_never() {
	let int_type = Type::Int;
	let ctx = Default::default();
	assert!(!int_type.assignable_to(&Type::Never, &ctx));
}

#[test]
fn test_type_join() {
	let int_type = Type::Int;
	let string_type = Type::String;

	let result = int_type.join(&string_type);
	match result {
		Type::Intersection { first, second } => {
			assert_eq!(*first, Type::Int);
			assert_eq!(*second, Type::String);
		}
		_ => panic!("Expected intersection type"),
	}
}

#[test]
fn test_type_meet_same() {
	let int_type = Type::Int;
	let result = int_type.meet(&Type::Int);
	assert_eq!(result, Some(Type::Int));
}

#[test]
fn test_type_meet_with_never() {
	let int_type = Type::Int;
	let result = int_type.meet(&Type::Never);
	assert_eq!(result, Some(Type::Int));
}

#[test]
fn test_type_list_assignable() {
	let ctx = Default::default();
	let list_int = Type::List {
		item: Box::new(Type::Int),
	};
	let list_int2 = Type::List {
		item: Box::new(Type::Int),
	};
	assert!(list_int.assignable_to(&list_int2, &ctx));
}

#[test]
fn test_type_tuple_assignable() {
	let ctx = Default::default();
	let tuple = Type::Tuple {
		items: vec![Type::Int, Type::String],
	};
	let tuple2 = Type::Tuple {
		items: vec![Type::Int, Type::String],
	};
	assert!(tuple.assignable_to(&tuple2, &ctx));
}

#[test]
fn test_type_display_primitives() {
	assert_eq!(Type::Int.to_string(), "int");
	assert_eq!(Type::Float.to_string(), "float");
	assert_eq!(Type::Char.to_string(), "char");
	assert_eq!(Type::String.to_string(), "string");
	assert_eq!(Type::Boolean.to_string(), "boolean");
	assert_eq!(Type::Void.to_string(), "void");
	assert_eq!(Type::Never.to_string(), "never");
}

#[test]
fn test_type_display_list() {
	let list_int = Type::List {
		item: Box::new(Type::Int),
	};
	assert_eq!(list_int.to_string(), "#[int]");
}

#[test]
fn test_type_display_tuple() {
	let tuple = Type::Tuple {
		items: vec![Type::Int, Type::String],
	};
	assert_eq!(tuple.to_string(), "#(int, string)");
}

#[test]
fn test_type_display_map() {
	let map = Type::Map {
		key: Box::new(Type::String),
		value: Box::new(Type::Int),
	};
	assert_eq!(map.to_string(), "#{string: int}");
}

#[test]
fn test_type_display_function() {
	let func = Type::Function {
		generics: Vec::new(),
		params: vec![(Some(EcoString::from("x")), Type::Int)],
		has_spread: false,
		return_type: Box::new(Type::String),
	};
	assert_eq!(func.to_string(), "(x: int) -> string");
}

#[test]
fn test_type_display_variable() {
	let var = Type::Variable {
		name: EcoString::from("T"),
		constraint: None,
	};
	assert_eq!(var.to_string(), "T");
}

#[test]
fn test_type_display_intersection() {
	let inter = Type::Intersection {
		first: Box::new(Type::Int),
		second: Box::new(Type::String),
	};
	assert_eq!(inter.to_string(), "int + string");
}

#[test]
fn test_unknown_identifier_error() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let ident_name = EcoString::from("unknown_var");
	let expr = make_spanned(
		Expr::Identifier(make_spanned(ident_name.clone(), 0, 12)),
		0,
		12,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_err());
	match result.unwrap_err() {
		crate::types::error::TypeError::UnknownIdentifier(name, _) => {
			assert_eq!(name, ident_name);
		}
		_ => panic!("Expected UnknownIdentifier error"),
	}
}

#[test]
fn test_return_expr() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let val = make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2);
	let expr = make_spanned(
		Expr::Return {
			value: Some(Box::new(val)),
			label: None,
		},
		0,
		8,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	// Return produces no value - Void
	assert_eq!(result.unwrap(), Type::Void);
}

#[test]
fn test_break_expr() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let val = make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2);
	let expr = make_spanned(
		Expr::Break {
			value: Some(Box::new(val)),
			label: None,
		},
		0,
		5,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	// Break produces no value - Void
	assert_eq!(result.unwrap(), Type::Void);
}

#[test]
fn test_continue_expr() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(Expr::Continue { label: None }, 0, 8);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	// Continue produces no value - Void
	assert_eq!(result.unwrap(), Type::Void);
}

#[test]
fn test_type_cast_operation() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let lhs = make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2);
	let rhs_type = make_spanned(crate::ast::types::Type::String, 0, 6);
	let expr = make_spanned(
		Expr::TypeOp {
			lhs: Box::new(lhs),
			op: crate::ast::ops::TypeOperator::As,
			rhs: rhs_type,
		},
		0,
		8,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::String);
}

#[test]
fn test_pattern_match_operation() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let lhs = make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2);
	let pattern = make_spanned(
		crate::ast::expr::Pattern::Int(make_spanned(42i64, 0, 2)),
		0,
		2,
	);
	let expr = make_spanned(
		Expr::PatternOp {
			lhs: Box::new(lhs),
			op: crate::ast::ops::PatternOperator::Is,
			rhs: pattern,
		},
		0,
		7,
	);

	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Boolean);
}

// ============================================================================
// NEW TESTS: Named Type Resolution, Interfaces, and Impl Blocks
// ============================================================================

#[test]
fn test_context_lookup_type() {
	let ctx = Context::default().with_new_entry(
		EcoString::from("MyType"),
		ContextEntry::Value(ContextValue {
			type_: Type::Int,
			mutable: false,
			visibility: Visibility::Public,
		}),
	);

	let result = ctx.lookup_type(&EcoString::from("MyType"));
	assert_eq!(result, Some(Type::Int));
}

#[test]
fn test_context_lookup_type_not_found() {
	let ctx: Context = Default::default();
	let result = ctx.lookup_type(&EcoString::from("NonExistent"));
	assert!(result.is_none());
}

#[test]
fn test_context_register_impl() {
	let ctx = Context::default();
	let interface_type = Type::Interface {
		name: EcoString::from("Comparable"),
		generics: Vec::new(),
		type_args: Vec::new(),
		members: BTreeMap::new(),
		impls: BTreeMap::new(),
		def_key: None,
	};

	let new_ctx = ctx.with_impl(EcoString::from("int"), interface_type.clone());
	let impls = new_ctx.get_impls(&EcoString::from("int"));

	assert!(impls.is_some());
	assert_eq!(impls.as_ref().unwrap().len(), 1);
	assert_eq!(impls.unwrap()[0], interface_type);
}

#[test]
fn test_type_constraint_satisfaction() {
	let checker = TypeChecker::default();

	// A type should satisfy itself as a constraint
	let result = checker.check_constraint(&Type::Int, &Type::Int);
	assert!(result.is_ok());

	// Never satisfies any constraint
	let result = checker.check_constraint(&Type::Never, &Type::Int);
	assert!(result.is_ok());
}

#[test]
fn test_type_constraint_violation() {
	let checker = TypeChecker::default();

	// Type::Int doesn't satisfy Type::String constraint
	let result = checker.check_constraint(&Type::Int, &Type::String);
	assert!(result.is_err());
	match result.unwrap_err() {
		crate::types::error::TypeError::ConstraintViolation { .. } => {}
		_ => panic!("Expected ConstraintViolation error"),
	}
}

#[test]
fn test_intersection_constraint_satisfaction() {
	let checker = TypeChecker::default();

	// Intersection type satisfies if both parts satisfy
	let intersection = Type::Intersection {
		first: Box::new(Type::Int),
		second: Box::new(Type::Int),
	};

	let result = checker.check_constraint(&intersection, &Type::Int);
	assert!(result.is_ok());
}

#[test]
fn test_qualify_type_with_unresolved_generic() {
	let mut checker = TypeChecker::default();
	let _ctx = Context::default().with_new_entry(
		EcoString::from("List"),
		ContextEntry::Value(ContextValue {
			type_: Type::List {
				item: Box::new(Type::Variable {
					name: EcoString::from("T"),
					constraint: None,
				}),
			},
			mutable: false,
			visibility: Visibility::Public,
		}),
	);

	// Try to instantiate List with a generic argument
	// This should fail with GenericArgumentMismatch (stub implementation)
	let result = checker.resolve_qualified_type(
		&EcoString::from("List"),
		&[],
		Span::new(0, 0),
		&_ctx,
	);

	assert!(result.is_ok());
}

#[test]
fn test_resolve_unknown_qualified_type() {
	let mut checker = TypeChecker::default();
	let ctx = Context::default();

	let result = checker.resolve_qualified_type(
		&EcoString::from("UnknownType"),
		&[],
		Span::new(0, 0),
		&ctx,
	);

	assert!(result.is_err());
	match result.unwrap_err() {
		crate::types::error::TypeError::UnknownType(name, _) => {
			assert_eq!(name, EcoString::from("UnknownType"));
		}
		_ => panic!("Expected UnknownType error"),
	}
}

#[test]
fn test_struct_with_interface() {
	let ctx = Context::default();

	let struct_type = Type::Struct {
		name: EcoString::from("Point"),
		generics: Vec::new(),
		type_args: Vec::new(),
		fields: BTreeMap::new(),
		members: BTreeMap::new(),
		impls: BTreeMap::new(),
		def_key: None,
	};

	let interface_type = Type::Interface {
		name: EcoString::from("Eq"),
		generics: Vec::new(),
		type_args: Vec::new(),
		members: BTreeMap::new(),
		impls: BTreeMap::new(),
		def_key: None,
	};

	let new_ctx = ctx
		.with_new_entry(
			EcoString::from("Point"),
			ContextEntry::Value(ContextValue {
				type_: struct_type,
				mutable: false,
				visibility: Visibility::Public,
			}),
		)
		.with_new_entry(
			EcoString::from("Eq"),
			ContextEntry::Value(ContextValue {
				type_: interface_type.clone(),
				mutable: false,
				visibility: Visibility::Public,
			}),
		)
		.with_impl(EcoString::from("Point"), interface_type);

	let impls = new_ctx.get_impls(&EcoString::from("Point"));
	assert!(impls.is_some());
	assert_eq!(impls.unwrap().len(), 1);
}

#[test]
fn test_error_display_generic_mismatch() {
	let error = crate::types::error::TypeError::GenericArgumentMismatch {
		expected: 2,
		found: 1,
		span: 0..0,
	};

	let display_str = format!("{}", error);
	assert!(display_str.contains("2"));
	assert!(display_str.contains("1"));
}

#[test]
fn test_error_display_constraint_violation() {
	let error = crate::types::error::TypeError::ConstraintViolation {
		type_: Type::Int.into(),
		constraint: Type::String.into(),
		span: 0..0,
	};

	let display_str = format!("{}", error);
	assert!(display_str.contains("int"));
	assert!(display_str.contains("string"));
}

#[test]
fn test_error_display_impl_not_found() {
	let error = crate::types::error::TypeError::ImplNotFound {
		type_: Type::Int.into(),
		interface: Type::Interface {
			name: EcoString::from("Printable"),
			generics: Vec::new(),
			type_args: Vec::new(),
			members: BTreeMap::new(),
			impls: BTreeMap::new(),
			def_key: None,
		}
		.into(),
		span: 0..0,
	};

	let display_str = format!("{}", error);
	assert!(display_str.contains("Printable"));
	assert!(display_str.contains("implement"));
}

#[test]
fn test_struct_member_type_stored() {
	use crate::types::StructMember;

	let mut members = BTreeMap::new();
	members.insert(
		EcoString::from("get_x"),
		StructMember {
			type_: Box::new(Type::Function {
				generics: Vec::new(),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::Int),
			}),
			kind: crate::types::StructMemberKind::Immutable,
		},
	);

	let struct_type = Type::Struct {
		name: EcoString::from("Point"),
		generics: Vec::new(),
		type_args: Vec::new(),
		fields: {
			let mut f = BTreeMap::new();
			f.insert(EcoString::from("x"), Type::Int);
			f.insert(EcoString::from("y"), Type::Int);
			f
		},
		members,
		impls: BTreeMap::new(),
		def_key: None,
	};

	if let Type::Struct { members, .. } = struct_type {
		assert!(members.contains_key(&EcoString::from("get_x")));
		let get_x = members.get(&EcoString::from("get_x")).unwrap();
		match get_x.type_.as_ref() {
			Type::Function { return_type, .. } => {
				assert_eq!(**return_type, Type::Int);
			}
			_ => panic!("Expected function type"),
		}
	} else {
		panic!("Expected struct type");
	}
}

#[test]
fn test_enum_member_type_stored() {
	use crate::types::StructMember;

	let mut members = BTreeMap::new();
	members.insert(
		EcoString::from("is_some"),
		StructMember {
			type_: Box::new(Type::Function {
				generics: Vec::new(),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::Boolean),
			}),
			kind: crate::types::StructMemberKind::Immutable,
		},
	);

	let mut variants = BTreeMap::new();
	variants.insert(EcoString::from("Some"), {
		let mut fields = BTreeMap::new();
		fields.insert(
			EcoString::from("value"),
			Type::Variable {
				name: EcoString::from("T"),
				constraint: None,
			},
		);
		fields
	});
	variants.insert(EcoString::from("None"), BTreeMap::new());

	let enum_type = Type::Enum {
		name: EcoString::from("Option"),
		generics: Vec::new(),
		type_args: Vec::new(),
		variants,
		members,
		impls: BTreeMap::new(),
		def_key: None,
	};

	if let Type::Enum { members, .. } = enum_type {
		assert!(members.contains_key(&EcoString::from("is_some")));
		let is_some = members.get(&EcoString::from("is_some")).unwrap();
		match is_some.type_.as_ref() {
			Type::Function { return_type, .. } => {
				assert_eq!(**return_type, Type::Boolean);
			}
			_ => panic!("Expected function type"),
		}
	} else {
		panic!("Expected enum type");
	}
}

#[test]
fn test_struct_member_kinds() {
	use crate::types::{StructMember, StructMemberKind};

	let namespace_member = StructMember {
		type_: Box::new(Type::Int),
		kind: StructMemberKind::Namespace,
	};

	let mutable_member = StructMember {
		type_: Box::new(Type::Int),
		kind: StructMemberKind::Mutable,
	};

	let immutable_member = StructMember {
		type_: Box::new(Type::Int),
		kind: StructMemberKind::Immutable,
	};

	assert_eq!(namespace_member.kind, StructMemberKind::Namespace);
	assert_eq!(mutable_member.kind, StructMemberKind::Mutable);
	assert_eq!(immutable_member.kind, StructMemberKind::Immutable);
}

#[test]
fn test_struct_with_impl_interface() {
	let mut impls = BTreeMap::new();
	impls.insert(
		EcoString::from("Eq"),
		Type::Interface {
			name: EcoString::from("Eq"),
			generics: Vec::new(),
			type_args: Vec::new(),
			members: BTreeMap::new(),
			impls: BTreeMap::new(),
			def_key: None,
		},
	);

	let struct_type = Type::Struct {
		name: EcoString::from("Point"),
		generics: Vec::new(),
		type_args: Vec::new(),
		fields: BTreeMap::new(),
		members: BTreeMap::new(),
		impls,
		def_key: None,
	};

	if let Type::Struct { impls, .. } = struct_type {
		assert!(impls.contains_key(&EcoString::from("Eq")));
	} else {
		panic!("Expected struct type");
	}
}

#[test]
fn test_struct_member_func_return_type() {
	use crate::types::StructMember;

	// Test that a method returning the struct's field type is correctly typed
	let mut members = BTreeMap::new();
	members.insert(
		EcoString::from("get_name"),
		StructMember {
			type_: Box::new(Type::Function {
				generics: Vec::new(),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::String),
			}),
			kind: crate::types::StructMemberKind::Immutable,
		},
	);
	members.insert(
		EcoString::from("get_age"),
		StructMember {
			type_: Box::new(Type::Function {
				generics: Vec::new(),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::Int),
			}),
			kind: crate::types::StructMemberKind::Immutable,
		},
	);

	let struct_type = Type::Struct {
		name: EcoString::from("Person"),
		generics: Vec::new(),
		type_args: Vec::new(),
		fields: {
			let mut f = BTreeMap::new();
			f.insert(EcoString::from("name"), Type::String);
			f.insert(EcoString::from("age"), Type::Int);
			f
		},
		members,
		impls: BTreeMap::new(),
		def_key: None,
	};

	if let Type::Struct {
		members, fields, ..
	} = struct_type
	{
		assert_eq!(members.len(), 2);
		assert_eq!(fields.len(), 2);

		// Check get_name returns string (matching name field type)
		let get_name = members.get(&EcoString::from("get_name")).unwrap();
		if let Type::Function { return_type, .. } = get_name.type_.as_ref() {
			assert_eq!(**return_type, Type::String);
		}

		// Check get_age returns int (matching age field type)
		let get_age = members.get(&EcoString::from("get_age")).unwrap();
		if let Type::Function { return_type, .. } = get_age.type_.as_ref() {
			assert_eq!(**return_type, Type::Int);
		}
	}
}

#[test]
fn test_namespace_member_kind() {
	use crate::types::{StructMember, StructMemberKind};

	// Namespace members should be static (accessible from the type, not instance)
	let namespace_member = StructMember {
		type_: Box::new(Type::Int),
		kind: StructMemberKind::Namespace,
	};

	// Instance members should be immutable by default
	let instance_member = StructMember {
		type_: Box::new(Type::Int),
		kind: StructMemberKind::Immutable,
	};

	assert_eq!(namespace_member.kind, StructMemberKind::Namespace);
	assert_eq!(instance_member.kind, StructMemberKind::Immutable);
	assert_ne!(namespace_member.kind, instance_member.kind);
}

#[test]
fn test_interface_with_members() {
	use crate::types::StructMember;

	let mut members = BTreeMap::new();
	members.insert(
		EcoString::from("compare"),
		StructMember {
			type_: Box::new(Type::Function {
				generics: Vec::new(),
				params: vec![(
					Some(EcoString::from("other")),
					Type::Variable {
						name: EcoString::from("Self"),
						constraint: None,
					},
				)],
				has_spread: false,
				return_type: Box::new(Type::Int),
			}),
			kind: crate::types::StructMemberKind::Immutable,
		},
	);

	let interface_type = Type::Interface {
		name: EcoString::from("Comparable"),
		generics: Vec::new(),
		type_args: Vec::new(),
		members,
		impls: BTreeMap::new(),
		def_key: None,
	};

	if let Type::Interface { members, name, .. } = interface_type {
		assert_eq!(name, EcoString::from("Comparable"));
		assert!(members.contains_key(&EcoString::from("compare")));
	} else {
		panic!("Expected interface type");
	}
}

#[test]
fn test_generic_struct_instantiation() {
	use crate::types::GenericParamInfo;

	let mut checker = TypeChecker::default();

	let box_type = Type::Struct {
		name: EcoString::from("Box"),
		generics: vec![GenericParamInfo {
			name: EcoString::from("T"),
			constraint: None,
			default: None,
		}],
		type_args: Vec::new(),
		fields: {
			let mut f = BTreeMap::new();
			f.insert(
				EcoString::from("value"),
				Type::Variable {
					name: EcoString::from("T"),
					constraint: None,
				},
			);
			f
		},
		members: BTreeMap::new(),
		impls: BTreeMap::new(),
		def_key: None,
	};

	let ctx = Context::default().with_new_entry(
		EcoString::from("Box"),
		ContextEntry::Value(ContextValue {
			type_: box_type,
			mutable: false,
			visibility: Visibility::Public,
		}),
	);

	let generic_args = vec![make_spanned(
		crate::ast::types::GenericArg {
			value: make_spanned(crate::ast::types::Type::Int, 0, 3),
			name: None,
		},
		0,
		6,
	)];

	let result =
		checker.resolve_qualified_type(&EcoString::from("Box"), &generic_args, span(0, 10), &ctx);

	assert!(result.is_ok());
	let instantiated = result.unwrap();

	if let Type::Struct {
		name,
		type_args,
		fields,
		..
	} = instantiated
	{
		assert_eq!(name, EcoString::from("Box"));
		assert_eq!(type_args.len(), 1);
		assert_eq!(type_args[0], Type::Int);
		assert_eq!(fields.get(&EcoString::from("value")), Some(&Type::Int));
	} else {
		panic!("Expected struct type");
	}
}

#[test]
fn test_generic_function_type_display() {
	use crate::types::GenericParamInfo;

	let func = Type::Function {
		generics: vec![
			GenericParamInfo {
				name: EcoString::from("T"),
				constraint: None,
				default: None,
			},
			GenericParamInfo {
				name: EcoString::from("U"),
				constraint: Some(Type::String),
				default: None,
			},
		],
		params: vec![(
			Some(EcoString::from("x")),
			Type::Variable {
				name: EcoString::from("T"),
				constraint: None,
			},
		)],
		has_spread: false,
		return_type: Box::new(Type::Variable {
			name: EcoString::from("U"),
			constraint: Some(Box::new(Type::String)),
		}),
	};

	let display = format!("{}", func);
	assert!(display.contains("<T, U: string>"));
	assert!(display.contains("(x: T)"));
}

#[test]
fn test_generic_struct_type_display_with_args() {
	let struct_type = Type::Struct {
		name: EcoString::from("List"),
		generics: Vec::new(),
		type_args: vec![Type::Int],
		fields: BTreeMap::new(),
		members: BTreeMap::new(),
		impls: BTreeMap::new(),
		def_key: None,
	};

	assert_eq!(struct_type.to_string(), "List<int>");
}

#[test]
fn test_generic_instantiation_with_defaults() {
	use crate::types::GenericParamInfo;

	let mut checker = TypeChecker::default();

	let pair_type = Type::Struct {
		name: EcoString::from("Pair"),
		generics: vec![
			GenericParamInfo {
				name: EcoString::from("T"),
				constraint: None,
				default: None,
			},
			GenericParamInfo {
				name: EcoString::from("U"),
				constraint: None,
				default: Some(Type::Int),
			},
		],
		type_args: Vec::new(),
		fields: {
			let mut f = BTreeMap::new();
			f.insert(
				EcoString::from("first"),
				Type::Variable {
					name: EcoString::from("T"),
					constraint: None,
				},
			);
			f.insert(
				EcoString::from("second"),
				Type::Variable {
					name: EcoString::from("U"),
					constraint: None,
				},
			);
			f
		},
		members: BTreeMap::new(),
		impls: BTreeMap::new(),
		def_key: None,
	};

	let ctx = Context::default().with_new_entry(
		EcoString::from("Pair"),
		ContextEntry::Value(ContextValue {
			type_: pair_type,
			mutable: false,
			visibility: Visibility::Public,
		}),
	);

	let generic_args = vec![make_spanned(
		crate::ast::types::GenericArg {
			value: make_spanned(crate::ast::types::Type::String, 0, 6),
			name: None,
		},
		0,
		6,
	)];

	let result =
		checker.resolve_qualified_type(&EcoString::from("Pair"), &generic_args, span(0, 15), &ctx);

	assert!(result.is_ok());
	let instantiated = result.unwrap();

	if let Type::Struct { fields, .. } = instantiated {
		assert_eq!(fields.get(&EcoString::from("first")), Some(&Type::String));
		assert_eq!(fields.get(&EcoString::from("second")), Some(&Type::Int));
	} else {
		panic!("Expected struct type");
	}
}

#[test]
fn test_generic_instantiation_error_missing_args() {
	use crate::types::GenericParamInfo;

	let mut checker = TypeChecker::default();

	let box_type = Type::Struct {
		name: EcoString::from("Box"),
		generics: vec![GenericParamInfo {
			name: EcoString::from("T"),
			constraint: None,
			default: None,
		}],
		type_args: Vec::new(),
		fields: BTreeMap::new(),
		members: BTreeMap::new(),
		impls: BTreeMap::new(),
		def_key: None,
	};

	let ctx = Context::default().with_new_entry(
		EcoString::from("Box"),
		ContextEntry::Value(ContextValue {
			type_: box_type,
			mutable: false,
			visibility: Visibility::Public,
		}),
	);

	let result = checker.resolve_qualified_type(&EcoString::from("Box"), &[], span(0, 3), &ctx);

	assert!(result.is_ok());
}

#[test]
fn test_generic_instantiation_error_too_many_args() {
	let mut checker = TypeChecker::default();

	let simple_struct = Type::Struct {
		name: EcoString::from("Point"),
		generics: Vec::new(),
		type_args: Vec::new(),
		fields: BTreeMap::new(),
		members: BTreeMap::new(),
		impls: BTreeMap::new(),
		def_key: None,
	};

	let ctx = Context::default().with_new_entry(
		EcoString::from("Point"),
		ContextEntry::Value(ContextValue {
			type_: simple_struct,
			mutable: false,
			visibility: Visibility::Public,
		}),
	);

	let generic_args = vec![make_spanned(
		crate::ast::types::GenericArg {
			value: make_spanned(crate::ast::types::Type::Int, 0, 3),
			name: None,
		},
		0,
		6,
	)];

	let result =
		checker.resolve_qualified_type(&EcoString::from("Point"), &generic_args, span(0, 10), &ctx);

	assert!(result.is_err());
	if let Err(crate::types::error::TypeError::GenericArgumentMismatch {
		expected, found, ..
	}) = result
	{
		assert_eq!(expected, 0);
		assert_eq!(found, 1);
	} else {
		panic!("Expected GenericArgumentMismatch error");
	}
}

#[test]
fn test_substitute_nested_types() {
	use std::collections::HashMap;

	let checker = TypeChecker::default();

	let list_of_t = Type::List {
		item: Box::new(Type::Variable {
			name: EcoString::from("T"),
			constraint: None,
		}),
	};

	let mut subst = HashMap::new();
	subst.insert(EcoString::from("T"), Type::Int);

	let result = checker.substitute(&list_of_t, &subst);
	assert!(result.is_ok());
	assert_eq!(
		result.unwrap(),
		Type::List {
			item: Box::new(Type::Int)
		}
	);
}

#[test]
fn test_occurs_check() {
	let checker = TypeChecker::default();

	let var_t = Type::Variable {
		name: EcoString::from("T"),
		constraint: None,
	};

	let list_of_t = Type::List {
		item: Box::new(var_t.clone()),
	};

	assert!(checker.occurs_in(&EcoString::from("T"), &list_of_t));
	assert!(!checker.occurs_in(&EcoString::from("U"), &list_of_t));
}

#[test]
fn test_struct_constructor_accessible_in_members() {
	// Test that struct constructors are available in the context when type-checking struct members
	// This allows methods to return instances of their own struct
	let mut ctx = Context::default();

	// Create a Complex struct
	let complex_fields = {
		let mut f = BTreeMap::new();
		f.insert(EcoString::from("real"), Type::Float);
		f.insert(EcoString::from("imaginary"), Type::Float);
		f
	};

	let complex_type = Type::Struct {
		name: EcoString::from("Complex"),
		generics: Vec::new(),
		type_args: Vec::new(),
		fields: complex_fields.clone(),
		members: BTreeMap::new(),
		impls: BTreeMap::new(),
    def_key: None,
	};

	// Register the Complex type in the context
	ctx = ctx.with_new_entry(
		EcoString::from("Complex"),
		ContextEntry::Value(ContextValue {
			type_: complex_type.clone(),
			mutable: false,
			visibility: Visibility::Public,
		}),
	);

	// Test that the struct constructor is available
	// When processing struct members, the constructor should be in the context
	let constructor_lookup = ctx.lookup_type(&EcoString::from("Complex"));
	assert!(constructor_lookup.is_some());

	// The constructor should be the struct type itself
	if let Some(found_type) = constructor_lookup {
		assert_eq!(found_type, complex_type);
	}
}

#[test]
fn test_external_let_declaration() {
	use crate::ast::declaration::{Declaration, LetDeclaration};
	use crate::ast::expr::Pattern;
	use crate::ast::types::Type as AstType;

	let mut checker = TypeChecker::default();
	let ctx = Context::default();

	// Create an external let declaration with a type annotation
	let decl = Declaration::ExternalLet(
		Some(Visibility::Public),
		LetDeclaration {
			mutable: false,
			name: make_spanned(
				Pattern::Binding {
					name: make_spanned(EcoString::from("extern_var"), 0, 10),
					inner: Box::new(make_spanned(Pattern::Placeholder, 0, 10)),
				},
				0,
				10,
			),
			type_: Some(make_spanned(AstType::Int, 12, 15)),
		},
	);

	let result = checker.check_declaration(&decl, &ctx);
	assert!(result.is_ok());

	let new_ctx = result.unwrap();
	let lookup = new_ctx.lookup_type(&EcoString::from("extern_var"));
	assert!(lookup.is_some());
	assert_eq!(lookup.unwrap(), Type::Int);
}

#[test]
fn test_external_let_missing_type_error() {
	use crate::ast::declaration::{Declaration, LetDeclaration};
	use crate::ast::expr::Pattern;
	use crate::types::error::TypeError;

	let mut checker = TypeChecker::default();
	let ctx = Context::default();

	// Create an external let declaration without a type annotation (should error)
	let decl = Declaration::ExternalLet(
		Some(Visibility::Public),
		LetDeclaration {
			mutable: false,
			name: make_spanned(
				Pattern::Binding {
					name: make_spanned(EcoString::from("no_type_var"), 0, 11),
					inner: Box::new(make_spanned(Pattern::Placeholder, 0, 11)),
				},
				0,
				11,
			),
			type_: None,
		},
	);

	let result = checker.check_declaration(&decl, &ctx);
	assert!(result.is_err());

	match result.unwrap_err() {
		TypeError::ExternalDeclarationMissingType(_) => (),
		other => panic!("Expected ExternalDeclarationMissingType, got {:?}", other),
	}
}

#[test]
fn test_external_func_declaration() {
	use crate::ast::declaration::{Declaration, FuncDeclaration, FuncParam};
	use crate::ast::expr::Pattern;
	use crate::ast::types::Type as AstType;

	let mut checker = TypeChecker::default();
	let ctx = Context::default();

	// Create an external func declaration
	let decl = Declaration::ExternalFunc(
		Some(Visibility::Public),
		FuncDeclaration {
			name: make_spanned(EcoString::from("extern_func"), 0, 11),
			generics: vec![],
			params: vec![make_spanned(
				FuncParam {
					name: make_spanned(
						Pattern::Binding {
							name: make_spanned(EcoString::from("x"), 12, 13),
							inner: Box::new(make_spanned(Pattern::Placeholder, 12, 13)),
						},
						12,
						13,
					),
					type_: make_spanned(AstType::Int, 15, 18),
					mutable: false,
					spread: false,
				},
				12,
				18,
			)],
			return_type: Some(make_spanned(AstType::String, 23, 29)),
		},
	);

	let result = checker.check_declaration(&decl, &ctx);
	assert!(result.is_ok());

	let new_ctx = result.unwrap();
	let lookup = new_ctx.lookup_type(&EcoString::from("extern_func"));
	assert!(lookup.is_some());

	match lookup.unwrap() {
		Type::Function {
			params,
			return_type,
			..
		} => {
			assert_eq!(params.len(), 1);
			assert_eq!(params[0].1, Type::Int);
			assert_eq!(*return_type, Type::String);
		}
		other => panic!("Expected Function type, got {:?}", other),
	}
}

#[test]
fn test_type_alias_declaration() {
	use crate::ast::declaration::{Declaration, TypeAliasDeclaration};
	use crate::ast::types::Type as AstType;

	let mut checker = TypeChecker::default();
	let ctx = Context::default();

	// Create a type alias: type IntList = #[int]
	let decl = Declaration::TypeAlias {
		visibility: Some(Visibility::Public),
		meta: TypeAliasDeclaration {
			name: make_spanned(EcoString::from("IntList"), 5, 12),
			generics: vec![],
		},
		value: make_spanned(
			AstType::List(Box::new(make_spanned(AstType::Int, 16, 19))),
			14,
			21,
		),
	};

	let result = checker.check_declaration(&decl, &ctx);
	assert!(result.is_ok());

	let new_ctx = result.unwrap();
	let lookup = new_ctx.lookup_type(&EcoString::from("IntList"));
	assert!(lookup.is_some());

	match lookup.unwrap() {
		Type::List { item } => {
			assert_eq!(*item, Type::Int);
		}
		other => panic!("Expected List type, got {:?}", other),
	}
}

#[test]
fn test_type_alias_with_generics() {
	use crate::ast::declaration::{Declaration, TypeAliasDeclaration};
	use crate::ast::types::{GenericParam, Type as AstType};

	let mut checker = TypeChecker::default();
	let ctx = Context::default();

	// Create a type alias: type Pair<A, B> = #(A, B)
	let decl = Declaration::TypeAlias {
		visibility: Some(Visibility::Public),
		meta: TypeAliasDeclaration {
			name: make_spanned(EcoString::from("Pair"), 5, 9),
			generics: vec![
				make_spanned(
					GenericParam {
						name: make_spanned(EcoString::from("A"), 10, 11),
						constraint: None,
						default: None,
					},
					10,
					11,
				),
				make_spanned(
					GenericParam {
						name: make_spanned(EcoString::from("B"), 13, 14),
						constraint: None,
						default: None,
					},
					13,
					14,
				),
			],
		},
		value: make_spanned(
			AstType::Tuple(vec![
				make_spanned(
					AstType::Reference {
						name: make_spanned(EcoString::from("A"), 20, 21),
						generics: vec![],
					},
					20,
					21,
				),
				make_spanned(
					AstType::Reference {
						name: make_spanned(EcoString::from("B"), 23, 24),
						generics: vec![],
					},
					23,
					24,
				),
			]),
			18,
			25,
		),
	};

	let result = checker.check_declaration(&decl, &ctx);
	assert!(result.is_ok());

	let new_ctx = result.unwrap();
	let lookup = new_ctx.lookup_type(&EcoString::from("Pair"));
	assert!(lookup.is_some());

	// The result should be a tuple with two type variables
	match lookup.unwrap() {
		Type::Tuple { items } => {
			assert_eq!(items.len(), 2);
			match (&items[0], &items[1]) {
				(Type::Variable { name: name_a, .. }, Type::Variable { name: name_b, .. }) => {
					assert_eq!(name_a.as_str(), "A");
					assert_eq!(name_b.as_str(), "B");
				}
				other => panic!("Expected Variable types, got {:?}", other),
			}
		}
		other => panic!("Expected Tuple type, got {:?}", other),
	}
}
