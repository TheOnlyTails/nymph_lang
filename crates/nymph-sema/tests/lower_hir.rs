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
#[should_panic(expected = "non-struct types")]
fn enum_inherent_methods_panic_in_lowering() {
	// `impl Color { func ... }` on an enum type-checks, but lowering does not yet
	// attach methods to enums; it must panic loudly instead of silently emitting
	// JS that crashes at runtime. This pins that behavior.
	lower(
		r#"
		enum Color { Red, Green }
		impl Color {
			func idx(): int = 0
		}
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
#[should_panic(expected = "non-struct types")]
fn impl_for_on_enum_panics_in_lowering() {
	// `impl Plus<...> for Color { ... }` on an enum type-checks (stdlib does this
	// for `Result`'s `Unwrap` impl), but lowering does not yet attach methods to
	// enums; it must panic loudly instead of silently dropping the impl.
	lower(
		r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		enum Color { Red, Green }
		impl Plus<Other = Color, Output = Color> for Color {
			func plus(other: Color): Color = other
		}
		"#,
	);
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
	// NB: comparison/equality/logical operators on a `Param` receiver do *not*
	// reach `dispatch_operator` at all (a pre-existing, out-of-scope hazard —
	// see the Slice 4C-b plan's investigation brief, "corrections" (1)), so this
	// pin deliberately uses an arithmetic operator, which does.
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
