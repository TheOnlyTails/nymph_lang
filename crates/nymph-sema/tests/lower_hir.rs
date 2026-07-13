use nymph_hir::hir::{BinOp, HirExpr, HirModule, HirStmt, UnOp};
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

#[test]
fn lowers_enum_inherent_methods() {
	// `impl Color { func ... }` on an enum type-checks and, per Slice 4D, now
	// lowers onto the enum's own `methods`, mirroring struct inherent methods.
	let hir = lower(
		r#"
		enum Color { Red, Green }
		impl Color {
			func idx(): int = 0
		}
		"#,
	);
	assert_eq!(hir.enums.len(), 1);
	let e = &hir.enums[0];
	assert_eq!(e.name, "Color");
	assert_eq!(e.methods.len(), 1);
	assert_eq!(e.methods[0].name, "idx");
}

#[test]
fn lowers_enum_inner_inherent_method() {
	// An inherent `func` inside the enum body (not a top-level `impl`) also
	// lands in the enum's methods — previously silently dropped by lowering
	// (Slice 4D corrections #1: the `Declaration::Enum` arm used to ignore
	// `members` entirely).
	let hir = lower(
		r#"
		enum Color { Red, Green
			func idx(): int = 0
		}
		"#,
	);
	let e = hir.enums.iter().find(|e| e.name == "Color").expect("Color");
	assert_eq!(e.methods.len(), 1);
	assert_eq!(e.methods[0].name, "idx");
}

#[test]
fn lowers_enum_inner_impl_with_default_materialization() {
	// A nested `impl Comparable<...> { .. }` block inside the enum body feeds
	// its own methods plus the interface's un-overridden defaults, mirroring
	// `lowers_nested_struct_impl_methods` / Slice 4C-b for structs.
	let hir = lower(
		r#"
		interface Comparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = true
		}
		enum Color { Red, Green
			impl Comparable<Other = Color> {
				func compare_to(other: Color): int = 0
			}
		}
		"#,
	);
	let e = hir.enums.iter().find(|e| e.name == "Color").expect("Color");
	assert_eq!(e.methods.len(), 2);
	assert!(e.methods.iter().any(|m| m.name == "compare_to"));
	assert!(e.methods.iter().any(|m| m.name == "less_than"));
}

#[test]
#[should_panic(expected = "multiple methods named")]
fn colliding_defaults_from_two_interfaces_panics_in_lowering_for_enum() {
	// The same V4 duplicate-method guard applies to enums as to structs.
	lower(
		r#"
		interface A { func describe(): int = 1 }
		interface B { func describe(): int = 2 }
		enum Color { Red, Green }
		impl A for Color { }
		impl B for Color { }
		"#,
	);
}

#[test]
fn lowers_nested_struct_impl_methods() {
	// A nested `impl Plus<...> { ... }` block inside a struct body (as in
	// stdlib/src/math/complex.nym) feeds its `func` members into the class's
	// methods, same as an inherent struct-inner `func`.
	let hir = lower(
		r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		struct Vec2(x: int, y: int) {
			impl Plus<Other = Vec2, Output = Vec2> {
				func plus(other: Vec2): Vec2 = other
			}
		}
		"#,
	);
	assert_eq!(hir.classes.len(), 1);
	let class = &hir.classes[0];
	assert_eq!(class.name, "Vec2");
	assert_eq!(class.methods.len(), 1);
	assert_eq!(class.methods[0].name, "plus");
}

#[test]
fn lowers_top_level_impl_for_methods() {
	// A top-level `impl Plus<...> for Vec2 { ... }` (interface impl) targeting a
	// struct also feeds its `func` members into that struct's class methods.
	let hir = lower(
		r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		struct Vec2(x: int, y: int)
		impl Plus<Other = Vec2, Output = Vec2> for Vec2 {
			func plus(other: Vec2): Vec2 = other
		}
		"#,
	);
	assert_eq!(hir.classes.len(), 1);
	let class = &hir.classes[0];
	assert_eq!(class.name, "Vec2");
	assert_eq!(class.methods.len(), 1);
	assert_eq!(class.methods[0].name, "plus");
}

#[test]
fn lowers_enum_impl_for_methods() {
	// `impl Plus<...> for Color { ... }` on an enum (stdlib does this for
	// `Result`'s `Unwrap` impl) now lowers onto the enum's methods (Slice 4D).
	let hir = lower(
		r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		enum Color { Red, Green }
		impl Plus<Other = Color, Output = Color> for Color {
			func plus(other: Color): Color = other
		}
		"#,
	);
	let e = hir.enums.iter().find(|e| e.name == "Color").expect("Color");
	assert_eq!(e.methods.len(), 1);
	assert_eq!(e.methods[0].name, "plus");
}

#[test]
fn lowers_user_operator_overload_to_a_method_call() {
	// `a + b` on a user struct with a directly-defined `Plus.plus` impl dispatches
	// to `a.plus(b)` rather than a native JS `+` (Slice 4B, D4: `UserImpl`).
	let hir = lower(
		r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		struct Vec2(x: int, y: int)
		impl Plus<Other = Vec2, Output = Vec2> for Vec2 {
			func plus(other: Vec2): Vec2 = other
		}
		func add(a: Vec2, b: Vec2): Vec2 = a + b
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "add").expect("add");
	let HirExpr::Call { callee, args } = &f.body else {
		panic!("expected Call, got {:?}", f.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "plus");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "a"));
	assert_eq!(args.len(), 1);
	assert!(matches!(&args[0], HirExpr::Local(n) if n == "b"));
}

#[test]
fn lowers_primitive_arithmetic_to_binary_unchanged() {
	// `int + int` still lowers to `HirExpr::Binary`, not a dispatched call — the
	// `BuiltinEager` resolution keeps the existing native-operator path.
	let hir = lower("func f(a: int, b: int): int = a + b");
	assert_eq!(
		hir.funcs[0].body,
		HirExpr::Binary {
			op: BinOp::Add,
			lhs: Box::new(HirExpr::Local("a".into())),
			rhs: Box::new(HirExpr::Local("b".into())),
		}
	);
}

#[test]
fn lowers_compound_assign_user_operator_overload_to_a_method_call() {
	// `v1 += v2` on a struct with a directly-defined `Plus.plus` impl dispatches to
	// `v1 = v1.plus(v2)` rather than a native JS `v1 = v1 + v2` (Finding 1).
	let hir = lower(
		r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		struct Vec2(x: int, y: int)
		impl Plus<Other = Vec2, Output = Vec2> for Vec2 {
			func plus(other: Vec2): Vec2 = other
		}
		func add(a: Vec2, b: Vec2): Vec2 = {
			let mut v1 = a
			v1 += b
			v1
		}
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "add").expect("add");
	let HirExpr::Block { stmts, .. } = &f.body else {
		panic!("expected Block, got {:?}", f.body);
	};
	// stmts[0] is the `let mut v1 = a`; stmts[1] is the compound assign (the
	// trailing `v1` is the block's separate `tail`, not a stmt).
	let HirStmt::Expr(HirExpr::Assign { target, value }) = &stmts[1] else {
		panic!("expected an Assign statement, got {:?}", stmts[1]);
	};
	assert!(matches!(target.as_ref(), HirExpr::Local(n) if n == "v1"));
	let HirExpr::Call { callee, args } = value.as_ref() else {
		panic!("expected Call, got {value:?}");
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "plus");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "v1"));
	assert_eq!(args.len(), 1);
	assert!(matches!(&args[0], HirExpr::Local(n) if n == "b"));
}

#[test]
fn lowers_compound_assign_on_int_stays_native() {
	// `x += 1` on a plain `int` still lowers to `HirExpr::Binary`, not a dispatched
	// call — the `BuiltinEager` resolution keeps the existing native-operator path.
	let hir = lower(
		r#"
		func f(): int = {
			let mut x = 1
			x += 1
			x
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Expr(HirExpr::Assign { target, value }) = &stmts[1] else {
		panic!("expected an Assign statement, got {:?}", stmts[1]);
	};
	assert!(matches!(target.as_ref(), HirExpr::Local(n) if n == "x"));
	assert_eq!(
		value.as_ref(),
		&HirExpr::Binary {
			op: BinOp::Add,
			lhs: Box::new(HirExpr::Local("x".into())),
			rhs: Box::new(HirExpr::Num(1.0)),
		}
	);
}

#[test]
fn user_comparable_default_method_materializes_and_dispatches() {
	// `v1 < v2` resolves through `Comparable`'s interface *default* method
	// (`less_than`, provided in terms of `compare_to`), which `Vec2`'s impl never
	// defines directly. Slice 4C-b materializes the un-overridden default onto
	// `Vec2`'s class, so `<` dispatches to a real, directly-callable method
	// (was a lowering panic pre-4C-b).
	let hir = lower(
		r#"
		interface Comparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = true
		}
		struct Vec2(x: int, y: int)
		impl Comparable<Other = Vec2> for Vec2 {
			func compare_to(other: Vec2): int = 0
		}
		func lt(v1: Vec2, v2: Vec2): boolean = v1 < v2
		"#,
	);
	let class = hir.classes.iter().find(|c| c.name == "Vec2").expect("Vec2");
	let mut names: Vec<_> = class.methods.iter().map(|m| m.name.as_str()).collect();
	names.sort_unstable();
	assert_eq!(names, ["compare_to", "less_than"]);

	let lt = hir.funcs.iter().find(|f| f.name == "lt").expect("lt");
	let HirExpr::Call { callee, args } = &lt.body else {
		panic!("expected Call, got {:?}", lt.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "less_than");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "v1"));
	assert_eq!(args.len(), 1);
	assert!(matches!(&args[0], HirExpr::Local(n) if n == "v2"));
}

#[test]
fn overridden_default_method_is_not_duplicated() {
	// `Vec2` overrides `Comparable`'s default `less_than` directly — the class
	// must carry the override's body, not also materialize the interface's
	// default (Slice 4C-b, V1: override always wins).
	let hir = lower(
		r#"
		interface Comparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = true
		}
		struct Vec2(x: int, y: int)
		impl Comparable<Other = Vec2> for Vec2 {
			func compare_to(other: Vec2): int = 0
			func less_than(other: Vec2): boolean = false
		}
		"#,
	);
	let class = hir.classes.iter().find(|c| c.name == "Vec2").expect("Vec2");
	assert_eq!(class.methods.len(), 2);
	let less_than = class
		.methods
		.iter()
		.find(|m| m.name == "less_than")
		.expect("less_than");
	// The override's body (`false`), not the interface default's (`true`).
	assert_eq!(less_than.body, HirExpr::Bool(false));
}

#[test]
#[should_panic(expected = "multiple methods named")]
fn colliding_defaults_from_two_interfaces_panics_in_lowering() {
	// Two interfaces both default a method named `describe`; `Vec2` implements
	// both without overriding either. Materializing both defaults onto the same
	// class would silently produce a duplicate-named JS method (last one wins);
	// V4 requires a loud panic naming the struct and method instead.
	lower(
		r#"
		interface A { func describe(): int = 1 }
		interface B { func describe(): int = 2 }
		struct Vec2(x: int, y: int)
		impl A for Vec2 { }
		impl B for Vec2 { }
		"#,
	);
}

#[test]
#[should_panic(expected = "does not yet dispatch operator to interface default method")]
fn bounded_generic_plus_default_still_panics_in_lowering() {
	// A bounded generic function's `t1 + t2` resolves through `T`'s interface
	// bound (`MethodSource::GenericBound`), not through any concrete impl — the
	// concrete impl is only known once `T` is instantiated, which this
	// type-erased-at-lowering compiler does not track. Codegen still cannot
	// dispatch that at compile time, so this stays a loud lowering deferral (V2:
	// only `InterfaceDefault` flips to `UserImpl`; `GenericBound` is unchanged).
	//
	// NB: prior to Slice 4C-c, comparison/equality/logical operators on a `Param`
	// receiver did not reach `dispatch_operator` at all, so this pin deliberately
	// used an arithmetic operator. 4C-c (W1) brings comparisons to parity — see
	// `bounded_generic_less_than_still_panics_in_lowering` below for the
	// comparison-operator sibling of this same case.
	lower(
		r#"
		interface Plus<Other, Output> {
			func base(): Output
			func plus(other: Other): Output = this.base()
		}
		func add<T: Plus<Other = T, Output = T>>(t1: T, t2: T): T = t1 + t2
		"#,
	);
}

// ── Slice 4C-c, Task 2: comparison-operator lowering pins (W1, W4) ──────────

#[test]
#[should_panic(expected = "does not yet dispatch operator to interface default method")]
fn bounded_generic_less_than_still_panics_in_lowering() {
	// W1's comparison-arm parity means a bounded generic parameter's `a < b` now
	// resolves through `T`'s `Comparable` bound (`MethodSource::GenericBound`),
	// exactly like the arithmetic case above — still a loud lowering deferral,
	// not the silent native `<` on still-generic operands this slice closes.
	lower(
		r#"
		interface Comparable<Other> { func less_than(other: Other): boolean }
		func lt<T: Comparable<Other = T>>(a: T, b: T): boolean = a < b
		"#,
	);
}

#[test]
#[should_panic(expected = "does not yet dispatch operator to interface default method")]
fn this_less_than_other_in_interface_default_body_panics_in_lowering() {
	// W4: an interface default method whose *own* body uses `this < other`
	// directly (rather than calling another method) checks `this` bound to a
	// rigid synthetic `Param` (`check_interface_default_bodies`) — W1 now routes
	// that `Param` receiver through `dispatch_operator`, recording
	// `MethodSource::GenericBound` → `UserImplDefaultMethod`. `Vec2` never
	// overrides `at_most`, so its default body (with this still-generic
	// resolution) is materialized verbatim onto `Vec2`'s class and lowering it
	// panics loudly instead of silently emitting a native `<` between two
	// `Vec2` instances.
	lower(
		r#"
		interface Comparable<Other> {
			func less_than(other: Other): boolean
			func at_most(other: Other): boolean = this < other
		}
		struct Vec2(x: int, y: int)
		impl Comparable<Other = Vec2> for Vec2 {
			func less_than(other: Vec2): boolean = true
		}
		func f(v: Vec2): Vec2 = v
		"#,
	);
}

#[test]
fn late_pinned_adt_less_than_lowers_to_a_method_call() {
	// W1: `xs[0] < xs[0]`'s element type is a genuinely unconstrained inference
	// variable at the moment the `BinaryOp` node is recorded, pinned to `Vec2`
	// only afterward. The pending-operator queue re-resolves it once `Vec2` is
	// known, finding the direct `less_than` impl (`UserImpl`) — lowering must
	// dispatch to `xs[0].less_than(xs[0])`, not a native `<` on two objects.
	let hir = lower(
		r#"
		interface Comparable<Other> { func less_than(other: Other): boolean }
		struct Vec2(x: int)
		impl Comparable<Other = Vec2> for Vec2 {
			func less_than(other: Vec2): boolean = true
		}
		func f(): boolean = {
			let xs = #[]
			let c = xs[0] < xs[0]
			let pin: #[Vec2] = xs
			c
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Let { value, .. } = &stmts[1] else {
		panic!("expected a Let statement, got {:?}", stmts[1]);
	};
	let HirExpr::Call { callee, args } = value else {
		panic!("expected Call, got {value:?}");
	};
	let HirExpr::Field { name, .. } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "less_than");
	assert_eq!(args.len(), 1);
}

#[test]
fn lowers_primitive_less_than_to_binary_unchanged() {
	// `int < int` still lowers to `HirExpr::Binary`, not a dispatched call — the
	// `BuiltinEager` resolution keeps the existing native-operator path.
	let hir = lower("func f(a: int, b: int): boolean = a < b");
	assert_eq!(
		hir.funcs[0].body,
		HirExpr::Binary {
			op: BinOp::Lt,
			lhs: Box::new(HirExpr::Local("a".into())),
			rhs: Box::new(HirExpr::Local("b".into())),
		}
	);
}

#[test]
#[should_panic(expected = "no operator resolution recorded for binary op")]
fn missing_resolution_still_panics_in_lowering() {
	// Finding 2 closes the two known valid-program gaps that used to leave a
	// `BinaryOp`/`AssignOp` node with no recorded `Resolution` (an unresolved
	// generic-parameter operand, and an inference variable resolved only after the
	// node was recorded) — every zero-diagnostic program now reaches lowering fully
	// resolved. This pins that the `None` panic itself is still live as an
	// invariant guard against a *future* checker regression, by handing lowering a
	// `Checked` whose annotations were wiped, as if the checker had failed to
	// record a resolution it should have.
	let parsed = parse_module("func f(a: int, b: int): int = a + b", "test");
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
	let stripped = nymph_sema::Checked {
		diags: checked.diags,
		annotations: nymph_sema::Annotations::default(),
		interner: checked.interner,
	};
	nymph_sema::lower_hir(&parsed.tree, &stripped);
}

// ── Slice 4C-a, Task 2: `PrefixOp` lowering dispatch ────────────────────────

#[test]
fn lowers_user_negate_overload_to_a_method_call() {
	// `-v` on a user struct with a directly-defined `Negate.negate` impl dispatches
	// to `v.negate()` rather than a native JS `-` (Slice 4C-a, U3: `UserImpl`).
	let hir = lower(
		r#"
		interface Negate<Output> { func negate(): Output }
		struct Vec2(x: int, y: int)
		impl Negate<Output = Vec2> for Vec2 {
			func negate(): Vec2 = this
		}
		func f(v: Vec2): Vec2 = -v
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Call { callee, args } = &f.body else {
		panic!("expected Call, got {:?}", f.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "negate");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "v"));
	assert!(args.is_empty());
}

#[test]
fn lowers_primitive_negate_to_unary_unchanged() {
	// `-x` on a plain `int` still lowers to `HirExpr::Unary { op: Neg, .. }`, not a
	// dispatched call — the `BuiltinEager` resolution keeps the existing
	// native-operator path.
	let hir = lower("func f(x: int): int = -x");
	assert_eq!(
		hir.funcs[0].body,
		HirExpr::Unary {
			op: UnOp::Neg,
			operand: Box::new(HirExpr::Local("x".into())),
		}
	);
}

#[test]
fn lowers_primitive_bit_not_to_unary_unchanged() {
	// `~x` on a plain `int` lowers to `HirExpr::Unary { op: BitNot, .. }`.
	let hir = lower("func f(x: int): int = ~x");
	assert_eq!(
		hir.funcs[0].body,
		HirExpr::Unary {
			op: UnOp::BitNot,
			operand: Box::new(HirExpr::Local("x".into())),
		}
	);
}

#[test]
fn user_negate_default_method_materializes_and_dispatches() {
	// `-v` resolves through `Negate`'s interface *default* method (`negate`,
	// provided in terms of `base`), which `Vec2`'s impl never defines directly.
	// Slice 4C-b materializes the un-overridden default (which itself calls
	// `this.base()`, another materialized/impl method) onto `Vec2`'s class, so
	// `-v` dispatches to a real method (was a lowering panic pre-4C-b).
	let hir = lower(
		r#"
		interface Negate<Output> {
			func base(): Output
			func negate(): Output = this.base()
		}
		struct Vec2(x: int, y: int)
		impl Negate<Output = Vec2> for Vec2 {
			func base(): Vec2 = this
		}
		func f(v: Vec2): Vec2 = -v
		"#,
	);
	let class = hir.classes.iter().find(|c| c.name == "Vec2").expect("Vec2");
	let mut names: Vec<_> = class.methods.iter().map(|m| m.name.as_str()).collect();
	names.sort_unstable();
	assert_eq!(names, ["base", "negate"]);

	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Call { callee, args } = &f.body else {
		panic!("expected Call, got {:?}", f.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "negate");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "v"));
	assert!(args.is_empty());

	// The materialized `negate` body itself lowers `this.base()` as an ordinary
	// call on `This` — same mechanism impl-defined method bodies already use.
	let negate = class
		.methods
		.iter()
		.find(|m| m.name == "negate")
		.expect("negate");
	let HirExpr::Call { callee, args } = &negate.body else {
		panic!("expected Call, got {:?}", negate.body);
	};
	assert!(args.is_empty());
	assert!(matches!(
		callee.as_ref(),
		HirExpr::Field { recv, name } if matches!(recv.as_ref(), HirExpr::This) && name == "base"
	));
}

#[test]
#[should_panic(expected = "no operator resolution recorded for prefix op")]
fn missing_prefix_resolution_still_panics_in_lowering() {
	// Mirrors `missing_resolution_still_panics_in_lowering` for the unary case:
	// pins that the `None` panic is live as an invariant guard against a future
	// checker regression, by handing lowering a `Checked` whose annotations were
	// wiped, as if the checker had failed to record a resolution it should have.
	let parsed = parse_module("func f(a: int): int = -a", "test");
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
	let stripped = nymph_sema::Checked {
		diags: checked.diags,
		annotations: nymph_sema::Annotations::default(),
		interner: checked.interner,
	};
	nymph_sema::lower_hir(&parsed.tree, &stripped);
}

// ── Slice 4E: `return`, let-shadowing, module lets ──────────────────────────

#[test]
fn lowers_return_with_value_as_last_statement_of_a_block() {
	// The exact corpus shape: an if-branch block whose only statement is
	// `return n` — it must become a `HirStmt::Return`, NOT the block's tail
	// expression (emit has no way to represent "return" as a value).
	let hir = lower(
		r#"
		func abs(n: int): int = {
			if (n >= 0) { return n }
			0 - n
		}
		"#,
	);
	let HirExpr::Block { stmts, tail } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	// `0 - n` is the block's LAST statement, so it becomes the `tail` expression,
	// not a pushed `stmts` entry — only the `if` is a statement here.
	assert_eq!(stmts.len(), 1);
	let HirStmt::Expr(HirExpr::If {
		then, otherwise, ..
	}) = &stmts[0]
	else {
		panic!("expected an If statement, got {:?}", stmts[0]);
	};
	assert!(otherwise.is_none());
	let HirExpr::Block {
		stmts: then_stmts,
		tail: then_tail,
	} = then.as_ref()
	else {
		panic!("expected the then-branch to be a Block, got {then:?}");
	};
	assert_eq!(
		then_stmts,
		&vec![HirStmt::Return(Some(HirExpr::Local("n".into())))]
	);
	assert!(
		then_tail.is_none(),
		"a block whose only statement is `return` must have no tail expression"
	);
	assert!(
		tail.is_some(),
		"the trailing `0 - n` stays the block's tail"
	);
}

#[test]
fn lowers_bare_return_in_a_void_function() {
	let hir = lower("func f(): void = { return }");
	assert_eq!(
		hir.funcs[0].body,
		HirExpr::Block {
			stmts: vec![HirStmt::Return(None)],
			tail: None,
		}
	);
}

#[test]
#[should_panic(expected = "only supported in statement position")]
fn return_as_an_unbraced_match_arm_body_panics_in_lowering() {
	// `return` reached in genuine expression position (an unbraced match-arm
	// body) has no HIR representation — lowering panics loudly rather than
	// silently dropping or misplacing it (Slice 4E, Y1).
	lower(
		r#"
		func f(n: int): int = match (n) {
			0 -> return 7,
			_ -> n,
		}
		"#,
	);
}

#[test]
fn lowers_same_scope_let_shadow_with_a_rename() {
	// `let x = 1; let x = x + 1` redeclares `x` in the SAME JS scope — the second
	// binding renames to `x$1`; the RHS reads the PRIOR `x`, and the tail
	// resolves through the renamed binding (Slice 4E, Y2).
	let hir = lower(
		r#"
		func f(): int = {
			let x = 1
			let x = x + 1
			x * 10
		}
		"#,
	);
	let HirExpr::Block { stmts, tail } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Let { name, value, .. } = &stmts[0] else {
		panic!("expected a Let statement, got {:?}", stmts[0]);
	};
	assert_eq!(name, "x");
	assert_eq!(value, &HirExpr::Num(1.0));

	let HirStmt::Let { name, value, .. } = &stmts[1] else {
		panic!("expected a Let statement, got {:?}", stmts[1]);
	};
	assert_eq!(name, "x$1", "same-scope redeclaration renames");
	assert_eq!(
		value,
		&HirExpr::Binary {
			op: BinOp::Add,
			lhs: Box::new(HirExpr::Local("x".into())),
			rhs: Box::new(HirExpr::Num(1.0)),
		},
		"the redeclaration's RHS reads the PRIOR binding, not itself"
	);

	let tail = tail.as_ref().expect("tail present");
	assert_eq!(
		tail.as_ref(),
		&HirExpr::Binary {
			op: BinOp::Mul,
			lhs: Box::new(HirExpr::Local("x$1".into())),
			rhs: Box::new(HirExpr::Num(10.0)),
		},
		"later references resolve through the renamed binding"
	);
}

#[test]
fn lowers_triple_same_scope_let_shadow() {
	// A third same-scope redeclaration renames again (`x$2`), not by reusing `x$1`.
	let hir = lower(
		r#"
		func f(): int = {
			let x = 1
			let x = x + 1
			let x = x + 1
			x
		}
		"#,
	);
	let HirExpr::Block { stmts, tail } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let names: Vec<&str> = stmts
		.iter()
		.map(|s| match s {
			HirStmt::Let { name, .. } => name.as_str(),
			other => panic!("expected a Let statement, got {other:?}"),
		})
		.collect();
	assert_eq!(names, ["x", "x$1", "x$2"]);
	assert_eq!(
		tail.as_deref(),
		Some(&HirExpr::Local("x$2".into())),
		"the tail resolves through the LAST rename"
	);
}

#[test]
fn nested_block_shadow_renames_to_avoid_the_tdz_hazard() {
	// A nested block (a separate JS scope — its own `BlockStatement`/IIFE) can
	// still trip JS's `const`/`let` TDZ if it reuses an outer name: JS hoists a
	// block's own declaration for the whole block, so if this rename didn't
	// happen, a *different* nested `let` reusing the same outer name (e.g. `let
	// i = i + 100`) would read the not-yet-initialized inner binding instead of
	// the outer one. Renaming on ANY active-scope collision — not only when
	// this specific initializer would hit the hazard — sidesteps having to
	// prove per-declaration whether the hazard applies (Slice 4E, Y2 fix). So
	// even this harmless-looking shadow (`let x = 5`, not referencing the outer
	// `x`) renames.
	let hir = lower(
		r#"
		func f(): int = {
			let x = 1
			let y = if (true) { let x = 5 x } else { 0 }
			x + y
		}
		"#,
	);
	let HirExpr::Block { stmts, tail } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Let { name, .. } = &stmts[0] else {
		panic!("expected a Let statement, got {:?}", stmts[0]);
	};
	assert_eq!(name, "x", "the outer `x` is never renamed");

	let HirStmt::Let { value, .. } = &stmts[1] else {
		panic!("expected a Let statement, got {:?}", stmts[1]);
	};
	let HirExpr::If { then, .. } = value else {
		panic!("expected an If, got {value:?}");
	};
	let HirExpr::Block {
		stmts: inner_stmts,
		tail: inner_tail,
	} = then.as_ref()
	else {
		panic!("expected the then-branch to be a Block, got {then:?}");
	};
	let HirStmt::Let {
		name: inner_name, ..
	} = &inner_stmts[0]
	else {
		panic!("expected a Let statement, got {:?}", inner_stmts[0]);
	};
	assert_eq!(
		inner_name, "x$1",
		"a nested-scope shadow of an active outer `x` renames too"
	);
	assert_eq!(
		inner_tail.as_deref(),
		Some(&HirExpr::Local("x$1".into())),
		"and resolves through the rename"
	);

	assert_eq!(
		tail.as_deref(),
		Some(&HirExpr::Binary {
			op: BinOp::Add,
			lhs: Box::new(HirExpr::Local("x".into())),
			rhs: Box::new(HirExpr::Local("y".into())),
		}),
		"the outer tail still resolves the outer (unrenamed) `x`"
	);
}

#[test]
fn nested_block_shadow_that_reads_the_outer_binding_renames_and_reads_the_prior_value() {
	// The exact defect this fix closes: `let i = 1; let r = { let i = i + 100;
	// i }; r` — without the rename, both the outer `i` and the inner `let i`
	// would emit as the identical JS identifier `i`, and since JS hoists the
	// inner block's own `const i` for its whole block, the inner initializer's
	// read of `i` would resolve to the not-yet-initialized inner binding
	// instead of the outer one (`ReferenceError: Cannot access 'i' before
	// initialization` at runtime) — silently-wrong JS from a zero-diagnostic
	// program.
	let hir = lower(
		r#"
		func f(): int = {
			let i = 1
			let r = { let i = i + 100 i }
			r
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Let { name, .. } = &stmts[0] else {
		panic!("expected a Let statement, got {:?}", stmts[0]);
	};
	assert_eq!(name, "i", "the outer `i` is never renamed");

	let HirStmt::Let { value, .. } = &stmts[1] else {
		panic!("expected a Let statement, got {:?}", stmts[1]);
	};
	let HirExpr::Block {
		stmts: inner_stmts,
		tail: inner_tail,
	} = value
	else {
		panic!("expected a Block, got {value:?}");
	};
	let HirStmt::Let {
		name: inner_name,
		value: inner_value,
		..
	} = &inner_stmts[0]
	else {
		panic!("expected a Let statement, got {:?}", inner_stmts[0]);
	};
	assert_eq!(
		inner_name, "i$1",
		"the nested redeclaration of the active outer `i` renames"
	);
	assert_eq!(
		inner_value,
		&HirExpr::Binary {
			op: BinOp::Add,
			lhs: Box::new(HirExpr::Local("i".into())),
			rhs: Box::new(HirExpr::Num(100.0)),
		},
		"its RHS reads the OUTER `i`, not the not-yet-declared inner one"
	);
	assert_eq!(
		inner_tail.as_deref(),
		Some(&HirExpr::Local("i$1".into())),
		"the inner tail resolves through the rename"
	);
}

#[test]
fn lowers_param_shadowed_by_a_body_let_inside_a_method() {
	// A body `let` reusing a PARAM's name is a same-scope redeclaration too —
	// params and the body block's own `let`s share one merged JS scope.
	let hir = lower(
		r#"
		struct Counter(n: int)
		impl Counter {
			func bump(n: int): int = {
				let n = n + this.n
				n
			}
		}
		"#,
	);
	let class = hir.classes.iter().find(|c| c.name == "Counter").unwrap();
	let method = class.methods.iter().find(|m| m.name == "bump").unwrap();
	assert_eq!(method.params, vec!["n".to_string()]);
	let HirExpr::Block { stmts, tail } = &method.body else {
		panic!("expected Block, got {:?}", method.body);
	};
	let HirStmt::Let { name, .. } = &stmts[0] else {
		panic!("expected a Let statement, got {:?}", stmts[0]);
	};
	assert_eq!(name, "n$1", "the body let renames, shadowing the param");
	assert_eq!(tail.as_deref(), Some(&HirExpr::Local("n$1".into())));
}

#[test]
fn lowers_a_top_level_let_into_the_module() {
	use nymph_hir::hir::HirLet;
	let hir = lower(
		r#"
		let answer = 42
		func f(): int = answer
		"#,
	);
	assert_eq!(
		hir.lets,
		vec![HirLet {
			name: "answer".into(),
			mutable: false,
			value: HirExpr::Num(42.0),
		}]
	);
	// A reference to it from a function body stays the bare (unrenamed) name.
	assert_eq!(hir.funcs[0].body, HirExpr::Local("answer".into()));
}

#[test]
fn lowers_two_top_level_lets_in_source_order_with_the_second_referencing_the_first() {
	use nymph_hir::hir::HirLet;
	let hir = lower(
		r#"
		let base = 10
		let total = base + 5
		func f(): int = total
		"#,
	);
	assert_eq!(
		hir.lets,
		vec![
			HirLet {
				name: "base".into(),
				mutable: false,
				value: HirExpr::Num(10.0),
			},
			HirLet {
				name: "total".into(),
				mutable: false,
				value: HirExpr::Binary {
					op: BinOp::Add,
					lhs: Box::new(HirExpr::Local("base".into())),
					rhs: Box::new(HirExpr::Num(5.0)),
				},
			},
		]
	);
}

#[test]
fn lowers_a_mutable_top_level_let() {
	use nymph_hir::hir::HirLet;
	let hir = lower("let mut counter = 0");
	assert_eq!(
		hir.lets,
		vec![HirLet {
			name: "counter".into(),
			mutable: true,
			value: HirExpr::Num(0.0),
		}]
	);
}

#[test]
fn reorders_a_top_level_let_that_references_a_later_let() {
	// `let a = b + 1; let b = 10; func f(): int = a` — naive source-order
	// emission would put `a`'s `const` before `b`'s, throwing a TDZ
	// `ReferenceError` under Node (Finding: module-let ordering). Lowering must
	// reorder `HirModule::lets` so `b` comes first.
	use nymph_hir::hir::HirLet;
	let hir = lower(
		r#"
		let a = b + 1
		let b = 10
		func f(): int = a
		"#,
	);
	let names: Vec<&str> = hir.lets.iter().map(|l| l.name.as_str()).collect();
	assert_eq!(
		names,
		["b", "a"],
		"`b` has no dependency and must be emitted before `a`, which needs it"
	);
	assert_eq!(
		hir.lets,
		vec![
			HirLet {
				name: "b".into(),
				mutable: false,
				value: HirExpr::Num(10.0),
			},
			HirLet {
				name: "a".into(),
				mutable: false,
				value: HirExpr::Binary {
					op: BinOp::Add,
					lhs: Box::new(HirExpr::Local("b".into())),
					rhs: Box::new(HirExpr::Num(1.0)),
				},
			},
		]
	);
}

#[test]
fn reorders_a_top_level_let_whose_called_function_reads_a_later_let() {
	// `let a = g(); func g(): int = b; let b = 5;` — `a`'s initializer calls
	// `g`, whose body reads `b`, a top-level `let` declared textually AFTER
	// both `a` and `g`. Naive source-order emission puts `b`'s `const` last, so
	// calling `g()` as part of `a`'s own initializer reads `b` while it's still
	// in its module-scope TDZ.
	let hir = lower(
		r#"
		let a = g()
		func g(): int = b
		let b = 5
		"#,
	);
	let names: Vec<&str> = hir.lets.iter().map(|l| l.name.as_str()).collect();
	assert_eq!(
		names,
		["b", "a"],
		"`b` must be emitted before `a`, whose initializer transitively reads it via `g`"
	);
}

#[test]
#[should_panic(expected = "circular top-level `let` dependency")]
fn circular_top_level_let_dependency_panics_in_lowering() {
	// `let a = b + 1; let b = a + 1;` has no valid JS module-init order at all
	// (`const`s can't forward-reference each other in either direction) — this
	// must panic loudly rather than silently pick a (broken) order.
	lower(
		r#"
		let a = b + 1
		let b = a + 1
		"#,
	);
}

// ── Slice 4E follow-up: `return` inside an UNBRACED if/while branch ─────────

#[test]
fn lowers_bare_return_as_an_unbraced_while_body() {
	// `while (n > 0) return n` — an unbraced while-body that is directly
	// `return n`, with no surrounding `{ .. }`. Must lower the same as the
	// braced `while (n > 0) { return n }` shape.
	let hir = lower(
		r#"
		func f(n: int): int = {
			while (n > 0) return n
			0
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Expr(HirExpr::While { body, .. }) = &stmts[0] else {
		panic!("expected a While statement, got {:?}", stmts[0]);
	};
	assert_eq!(
		body.as_ref(),
		&HirExpr::Block {
			stmts: vec![HirStmt::Return(Some(HirExpr::Local("n".into())))],
			tail: None,
		}
	);
}

#[test]
fn lowers_bare_return_as_an_unbraced_if_then_branch() {
	// `if (n < 0) return 0 - n` — an unbraced then-branch that is directly
	// `return ..`, with no surrounding `{ .. }` and no `else`.
	let hir = lower(
		r#"
		func f(n: int): int = {
			if (n < 0) return 0 - n
			n
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Expr(HirExpr::If {
		then, otherwise, ..
	}) = &stmts[0]
	else {
		panic!("expected an If statement, got {:?}", stmts[0]);
	};
	assert!(otherwise.is_none());
	assert_eq!(
		then.as_ref(),
		&HirExpr::Block {
			stmts: vec![HirStmt::Return(Some(HirExpr::Binary {
				op: BinOp::Sub,
				lhs: Box::new(HirExpr::Num(0.0)),
				rhs: Box::new(HirExpr::Local("n".into())),
			}))],
			tail: None,
		}
	);
}
