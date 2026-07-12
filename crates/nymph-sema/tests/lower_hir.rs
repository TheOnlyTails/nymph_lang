use nymph_hir::hir::{BinOp, HirExpr, HirModule};
use nymph_sema::check_module;
use nymph_syntax::parse_module;

fn lower(src: &str) -> HirModule {
	let parsed = parse_module(src, "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed"
	);
	let checked = check_module(&parsed.tree);
	assert!(
		checked.diags.is_empty(),
		"check failed: {:?}",
		checked.diags
	);
	nymph_sema::lower_hir(&parsed.tree, &checked)
}

#[test]
fn lowers_a_function_with_arithmetic() {
	let hir = lower("func f(a: int, b: int): int = a + b");
	assert_eq!(hir.funcs.len(), 1);
	let f = &hir.funcs[0];
	assert_eq!(f.name, "f");
	assert_eq!(f.params, vec!["a".to_string(), "b".to_string()]);
	assert_eq!(
		f.body,
		HirExpr::Binary {
			op: BinOp::Add,
			lhs: Box::new(HirExpr::Local("a".into())),
			rhs: Box::new(HirExpr::Local("b".into())),
		}
	);
}

#[test]
fn lowers_a_call_and_int_literal() {
	// `h` must be a real, resolvable name — the checker's name resolution rejects
	// calls to undefined functions — so define it and assert against `g`'s body.
	let hir = lower(
		r#"
		func h(x: int): int = x
		func g(): int = h(1)
		"#,
	);
	let g = hir
		.funcs
		.iter()
		.find(|f| f.name == "g")
		.expect("g in module");
	assert_eq!(
		g.body,
		HirExpr::Call {
			callee: Box::new(HirExpr::Local("h".into())),
			args: vec![HirExpr::Num(1.0)],
		}
	);
}

#[test]
fn lowers_collections_and_index() {
	let hir = lower("func f(): #[int] = #[1, 2, 3]");
	assert_eq!(
		hir.funcs[0].body,
		HirExpr::Array(vec![
			HirExpr::Num(1.0),
			HirExpr::Num(2.0),
			HirExpr::Num(3.0)
		]),
	);

	// Indexing a map dispatches to `MapGet`. Int keys keep this collections slice
	// free of string-literal lowering, which lands in a later slice.
	let hir = lower("func g(): int = #{ 1: 10 }[1]");
	assert!(
		matches!(hir.funcs[0].body, HirExpr::MapGet { .. }),
		"map index → MapGet"
	);

	let hir = lower("func h(): int = #[10, 20][1]");
	assert!(
		matches!(hir.funcs[0].body, HirExpr::Index { .. }),
		"list index → Index"
	);
}

#[test]
fn lowers_struct_decl_and_construction() {
	let hir = lower(
		r#"
		struct Point(x: int, y: int)
		func origin(): Point = Point(x = 0, y = 0)
		"#,
	);
	// The struct becomes a class carrying its field names in order.
	assert_eq!(hir.classes.len(), 1);
	assert_eq!(hir.classes[0].name, "Point");
	assert_eq!(
		hir.classes[0].fields,
		vec!["x".to_string(), "y".to_string()]
	);

	// Construction lowers to a `New` naming the class, with labeled field values.
	let f = hir
		.funcs
		.iter()
		.find(|f| f.name == "origin")
		.expect("origin");
	let HirExpr::New { class, fields } = &f.body else {
		panic!("expected New, got {:?}", f.body);
	};
	assert_eq!(class, "Point");
	assert_eq!(fields.len(), 2);
	assert_eq!(fields[0].0, "x");
	assert_eq!(fields[1].0, "y");
}

#[test]
fn lowers_field_access() {
	let hir = lower(
		r#"
		struct Point(x: int, y: int)
		func get_x(p: Point): int = p.x
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "get_x").expect("get_x");
	let HirExpr::Field { recv, name } = &f.body else {
		panic!("expected Field, got {:?}", f.body);
	};
	assert_eq!(name, "x");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "p"));
}

#[test]
fn lowers_enum_decl_and_variants() {
	let hir = lower(
		r#"
		enum Opt { Some(value: int), None }
		func s(): Opt = Some(value = 1)
		func n(): Opt = None
		func q(): Opt = Opt.Some(value = 2)
		"#,
	);
	// The enum becomes an HirEnum with both variants (Some has a field, None nullary).
	assert_eq!(hir.enums.len(), 1);
	assert_eq!(hir.enums[0].name, "Opt");
	let some = hir.enums[0]
		.variants
		.iter()
		.find(|v| v.name == "Some")
		.unwrap();
	let none = hir.enums[0]
		.variants
		.iter()
		.find(|v| v.name == "None")
		.unwrap();
	assert_eq!(some.fields, vec!["value".to_string()]);
	assert!(none.fields.is_empty());

	// Bare construction, bare nullary ref, and qualified construction all lower.
	let body = |name: &str| {
		hir
			.funcs
			.iter()
			.find(|f| f.name == name)
			.unwrap()
			.body
			.clone()
	};
	assert!(
		matches!(body("s"), HirExpr::VariantNew { .. }),
		"bare ctor → VariantNew, got {:?}",
		body("s")
	);
	assert!(
		matches!(body("n"), HirExpr::VariantRef { .. }),
		"bare nullary → VariantRef, got {:?}",
		body("n")
	);
	assert!(
		matches!(body("q"), HirExpr::VariantNew { .. }),
		"qualified ctor → VariantNew, got {:?}",
		body("q")
	);
}
