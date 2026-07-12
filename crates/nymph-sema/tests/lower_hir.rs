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
		func qn(): Opt = Opt.None
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
	assert!(
		matches!(body("qn"), HirExpr::VariantRef { .. }),
		"qualified nullary (member access) → VariantRef, got {:?}",
		body("qn")
	);
}

#[test]
fn lowers_match_over_enum() {
	use nymph_hir::hir::{HirArm, HirPat};
	let hir = lower(
		r#"
		enum Opt { Some(value: int), None }
		func unwrap_or(o: Opt): int = match (o) {
			Some(value) -> value,
			None -> 0,
		}
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "unwrap_or").unwrap();
	let HirExpr::Match { arms, .. } = &f.body else {
		panic!("expected Match, got {:?}", f.body);
	};
	assert_eq!(arms.len(), 2);
	// First arm: Some(value) → a Variant pattern binding `value`.
	let HirArm {
		pat: HirPat::Variant {
			enum_name,
			variant,
			fields,
		},
		..
	} = &arms[0]
	else {
		panic!("expected Variant pattern, got {:?}", arms[0].pat);
	};
	assert_eq!(enum_name, "Opt");
	assert_eq!(variant, "Some");
	assert_eq!(fields.len(), 1);
	assert_eq!(fields[0].0, "value");
	assert!(matches!(&fields[0].1, HirPat::Binding { name, sub: None } if name == "value"));
	// Second arm: None → a nullary Variant pattern.
	assert!(
		matches!(&arms[1].pat, HirPat::Variant { variant, fields, .. } if variant == "None" && fields.is_empty())
	);
}

#[test]
fn lowers_struct_methods_and_this() {
	let hir = lower(
		r#"
		struct Point(x: int, y: int)
		impl Point {
			func sum(): int = this.x + this.y
		}
		"#,
	);
	assert_eq!(hir.classes.len(), 1);
	let class = &hir.classes[0];
	assert_eq!(class.name, "Point");
	assert_eq!(class.methods.len(), 1);
	assert_eq!(class.methods[0].name, "sum");
	// The body `this.x + this.y` lowers with `This` receivers under the field access.
	let HirExpr::Binary { lhs, .. } = &class.methods[0].body else {
		panic!("expected a binary body, got {:?}", class.methods[0].body);
	};
	assert!(matches!(
		lhs.as_ref(),
		HirExpr::Field { recv, name } if matches!(recv.as_ref(), HirExpr::This) && name == "x"
	));
}

#[test]
fn lowers_full_patterns_and_guards() {
	use nymph_hir::hir::{HirLit, HirPat, HirRange};
	let hir = lower(
		r#"
		struct Point(x: int, y: int)
		enum Color { Red, Green, Blue }
		func tup(p: #(int, int)): int = match (p) {
			#(0, y) -> y,
			#(x, _) if x > 0 -> x,
			#(x, _) -> 0,
		}
		func strukt(pt: Point): int = match (pt) {
			Point(x = px, y = _) -> px,
		}
		func lst(xs: #[int]): int = match (xs) {
			#[] -> 0,
			#[a, ...rest] -> a,
			_ -> -1,
		}
		func rng(n: int): int = match (n) {
			1..10 -> 1,
			_ -> 0,
		}
		func str_match(s: string): int = match (s) {
			"hi" -> 1,
			_ -> 0,
		}
		func uni(c: Color): boolean = match (c) {
			Red | Green -> true,
			Blue -> false,
		}
		"#,
	);
	let arm0 = |name: &str| {
		let f = hir.funcs.iter().find(|f| f.name == name).unwrap();
		let HirExpr::Match { arms, .. } = &f.body else {
			panic!("expected Match in {name}");
		};
		arms.clone()
	};
	assert!(matches!(&arm0("tup")[0].pat, HirPat::Tuple(e) if e.len() == 2));
	assert!(arm0("tup")[1].guard.is_some(), "guard lowered");
	assert!(matches!(&arm0("strukt")[0].pat, HirPat::Struct { fields } if fields.len() == 2));
	assert!(
		matches!(&arm0("lst")[1].pat, HirPat::List { prefix, rest: Some(_), .. } if prefix.len() == 1)
	);
	assert!(matches!(
		&arm0("rng")[0].pat,
		HirPat::Range(HirRange::Exclusive { .. })
	));
	assert!(matches!(&arm0("str_match")[0].pat, HirPat::Lit(HirLit::Str(s)) if s == "hi"));
	assert!(matches!(&arm0("uni")[0].pat, HirPat::Or(..)));
}

#[test]
#[should_panic(expected = "union patterns that bind")]
fn binding_union_panics_in_lowering() {
	// A union whose side binds (`A(n) | B`) type-checks but is deferred; lowering
	// panics loudly rather than silently miscompiling. This pins that behavior.
	lower(
		r#"
		enum E { A(n: int), B }
		func f(e: E): int = match (e) {
			A(n) | B -> 0,
		}
		"#,
	);
}
