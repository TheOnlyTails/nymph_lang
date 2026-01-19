use chumsky::Parser;

use crate::{
	ast::{
		Spanned,
		declaration::{Declaration, ImportRoot, Visibility},
		expr::{Expr, Pattern},
		ops::{AssignOperator, BinaryOperator, PrefixOperator},
		types::Type,
	},
	lexer::lexer,
};

use super::parse;

fn parse_module(
	source: &str,
) -> (
	crate::ast::declaration::Module,
	Vec<super::error::ParseError>,
) {
	let tokens = lexer().parse(source).unwrap();
	let eoi = crate::ast::Span::new(source.len(), source.len());
	let (Spanned(module, _), errors) = parse(&tokens, eoi, "test.nym".into());
	(module, errors)
}

fn parse_expr(source: &str) -> Option<Spanned<Expr>> {
	let tokens = lexer().parse(source).unwrap();
	let eoi = crate::ast::Span::new(source.len(), source.len());
	let mut parser = super::core::Parser::new(&tokens, eoi, "test.nym".into());
	parser.parse_expression()
}

fn parse_type(source: &str) -> Option<Spanned<Type>> {
	let tokens = lexer().parse(source).unwrap();
	let eoi = crate::ast::Span::new(source.len(), source.len());
	let mut parser = super::core::Parser::new(&tokens, eoi, "test.nym".into());
	parser.parse_type()
}

fn parse_pattern(source: &str) -> Option<Spanned<Pattern>> {
	let tokens = lexer().parse(source).unwrap();
	let eoi = crate::ast::Span::new(source.len(), source.len());
	let mut parser = super::core::Parser::new(&tokens, eoi, "test.nym".into());
	parser.parse_pattern()
}

#[test]
fn test_parse_import_basic() {
	let (module, errors) = parse_module("import std/math");
	assert!(errors.is_empty(), "errors: {errors:?}");
	assert_eq!(module.members.len(), 1);

	match &module.members[0] {
		Declaration::Import { root, path, idents } => {
			assert!(matches!(root, ImportRoot::Package(ident) if ident.0 == "std"));
			assert_eq!(path.len(), 1);
			assert_eq!(path[0].0, "math");
			assert!(idents.is_none());
		}
		_ => panic!("expected import declaration"),
	}
}

#[test]
fn test_parse_import_with_clause() {
	let (module, errors) = parse_module("import std/math with (sin, cos as cosine)");
	assert!(errors.is_empty(), "errors: {errors:?}");
	assert_eq!(module.members.len(), 1);

	match &module.members[0] {
		Declaration::Import { root, path, idents } => {
			assert!(matches!(root, ImportRoot::Package(ident) if ident.0 == "std"));
			assert_eq!(path.len(), 1);
			let idents = idents.as_ref().unwrap();
			assert_eq!(idents.len(), 2);
			assert_eq!(idents[0].0.0, "sin");
			assert!(idents[0].1.is_none());
			assert_eq!(idents[1].0.0, "cos");
			assert_eq!(idents[1].1.as_ref().unwrap().0, "cosine");
		}
		_ => panic!("expected import declaration"),
	}
}

#[test]
fn test_parse_import_project_root() {
	let (module, errors) = parse_module("import @/result with (Result)");
	assert!(errors.is_empty(), "errors: {errors:?}");

	match &module.members[0] {
		Declaration::Import { root, path, idents } => {
			assert!(matches!(root, ImportRoot::Project));
			assert_eq!(path.len(), 1);
			assert_eq!(path[0].0, "result");
			let idents = idents.as_ref().unwrap();
			assert_eq!(idents.len(), 1);
			assert_eq!(idents[0].0.0, "Result");
		}
		_ => panic!("expected import declaration"),
	}
}

#[test]
fn test_parse_let_declaration() {
	let (module, errors) = parse_module("let x = 42");
	assert!(errors.is_empty(), "errors: {errors:?}");
	assert_eq!(module.members.len(), 1);

	match &module.members[0] {
		Declaration::Let {
			visibility,
			meta,
			value,
		} => {
			assert!(visibility.is_none());
			assert!(!meta.mutable);
			match &value.0 {
				Expr::Int(Spanned(val, _)) => assert_eq!(*val, 42),
				_ => panic!("expected int literal"),
			}
		}
		_ => panic!("expected let declaration"),
	}
}

#[test]
fn test_parse_let_mut_with_type() {
	let (module, errors) = parse_module("let mut counter: int = 0");
	assert!(errors.is_empty(), "errors: {errors:?}", );

	match &module.members[0] {
		Declaration::Let { meta, .. } => {
			assert!(meta.mutable);
			assert!(meta.type_.is_some());
			assert!(matches!(&meta.type_.as_ref().unwrap().0, Type::Int));
		}
		_ => panic!("expected let declaration"),
	}
}

#[test]
fn test_parse_public_let() {
	let (module, errors) = parse_module("public let PI = 3.14");
	assert!(errors.is_empty(), "errors: {errors:?}");

	match &module.members[0] {
		Declaration::Let { visibility, .. } => {
			assert_eq!(*visibility, Some(Visibility::Public));
		}
		_ => panic!("expected let declaration"),
	}
}

#[test]
fn test_parse_func_declaration() {
	let (module, errors) = parse_module("func add(a: int, b: int): int -> a + b");
	assert!(errors.is_empty(), "errors: {errors:?}");

	match &module.members[0] {
		Declaration::Func { meta, body, .. } => {
			assert_eq!(meta.name.0, "add");
			assert_eq!(meta.params.len(), 2);
			assert!(meta.return_type.is_some());
			assert!(matches!(
				&body.0,
				Expr::BinaryOp {
					op: BinaryOperator::Plus,
					..
				}
			));
		}
		_ => panic!("expected func declaration"),
	}
}

#[test]
fn test_parse_func_with_generics() {
	let (module, errors) = parse_module("func identity<T>(x: T): T -> x");
	assert!(errors.is_empty(), "errors: {errors:?}");

	match &module.members[0] {
		Declaration::Func { meta, .. } => {
			assert_eq!(meta.name.0, "identity");
			assert_eq!(meta.generics.len(), 1);
			assert_eq!(meta.generics[0].0.name.0, "T");
		}
		_ => panic!("expected func declaration"),
	}
}

#[test]
fn test_parse_struct() {
	let (module, errors) = parse_module("struct Point(x: int, y: int) {}");
	assert!(errors.is_empty(), "errors: {errors:?}");

	match &module.members[0] {
		Declaration::Struct { name, fields, .. } => {
			assert_eq!(name.0, "Point");
			assert_eq!(fields.len(), 2);
			assert_eq!(fields[0].0.name.0, "x");
			assert_eq!(fields[1].0.name.0, "y");
		}
		_ => panic!("expected struct declaration"),
	}
}

#[test]
fn test_parse_enum() {
	let (module, errors) = parse_module(
		r#"enum Option<T> {
			Some(value: T),
			None
		}"#,
	);
	assert!(errors.is_empty(), "errors: {:?}", errors);

	match &module.members[0] {
		Declaration::Enum {
			name,
			generics,
			variants,
			..
		} => {
			assert_eq!(name.0, "Option");
			assert_eq!(generics.len(), 1);
			assert_eq!(variants.len(), 2);
			assert_eq!(variants[0].0.name.0, "Some");
			assert_eq!(variants[0].0.fields.len(), 1);
			assert_eq!(variants[1].0.name.0, "None");
			assert_eq!(variants[1].0.fields.len(), 0);
		}
		_ => panic!("expected enum declaration"),
	}
}

#[test]
fn test_parse_interface() {
	let (module, errors) = parse_module(
		r#"interface Default {
			func default(): self
		}"#,
	);
	assert!(errors.is_empty(), "errors: {:?}", errors);

	match &module.members[0] {
		Declaration::Interface { name, members, .. } => {
			assert_eq!(name.0, "Default");
			assert_eq!(members.len(), 1);
		}
		_ => panic!("expected interface declaration"),
	}
}

#[test]
fn test_parse_impl_for() {
	let (module, errors) = parse_module(
		r#"impl Default for int {
			func default() -> 0
		}"#,
	);
	assert!(errors.is_empty(), "errors: {:?}", errors);

	match &module.members[0] {
		Declaration::ImplFor {
			type_,
			for_interface,
			..
		} => {
			assert!(matches!(&type_.0, Type::Int));
			assert_eq!(for_interface.0.0, "Default");
		}
		_ => panic!("expected impl for declaration"),
	}
}

#[test]
fn test_parse_expression_int() {
	let expr = parse_expr("42").unwrap();
	match expr.0 {
		Expr::Int(Spanned(val, _)) => assert_eq!(val, 42),
		_ => panic!("expected int literal"),
	}
}

#[test]
fn test_parse_expression_float() {
	let expr = parse_expr("3.15").unwrap();
	match expr.0 {
		Expr::Float(Spanned(val, _)) => assert!((val.0 - 3.15).abs() < 0.001),
		_ => panic!("expected float literal"),
	}
}

#[test]
fn test_parse_expression_string() {
	let expr = parse_expr(r#""hello""#).unwrap();
	match &expr.0 {
		Expr::String(parts) => {
			assert_eq!(parts.len(), 1);
		}
		_ => panic!("expected string literal"),
	}
}

#[test]
fn test_parse_expression_binary_op() {
	let expr = parse_expr("1 + 2 * 3").unwrap();
	match &expr.0 {
		Expr::BinaryOp { lhs: _, op, rhs } => {
			assert_eq!(*op, BinaryOperator::Plus);
			match &rhs.0 {
				Expr::BinaryOp { op, .. } => assert_eq!(*op, BinaryOperator::Times),
				_ => panic!("expected multiplication on rhs"),
			}
		}
		_ => panic!("expected binary op"),
	}
}

#[test]
fn test_parse_expression_prefix_op() {
	let expr = parse_expr("-x").unwrap();
	match &expr.0 {
		Expr::PrefixOp { op, value } => {
			assert_eq!(*op, PrefixOperator::Negate);
			match &value.0 {
				Expr::Identifier(ident) => assert_eq!(ident.0, "x"),
				_ => panic!("expected identifier"),
			}
		}
		_ => panic!("expected prefix op"),
	}
}

#[test]
fn test_parse_expression_call() {
	let expr = parse_expr("add(1, 2)").unwrap();
	match &expr.0 {
		Expr::Call { func, args, .. } => {
			match &func.0 {
				Expr::Identifier(ident) => assert_eq!(ident.0, "add"),
				_ => panic!("expected identifier"),
			}
			assert_eq!(args.len(), 2);
		}
		_ => panic!("expected call expression"),
	}
}

#[test]
fn test_parse_expression_member_access() {
	let expr = parse_expr("obj.field").unwrap();
	match &expr.0 {
		Expr::MemberAccess {
			parent,
			member,
			optional,
		} => {
			let Expr::Identifier(Spanned(parent, _)) = parent.inner() else {
				panic!("expected identifier")
			};
			assert_eq!(parent, "obj");
			assert!(!optional);
			assert_eq!(member.0, "field");
		}
		_ => panic!("expected member access"),
	}
}

#[test]
fn test_parse_expression_optional_chaining() {
	let expr = parse_expr("obj?.field").unwrap();
	match &expr.0 {
		Expr::MemberAccess { optional, .. } => {
			assert!(*optional);
		}
		_ => panic!("expected member access"),
	}
}

#[test]
fn test_parse_expression_index_access() {
	let expr = parse_expr("arr[0]").unwrap();
	match &expr.0 {
		Expr::IndexAccess {
			parent,
			index,
			optional,
		} => {
			let Expr::Identifier(Spanned(parent, _)) = parent.inner() else {
				panic!("expected identifier")
			};
			assert_eq!(parent, "arr");
			assert!(!optional);
			match &index.0 {
				Expr::Int(Spanned(val, _)) => assert_eq!(*val, 0),
				_ => panic!("expected int index"),
			}
		}
		_ => panic!("expected index access"),
	}
}

#[test]
fn test_parse_expression_closure() {
	let expr = parse_expr("(x) -> x + 1").unwrap();
	match &expr.0 {
		Expr::Closure { params, body, .. } => {
			assert_eq!(params.len(), 1);
			assert!(matches!(&body.0, Expr::BinaryOp { .. }));
		}
		_ => panic!("expected closure"),
	}
}

#[test]
fn test_parse_expression_if() {
	let expr = parse_expr("if (x > 0) 1 else -1").unwrap();
	match &expr.0 {
		Expr::If {
			condition: _,
			then: _,
			otherwise,
		} => {
			assert!(otherwise.is_some());
		}
		_ => panic!("expected if expression"),
	}
}

#[test]
fn test_parse_expression_match() {
	let expr = parse_expr(
		r#"match (x) {
			1 -> "one",
			2 -> "two",
			_ -> "other"
		}"#,
	)
	.unwrap();
	match &expr.0 {
		Expr::Match { value: _, arms } => {
			assert_eq!(arms.len(), 3);
		}
		_ => panic!("expected match expression"),
	}
}

#[test]
fn test_parse_expression_for() {
	let expr = parse_expr("for (item in items) process(item)").unwrap();
	match &expr.0 {
		Expr::For {
			variable,
			iterable: _,
			body: _,
			..
		} => match &variable.0 {
			Pattern::Struct { path, .. } => assert_eq!(path[0].0, "item"),
			_ => panic!("expected struct pattern"),
		},
		_ => panic!("expected for expression"),
	}
}

#[test]
fn test_parse_expression_while() {
	let expr = parse_expr("while (running) loop()").unwrap();
	match &expr.0 {
		Expr::While {
			condition: _, body: _, ..
		} => {}
		_ => panic!("expected while expression"),
	}
}

#[test]
fn test_parse_expression_block() {
	let expr = parse_expr("{ let x = 1 x + 1 }").unwrap();
	match &expr.0 {
		Expr::Block { body, label } => {
			assert!(label.is_none());
			assert_eq!(body.len(), 2);
		}
		_ => panic!("expected block expression"),
	}
}

#[test]
fn test_parse_expression_labeled_block() {
	let expr = parse_expr("outer@ { break@outer 42 }").unwrap();
	match &expr.0 {
		Expr::Block { label, .. } => {
			assert!(label.is_some());
			assert_eq!(label.as_ref().unwrap().0, "outer");
		}
		_ => panic!("expected labeled block"),
	}
}

#[test]
fn test_parse_expression_list() {
	let expr = parse_expr("#[1, 2, 3]").unwrap();
	match &expr.0 {
		Expr::List(items) => {
			assert_eq!(items.len(), 3);
		}
		_ => panic!("expected list"),
	}
}

#[test]
fn test_parse_expression_tuple() {
	let expr = parse_expr("#(1, true, 'a')").unwrap();
	match &expr.0 {
		Expr::Tuple(items) => {
			assert_eq!(items.len(), 3);
		}
		_ => panic!("expected tuple"),
	}
}

#[test]
fn test_parse_expression_map() {
	let expr = parse_expr("#{'a': 1, 'b': 2}").unwrap();
	match &expr.0 {
		Expr::Map(entries) => {
			assert_eq!(entries.len(), 2);
		}
		_ => panic!("expected map"),
	}
}

#[test]
fn test_parse_expression_range() {
	let expr = parse_expr("1..10").unwrap();
	match &expr.0 {
		Expr::Range(_) => {}
		_ => panic!("expected range"),
	}
}

#[test]
fn test_parse_expression_is() {
	let expr = parse_expr("x is Some(value)").unwrap();
	match &expr.0 {
		Expr::PatternOp { op, .. } => {
			assert!(matches!(op, crate::ast::ops::PatternOperator::Is));
		}
		_ => panic!("expected pattern op"),
	}
}

#[test]
fn test_parse_expression_assignment() {
	let expr = parse_expr("x = 42").unwrap();
	match &expr.0 {
		Expr::AssignOp { op, .. } => {
			assert!(matches!(op, AssignOperator::Assign));
		}
		_ => panic!("expected assignment"),
	}
}

#[test]
fn test_parse_expression_compound_assignment() {
	let expr = parse_expr("x += 1").unwrap();
	match &expr.0 {
		Expr::AssignOp { op, .. } => {
			assert!(matches!(op, AssignOperator::PlusAssign));
		}
		_ => panic!("expected compound assignment"),
	}
}

#[test]
fn test_parse_type_primitives() {
	assert!(matches!(parse_type("int").unwrap().0, Type::Int));
	assert!(matches!(parse_type("float").unwrap().0, Type::Float));
	assert!(matches!(parse_type("boolean").unwrap().0, Type::Boolean));
	assert!(matches!(parse_type("char").unwrap().0, Type::Char));
	assert!(matches!(parse_type("string").unwrap().0, Type::String));
	assert!(matches!(parse_type("void").unwrap().0, Type::Void));
	assert!(matches!(parse_type("never").unwrap().0, Type::Never));
	assert!(matches!(parse_type("self").unwrap().0, Type::Self_));
}

#[test]
fn test_parse_type_list() {
	let ty = parse_type("#[int]").unwrap();
	match ty.0 {
		Type::List(inner) => {
			assert!(matches!(inner.0, Type::Int));
		}
		_ => panic!("expected list type"),
	}
}

#[test]
fn test_parse_type_tuple() {
	let ty = parse_type("#(int, string, boolean)").unwrap();
	match ty.0 {
		Type::Tuple(types) => {
			assert_eq!(types.len(), 3);
		}
		_ => panic!("expected tuple type"),
	}
}

#[test]
fn test_parse_type_map() {
	let ty = parse_type("#{string: int}").unwrap();
	match ty.0 {
		Type::Map(key, value) => {
			assert!(matches!(key.0, Type::String));
			assert!(matches!(value.0, Type::Int));
		}
		_ => panic!("expected map type"),
	}
}

#[test]
fn test_parse_type_function() {
	let ty = parse_type("(int, int) -> int").unwrap();
	match ty.0 {
		Type::Function {
			params,
			return_type,
		} => {
			assert_eq!(params.len(), 2);
			assert!(matches!(return_type.0, Type::Int));
		}
		_ => panic!("expected function type"),
	}
}

#[test]
fn test_parse_type_generic() {
	let ty = parse_type("Option<T>").unwrap();
	match ty.0 {
		Type::Reference { name, generics } => {
			assert_eq!(name.0, "Option");
			assert_eq!(generics.len(), 1);
		}
		_ => panic!("expected reference type"),
	}
}

#[test]
fn test_parse_type_intersection() {
	let ty = parse_type("A + B").unwrap();
	match ty.0 {
		Type::Intersection(_, _) => {}
		_ => panic!("expected intersection type"),
	}
}

#[test]
fn test_parse_pattern_literals() {
	assert!(matches!(parse_pattern("42").unwrap().0, Pattern::Int(_)));
	assert!(matches!(parse_pattern("-10").unwrap().0, Pattern::Int(_)));
	assert!(matches!(
		parse_pattern("3.14").unwrap().0,
		Pattern::Float(_)
	));
	assert!(matches!(
		parse_pattern("true").unwrap().0,
		Pattern::Boolean(_)
	));
	assert!(matches!(parse_pattern("'a'").unwrap().0, Pattern::Char(_)));
	assert!(matches!(
		parse_pattern("_").unwrap().0,
		Pattern::Placeholder
	));
}

#[test]
fn test_parse_pattern_struct() {
	let pat = parse_pattern("Some(value)").unwrap();
	match pat.0 {
		Pattern::Struct { path, fields } => {
			assert_eq!(path.len(), 1);
			assert_eq!(path[0].0, "Some");
			assert_eq!(fields.len(), 1);
		}
		_ => panic!("expected struct pattern"),
	}
}

#[test]
fn test_parse_pattern_list() {
	let pat = parse_pattern("#[1, 2, ...rest]").unwrap();
	match pat.0 {
		Pattern::List(entries) => {
			assert_eq!(entries.len(), 3);
		}
		_ => panic!("expected list pattern"),
	}
}

#[test]
fn test_parse_pattern_tuple() {
	let pat = parse_pattern("#(a, b, c)").unwrap();
	match pat.0 {
		Pattern::Tuple(entries) => {
			assert_eq!(entries.len(), 3);
		}
		_ => panic!("expected tuple pattern"),
	}
}

#[test]
fn test_parse_pattern_union() {
	let pat = parse_pattern("1 | 2 | 3").unwrap();
	match pat.0 {
		Pattern::Union(_, _) => {}
		_ => panic!("expected union pattern"),
	}
}

#[test]
fn test_parse_pattern_binding() {
	let pat = parse_pattern("Some(x) as result").unwrap();
	match pat.0 {
		Pattern::Binding { name, inner: _ } => {
			assert_eq!(name.0, "result");
		}
		_ => panic!("expected binding pattern"),
	}
}

#[test]
fn test_parse_pattern_range() {
	let pat = parse_pattern("1..10").unwrap();
	match pat.0 {
		Pattern::Range(_) => {}
		_ => panic!("expected range pattern"),
	}
}

#[test]
fn test_error_recovery_multiple_declarations() {
	let (module, errors) = parse_module(
		r#"
		let x = 42
		let y = ???
		let z = 100
		"#,
	);

	assert!(!errors.is_empty());
	assert!(
		module.members.len() >= 2,
		"should recover and parse other declarations"
	);
}

#[test]
fn test_error_recovery_missing_equals() {
	let (_, errors) = parse_module("let x 42");
	assert!(!errors.is_empty());
}

#[test]
fn test_error_recovery_missing_arrow() {
	let (_, errors) = parse_module("func foo() 42");
	assert!(!errors.is_empty());
}

#[test]
fn test_parse_complex_enum_with_methods() {
	let (module, errors) = parse_module(
		r#"public enum Option<T> {
			Some(value: T),
			None

			func is_some() -> match (this) {
				Some(...) -> true,
				None -> false,
			}
			
			func map<R>(f: (T) -> R) -> match (this) {
				Some(value) -> Some(value = f(value)),
				None -> None
			}
		}"#,
	);
	assert!(errors.is_empty(), "errors: {:?}", errors);

	match &module.members[0] {
		Declaration::Enum {
			name,
			variants,
			members,
			..
		} => {
			assert_eq!(name.0, "Option");
			assert_eq!(variants.len(), 2);
			assert!(!members.is_empty());
		}
		_ => panic!("expected enum declaration"),
	}
}

#[test]
fn test_parse_struct_with_impl() {
	let (_module, errors) = parse_module(
		r#"struct Point(x: int, y: int) {
			func distance() -> (x * x + y * y)
			
			impl Default {
				func default() -> Point(x = 0, y = 0)
			}
		}"#,
	);
	assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_parse_pipeline_operator() {
	let expr = parse_expr("x |> f |> g").unwrap();
	match &expr.0 {
		Expr::BinaryOp {
			op: BinaryOperator::Pipe,
			..
		} => {}
		_ => panic!("expected pipeline"),
	}
}

#[test]
fn test_parse_null_coalescing() {
	let expr = parse_expr("x ?? default").unwrap();
	match &expr.0 {
		Expr::BinaryOp {
			op: BinaryOperator::Unwrap,
			..
		} => {}
		_ => panic!("expected unwrap operator"),
	}
}

#[test]
fn test_parse_generic_call() {
	let expr = parse_expr("Vec.new<int>()").unwrap();
	match &expr.0 {
		Expr::Call { func, generics, .. } => {
			match &func.0 {
				Expr::MemberAccess { parent: _, member, .. } => {
					assert_eq!(member.0, "new");
				}
				_ => panic!("expected member access"),
			};

			assert_eq!(generics[0].0.value.0, Type::Int);
		}
		_ => panic!("expected call"),
	}
}
