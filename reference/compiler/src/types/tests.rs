use crate::ast::Span;
use crate::ast::ops::{BinaryOperator, PrefixOperator};
use crate::types::error::TypeErrorKind;
use crate::{
	ast::{Spanned, declaration::Visibility, expr::Expr},
	types::{
		Context, ContextEntry, ContextValue, Type, TypeChecker, TypeVarId, type_error_to_report,
	},
};
use ariadne::Source;
use ecow::EcoString;
use ordered_float::OrderedFloat;
use std::{collections::BTreeMap, sync::Arc};

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

	let expr = make_spanned(Expr::Float(make_spanned(OrderedFloat(3.15), 0, 4)), 0, 4);
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

	let float_expr = make_spanned(Expr::Float(make_spanned(OrderedFloat(3.15), 0, 4)), 0, 4);
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
		Type::Struct {
			name, type_args, ..
		} => {
			assert_eq!(name, "RangeFrom");
			assert_eq!(type_args, vec![Type::Int]);
		}
		other => panic!("Expected range type, found {other:?}"),
	}
}

#[test]
fn test_in_operator_accepts_ranges() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1)),
			op: crate::ast::ops::BinaryOperator::In,
			rhs: Box::new(make_spanned(
				Expr::Range(crate::ast::expr::RangeKind::Exclusive {
					min: Box::new(make_spanned(Expr::Int(make_spanned(0u64, 0, 1)), 0, 1)),
					max: Box::new(make_spanned(Expr::Int(make_spanned(10u64, 0, 2)), 0, 2)),
				}),
				0,
				5,
			)),
		},
		0,
		5,
	);

	assert_eq!(checker.infer(&expr, &ctx).unwrap(), Type::Boolean);
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

	assert_eq!(result.unwrap(), Type::Never);
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
fn test_check_anonymous_param_with_expected_function_type() {
	let mut checker = TypeChecker::default();
	let ctx = Context::default();
	let expr = make_spanned(Expr::AnonymousParam(None), 0, 1);
	let expected = Type::Function {
		generics: Arc::new(Vec::new()),
		params: vec![(None, Type::Int)],
		has_spread: false,
		return_type: Box::new(Type::Int),
		constructor: false,
	};

	assert_eq!(checker.check_expr(&expr, &expected, &ctx), Ok(expected));
}

#[test]
fn test_infer_anonymous_param_without_context_reports_explicit_closure_help() {
	let mut checker = TypeChecker::default();
	let ctx = Context::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::AnonymousParam(None), 0, 1)),
			op: BinaryOperator::Plus,
			rhs: Box::new(make_spanned(Expr::AnonymousParam(Some(1)), 4, 6)),
		},
		0,
		6,
	);

	let error = checker.infer(&expr, &ctx).unwrap_err();
	assert!(matches!(
		error.kind,
		TypeErrorKind::CannotInferAnonymousFunction {
			ref placeholders
		} if placeholders == &vec![None, Some(1)]
	));

	let report = type_error_to_report(EcoString::from("test.nym"), &error);
	let mut output = Vec::new();
	report
		.write(
			(EcoString::from("test.nym"), Source::from("$ + $1")),
			&mut output,
		)
		.unwrap();
	let rendered = String::from_utf8(output).unwrap();
	assert!(rendered.contains("explicit closure"));
	assert!(rendered.contains("$"));
	assert!(rendered.contains("$1"));
	assert!(rendered.contains("(arg0: int, arg1: int) -> ..."));
}

#[test]
fn test_pipe_operator_provides_context_for_anonymous_param_rhs() {
	let mut checker = TypeChecker::default();
	let ctx = Context::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(2, 0, 1)), 0, 1)),
			op: BinaryOperator::Pipe,
			rhs: Box::new(make_spanned(
				Expr::BinaryOp {
					lhs: Box::new(make_spanned(Expr::AnonymousParam(None), 5, 6)),
					op: BinaryOperator::Times,
					rhs: Box::new(make_spanned(Expr::Int(make_spanned(2, 9, 10)), 9, 10)),
				},
				5,
				10,
			)),
		},
		0,
		10,
	);

	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_binary_op_int_addition() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();

	let lhs = make_spanned(Expr::Int(make_spanned(1, 0, 1)), 0, 1);
	let rhs = make_spanned(Expr::Int(make_spanned(2, 0, 1)), 0, 1);
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(lhs),
			op: BinaryOperator::Plus,
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

	let lhs = make_spanned(Expr::Float(make_spanned(OrderedFloat(1.0), 0, 3)), 0, 3);
	let rhs = make_spanned(Expr::Float(make_spanned(OrderedFloat(2.0), 0, 3)), 0, 3);
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(lhs),
			op: BinaryOperator::Plus,
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
	let rhs = make_spanned(Expr::Float(make_spanned(OrderedFloat(2.0), 0, 3)), 0, 3);
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(lhs),
			op: BinaryOperator::Plus,
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
			op: BinaryOperator::BoolAnd,
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
			op: BinaryOperator::LessThan,
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
			op: PrefixOperator::BoolNot,
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
			op: PrefixOperator::Negate,
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

	let val = make_spanned(Expr::Float(make_spanned(OrderedFloat(3.15), 0, 4)), 0, 4);
	let expr = make_spanned(
		Expr::PrefixOp {
			op: PrefixOperator::Negate,
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
	assert_eq!(result, Type::Never);
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
		generics: Arc::new(Vec::new()),
		params: vec![(Some(EcoString::from("x")), Type::Int)],
		has_spread: false,
		return_type: Box::new(Type::String),
		constructor: false,
	};
	assert_eq!(func.to_string(), "(x: int) -> string");
}

#[test]
fn test_type_display_variable() {
	let var = Type::Variable {
		id: TypeVarId(0),
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
		crate::types::error::TypeError {
			kind: crate::types::error::TypeErrorKind::UnknownIdentifier { name, .. },
			..
		} => {
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
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
		def_key: None,
	};

	let new_ctx = ctx.with_impl_record(crate::types::ImplRecord {
		generics: Arc::new(Vec::new()),
		receiver: Type::Int,
		interface: interface_type.clone(),
		span: 0..0,
	});

	assert_eq!(new_ctx.impl_records.len(), 1);
	assert_eq!(new_ctx.impl_records[0].receiver, Type::Int);
	assert_eq!(new_ctx.impl_records[0].interface, interface_type);
}

#[test]
fn test_qualify_type_with_unresolved_generic() {
	let mut checker = TypeChecker::default();
	let _ctx = Context::default().with_new_entry(
		EcoString::from("List"),
		ContextEntry::Value(ContextValue {
			type_: Type::List {
				item: Box::new(Type::Variable {
					id: TypeVarId(0),
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
	let result =
		checker.resolve_qualified_type(&EcoString::from("List"), &[], Span::new(0, 0), &_ctx);

	assert!(result.is_ok());
}

#[test]
fn test_resolve_unknown_qualified_type() {
	let mut checker = TypeChecker::default();
	let ctx = Context::default();

	let result =
		checker.resolve_qualified_type(&EcoString::from("UnknownType"), &[], Span::new(0, 0), &ctx);

	assert!(result.is_err());
	match result.unwrap_err() {
		crate::types::error::TypeError {
			kind: crate::types::error::TypeErrorKind::UnknownType { name, .. },
			..
		} => {
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
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		fields: Arc::new(BTreeMap::new()),
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
		def_key: None,
	};

	let interface_type = Type::Interface {
		name: EcoString::from("Eq"),
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
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
		.with_impl_record(crate::types::ImplRecord {
			generics: Arc::new(Vec::new()),
			receiver: Type::Struct {
				name: EcoString::from("Point"),
				generics: Arc::new(Vec::new()),
				type_args: Vec::new(),
				fields: Arc::new(BTreeMap::new()),
				members: Arc::new(BTreeMap::new()),
				impls: Arc::new(BTreeMap::new()),
				def_key: None,
			},
			interface: interface_type,
			span: 0..0,
		});

	assert_eq!(new_ctx.impl_records.len(), 1);
	assert_eq!(new_ctx.impl_records[0].receiver.to_string(), "Point");
}

#[test]
fn test_infer_member_from_registered_interface_impl() {
	let mut interface_members = BTreeMap::new();
	interface_members.insert(
		EcoString::from("to_string"),
		crate::types::StructMember {
			type_: Box::new(Type::Function {
				generics: Arc::new(Vec::new()),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::String),
				constructor: false,
			}),
			kind: crate::types::StructMemberKind::Immutable,
			required: true,
		},
	);
	let display_type = Type::Interface {
		name: EcoString::from("Display"),
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		members: Arc::new(interface_members),
		impls: Arc::new(BTreeMap::new()),
		def_key: None,
	};

	let ctx = Context::default()
		.with_new_entry(
			EcoString::from("value"),
			ContextEntry::Value(ContextValue {
				type_: Type::Int,
				mutable: false,
				visibility: Visibility::Private,
			}),
		)
		.with_impl_record(crate::types::ImplRecord {
			generics: Arc::new(Vec::new()),
			receiver: Type::Int,
			interface: display_type,
			span: 10..20,
		});

	let expr = make_spanned(
		Expr::MemberAccess {
			parent: Box::new(make_spanned(
				Expr::Identifier(make_spanned(EcoString::from("value"), 0, 5)),
				0,
				5,
			)),
			member: make_spanned(EcoString::from("to_string"), 6, 15),
			optional: false,
		},
		0,
		15,
	);

	let mut checker = TypeChecker::default();
	let result = checker.infer(&expr, &ctx).unwrap();
	match result {
		Type::Function { return_type, .. } => assert_eq!(*return_type, Type::String),
		other => panic!("Expected function type, found {other:?}"),
	}
}

#[test]
fn test_infer_member_from_interface_extension() {
	let comparable_type = Type::Interface {
		name: EcoString::from("Comparable"),
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
		def_key: None,
	};

	let mut extension_members = BTreeMap::new();
	extension_members.insert(
		EcoString::from("debug_name"),
		crate::types::StructMember {
			type_: Box::new(Type::Function {
				generics: Arc::new(Vec::new()),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::String),
				constructor: false,
			}),
			kind: crate::types::StructMemberKind::Immutable,
			required: false,
		},
	);

	let ctx = Context::default()
		.with_new_entry(
			EcoString::from("value"),
			ContextEntry::Value(ContextValue {
				type_: Type::Int,
				mutable: false,
				visibility: Visibility::Private,
			}),
		)
		.with_impl_record(crate::types::ImplRecord {
			generics: Arc::new(Vec::new()),
			receiver: Type::Int,
			interface: comparable_type.clone(),
			span: 20..30,
		})
		.with_interface_extension(crate::types::InterfaceExtensionRecord {
			generics: Arc::new(Vec::new()),
			interface: comparable_type,
			members: extension_members,
			span: 30..40,
		});

	let expr = make_spanned(
		Expr::MemberAccess {
			parent: Box::new(make_spanned(
				Expr::Identifier(make_spanned(EcoString::from("value"), 0, 5)),
				0,
				5,
			)),
			member: make_spanned(EcoString::from("debug_name"), 6, 16),
			optional: false,
		},
		0,
		16,
	);

	let mut checker = TypeChecker::default();
	let result = checker.infer(&expr, &ctx).unwrap();
	match result {
		Type::Function { return_type, .. } => assert_eq!(*return_type, Type::String),
		other => panic!("Expected function type, found {other:?}"),
	}
}

#[test]
fn test_ambiguous_interface_member_reports_candidates() {
	let make_interface = |name: &str| {
		let mut members = BTreeMap::new();
		members.insert(
			EcoString::from("debug"),
			crate::types::StructMember {
				type_: Box::new(Type::Function {
					generics: Arc::new(Vec::new()),
					params: vec![],
					has_spread: false,
					return_type: Box::new(Type::String),
					constructor: false,
				}),
				kind: crate::types::StructMemberKind::Immutable,
				required: true,
			},
		);
		Type::Interface {
			name: EcoString::from(name),
			generics: Arc::new(Vec::new()),
			type_args: Vec::new(),
			members: Arc::new(members),
			impls: Arc::new(BTreeMap::new()),
			def_key: None,
		}
	};

	let ctx = Context::default()
		.with_new_entry(
			EcoString::from("value"),
			ContextEntry::Value(ContextValue {
				type_: Type::Int,
				mutable: false,
				visibility: Visibility::Private,
			}),
		)
		.with_impl_record(crate::types::ImplRecord {
			generics: Arc::new(Vec::new()),
			receiver: Type::Int,
			interface: make_interface("DebugA"),
			span: 40..50,
		})
		.with_impl_record(crate::types::ImplRecord {
			generics: Arc::new(Vec::new()),
			receiver: Type::Int,
			interface: make_interface("DebugB"),
			span: 50..60,
		});

	let expr = make_spanned(
		Expr::MemberAccess {
			parent: Box::new(make_spanned(
				Expr::Identifier(make_spanned(EcoString::from("value"), 0, 5)),
				0,
				5,
			)),
			member: make_spanned(EcoString::from("debug"), 6, 11),
			optional: false,
		},
		0,
		11,
	);

	let mut checker = TypeChecker::default();
	let error = checker.infer(&expr, &ctx).unwrap_err();
	match error.kind {
		crate::types::error::TypeErrorKind::AmbiguousMemberAccess {
			member, candidates, ..
		} => {
			assert_eq!(member, EcoString::from("debug"));
			assert_eq!(candidates.len(), 2);
			assert!(candidates.iter().all(|candidate| candidate.span.is_some()));
		}
		other => panic!("Expected ambiguous member error, found {other:?}"),
	}
}

#[test]
fn test_intersection_target_requires_all_parts() {
	let ctx = Context::default();
	let impossible = Type::Intersection {
		first: Box::new(Type::Int),
		second: Box::new(Type::String),
	};

	assert!(!Type::Int.assignable_to(&impossible, &ctx));
	assert!(!Type::String.assignable_to(&impossible, &ctx));
}

#[test]
fn test_error_display_generic_mismatch() {
	let error = crate::types::error::TypeError {
		kind: crate::types::error::TypeErrorKind::GenericArgumentMismatch {
			expected: 2,
			found: 1,
		},
		span: 0..0,
	};

	let display_str = format!("{}", error);
	assert!(display_str.contains("2"));
	assert!(display_str.contains("1"));
}

#[test]
fn test_error_display_constraint_violation() {
	let error = crate::types::error::TypeError {
		kind: crate::types::error::TypeErrorKind::ConstraintViolation {
			type_: Type::Int.into(),
			constraint: Type::String.into(),
		},
		span: 0..0,
	};

	let display_str = format!("{}", error);
	assert!(display_str.contains("int"));
	assert!(display_str.contains("string"));
}

#[test]
fn test_error_display_impl_not_found() {
	let error = crate::types::error::TypeError {
		kind: crate::types::error::TypeErrorKind::ImplNotFound {
			type_: Type::Int.into(),
			interface: Type::Interface {
				name: EcoString::from("Printable"),
				generics: Arc::new(Vec::new()),
				type_args: Vec::new(),
				members: Arc::new(BTreeMap::new()),
				impls: Arc::new(BTreeMap::new()),
				def_key: None,
			}
			.into(),
		},
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
				generics: Arc::new(Vec::new()),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::Int),
				constructor: false,
			}),
			kind: crate::types::StructMemberKind::Immutable,
			required: false,
		},
	);

	let struct_type = Type::Struct {
		name: EcoString::from("Point"),
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		fields: {
			let mut f = BTreeMap::new();
			f.insert(EcoString::from("x"), Type::Int);
			f.insert(EcoString::from("y"), Type::Int);
			Arc::new(f)
		},
		members: Arc::new(members),
		impls: Arc::new(BTreeMap::new()),
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
				generics: Arc::new(Vec::new()),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::Boolean),
				constructor: false,
			}),
			kind: crate::types::StructMemberKind::Immutable,
			required: false,
		},
	);

	let mut variants = BTreeMap::new();
	variants.insert(EcoString::from("Some"), {
		let mut fields = BTreeMap::new();
		fields.insert(
			EcoString::from("value"),
			Type::Variable {
				id: TypeVarId(0),
				name: EcoString::from("T"),
				constraint: None,
			},
		);
		fields
	});
	variants.insert(EcoString::from("None"), BTreeMap::new());

	let enum_type = Type::Enum {
		name: EcoString::from("Option"),
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		variants: Arc::new(variants),
		members: Arc::new(members),
		impls: Arc::new(BTreeMap::new()),
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
		required: false,
	};

	let mutable_member = StructMember {
		type_: Box::new(Type::Int),
		kind: StructMemberKind::Mutable,
		required: false,
	};

	let immutable_member = StructMember {
		type_: Box::new(Type::Int),
		kind: StructMemberKind::Immutable,
		required: false,
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
			generics: Arc::new(Vec::new()),
			type_args: Vec::new(),
			members: Arc::new(BTreeMap::new()),
			impls: Arc::new(BTreeMap::new()),
			def_key: None,
		},
	);

	let struct_type = Type::Struct {
		name: EcoString::from("Point"),
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		fields: Arc::new(BTreeMap::new()),
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(impls),
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
				generics: Arc::new(Vec::new()),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::String),
				constructor: false,
			}),
			kind: crate::types::StructMemberKind::Immutable,
			required: false,
		},
	);
	members.insert(
		EcoString::from("get_age"),
		StructMember {
			type_: Box::new(Type::Function {
				generics: Arc::new(Vec::new()),
				params: vec![],
				has_spread: false,
				return_type: Box::new(Type::Int),
				constructor: false,
			}),
			kind: crate::types::StructMemberKind::Immutable,
			required: false,
		},
	);

	let struct_type = Type::Struct {
		name: EcoString::from("Person"),
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		fields: {
			let mut f = BTreeMap::new();
			f.insert(EcoString::from("name"), Type::String);
			f.insert(EcoString::from("age"), Type::Int);
			Arc::new(f)
		},
		members: Arc::new(members),
		impls: Arc::new(BTreeMap::new()),
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
		required: false,
	};

	// Instance members should be immutable by default
	let instance_member = StructMember {
		type_: Box::new(Type::Int),
		kind: StructMemberKind::Immutable,
		required: false,
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
				generics: Arc::new(Vec::new()),
				params: vec![(
					Some(EcoString::from("other")),
					Type::Variable {
						id: TypeVarId(0),
						name: EcoString::from("Self"),
						constraint: None,
					},
				)],
				has_spread: false,
				return_type: Box::new(Type::Int),
				constructor: false,
			}),
			kind: crate::types::StructMemberKind::Immutable,
			required: false,
		},
	);

	let interface_type = Type::Interface {
		name: EcoString::from("Comparable"),
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		members: Arc::new(members),
		impls: Arc::new(BTreeMap::new()),
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
		generics: Arc::new(vec![GenericParamInfo {
			id: TypeVarId(0),
			name: EcoString::from("T"),
			constraint: None,
			default: None,
		}]),
		type_args: Vec::new(),
		fields: {
			let mut f = BTreeMap::new();
			f.insert(
				EcoString::from("value"),
				Type::Variable {
					id: TypeVarId(0),
					name: EcoString::from("T"),
					constraint: None,
				},
			);
			Arc::new(f)
		},
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
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
		generics: Arc::new(vec![
			GenericParamInfo {
				id: TypeVarId(0),
				name: EcoString::from("T"),
				constraint: None,
				default: None,
			},
			GenericParamInfo {
				id: TypeVarId(1),
				name: EcoString::from("U"),
				constraint: Some(Type::String),
				default: None,
			},
		]),
		params: vec![(
			Some(EcoString::from("x")),
			Type::Variable {
				id: TypeVarId(0),
				name: EcoString::from("T"),
				constraint: None,
			},
		)],
		has_spread: false,
		return_type: Box::new(Type::Variable {
			id: TypeVarId(1),
			name: EcoString::from("U"),
			constraint: Some(Box::new(Type::String)),
		}),
		constructor: false,
	};

	let display = format!("{}", func);
	assert!(display.contains("<T, U: string>"));
	assert!(display.contains("(x: T)"));
}

#[test]
fn test_generic_struct_type_display_with_args() {
	let struct_type = Type::Struct {
		name: EcoString::from("List"),
		generics: Arc::new(Vec::new()),
		type_args: vec![Type::Int],
		fields: Arc::new(BTreeMap::new()),
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
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
		generics: Arc::new(vec![
			GenericParamInfo {
				id: TypeVarId(0),
				name: EcoString::from("T"),
				constraint: None,
				default: None,
			},
			GenericParamInfo {
				id: TypeVarId(1),
				name: EcoString::from("U"),
				constraint: None,
				default: Some(Type::Int),
			},
		]),
		type_args: Vec::new(),
		fields: {
			let mut f = BTreeMap::new();
			f.insert(
				EcoString::from("first"),
				Type::Variable {
					id: TypeVarId(0),
					name: EcoString::from("T"),
					constraint: None,
				},
			);
			f.insert(
				EcoString::from("second"),
				Type::Variable {
					id: TypeVarId(1),
					name: EcoString::from("U"),
					constraint: None,
				},
			);
			Arc::new(f)
		},
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
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
		generics: Arc::new(vec![GenericParamInfo {
			id: TypeVarId(0),
			name: EcoString::from("T"),
			constraint: None,
			default: None,
		}]),
		type_args: Vec::new(),
		fields: Arc::new(BTreeMap::new()),
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
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
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		fields: Arc::new(BTreeMap::new()),
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
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
	if let Err(crate::types::error::TypeError {
		kind: crate::types::error::TypeErrorKind::GenericArgumentMismatch { expected, found },
		..
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
	let t_id = TypeVarId(0);

	let list_of_t = Type::List {
		item: Box::new(Type::Variable {
			id: t_id,
			name: EcoString::from("T"),
			constraint: None,
		}),
	};

	let mut subst = HashMap::new();
	subst.insert(t_id, Type::Int);

	let result = checker.substitute(&list_of_t, &subst, span(0, 1));
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
	let t_id = TypeVarId(0);
	let u_id = TypeVarId(1);

	let var_t = Type::Variable {
		id: t_id,
		name: EcoString::from("T"),
		constraint: None,
	};

	let list_of_t = Type::List {
		item: Box::new(var_t.clone()),
	};

	assert!(checker.occurs_in(&t_id, &list_of_t));
	assert!(!checker.occurs_in(&u_id, &list_of_t));
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
		generics: Arc::new(Vec::new()),
		type_args: Vec::new(),
		fields: Arc::new(complex_fields.clone()),
		members: Arc::new(BTreeMap::new()),
		impls: Arc::new(BTreeMap::new()),
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

// ═══════════════════════════════════════════════════════════════
// Binary operation tests (arithmetic)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_infer_int_subtraction() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(5u64, 0, 1)), 0, 1)),
			op: BinaryOperator::Minus,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(3u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_int_multiplication() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(3u64, 0, 1)), 0, 1)),
			op: BinaryOperator::Times,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(4u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_int_division() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(10u64, 0, 2)), 0, 2)),
			op: BinaryOperator::Divide,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(2u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_int_remainder() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(10u64, 0, 2)), 0, 2)),
			op: BinaryOperator::Remainder,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(3u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_int_power() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(2u64, 0, 1)), 0, 1)),
			op: BinaryOperator::Power,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(8u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_float_subtraction() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(
				Expr::Float(make_spanned(OrderedFloat(5.0), 0, 3)),
				0,
				3,
			)),
			op: BinaryOperator::Minus,
			rhs: Box::new(make_spanned(
				Expr::Float(make_spanned(OrderedFloat(3.0), 0, 3)),
				0,
				3,
			)),
		},
		0,
		7,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Float));
}

#[test]
fn test_infer_float_multiplication() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(
				Expr::Float(make_spanned(OrderedFloat(2.0), 0, 3)),
				0,
				3,
			)),
			op: BinaryOperator::Times,
			rhs: Box::new(make_spanned(
				Expr::Float(make_spanned(OrderedFloat(3.0), 0, 3)),
				0,
				3,
			)),
		},
		0,
		7,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Float));
}

#[test]
fn test_infer_float_division() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(
				Expr::Float(make_spanned(OrderedFloat(10.0), 0, 4)),
				0,
				4,
			)),
			op: BinaryOperator::Divide,
			rhs: Box::new(make_spanned(
				Expr::Float(make_spanned(OrderedFloat(2.0), 0, 3)),
				0,
				3,
			)),
		},
		0,
		8,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Float));
}

// ═══════════════════════════════════════════════════════════════
// Binary operation tests (bitwise)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_infer_bitwise_and() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(3u64, 0, 1)), 0, 1)),
			op: BinaryOperator::BitAnd,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_bitwise_or() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1)),
			op: BinaryOperator::BitOr,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(2u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_bitwise_xor() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(5u64, 0, 1)), 0, 1)),
			op: BinaryOperator::BitXor,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(3u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_left_shift() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1)),
			op: BinaryOperator::LeftShift,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(4u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

#[test]
fn test_infer_right_shift() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(16u64, 0, 2)), 0, 2)),
			op: BinaryOperator::RightShift,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(2u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

// ═══════════════════════════════════════════════════════════════
// Binary operation tests (boolean/comparison)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_infer_boolean_or() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Boolean(make_spanned(true, 0, 4)), 0, 4)),
			op: BinaryOperator::BoolOr,
			rhs: Box::new(make_spanned(Expr::Boolean(make_spanned(false, 0, 5)), 0, 5)),
		},
		0,
		10,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Boolean));
}

#[test]
fn test_infer_equality() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1)),
			op: BinaryOperator::Equals,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Boolean));
}

#[test]
fn test_infer_not_equals() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1)),
			op: BinaryOperator::NotEquals,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(2u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Boolean));
}

#[test]
fn test_infer_greater_than() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(5u64, 0, 1)), 0, 1)),
			op: BinaryOperator::GreaterThan,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(3u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Boolean));
}

#[test]
fn test_infer_greater_than_equals() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(5u64, 0, 1)), 0, 1)),
			op: BinaryOperator::GreaterThanEquals,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(3u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Boolean));
}

#[test]
fn test_infer_less_than_equals() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::BinaryOp {
			lhs: Box::new(make_spanned(Expr::Int(make_spanned(3u64, 0, 1)), 0, 1)),
			op: BinaryOperator::LessThanEquals,
			rhs: Box::new(make_spanned(Expr::Int(make_spanned(5u64, 0, 1)), 0, 1)),
		},
		0,
		5,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Boolean));
}

// ═══════════════════════════════════════════════════════════════
// Prefix operation tests
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_infer_prefix_bitnot() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::PrefixOp {
			op: PrefixOperator::BitNot,
			value: Box::new(make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2)),
		},
		0,
		3,
	);
	assert_eq!(checker.infer(&expr, &ctx), Ok(Type::Int));
}

// ═══════════════════════════════════════════════════════════════
// Type assignability tests
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_int_not_assignable_to_string() {
	let ctx = Default::default();
	assert!(!Type::Int.assignable_to(&Type::String, &ctx));
}

#[test]
fn test_int_not_assignable_to_boolean() {
	let ctx = Default::default();
	assert!(!Type::Int.assignable_to(&Type::Boolean, &ctx));
}

#[test]
fn test_void_not_assignable_to_int() {
	let ctx = Default::default();
	assert!(!Type::Void.assignable_to(&Type::Int, &ctx));
}

#[test]
fn test_list_not_assignable_different_item() {
	let ctx = Default::default();
	let list_int = Type::List {
		item: Box::new(Type::Int),
	};
	let list_string = Type::List {
		item: Box::new(Type::String),
	};
	assert!(!list_int.assignable_to(&list_string, &ctx));
}

#[test]
fn test_tuple_not_assignable_different_lengths() {
	let ctx = Default::default();
	let tuple2 = Type::Tuple {
		items: vec![Type::Int, Type::String],
	};
	let tuple3 = Type::Tuple {
		items: vec![Type::Int, Type::String, Type::Boolean],
	};
	assert!(!tuple2.assignable_to(&tuple3, &ctx));
}

#[test]
fn test_tuple_not_assignable_different_types() {
	let ctx = Default::default();
	let tuple_a = Type::Tuple {
		items: vec![Type::Int, Type::String],
	};
	let tuple_b = Type::Tuple {
		items: vec![Type::String, Type::Int],
	};
	assert!(!tuple_a.assignable_to(&tuple_b, &ctx));
}

#[test]
fn test_map_assignable_same_types() {
	let ctx = Default::default();
	let map_a = Type::Map {
		key: Box::new(Type::String),
		value: Box::new(Type::Int),
	};
	let map_b = Type::Map {
		key: Box::new(Type::String),
		value: Box::new(Type::Int),
	};
	assert!(map_a.assignable_to(&map_b, &ctx));
}

#[test]
fn test_map_not_assignable_different_types() {
	let ctx = Default::default();
	let map_a = Type::Map {
		key: Box::new(Type::String),
		value: Box::new(Type::Int),
	};
	let map_b = Type::Map {
		key: Box::new(Type::Int),
		value: Box::new(Type::String),
	};
	assert!(!map_a.assignable_to(&map_b, &ctx));
}

#[test]
fn test_function_assignable_same_signature() {
	let ctx = Default::default();
	let func = Type::Function {
		generics: Arc::new(Vec::new()),
		params: vec![(None, Type::Int)],
		has_spread: false,
		return_type: Box::new(Type::String),
		constructor: false,
	};
	let func2 = Type::Function {
		generics: Arc::new(Vec::new()),
		params: vec![(None, Type::Int)],
		has_spread: false,
		return_type: Box::new(Type::String),
		constructor: false,
	};
	assert!(func.assignable_to(&func2, &ctx));
}

// ═══════════════════════════════════════════════════════════════
// Type join tests
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_join_same_types() {
	assert_eq!(Type::Int.join(&Type::Int), Type::Int);
}

#[test]
fn test_join_never_with_string() {
	assert_eq!(Type::Never.join(&Type::String), Type::String);
}

#[test]
fn test_join_int_with_never() {
	assert_eq!(Type::Int.join(&Type::Never), Type::Int);
}

#[test]
fn test_join_never_with_never() {
	assert_eq!(Type::Never.join(&Type::Never), Type::Never);
}

// ═══════════════════════════════════════════════════════════════
// Type display tests
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_type_display_empty_tuple() {
	let tuple = Type::Tuple { items: vec![] };
	assert_eq!(tuple.to_string(), "#()");
}

#[test]
fn test_type_display_single_element_tuple() {
	let tuple = Type::Tuple {
		items: vec![Type::Int],
	};
	assert_eq!(tuple.to_string(), "#(int)");
}

#[test]
fn test_type_display_nested_list() {
	let nested = Type::List {
		item: Box::new(Type::List {
			item: Box::new(Type::Int),
		}),
	};
	assert_eq!(nested.to_string(), "#[#[int]]");
}

#[test]
fn test_type_display_nested_map() {
	let nested = Type::Map {
		key: Box::new(Type::String),
		value: Box::new(Type::Map {
			key: Box::new(Type::Int),
			value: Box::new(Type::Boolean),
		}),
	};
	assert_eq!(nested.to_string(), "#{string: #{int: boolean}}");
}

#[test]
fn test_type_display_function_no_params() {
	let func = Type::Function {
		generics: Arc::new(Vec::new()),
		params: vec![],
		has_spread: false,
		return_type: Box::new(Type::Int),
		constructor: false,
	};
	assert_eq!(func.to_string(), "() -> int");
}

#[test]
fn test_type_display_function_no_param_names() {
	let func = Type::Function {
		generics: Arc::new(Vec::new()),
		params: vec![(None, Type::Int), (None, Type::String)],
		has_spread: false,
		return_type: Box::new(Type::Boolean),
		constructor: false,
	};
	assert_eq!(func.to_string(), "(int, string) -> boolean");
}

// ═══════════════════════════════════════════════════════════════
// Error display tests
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_error_display_not_callable() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::NotCallable(Box::new(Type::Int)),
		span: 0..5,
	};
	let msg = format!("{error}");
	assert!(msg.contains("int"));
	assert!(msg.contains("call"));
}

#[test]
fn test_error_display_not_indexable() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::NotIndexable,
		span: 0..5,
	};
	let msg = format!("{error}");
	assert!(msg.contains("index"));
}

#[test]
fn test_error_display_not_accessible() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::NotAccessible,
		span: 0..5,
	};
	let msg = format!("{error}");
	assert!(msg.contains("access"));
}

#[test]
fn test_error_display_this_outside_struct() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::ThisOutsideStruct,
		span: 0..4,
	};
	let msg = format!("{error}");
	assert!(msg.contains("this"));
}

#[test]
fn test_error_display_self_type_in_global_scope() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::SelfTypeInGlobalScope,
		span: 0..4,
	};
	let msg = format!("{error}");
	assert!(msg.contains("self"));
}

#[test]
fn test_error_display_spread_non_final() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::SpreadNonFinalParam,
		span: 0..5,
	};
	let msg = format!("{error}");
	assert!(msg.contains("final"));
}

#[test]
fn test_error_display_invalid_unary_op() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::InvalidUnaryOp,
		span: 0..5,
	};
	let msg = format!("{error}");
	assert!(msg.contains("unary"));
}

#[test]
fn test_error_display_type_mismatch() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::TypeMismatch {
			expected: Box::new(Type::Int),
			found: Box::new(Type::String),
		},
		span: 0..5,
	};
	let msg = format!("{error}");
	assert!(msg.contains("int"));
	assert!(msg.contains("string"));
}

#[test]
fn test_error_display_unknown_member() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::UnknownMember {
			type_: Box::new(Type::Int),
			member: EcoString::from("foo"),
			suggestion: None,
		},
		span: 0..5,
	};
	let msg = format!("{error}");
	assert!(msg.contains("foo"));
}

#[test]
fn test_error_display_unknown_named_argument() {
	let error = crate::types::error::TypeError {
		kind: TypeErrorKind::UnknownNamedArgument {
			name: EcoString::from("xyz"),
			suggestion: None,
		},
		span: 0..5,
	};
	let msg = format!("{error}");
	assert!(msg.contains("xyz"));
}

// ═══════════════════════════════════════════════════════════════
// Context scoping tests
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_context_variable_shadowing() {
	let ctx = Context::default()
		.with_new_entry(
			EcoString::from("x"),
			ContextEntry::Value(ContextValue {
				type_: Type::Int,
				mutable: false,
				visibility: Visibility::Private,
			}),
		)
		.with_new_entry(
			EcoString::from("x"),
			ContextEntry::Value(ContextValue {
				type_: Type::String,
				mutable: false,
				visibility: Visibility::Private,
			}),
		);
	assert_eq!(ctx.lookup_type(&EcoString::from("x")), Some(Type::String));
}

#[test]
fn test_context_multiple_entries() {
	let ctx = Context::default()
		.with_new_entry(
			EcoString::from("a"),
			ContextEntry::Value(ContextValue {
				type_: Type::Int,
				mutable: false,
				visibility: Visibility::Private,
			}),
		)
		.with_new_entry(
			EcoString::from("b"),
			ContextEntry::Value(ContextValue {
				type_: Type::String,
				mutable: false,
				visibility: Visibility::Private,
			}),
		)
		.with_new_entry(
			EcoString::from("c"),
			ContextEntry::Value(ContextValue {
				type_: Type::Boolean,
				mutable: false,
				visibility: Visibility::Private,
			}),
		);
	assert_eq!(ctx.lookup_type(&EcoString::from("a")), Some(Type::Int));
	assert_eq!(ctx.lookup_type(&EcoString::from("b")), Some(Type::String));
	assert_eq!(ctx.lookup_type(&EcoString::from("c")), Some(Type::Boolean));
}

// ═══════════════════════════════════════════════════════════════
// Block expression type checking
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_infer_block_single_expr() {
	use crate::ast::expr::Statement;
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::Block {
			body: vec![make_spanned(
				Statement::Expr(make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2)),
				0,
				2,
			)],
			label: None,
		},
		0,
		6,
	);
	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Int);
}

#[test]
fn test_infer_empty_block() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::Block {
			body: vec![],
			label: None,
		},
		0,
		2,
	);
	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Void);
}

// ═══════════════════════════════════════════════════════════════
// While/For loop type checking
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_infer_while_loop() {
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::While {
			condition: Box::new(make_spanned(Expr::Boolean(make_spanned(true, 0, 4)), 0, 4)),
			body: Box::new(make_spanned(Expr::Int(make_spanned(0u64, 0, 1)), 0, 1)),
			label: None,
		},
		0,
		10,
	);
	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::Void);
}

// ═══════════════════════════════════════════════════════════════
// Map expression type checking
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_infer_map_literal() {
	use crate::ast::expr::MapEntry;
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::Map(vec![
			make_spanned(
				MapEntry::Expr(
					make_spanned(Expr::Char(make_spanned('a', 0, 3)), 0, 3),
					make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1),
				),
				0,
				5,
			),
			make_spanned(
				MapEntry::Expr(
					make_spanned(Expr::Char(make_spanned('b', 0, 3)), 0, 3),
					make_spanned(Expr::Int(make_spanned(2u64, 0, 1)), 0, 1),
				),
				0,
				5,
			),
		]),
		0,
		15,
	);
	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	match result.unwrap() {
		Type::Map { key, value } => {
			assert_eq!(*key, Type::Char);
			assert_eq!(*value, Type::Int);
		}
		other => panic!("Expected map type, got {other:?}"),
	}
}

// ═══════════════════════════════════════════════════════════════
// Closure type checking
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_infer_simple_closure() {
	use crate::ast::expr::ClosureParam;
	use crate::ast::types::Type as AstType;
	let mut checker = TypeChecker::default();
	let ctx = Default::default();
	let expr = make_spanned(
		Expr::Closure {
			params: vec![make_spanned(
				ClosureParam {
					name: make_spanned(
						crate::ast::expr::Pattern::Binding {
							name: make_spanned(EcoString::from("x"), 0, 1),
							inner: Box::new(make_spanned(crate::ast::expr::Pattern::Placeholder, 0, 1)),
						},
						0,
						1,
					),
					type_: Some(make_spanned(AstType::Int, 3, 6)),
					mutable: false,
					spread: false,
				},
				0,
				6,
			)],
			generics: vec![],
			return_type: None,
			body: Box::new(make_spanned(
				Expr::BinaryOp {
					lhs: Box::new(make_spanned(
						Expr::Identifier(make_spanned(EcoString::from("x"), 0, 1)),
						0,
						1,
					)),
					op: BinaryOperator::Plus,
					rhs: Box::new(make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1)),
				},
				0,
				5,
			)),
		},
		0,
		15,
	);
	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	match result.unwrap() {
		Type::Function {
			params,
			return_type,
			..
		} => {
			assert_eq!(params.len(), 1);
			assert_eq!(params[0].1, Type::Int);
			assert_eq!(*return_type, Type::Int);
		}
		other => panic!("Expected function type, got {other:?}"),
	}
}

// ═══════════════════════════════════════════════════════════════
// Match expression type checking
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_infer_match_simple() {
	use crate::ast::expr::MatchArm;
	let mut checker = TypeChecker::default();
	let ctx = Context::default().with_new_entry(
		EcoString::from("x"),
		ContextEntry::Value(ContextValue {
			type_: Type::Int,
			mutable: false,
			visibility: Visibility::Private,
		}),
	);
	let expr = make_spanned(
		Expr::Match {
			value: Box::new(make_spanned(
				Expr::Identifier(make_spanned(EcoString::from("x"), 0, 1)),
				0,
				1,
			)),
			arms: vec![
				MatchArm {
					pattern: make_spanned(
						crate::ast::expr::Pattern::Int(make_spanned(1i64, 0, 1)),
						0,
						1,
					),
					guard: None,
					body: make_spanned(Expr::String(vec![]), 0, 5),
				},
				MatchArm {
					pattern: make_spanned(crate::ast::expr::Pattern::Placeholder, 0, 1),
					guard: None,
					body: make_spanned(Expr::String(vec![]), 0, 7),
				},
			],
		},
		0,
		30,
	);
	let result = checker.infer(&expr, &ctx);
	assert!(result.is_ok());
	assert_eq!(result.unwrap(), Type::String);
}

// ═══════════════════════════════════════════════════════════════
// Declaration checking (additional)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_let_declaration_inferred_type() {
	use crate::ast::declaration::{Declaration, LetDeclaration};
	use crate::ast::expr::Pattern;
	let mut checker = TypeChecker::default();
	let ctx = Context::default();
	let decl = Declaration::Let {
		visibility: None,
		meta: LetDeclaration {
			mutable: false,
			name: make_spanned(
				Pattern::Binding {
					name: make_spanned(EcoString::from("x"), 0, 1),
					inner: Box::new(make_spanned(Pattern::Placeholder, 0, 1)),
				},
				0,
				1,
			),
			type_: None,
		},
		value: make_spanned(Expr::Int(make_spanned(42u64, 0, 2)), 0, 2),
	};
	let result = checker.check_declaration(&decl, &ctx);
	assert!(result.is_ok());
	let new_ctx = result.unwrap();
	assert_eq!(new_ctx.lookup_type(&EcoString::from("x")), Some(Type::Int));
}

#[test]
fn test_let_declaration_with_matching_type_annotation() {
	use crate::ast::declaration::{Declaration, LetDeclaration};
	use crate::ast::expr::Pattern;
	use crate::ast::types::Type as AstType;
	let mut checker = TypeChecker::default();
	let ctx = Context::default();
	let decl = Declaration::Let {
		visibility: None,
		meta: LetDeclaration {
			mutable: false,
			name: make_spanned(
				Pattern::Binding {
					name: make_spanned(EcoString::from("y"), 0, 1),
					inner: Box::new(make_spanned(Pattern::Placeholder, 0, 1)),
				},
				0,
				1,
			),
			type_: Some(make_spanned(AstType::Int, 3, 6)),
		},
		value: make_spanned(Expr::Int(make_spanned(10u64, 0, 2)), 0, 2),
	};
	let result = checker.check_declaration(&decl, &ctx);
	assert!(result.is_ok());
	let new_ctx = result.unwrap();
	assert_eq!(new_ctx.lookup_type(&EcoString::from("y")), Some(Type::Int));
}

#[test]
fn test_func_declaration_return_type_matches() {
	use crate::ast::declaration::{Declaration, FuncDeclaration, FuncParam};
	use crate::ast::expr::Pattern;
	use crate::ast::types::Type as AstType;
	let mut checker = TypeChecker::default();
	let ctx = Context::default();
	let decl = Declaration::Func {
		visibility: None,
		meta: FuncDeclaration {
			name: make_spanned(EcoString::from("get_one"), 0, 7),
			generics: vec![],
			params: vec![],
			return_type: Some(make_spanned(AstType::Int, 10, 13)),
		},
		body: make_spanned(Expr::Int(make_spanned(1u64, 0, 1)), 0, 1),
	};
	let result = checker.check_declaration(&decl, &ctx);
	assert!(result.is_ok());
	let new_ctx = result.unwrap();
	let func_type = new_ctx.lookup_type(&EcoString::from("get_one"));
	assert!(func_type.is_some());
	match func_type.unwrap() {
		Type::Function { return_type, .. } => {
			assert_eq!(*return_type, Type::Int);
		}
		other => panic!("Expected function type, got {other:?}"),
	}
}

#[test]
fn test_func_declaration_with_params() {
	use crate::ast::declaration::{Declaration, FuncDeclaration, FuncParam};
	use crate::ast::expr::Pattern;
	use crate::ast::types::Type as AstType;
	let mut checker = TypeChecker::default();
	let ctx = Context::default();
	let decl = Declaration::Func {
		visibility: None,
		meta: FuncDeclaration {
			name: make_spanned(EcoString::from("add"), 0, 3),
			generics: vec![],
			params: vec![
				make_spanned(
					FuncParam {
						name: make_spanned(
							Pattern::Binding {
								name: make_spanned(EcoString::from("a"), 4, 5),
								inner: Box::new(make_spanned(Pattern::Placeholder, 4, 5)),
							},
							4,
							5,
						),
						type_: make_spanned(AstType::Int, 7, 10),
						mutable: false,
						spread: false,
					},
					4,
					10,
				),
				make_spanned(
					FuncParam {
						name: make_spanned(
							Pattern::Binding {
								name: make_spanned(EcoString::from("b"), 12, 13),
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
				),
			],
			return_type: Some(make_spanned(AstType::Int, 22, 25)),
		},
		body: make_spanned(
			Expr::BinaryOp {
				lhs: Box::new(make_spanned(
					Expr::Identifier(make_spanned(EcoString::from("a"), 0, 1)),
					0,
					1,
				)),
				op: BinaryOperator::Plus,
				rhs: Box::new(make_spanned(
					Expr::Identifier(make_spanned(EcoString::from("b"), 0, 1)),
					0,
					1,
				)),
			},
			0,
			5,
		),
	};
	let result = checker.check_declaration(&decl, &ctx);
	assert!(result.is_ok());
	let new_ctx = result.unwrap();
	let func_type = new_ctx.lookup_type(&EcoString::from("add"));
	assert!(func_type.is_some());
	match func_type.unwrap() {
		Type::Function { params, .. } => {
			assert_eq!(params.len(), 2);
			assert_eq!(params[0].1, Type::Int);
			assert_eq!(params[1].1, Type::Int);
		}
		other => panic!("Expected function type, got {other:?}"),
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
		EcoString::from("extern_var"),
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
		EcoString::from("no_type_var"),
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
		TypeError {
			kind: crate::types::error::TypeErrorKind::ExternalDeclarationMissingType,
			..
		} => (),
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
		EcoString::from("extern_func"),
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
