use ecow::EcoString;
use ordered_float::OrderedFloat;

use crate::{
	ast::{
		Span, SpannedExt,
		declaration::{
			Declaration, FuncDeclaration, FuncParam, LetDeclaration, Module, StructField,
			Visibility,
		},
		expr::{
			CallArg, ClosureParam, Expr, ListItem, MapEntry, Pattern, Statement as NymphStatement,
			StringPart,
		},
		ops::{AssignOperator, BinaryOperator, PrefixOperator},
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
	let code = emit_single_expr(Expr::String(vec![StringPart::Text(EcoString::from(
		"hello",
	))
	.spanned(S)]));
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
	let code = emit_single_expr(Expr::Map(vec![MapEntry::Expr(
		Expr::String(vec![StringPart::Text(EcoString::from("a")).spanned(S)]).spanned(S),
		Expr::Int(1u64.spanned(S)).spanned(S),
	)
	.spanned(S)]));
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
		params: vec![ClosureParam {
			name: Pattern::Binding {
				name: ident("n"),
				inner: Box::new(Pattern::Placeholder.spanned(S)),
			}
			.spanned(S),
			type_: None,
			mutable: false,
			spread: false,
		}
		.spanned(S)],
		generics: vec![],
		return_type: None,
		body: Box::new(Expr::Identifier(ident("n")).spanned(S)),
	});
	assert_eq!(code, "const x = (n) => ;\n");
}

// ───────────────── call expressions ─────────────────

#[test]
fn test_call_expr() {
	let code = emit_single_expr(Expr::Call {
		func: Box::new(Expr::Identifier(ident("foo")).spanned(S)),
		generics: vec![],
		args: vec![CallArg {
			value: Expr::Int(1u64.spanned(S)).spanned(S),
			name: None,
			spread: false,
		}
		.spanned(S)],
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
