use ecow::EcoString;
use ordered_float::OrderedFloat;

use crate::{
	ast::{
		Span, SpannedExt,
		declaration::{
			Declaration, EnumVariant, FuncDeclaration, FuncParam, ImplMember, LetDeclaration, Module,
			StructField, StructInnerMember, Visibility,
		},
		expr::{
			CallArg, ClosureParam, Expr, ListItem, MapEntry, MatchArm, Pattern, RangeKind,
			Statement as NymphStatement, StringEscape, StringPart,
		},
		ops::{AssignOperator, BinaryOperator, PatternOperator, PrefixOperator},
		types::Type,
	},
	transpiler::transpile,
	types::Context,
};

const S: Span = Span { start: 0, end: 0 };

fn ident(name: &str) -> crate::ast::Ident {
	EcoString::from(name).spanned(S)
}

fn emit(module: &Module) -> String {
	transpile(module, &Context::default(), None).code
}

fn emit_decls(decls: Vec<Declaration>) -> String {
	emit(&Module {
		members: decls,
		path: EcoString::from("test"),
	})
}

fn emit_single_expr(expr: Expr) -> String {
	emit_decls(vec![Declaration::Let {
		visibility: None,
		meta: LetDeclaration {
			mutable: false,
			name: Pattern::Binding {
				name: ident("x"),
				inner: Box::new(Pattern::Placeholder.spanned(S)),
			}
			.spanned(S),
			type_: None,
		},
		value: expr.spanned(S),
	}])
}

// ───────────────── literal expressions ─────────────────

#[test]
fn test_int_literal() {
	let code = emit_single_expr(Expr::Int(42u64.spanned(S)));
	assert_eq!(code, "const x = 42;\n");
}

#[test]
fn test_float_literal() {
	let code = emit_single_expr(Expr::Float(OrderedFloat(3.16).spanned(S)));
	assert_eq!(code, "const x = 3.16;\n");
}

#[test]
fn test_boolean_literal() {
	let code = emit_single_expr(Expr::Boolean(true.spanned(S)));
	assert_eq!(code, "const x = true;\n");
}

#[test]
fn test_string_literal() {
	let code = emit_single_expr(Expr::String(vec![
		StringPart::Text(EcoString::from("hello")).spanned(S),
	]));
	assert_eq!(code, "const x = __nymph_str('hello');\n");
}

#[test]
fn test_identifier() {
	let code = emit_single_expr(Expr::Identifier(ident("foo")));
	assert_eq!(code, "const x = foo;\n");
}

// ───────────────── let declarations ─────────────────

#[test]
fn test_let_mutable() {
	let code = emit_decls(vec![Declaration::Let {
		visibility: None,
		meta: LetDeclaration {
			mutable: true,
			name: Pattern::Binding {
				name: ident("y"),
				inner: Box::new(Pattern::Placeholder.spanned(S)),
			}
			.spanned(S),
			type_: None,
		},
		value: Expr::Int(10u64.spanned(S)).spanned(S),
	}]);
	assert_eq!(code, "let y = 10;\n");
}

#[test]
fn test_let_exported() {
	let code = emit_decls(vec![Declaration::Let {
		visibility: Some(Visibility::Public),
		meta: LetDeclaration {
			mutable: false,
			name: Pattern::Binding {
				name: ident("z"),
				inner: Box::new(Pattern::Placeholder.spanned(S)),
			}
			.spanned(S),
			type_: None,
		},
		value: Expr::Int(1u64.spanned(S)).spanned(S),
	}]);
	assert_eq!(code, "export const z = 1;\n");
}

// ───────────────── function declarations ─────────────────

#[test]
fn test_func_no_params() {
	let code = emit_decls(vec![Declaration::Func {
		visibility: None,
		meta: FuncDeclaration {
			name: ident("greet"),
			generics: vec![],
			params: vec![],
			return_type: None,
		},
		body: Expr::String(vec![StringPart::Text(EcoString::from("hi")).spanned(S)]).spanned(S),
	}]);
	assert_eq!(code, "function greet() {\n\treturn __nymph_str('hi');\n}\n");
}

#[test]
fn test_func_with_params() {
	let code = emit_decls(vec![Declaration::Func {
		visibility: None,
		meta: FuncDeclaration {
			name: ident("add"),
			generics: vec![],
			params: vec![
				FuncParam {
					name: Pattern::Binding {
						name: ident("a"),
						inner: Box::new(Pattern::Placeholder.spanned(S)),
					}
					.spanned(S),
					type_: Type::Int.spanned(S),
					mutable: false,
					spread: false,
				}
				.spanned(S),
				FuncParam {
					name: Pattern::Binding {
						name: ident("b"),
						inner: Box::new(Pattern::Placeholder.spanned(S)),
					}
					.spanned(S),
					type_: Type::Int.spanned(S),
					mutable: false,
					spread: false,
				}
				.spanned(S),
			],
			return_type: None,
		},
		body: Expr::BinaryOp {
			lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
			op: BinaryOperator::Plus,
			rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
		}
		.spanned(S),
	}]);
	assert_eq!(code, "function add(a, b) {\n\treturn a.plus(b);\n}\n");
}

#[test]
fn test_func_exported() {
	let code = emit_decls(vec![Declaration::Func {
		visibility: Some(Visibility::Public),
		meta: FuncDeclaration {
			name: ident("run"),
			generics: vec![],
			params: vec![],
			return_type: None,
		},
		body: Expr::Int(0u64.spanned(S)).spanned(S),
	}]);
	assert_eq!(code, "export function run() {\n\treturn 0;\n}\n");
}

// ───────────────── collection literals ─────────────────

#[test]
fn test_list_literal() {
	let code = emit_single_expr(Expr::List(vec![
		ListItem::Expr(Expr::Int(1u64.spanned(S)).spanned(S)).spanned(S),
		ListItem::Expr(Expr::Int(2u64.spanned(S)).spanned(S)).spanned(S),
	]));
	assert_eq!(code, "const x = [1, 2];\n");
}

#[test]
fn test_tuple_literal() {
	let code = emit_single_expr(Expr::Tuple(vec![
		ListItem::Expr(Expr::Int(1u64.spanned(S)).spanned(S)).spanned(S),
		ListItem::Expr(Expr::Boolean(true.spanned(S)).spanned(S)).spanned(S),
	]));
	assert_eq!(code, "const x = [1, true];\n");
}

#[test]
fn test_map_literal() {
	let code = emit_single_expr(Expr::Map(vec![
		MapEntry::Expr(
			Expr::String(vec![StringPart::Text(EcoString::from("a")).spanned(S)]).spanned(S),
			Expr::Int(1u64.spanned(S)).spanned(S),
		)
		.spanned(S),
	]));
	assert_eq!(code, "const x = new Map([[__nymph_str('a'), 1]]);\n");
}

// ───────────────── binary operators ─────────────────

#[test]
fn test_pipe_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Int(5u64.spanned(S)).spanned(S)),
		op: BinaryOperator::Pipe,
		rhs: Box::new(Expr::Identifier(ident("double")).spanned(S)),
	});
	assert_eq!(code, "const x = double(5);\n");
}

#[test]
fn test_pipe_operator_with_anonymous_rhs() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Int(1u64.spanned(S)).spanned(S)),
		op: BinaryOperator::Pipe,
		rhs: Box::new(
			Expr::BinaryOp {
				lhs: Box::new(Expr::AnonymousParam(None).spanned(S)),
				op: BinaryOperator::Times,
				rhs: Box::new(Expr::Int(2u64.spanned(S)).spanned(S)),
			}
			.spanned(S),
		),
	});
	assert_eq!(
		code,
		"const x = ((__anon_param_0) => {\n\treturn __anon_param_0.times(2);\n})(1);\n"
	);
}

#[test]
fn test_anonymous_param_expression_emits_arrow_function() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::AnonymousParam(Some(1)).spanned(S)),
		op: BinaryOperator::Plus,
		rhs: Box::new(Expr::AnonymousParam(None).spanned(S)),
	});
	assert_eq!(
		code,
		"const x = (__anon_param_0, __anon_param_1) => {\n\treturn __anon_param_1.plus(__anon_param_0);\n};\n"
	);
}

#[test]
fn test_in_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Int(1u64.spanned(S)).spanned(S)),
		op: BinaryOperator::In,
		rhs: Box::new(Expr::Identifier(ident("list")).spanned(S)),
	});
	assert_eq!(code, "const x = list.contains(1);\n");
}

#[test]
fn test_not_in_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Int(1u64.spanned(S)).spanned(S)),
		op: BinaryOperator::NotIn,
		rhs: Box::new(Expr::Identifier(ident("list")).spanned(S)),
	});
	assert_eq!(code, "const x = !list.contains(1);\n");
}

#[test]
fn test_range_literal_emits_range_object() {
	let code = emit_single_expr(Expr::Range(RangeKind::Exclusive {
		min: Box::new(Expr::Int(1u64.spanned(S)).spanned(S)),
		max: Box::new(Expr::Int(10u64.spanned(S)).spanned(S)),
	}));

	assert!(code.contains("start: 1"));
	assert!(code.contains("end: 10"));
	assert!(code.contains("contains: function(item)"));
	assert!(code.contains("is_empty: function()"));
	assert!(code.contains("into: function()"));
}

// ───────────────── prefix operator ─────────────────

#[test]
fn test_prefix_negate() {
	let code = emit_single_expr(Expr::PrefixOp {
		op: PrefixOperator::Negate,
		value: Box::new(Expr::Identifier(ident("n")).spanned(S)),
	});
	assert_eq!(code, "const x = n.negate();\n");
}

// ───────────────── closures ─────────────────

#[test]
fn test_closure() {
	let code = emit_single_expr(Expr::Closure {
		params: vec![
			ClosureParam {
				name: Pattern::Binding {
					name: ident("n"),
					inner: Box::new(Pattern::Placeholder.spanned(S)),
				}
				.spanned(S),
				type_: None,
				mutable: false,
				spread: false,
			}
			.spanned(S),
		],
		generics: vec![],
		return_type: None,
		body: Box::new(Expr::Identifier(ident("n")).spanned(S)),
	});
	assert_eq!(code, "const x = (n) => {\n\treturn n;\n};\n");
}

// ───────────────── call expressions ─────────────────

#[test]
fn test_call_expr() {
	let code = emit_single_expr(Expr::Call {
		func: Box::new(Expr::Identifier(ident("foo")).spanned(S)),
		generics: vec![],
		args: vec![
			CallArg {
				value: Expr::Int(1u64.spanned(S)).spanned(S),
				name: None,
				spread: false,
			}
			.spanned(S),
		],
	});
	assert_eq!(code, "const x = foo(1);\n");
}

// ───────────────── member access ─────────────────

#[test]
fn test_member_access() {
	let code = emit_single_expr(Expr::MemberAccess {
		parent: Box::new(Expr::Identifier(ident("obj")).spanned(S)),
		member: ident("field"),
		optional: false,
	});
	assert_eq!(code, "const x = obj.field;\n");
}

// ───────────────── control flow ─────────────────

#[test]
fn test_if_expression() {
	let code = emit_single_expr(Expr::If {
		condition: Box::new(Expr::Boolean(true.spanned(S)).spanned(S)),
		then: Box::new(Expr::Int(1u64.spanned(S)).spanned(S)),
		otherwise: Some(Box::new(Expr::Int(2u64.spanned(S)).spanned(S))),
	});
	assert_eq!(code, "const x = true ? 1 : 2;\n");
}

#[test]
fn test_while_loop() {
	let code = emit_single_expr(Expr::While {
		condition: Box::new(Expr::Boolean(true.spanned(S)).spanned(S)),
		body: Box::new(Expr::Int(0u64.spanned(S)).spanned(S)),
		label: None,
	});
	assert!(code.contains("while"));
	assert!(code.contains("true"));
}

#[test]
fn test_for_loop() {
	let code = emit_single_expr(Expr::For {
		variable: Pattern::Binding {
			name: ident("i"),
			inner: Box::new(Pattern::Placeholder.spanned(S)),
		}
		.spanned(S),
		iterable: Box::new(Expr::Identifier(ident("items")).spanned(S)),
		body: Box::new(Expr::Identifier(ident("i")).spanned(S)),
		label: None,
	});
	assert!(code.contains("for"));
	assert!(code.contains("of"));
	assert!(code.contains("items"));
}

// ───────────────── assignment ─────────────────

#[test]
fn test_simple_assignment() {
	let code = emit_single_expr(Expr::AssignOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: AssignOperator::Assign,
		rhs: Box::new(Expr::Int(5u64.spanned(S)).spanned(S)),
	});
	assert_eq!(code, "const x = a = 5;\n");
}

// ───────────────── this ─────────────────

#[test]
fn test_this() {
	let code = emit_single_expr(Expr::This);
	assert_eq!(code, "const x = this;\n");
}

// ───────────────── break / continue ─────────────────

#[test]
fn test_break_no_label() {
	let code = emit_single_expr(Expr::Break {
		value: None,
		label: None,
	});
	assert!(code.contains("break;"));
}

#[test]
fn test_continue_no_label() {
	let code = emit_single_expr(Expr::Continue { label: None });
	assert!(code.contains("continue;"));
}

// ───────────────── return ─────────────────

#[test]
fn test_return_with_value() {
	let code = emit_single_expr(Expr::Return {
		value: Some(Box::new(Expr::Int(42u64.spanned(S)).spanned(S))),
		label: None,
	});
	assert!(code.contains("return 42"));
}

#[test]
fn test_return_no_value() {
	let code = emit_single_expr(Expr::Return {
		value: None,
		label: None,
	});
	assert!(code.contains("return;"));
}

// ───────────────── struct declarations ─────────────────

#[test]
fn test_struct_declaration() {
	let code = emit_decls(vec![Declaration::Struct {
		visibility: None,
		name: ident("Point"),
		generics: vec![],
		fields: vec![
			StructField {
				visibility: None,
				name: ident("x"),
				type_: Type::Int.spanned(S),
				default: None,
			}
			.spanned(S),
			StructField {
				visibility: None,
				name: ident("y"),
				type_: Type::Int.spanned(S),
				default: None,
			}
			.spanned(S),
		],
		members: vec![],
	}]);
	assert!(code.contains("class Point"));
	assert!(code.contains("constructor"));
	assert!(code.contains("this.x = x"));
	assert!(code.contains("this.y = y"));
}

// ───────────────── block expressions ─────────────────

#[test]
fn test_block_with_statements() {
	let code = emit_single_expr(Expr::Block {
		body: vec![
			NymphStatement::Let {
				meta: LetDeclaration {
					mutable: false,
					name: Pattern::Binding {
						name: ident("a"),
						inner: Box::new(Pattern::Placeholder.spanned(S)),
					}
					.spanned(S),
					type_: None,
				},
				value: Expr::Int(1u64.spanned(S)).spanned(S),
			}
			.spanned(S),
			NymphStatement::Expr(Expr::Identifier(ident("a")).spanned(S)).spanned(S),
		],
		label: None,
	});
	// Block should produce an IIFE
	assert!(code.contains("const a = 1"));
	assert!(code.contains("return a"));
}

// ───────────────── type alias erasure ─────────────────

#[test]
fn test_type_alias_erased() {
	let code = emit_decls(vec![Declaration::TypeAlias {
		visibility: None,
		meta: crate::ast::declaration::TypeAliasDeclaration {
			name: ident("Num"),
			generics: vec![],
		},
		value: Type::Int.spanned(S),
	}]);
	assert_eq!(code, "");
}

// ───────────────── multiple declarations ─────────────────

#[test]
fn test_multiple_declarations() {
	let code = emit_decls(vec![
		Declaration::Let {
			visibility: None,
			meta: LetDeclaration {
				mutable: false,
				name: Pattern::Binding {
					name: ident("a"),
					inner: Box::new(Pattern::Placeholder.spanned(S)),
				}
				.spanned(S),
				type_: None,
			},
			value: Expr::Int(1u64.spanned(S)).spanned(S),
		},
		Declaration::Let {
			visibility: None,
			meta: LetDeclaration {
				mutable: false,
				name: Pattern::Binding {
					name: ident("b"),
					inner: Box::new(Pattern::Placeholder.spanned(S)),
				}
				.spanned(S),
				type_: None,
			},
			value: Expr::Int(2u64.spanned(S)).spanned(S),
		},
	]);
	assert_eq!(code, "const a = 1;\nconst b = 2;\n");
}

// ═══════════════════════════════════════════════════════════════
// Binary operator method calls
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_minus_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::Minus,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.minus(b);\n");
}

#[test]
fn test_times_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::Times,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.times(b);\n");
}

#[test]
fn test_divide_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::Divide,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.divide(b);\n");
}

#[test]
fn test_remainder_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::Remainder,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.remainder(b);\n");
}

#[test]
fn test_power_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::Power,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.power(b);\n");
}

#[test]
fn test_bit_and_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::BitAnd,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.bit_and(b);\n");
}

#[test]
fn test_bit_or_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::BitOr,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.bit_or(b);\n");
}

#[test]
fn test_bit_xor_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::BitXor,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.bit_xor(b);\n");
}

#[test]
fn test_left_shift_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::LeftShift,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.shl(b);\n");
}

#[test]
fn test_right_shift_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::RightShift,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.shr(b);\n");
}

#[test]
fn test_equals_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::Equals,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.equals(b);\n");
}

#[test]
fn test_not_equals_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::NotEquals,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.not_equals(b);\n");
}

#[test]
fn test_less_than_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::LessThan,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.less_than(b);\n");
}

#[test]
fn test_less_than_equals_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::LessThanEquals,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.less_than_eq(b);\n");
}

#[test]
fn test_greater_than_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::GreaterThan,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.greater_than(b);\n");
}

#[test]
fn test_greater_than_equals_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::GreaterThanEquals,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.greater_than_eq(b);\n");
}

#[test]
fn test_bool_and_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::BoolAnd,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.and(b);\n");
}

#[test]
fn test_bool_or_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::BoolOr,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert_eq!(code, "const x = a.or(b);\n");
}

#[test]
fn test_unwrap_operator() {
	let code = emit_single_expr(Expr::BinaryOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: BinaryOperator::Unwrap,
		rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
	});
	assert!(code.contains("unwrap_or"));
}

// ═══════════════════════════════════════════════════════════════
// Prefix operators
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_prefix_bool_not() {
	let code = emit_single_expr(Expr::PrefixOp {
		op: PrefixOperator::BoolNot,
		value: Box::new(Expr::Identifier(ident("x")).spanned(S)),
	});
	assert_eq!(code, "const x = x.not();\n");
}

#[test]
fn test_prefix_bit_not() {
	let code = emit_single_expr(Expr::PrefixOp {
		op: PrefixOperator::BitNot,
		value: Box::new(Expr::Identifier(ident("x")).spanned(S)),
	});
	assert_eq!(code, "const x = x.bit_not();\n");
}

// ═══════════════════════════════════════════════════════════════
// Compound assignment operators
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_plus_assign() {
	let code = emit_single_expr(Expr::AssignOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: AssignOperator::PlusAssign,
		rhs: Box::new(Expr::Int(1u64.spanned(S)).spanned(S)),
	});
	assert!(code.contains("a.plus(1)"));
}

#[test]
fn test_minus_assign() {
	let code = emit_single_expr(Expr::AssignOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: AssignOperator::MinusAssign,
		rhs: Box::new(Expr::Int(1u64.spanned(S)).spanned(S)),
	});
	assert!(code.contains("a.minus(1)"));
}

#[test]
fn test_times_assign() {
	let code = emit_single_expr(Expr::AssignOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: AssignOperator::TimesAssign,
		rhs: Box::new(Expr::Int(2u64.spanned(S)).spanned(S)),
	});
	assert!(code.contains("a.times(2)"));
}

#[test]
fn test_divide_assign() {
	let code = emit_single_expr(Expr::AssignOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: AssignOperator::DivideAssign,
		rhs: Box::new(Expr::Int(2u64.spanned(S)).spanned(S)),
	});
	assert!(code.contains("a.divide(2)"));
}

#[test]
fn test_power_assign() {
	let code = emit_single_expr(Expr::AssignOp {
		lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
		op: AssignOperator::PowerAssign,
		rhs: Box::new(Expr::Int(2u64.spanned(S)).spanned(S)),
	});
	assert!(code.contains("a.power(2)"));
}

// ═══════════════════════════════════════════════════════════════
// Collection literals
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_empty_list() {
	let code = emit_single_expr(Expr::List(vec![]));
	assert_eq!(code, "const x = [];\n");
}

#[test]
fn test_empty_tuple() {
	let code = emit_single_expr(Expr::Tuple(vec![]));
	assert_eq!(code, "const x = [];\n");
}

#[test]
fn test_empty_map() {
	let code = emit_single_expr(Expr::Map(vec![]));
	assert_eq!(code, "const x = new Map([]);\n");
}

#[test]
fn test_list_with_spread() {
	let code = emit_single_expr(Expr::List(vec![
		ListItem::Expr(Expr::Int(1u64.spanned(S)).spanned(S)).spanned(S),
		ListItem::Spread(Expr::Identifier(ident("rest")).spanned(S)).spanned(S),
	]));
	assert!(code.contains("1"));
	assert!(code.contains("rest"));
}

#[test]
fn test_map_multiple_entries() {
	let code = emit_single_expr(Expr::Map(vec![
		MapEntry::Expr(
			Expr::String(vec![StringPart::Text(EcoString::from("a")).spanned(S)]).spanned(S),
			Expr::Int(1u64.spanned(S)).spanned(S),
		)
		.spanned(S),
		MapEntry::Expr(
			Expr::String(vec![StringPart::Text(EcoString::from("b")).spanned(S)]).spanned(S),
			Expr::Int(2u64.spanned(S)).spanned(S),
		)
		.spanned(S),
	]));
	assert!(code.contains("new Map"));
	assert!(code.contains("1"));
	assert!(code.contains("2"));
}

// ═══════════════════════════════════════════════════════════════
// String expressions
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_empty_string() {
	let code = emit_single_expr(Expr::String(vec![]));
	assert_eq!(code, "const x = __nymph_str('');\n");
}

#[test]
fn test_string_with_interpolation() {
	let code = emit_single_expr(Expr::String(vec![
		StringPart::Text(EcoString::from("Value: ")).spanned(S),
		StringPart::InterpolatedExpr(Expr::Identifier(ident("x")).spanned(S)).spanned(S),
	]));
	assert!(code.contains("Value: "));
	assert!(code.contains("x"));
}

#[test]
fn test_string_with_escape() {
	let code = emit_single_expr(Expr::String(vec![
		StringPart::Text(EcoString::from("hello")).spanned(S),
		StringPart::EscapeSequence(crate::ast::expr::StringEscape::Newline).spanned(S),
		StringPart::Text(EcoString::from("world")).spanned(S),
	]));
	assert!(code.contains("hello"));
	assert!(code.contains("world"));
}

// ═══════════════════════════════════════════════════════════════
// Control flow
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_if_without_else() {
	let code = emit_single_expr(Expr::If {
		condition: Box::new(Expr::Boolean(true.spanned(S)).spanned(S)),
		then: Box::new(Expr::Int(1u64.spanned(S)).spanned(S)),
		otherwise: None,
	});
	assert!(code.contains("true"));
	assert!(code.contains("1"));
	assert!(code.contains("undefined"));
}

#[test]
fn test_break_with_label() {
	let code = emit_single_expr(Expr::Break {
		value: None,
		label: Some(ident("outer")),
	});
	assert!(code.contains("break outer"));
}

#[test]
fn test_continue_with_label() {
	let code = emit_single_expr(Expr::Continue {
		label: Some(ident("outer")),
	});
	assert!(code.contains("continue outer"));
}

// ═══════════════════════════════════════════════════════════════
// Member & index access
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_optional_member_access() {
	let code = emit_single_expr(Expr::MemberAccess {
		parent: Box::new(Expr::Identifier(ident("obj")).spanned(S)),
		member: ident("field"),
		optional: true,
	});
	assert!(code.contains("obj"));
	assert!(code.contains("field"));
}

#[test]
fn test_chained_member_access() {
	let code = emit_single_expr(Expr::MemberAccess {
		parent: Box::new(
			Expr::MemberAccess {
				parent: Box::new(Expr::Identifier(ident("a")).spanned(S)),
				member: ident("b"),
				optional: false,
			}
			.spanned(S),
		),
		member: ident("c"),
		optional: false,
	});
	assert_eq!(code, "const x = a.b.c;\n");
}

#[test]
fn test_index_access() {
	let code = emit_single_expr(Expr::IndexAccess {
		parent: Box::new(Expr::Identifier(ident("arr")).spanned(S)),
		index: Box::new(Expr::Int(0u64.spanned(S)).spanned(S)),
		optional: false,
	});
	assert!(code.contains("arr"));
	assert!(code.contains("0"));
}

// ═══════════════════════════════════════════════════════════════
// Call expressions
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_call_multiple_args() {
	let code = emit_single_expr(Expr::Call {
		func: Box::new(Expr::Identifier(ident("foo")).spanned(S)),
		generics: vec![],
		args: vec![
			CallArg {
				value: Expr::Int(1u64.spanned(S)).spanned(S),
				name: None,
				spread: false,
			}
			.spanned(S),
			CallArg {
				value: Expr::Int(2u64.spanned(S)).spanned(S),
				name: None,
				spread: false,
			}
			.spanned(S),
			CallArg {
				value: Expr::Int(3u64.spanned(S)).spanned(S),
				name: None,
				spread: false,
			}
			.spanned(S),
		],
	});
	assert_eq!(code, "const x = foo(1, 2, 3);\n");
}

#[test]
fn test_call_with_spread() {
	let code = emit_single_expr(Expr::Call {
		func: Box::new(Expr::Identifier(ident("foo")).spanned(S)),
		generics: vec![],
		args: vec![
			CallArg {
				value: Expr::Identifier(ident("args")).spanned(S),
				name: None,
				spread: true,
			}
			.spanned(S),
		],
	});
	assert!(code.contains("...args"));
}

// ═══════════════════════════════════════════════════════════════
// Closure expressions
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_closure_no_params() {
	let code = emit_single_expr(Expr::Closure {
		params: vec![],
		generics: vec![],
		return_type: None,
		body: Box::new(Expr::Int(42u64.spanned(S)).spanned(S)),
	});
	assert!(code.contains("() =>"));
	assert!(code.contains("42"));
}

#[test]
fn test_closure_multiple_params() {
	let code = emit_single_expr(Expr::Closure {
		params: vec![
			ClosureParam {
				name: Pattern::Binding {
					name: ident("a"),
					inner: Box::new(Pattern::Placeholder.spanned(S)),
				}
				.spanned(S),
				type_: None,
				mutable: false,
				spread: false,
			}
			.spanned(S),
			ClosureParam {
				name: Pattern::Binding {
					name: ident("b"),
					inner: Box::new(Pattern::Placeholder.spanned(S)),
				}
				.spanned(S),
				type_: None,
				mutable: false,
				spread: false,
			}
			.spanned(S),
		],
		generics: vec![],
		return_type: None,
		body: Box::new(
			Expr::BinaryOp {
				lhs: Box::new(Expr::Identifier(ident("a")).spanned(S)),
				op: BinaryOperator::Plus,
				rhs: Box::new(Expr::Identifier(ident("b")).spanned(S)),
			}
			.spanned(S),
		),
	});
	assert!(code.contains("(a, b) =>"));
	assert!(code.contains("a.plus(b)"));
}

// ═══════════════════════════════════════════════════════════════
// Placeholder & Grouped
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_placeholder() {
	let code = emit_single_expr(Expr::Placeholder);
	assert_eq!(code, "const x = undefined;\n");
}

#[test]
fn test_grouped() {
	let code = emit_single_expr(Expr::Grouped(Box::new(
		Expr::Int(42u64.spanned(S)).spanned(S),
	)));
	assert_eq!(code, "const x = 42;\n");
}

// ═══════════════════════════════════════════════════════════════
// Pattern ops (is / !is)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_is_pattern_placeholder() {
	let code = emit_single_expr(Expr::PatternOp {
		lhs: Box::new(Expr::Identifier(ident("val")).spanned(S)),
		op: crate::ast::ops::PatternOperator::Is,
		rhs: Pattern::Placeholder.spanned(S),
	});
	assert!(code.contains("true"));
}

#[test]
fn test_not_is_pattern() {
	let code = emit_single_expr(Expr::PatternOp {
		lhs: Box::new(Expr::Identifier(ident("val")).spanned(S)),
		op: crate::ast::ops::PatternOperator::NotIs,
		rhs: Pattern::Placeholder.spanned(S),
	});
	assert!(code.contains("!"));
}

// ═══════════════════════════════════════════════════════════════
// Match expressions
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_match_with_arms() {
	use crate::ast::expr::MatchArm;
	let code = emit_single_expr(Expr::Match {
		value: Box::new(Expr::Identifier(ident("val")).spanned(S)),
		arms: vec![
			MatchArm {
				pattern: Pattern::Int(42i64.spanned(S)).spanned(S),
				guard: None,
				body: Expr::String(vec![StringPart::Text(EcoString::from("found")).spanned(S)]).spanned(S),
			},
			MatchArm {
				pattern: Pattern::Placeholder.spanned(S),
				guard: None,
				body: Expr::String(vec![StringPart::Text(EcoString::from("other")).spanned(S)]).spanned(S),
			},
		],
	});
	assert!(code.contains("if"));
	assert!(code.contains("42"));
	assert!(code.contains("found"));
	assert!(code.contains("other"));
}

// ═══════════════════════════════════════════════════════════════
// Block expressions
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_block_single_expression() {
	let code = emit_single_expr(Expr::Block {
		body: vec![NymphStatement::Expr(Expr::Int(42u64.spanned(S)).spanned(S)).spanned(S)],
		label: None,
	});
	assert!(code.contains("42"));
}

#[test]
fn test_block_nested() {
	let code = emit_single_expr(Expr::Block {
		body: vec![
			NymphStatement::Expr(
				Expr::Block {
					body: vec![NymphStatement::Expr(Expr::Int(1u64.spanned(S)).spanned(S)).spanned(S)],
					label: None,
				}
				.spanned(S),
			)
			.spanned(S),
		],
		label: None,
	});
	assert!(code.contains("1"));
}

// ═══════════════════════════════════════════════════════════════
// Struct declarations (transpiler)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_struct_exported() {
	let code = emit_decls(vec![Declaration::Struct {
		visibility: Some(Visibility::Public),
		name: ident("Vec2"),
		generics: vec![],
		fields: vec![
			StructField {
				visibility: None,
				name: ident("x"),
				type_: Type::Float.spanned(S),
				default: None,
			}
			.spanned(S),
			StructField {
				visibility: None,
				name: ident("y"),
				type_: Type::Float.spanned(S),
				default: None,
			}
			.spanned(S),
		],
		members: vec![],
	}]);
	assert!(code.contains("export"));
	assert!(code.contains("class Vec2"));
	assert!(code.contains("constructor"));
}

// ═══════════════════════════════════════════════════════════════
// Func declarations (transpiler)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_func_with_block_body() {
	let code = emit_decls(vec![Declaration::Func {
		visibility: None,
		meta: FuncDeclaration {
			name: ident("compute"),
			generics: vec![],
			params: vec![
				FuncParam {
					name: Pattern::Binding {
						name: ident("n"),
						inner: Box::new(Pattern::Placeholder.spanned(S)),
					}
					.spanned(S),
					type_: Type::Int.spanned(S),
					mutable: false,
					spread: false,
				}
				.spanned(S),
			],
			return_type: None,
		},
		body: Expr::Block {
			body: vec![
				NymphStatement::Let {
					meta: LetDeclaration {
						mutable: false,
						name: Pattern::Binding {
							name: ident("result"),
							inner: Box::new(Pattern::Placeholder.spanned(S)),
						}
						.spanned(S),
						type_: None,
					},
					value: Expr::BinaryOp {
						lhs: Box::new(Expr::Identifier(ident("n")).spanned(S)),
						op: BinaryOperator::Times,
						rhs: Box::new(Expr::Int(2u64.spanned(S)).spanned(S)),
					}
					.spanned(S),
				}
				.spanned(S),
				NymphStatement::Expr(Expr::Identifier(ident("result")).spanned(S)).spanned(S),
			],
			label: None,
		}
		.spanned(S),
	}]);
	assert!(code.contains("function compute(n)"));
	assert!(code.contains("const result"));
	assert!(code.contains("n.times(2)"));
	assert!(code.contains("return result"));
}

// ═══════════════════════════════════════════════════════════════
// Let declarations (additional)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_let_mutable_exported() {
	let code = emit_decls(vec![Declaration::Let {
		visibility: Some(Visibility::Public),
		meta: LetDeclaration {
			mutable: true,
			name: Pattern::Binding {
				name: ident("counter"),
				inner: Box::new(Pattern::Placeholder.spanned(S)),
			}
			.spanned(S),
			type_: None,
		},
		value: Expr::Int(0u64.spanned(S)).spanned(S),
	}]);
	assert_eq!(code, "export let counter = 0;\n");
}
