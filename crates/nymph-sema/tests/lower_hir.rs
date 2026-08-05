use nymph_hir::hir::{
	BinOp, BuiltinResult, HirArrayElem, HirArrayKind, HirExpr, HirLit, HirMapElem, HirModule, HirPat,
	HirStmt, NumKind, ScalarCastKind, UnOp,
};
use nymph_sema::{
	RuntimeOwner, check_module, check_module_with_prelude, lower_hir_with_prelude,
	lower_hir_with_prelude_runtime_and_deps, lower_hir_with_prelude_runtime_and_deps_with_owners,
};
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

fn value_with_runtime_prototype<'a>(
	expr: &'a HirExpr,
	binding: &str,
	arguments: &[&str],
) -> &'a HirExpr {
	let HirExpr::WithPrototype { value, prototype } = expr else {
		panic!("expected WithPrototype, got {expr:?}");
	};
	let HirExpr::RuntimeTypeObject {
		binding: actual_binding,
		box_runtime,
		is_enum,
		arguments: actual_arguments,
	} = prototype.as_ref()
	else {
		panic!("expected canonical runtime type object, got {prototype:?}");
	};
	assert_eq!(actual_binding, binding);
	assert!(*box_runtime);
	assert!(!is_enum);
	assert_eq!(actual_arguments.len(), arguments.len());
	for (actual, expected_binding) in actual_arguments.iter().zip(arguments) {
		assert!(matches!(
			actual,
			HirExpr::RuntimeTypeObject {
				binding,
				box_runtime: true,
				is_enum: false,
				arguments,
			} if binding == expected_binding && arguments.is_empty()
		));
	}
	value
}

#[test]
fn compatibility_lowering_preserves_loop_control_targets_and_option_results() {
	let hir = lower(
		"enum Option<T> { Some(value: T), None }\nfunc stop(): Option<int> = while (true) { break 1 }",
	);
	let HirExpr::While {
		target,
		body,
		option: Some(option),
		..
	} = &hir.funcs[0].body
	else {
		panic!("expected Option-valued while")
	};
	assert_eq!(option.enum_name, "Option");
	assert!(matches!(
		body.as_ref(),
		HirExpr::Block {
			tail: Some(value),
			..
		} if matches!(value.as_ref(), HirExpr::Break { target: found, .. } if found == target)
	));
}

#[test]
fn compatibility_lowering_preserves_resolved_labeled_block_returns() {
	let hir = lower(
		"func choose(flag: boolean): int = result@{ if (flag) { return@choose 1 } return@result 7 }",
	);
	assert!(matches!(
		&hir.funcs[0].body,
		HirExpr::LabeledBlock { target, body }
			if matches!(body.as_ref(), HirExpr::Block { stmts, .. }
				if matches!(stmts.last(), Some(HirStmt::Return {
					target: nymph_hir::hir::HirReturnTarget::Block(return_target), ..
				}) if return_target == target)
					&& matches!(stmts.first(), Some(HirStmt::Expr(HirExpr::If { then, .. }))
						if matches!(then.as_ref(), HirExpr::Block { stmts, .. }
							if matches!(stmts.as_slice(), [HirStmt::Return {
								target: nymph_hir::hir::HirReturnTarget::Callable, ..
							}])
						)
					)
			)
	));
}

#[test]
fn separates_demanded_prelude_runtime_from_consumer_hir() {
	let prelude = parse_module("enum Order { Less, Equal, Greater }", "prelude");
	let user = parse_module("func equal(): Order = Order.Equal", "test");
	assert!(prelude.diagnostics.is_empty(), "prelude parse failed");
	assert!(user.diagnostics.is_empty(), "user parse failed");

	let checked = check_module_with_prelude(&user.tree, std::slice::from_ref(&prelude.tree));
	assert!(
		checked.diags.is_empty(),
		"check failed: {:?}",
		checked.diags
	);
	let lowered = lower_hir_with_prelude_runtime_and_deps(
		&user.tree,
		std::slice::from_ref(&prelude.tree),
		1,
		&checked,
	);

	assert!(
		lowered
			.module
			.enums
			.iter()
			.all(|enum_| enum_.name != "Order")
	);
	assert!(
		lowered
			.prelude_runtime
			.enums
			.iter()
			.any(|enum_| enum_.name == "Order")
	);
	assert_eq!(lowered.module.funcs.len(), 1);
	assert_eq!(lowered.module.funcs[0].name, "equal");
	assert!(lowered.prelude_runtime.funcs.is_empty());

	let merged = lower_hir_with_prelude(&user.tree, std::slice::from_ref(&prelude.tree), &checked);
	assert_eq!(merged.funcs[0].name, "equal");
	assert_eq!(merged.enums[0].name, "Order");
}

fn lower_split(prelude_src: &str, user_src: &str) -> nymph_sema::LoweredHir {
	let prelude = parse_module(prelude_src, "prelude");
	let user = parse_module(user_src, "test");
	assert!(prelude.diagnostics.is_empty(), "prelude parse failed");
	assert!(user.diagnostics.is_empty(), "user parse failed");
	let checked = check_module_with_prelude(&user.tree, std::slice::from_ref(&prelude.tree));
	assert!(
		checked.diags.is_empty(),
		"check failed: {:?}",
		checked.diags
	);
	lower_hir_with_prelude_runtime_and_deps(
		&user.tree,
		std::slice::from_ref(&prelude.tree),
		1,
		&checked,
	)
}

#[test]
fn separates_demanded_ambient_struct_and_let_from_consumer_declarations() {
	let lowered = lower_split(
		"let answer: int = 42\nstruct Box(value: int)",
		"let local: int = answer\nfunc make(): Box = Box(value = local)",
	);
	assert_eq!(
		lowered
			.module
			.lets
			.iter()
			.map(|l| l.name.as_str())
			.collect::<Vec<_>>(),
		["local"]
	);
	assert_eq!(
		lowered
			.module
			.funcs
			.iter()
			.map(|f| f.name.as_str())
			.collect::<Vec<_>>(),
		["make"]
	);
	assert!(lowered.module.classes.is_empty());
	assert_eq!(
		lowered
			.prelude_runtime
			.lets
			.iter()
			.map(|l| l.name.as_str())
			.collect::<Vec<_>>(),
		["answer"]
	);
	assert_eq!(
		lowered
			.prelude_runtime
			.classes
			.iter()
			.map(|c| c.name.as_str())
			.collect::<Vec<_>>(),
		["Box"]
	);
}

#[test]
fn keeps_consumer_enums_and_classes_separate_from_ambient_runtime_declarations() {
	let lowered = lower_split(
		"enum AmbientChoice { Selected }\nstruct AmbientBox(value: int)",
		"enum ConsumerChoice { Selected }\nstruct ConsumerBox(value: int)\nfunc ambient_choice(): AmbientChoice = AmbientChoice.Selected\nfunc ambient_box(): AmbientBox = AmbientBox(value = 1)",
	);

	assert_eq!(
		lowered
			.module
			.enums
			.iter()
			.map(|enum_| enum_.name.as_str())
			.collect::<Vec<_>>(),
		["ConsumerChoice"]
	);
	assert_eq!(
		lowered
			.module
			.classes
			.iter()
			.map(|class| class.name.as_str())
			.collect::<Vec<_>>(),
		["ConsumerBox"]
	);
	assert_eq!(
		lowered
			.prelude_runtime
			.enums
			.iter()
			.map(|enum_| enum_.name.as_str())
			.collect::<Vec<_>>(),
		["AmbientChoice"]
	);
	assert_eq!(
		lowered
			.prelude_runtime
			.classes
			.iter()
			.map(|class| class.name.as_str())
			.collect::<Vec<_>>(),
		["AmbientBox"]
	);
}

#[test]
fn compatibility_wrapper_orders_ambient_lets_before_referencing_consumer_lets() {
	let prelude = parse_module("let ambient: int = 41", "prelude");
	let user = parse_module("let consumer: int = ambient + 1", "test");
	assert!(prelude.diagnostics.is_empty(), "prelude parse failed");
	assert!(user.diagnostics.is_empty(), "user parse failed");
	let checked = check_module_with_prelude(&user.tree, std::slice::from_ref(&prelude.tree));
	assert!(
		checked.diags.is_empty(),
		"check failed: {:?}",
		checked.diags
	);

	let merged = lower_hir_with_prelude(&user.tree, std::slice::from_ref(&prelude.tree), &checked);
	assert_eq!(
		merged
			.lets
			.iter()
			.map(|let_| let_.name.as_str())
			.collect::<Vec<_>>(),
		["ambient", "consumer"]
	);
}

#[test]
fn separates_demanded_runtime_top_level_function() {
	let lowered = lower_split(
		"impl #[int] { func second(): int = this[1] }",
		"func read(xs: #[int]): int = xs.second()",
	);
	assert_eq!(
		lowered
			.module
			.funcs
			.iter()
			.map(|f| f.name.as_str())
			.collect::<Vec<_>>(),
		["read"]
	);
	assert!(
		lowered
			.prelude_runtime
			.funcs
			.iter()
			.any(|f| f.name == "$std$$list$second")
	);
}

#[test]
fn demanded_runtime_function_carries_its_declaration_module_owner() {
	let prelude = parse_module("impl #[int] { func second(): int = this[1] }", "list");
	let user = parse_module("func read(xs: #[int]): int = xs.second()", "test");
	let checked = check_module_with_prelude(&user.tree, std::slice::from_ref(&prelude.tree));
	assert!(
		checked.diags.is_empty(),
		"check failed: {:?}",
		checked.diags
	);
	let lowered = nymph_sema::lower_hir_with_prelude_runtime_and_deps_with_owners(
		&user.tree,
		std::slice::from_ref(&prelude.tree),
		&[nymph_sema::RuntimeOwner::Project(
			"std/collections/list".into(),
		)],
		1,
		&checked,
	);
	assert_eq!(
		lowered.runtime_func_owners.get("$std$$list$second"),
		Some(&nymph_sema::RuntimeOwner::Project(
			"std/collections/list".into()
		))
	);
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
			result: BuiltinResult::Int,
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
			args: vec![HirExpr::Num(1.0, NumKind::Int)],
		}
	);
}

#[test]
fn lowers_collections_and_index() {
	let hir = lower("func f(): #[int] = #[1, 2, 3]");
	assert_eq!(
		value_with_runtime_prototype(&hir.funcs[0].body, "NList", &["NInt"]),
		&HirExpr::Array {
			kind: HirArrayKind::List,
			items: vec![
				HirExpr::Num(1.0, NumKind::Int),
				HirExpr::Num(2.0, NumKind::Int),
				HirExpr::Num(3.0, NumKind::Int)
			],
		},
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
fn lowers_custom_index_access_to_the_resolved_method() {
	let hir = lower(
		r#"
		interface Index<Key, Output> { func index(key: Key): Output }
		struct Offset(base: int) {
			impl Index<Key = int, Output = int> {
				func index(key: int): int = this.base + key
			}
		}
		func f(): int = Offset(base = 40)[2]
		"#,
	);
	let f = hir.funcs.iter().find(|func| func.name == "f").expect("f");
	assert!(matches!(
		&f.body,
		HirExpr::Call { callee, args }
			if matches!(callee.as_ref(), HirExpr::Field { name, .. } if name == "index")
				&& args.len() == 1
	));
}

#[test]
fn custom_index_widens_an_integer_literal_to_the_key_type() {
	let hir = lower(
		r#"
		interface Index<Key, Output> { func index(key: Key): Output }
		struct Echo {
			impl Index<Key = uint, Output = uint> {
				func index(key: uint): uint = key
			}
		}
		func f(): uint = Echo()[2]
		"#,
	);
	let f = hir.funcs.iter().find(|func| func.name == "f").expect("f");
	assert!(matches!(
		&f.body,
		HirExpr::Call { args, .. } if args == &[HirExpr::Num(2.0, NumKind::UInt)]
	));
}

#[test]
fn custom_index_widens_keys_through_generic_substitution_and_bounds() {
	let hir = lower(
		r#"
		interface Index<Key, Output> { func index(key: Key): Output }
		struct Echo<T>(value: T) {
			impl Index<Key = T, Output = T> {
				func index(key: T): T = key
			}
		}
		func concrete(): uint = Echo(value = 1u)[2]
		func generic<T: Index<Key = uint, Output = uint>>(value: T): uint = value[3]
		"#,
	);
	for (name, value) in [("concrete", 2.0), ("generic", 3.0)] {
		let func = hir.funcs.iter().find(|func| func.name == name).expect(name);
		assert!(matches!(
			&func.body,
			HirExpr::Call { args, .. }
				if matches!(args.as_slice(), [HirExpr::Num(n, NumKind::UInt)] if *n == value)
		));
	}
}

#[test]
fn inherent_index_widens_an_integer_literal_to_its_parameter_type() {
	let hir = lower(
		r#"
		struct Echo {
			func index(key: uint): uint = key
		}
		func f(): uint = Echo()[2]
		"#,
	);
	let f = hir.funcs.iter().find(|func| func.name == "f").expect("f");
	assert!(matches!(
		&f.body,
		HirExpr::Call { args, .. } if args == &[HirExpr::Num(2.0, NumKind::UInt)]
	));
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
	let HirExpr::New { class, fields, .. } = &f.body else {
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
fn compatibility_lowering_preserves_positional_variant_field_selection() {
	let hir = lower(
		"enum Choice { Some(value: int), None }\n\
		 func named(choice: Choice): int = match (choice) { Choice.Some(item) -> item, Choice.None -> 0 }\n\
		 func explicit(choice: Choice): int = match (choice) { Choice.Some(_) -> 1, Choice.None -> 0 }",
	);
	for function in &hir.funcs {
		let HirExpr::Match { arms, .. } = &function.body else {
			panic!("{} must lower to a match", function.name)
		};
		let HirPat::Variant { fields, .. } = &arms[0].pat else {
			panic!("{} first arm must be a variant", function.name)
		};
		assert_eq!(fields.len(), 1);
		assert_eq!(fields[0].0, "value");
	}
}

#[test]
fn lowers_whole_and_nested_binding_subpatterns() {
	let hir = lower(
		"func f(value: #(int, int)): int = match (value) {
		   whole = #(left, right) -> left + right,
		 }",
	);
	let HirExpr::Match { arms, .. } = &hir.funcs[0].body else {
		panic!("expected match");
	};
	assert!(matches!(
		&arms[0].pat,
		HirPat::Binding { name, sub: Some(sub) }
			if name == "whole"
				&& matches!(sub.as_ref(), HirPat::Tuple(items)
					if matches!(&items[0], HirPat::Binding { name, .. } if name == "left")
						&& matches!(&items[1], HirPat::Binding { name, .. } if name == "right"))
	));
}

#[test]
fn lowers_consistently_bound_union_patterns() {
	let hir = lower(
		"func f(value: int): int = match (value) {
		   (x = 1 | x = 2) -> x,
		   _ -> 0,
		 }",
	);
	let HirExpr::Match { arms, .. } = &hir.funcs[0].body else {
		panic!("expected match");
	};
	let HirPat::Or(left, right) = &arms[0].pat else {
		panic!("expected union pattern");
	};
	assert!(matches!(left.as_ref(), HirPat::Binding { name, .. } if name == "x"));
	assert!(matches!(right.as_ref(), HirPat::Binding { name, .. } if name == "x"));
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
			result: BuiltinResult::Int,
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
			result: BuiltinResult::Int,
			lhs: Box::new(HirExpr::Local("x".into())),
			rhs: Box::new(HirExpr::Num(1.0, NumKind::Int)),
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
fn bounded_generic_plus_default_lowers_through_the_bound() {
	let hir = lower(
		r#"
		interface Plus<Other, Output> {
			func base(): Output
			func plus(other: Other): Output = this.base()
		}
		func add<T: Plus<Other = T, Output = T>>(t1: T, t2: T): T = t1 + t2
		"#,
	);
	let add = hir.funcs.iter().find(|f| f.name == "add").expect("add");
	assert!(
		matches!(&add.body, HirExpr::BoundDispatch { method, .. } if method == "plus"),
		"expected generic bound dispatch, got {:?}",
		add.body
	);
}

// ── Slice 4C-c, Task 2: comparison-operator lowering pins (W1, W4) ──────────

#[test]
fn bounded_generic_less_than_lowers_through_the_bound() {
	let hir = lower(
		r#"
		interface Comparable<Other> { func less_than(other: Other): boolean }
		func lt<T: Comparable<Other = T>>(a: T, b: T): boolean = a < b
		"#,
	);
	let lt = hir.funcs.iter().find(|f| f.name == "lt").expect("lt");
	assert!(
		matches!(&lt.body, HirExpr::BoundDispatch { method, .. } if method == "less_than"),
		"expected generic bound dispatch, got {:?}",
		lt.body
	);
}

#[test]
fn this_less_than_other_in_interface_default_body_dispatches_to_this_method() {
	// W4 (closed by the stdlib body materialization slice's
	// `materializing_onto_class` mechanism): an interface default method
	// whose *own* body uses `this < other` directly (rather than calling
	// another method) checks `this` bound to a rigid synthetic `Param`
	// (`check_interface_default_bodies`) — W1 routes that `Param` receiver
	// through `dispatch_operator`, recording `MethodSource::GenericBound` →
	// `UserImplDefaultMethod`, regardless of whether the interface is local
	// or a prelude one. `Vec2` never overrides `at_most`, so its default
	// body (with this still-generic resolution) is materialized onto
	// `Vec2`'s class — and unlike before, lowering now recognizes it's
	// materializing a default body ONTO A CONCRETE CLASS (not a still-
	// generic function parameter, where the native-op-vs-method-call
	// ambiguity `dispatch_kind_for_operator`'s doc comment describes is
	// real): `Vec2` can only ever satisfy `<` via a method call, so `this <
	// other` inside the materialized `at_most` lowers to an ordinary
	// `this.less_than(other)` dispatch instead of panicking.
	let hir = lower(
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
	let class = hir.classes.iter().find(|c| c.name == "Vec2").expect("Vec2");
	let at_most = class
		.methods
		.iter()
		.find(|m| m.name == "at_most")
		.expect("at_most materialized onto Vec2");
	let HirExpr::Call { callee, args } = &at_most.body else {
		panic!("expected Call, got {:?}", at_most.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "less_than");
	assert!(matches!(recv.as_ref(), HirExpr::This));
	assert_eq!(args.len(), 1);
	assert!(matches!(&args[0], HirExpr::Local(n) if n == "other"));
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
			result: BuiltinResult::Boolean,
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
	let mut stripped = checked;
	stripped.facts.annotations = nymph_sema::Annotations::default();
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
			result: BuiltinResult::Int,
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
			result: BuiltinResult::Int,
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
	let mut stripped = checked;
	stripped.facts.annotations = nymph_sema::Annotations::default();
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
		&vec![HirStmt::Return {
			value: Some(HirExpr::Local("n".into())),
			target: nymph_hir::hir::HirReturnTarget::Callable
		}]
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
			stmts: vec![HirStmt::Return {
				value: None,
				target: nymph_hir::hir::HirReturnTarget::Callable
			}],
			tail: None,
		}
	);
}

#[test]
fn lowers_return_as_an_unbraced_match_arm_body() {
	let hir = lower(
		r#"
		func f(n: int): int = match (n) {
			0 -> return 7,
			_ -> n,
		}
		"#,
	);
	let HirExpr::Match { arms, .. } = &hir.funcs[0].body else {
		panic!("expected match body");
	};
	assert!(matches!(
		&arms[0].body,
		HirExpr::Block { stmts, tail: None }
			if matches!(stmts.as_slice(), [HirStmt::Return { value: Some(HirExpr::Num(7.0, NumKind::Int)), .. }])
	));
}

#[test]
fn lowers_return_as_a_direct_eager_operand_without_operator_dispatch() {
	let hir = lower("func value(): int = 1 + return 9");
	assert!(matches!(
		&hir.funcs[0].body,
		HirExpr::Block { stmts, tail: Some(tail) }
			if matches!(stmts.as_slice(), [HirStmt::Expr(HirExpr::Num(1.0, NumKind::Int))])
				&& matches!(tail.as_ref(), HirExpr::Block { stmts, tail: None }
					if matches!(stmts.as_slice(), [HirStmt::Return { value: Some(HirExpr::Num(9.0, NumKind::Int)), .. }])
				)
	));
}

#[test]
fn lowers_semantic_never_operands_without_operator_annotations() {
	let hir = lower(
		"func stop(): never = stop()\nfunc binary(): int = 1 + stop()\nfunc prefix(): int = -(stop())\nfunc cast(): int = stop() as int\nfunc index(): int = stop()[0]",
	);
	for function in &hir.funcs[1..] {
		assert!(
			matches!(function.body, HirExpr::Call { .. } | HirExpr::Block { .. }),
			"{} retained a shell around its never operand: {:?}",
			function.name,
			function.body
		);
	}
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
	assert_eq!(value, &HirExpr::Num(1.0, NumKind::Int));

	let HirStmt::Let { name, value, .. } = &stmts[1] else {
		panic!("expected a Let statement, got {:?}", stmts[1]);
	};
	assert_eq!(name, "x$1", "same-scope redeclaration renames");
	assert_eq!(
		value,
		&HirExpr::Binary {
			op: BinOp::Add,
			result: BuiltinResult::Int,
			lhs: Box::new(HirExpr::Local("x".into())),
			rhs: Box::new(HirExpr::Num(1.0, NumKind::Int)),
		},
		"the redeclaration's RHS reads the PRIOR binding, not itself"
	);

	let tail = tail.as_ref().expect("tail present");
	assert_eq!(
		tail.as_ref(),
		&HirExpr::Binary {
			op: BinOp::Mul,
			result: BuiltinResult::Int,
			lhs: Box::new(HirExpr::Local("x$1".into())),
			rhs: Box::new(HirExpr::Num(10.0, NumKind::Int)),
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
			result: BuiltinResult::Int,
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
			result: BuiltinResult::Int,
			lhs: Box::new(HirExpr::Local("i".into())),
			rhs: Box::new(HirExpr::Num(100.0, NumKind::Int)),
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
			value: HirExpr::Num(42.0, NumKind::Int),
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
				value: HirExpr::Num(10.0, NumKind::Int),
			},
			HirLet {
				name: "total".into(),
				mutable: false,
				value: HirExpr::Binary {
					op: BinOp::Add,
					result: BuiltinResult::Int,
					lhs: Box::new(HirExpr::Local("base".into())),
					rhs: Box::new(HirExpr::Num(5.0, NumKind::Int)),
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
			value: HirExpr::Num(0.0, NumKind::Int),
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
				value: HirExpr::Num(10.0, NumKind::Int),
			},
			HirLet {
				name: "a".into(),
				mutable: false,
				value: HirExpr::Binary {
					op: BinOp::Add,
					result: BuiltinResult::Int,
					lhs: Box::new(HirExpr::Local("b".into())),
					rhs: Box::new(HirExpr::Num(1.0, NumKind::Int)),
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

#[test]
fn closure_param_shadowing_another_top_level_let_name_does_not_create_a_false_dependency() {
	// `let a = (b: int) -> b + 1; let b = (a: int) -> a + 1;` — each closure's
	// OWN parameter merely shares the OTHER top-level `let`'s name; neither
	// closure body actually reads the other top-level `let`. The Y3 module-let
	// dependency analysis (`collect_locals`/`reorder_lets_by_dependency`) must
	// not conflate a closure's bound parameter with a genuine free-variable
	// reference to a same-named top-level `let` — otherwise this legal,
	// non-cyclic program spuriously looks circular and lowering panics.
	let hir = lower(
		r#"
		let a = (b: int) -> b + 1
		let b = (a: int) -> a + 1
		func f(): int = a(1) + b(2)
		"#,
	);
	let names: Vec<&str> = hir.lets.iter().map(|l| l.name.as_str()).collect();
	assert_eq!(
		names,
		["a", "b"],
		"neither closure body references the other top-level `let`; a closure's OWN param \
		 (which merely shares the other let's name) must not be treated as a free-variable \
		 dependency edge, so source order is preserved"
	);
}

#[test]
fn closure_param_shadowing_a_later_let_does_not_silently_reorder_it_first() {
	// One-directional variant of the above: `let a = (b: int) -> b + 1; let b
	// = (x: int) -> x + 1;` — `a`'s closure param happens to be named `b`, the
	// OTHER top-level let's name, but the closure body never reads the real
	// top-level `b`. Before the fix, `collect_locals` reported the closure's
	// bound param as if it were a free reference to top-level `b`, so `b` got
	// spuriously reordered ahead of `a` even though nothing requires it.
	let hir = lower(
		r#"
		let a = (b: int) -> b + 1
		let b = (x: int) -> x + 1
		"#,
	);
	let names: Vec<&str> = hir.lets.iter().map(|l| l.name.as_str()).collect();
	assert_eq!(
		names,
		["a", "b"],
		"`a`'s closure param `b` shadows, but never references, the real top-level `let b`; \
		 source order must be preserved rather than spuriously reordered"
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
			stmts: vec![HirStmt::Return {
				value: Some(HirExpr::Local("n".into())),
				target: nymph_hir::hir::HirReturnTarget::Callable
			}],
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
			stmts: vec![HirStmt::Return {
				value: Some(HirExpr::Binary {
					op: BinOp::Sub,
					result: BuiltinResult::Int,
					lhs: Box::new(HirExpr::Num(0.0, NumKind::Int)),
					rhs: Box::new(HirExpr::Local("n".into())),
				}),
				target: nymph_hir::hir::HirReturnTarget::Callable
			}],
			tail: None,
		}
	);
}

// ── Slice 4H: string expressions ─────────────────────────────────────────────

#[test]
fn lowers_a_plain_string_literal() {
	let hir = lower(r#"func f(): string = "hello""#);
	assert_eq!(hir.funcs[0].body, HirExpr::Str("hello".into()));
}

#[test]
fn lowers_string_escapes() {
	let hir = lower(r#"func f(): string = "a\nb\"c\\d""#);
	assert_eq!(hir.funcs[0].body, HirExpr::Str("a\nb\"c\\d".into()));
}

#[test]
fn lowers_string_interpolation_through_display() {
	let hir = lower(r#"func f(name: string): string = "Hello, ${name}!""#);
	assert_eq!(
		hir.funcs[0].body,
		HirExpr::InterpolatedString(vec![
			HirExpr::Str("Hello, ".into()),
			HirExpr::ExternCall {
				module: "std/display",
				symbol: "display",
				args: vec![HirExpr::Local("name".into())],
			},
			HirExpr::Str("!".into()),
		])
	);
}

#[test]
fn lowers_balanced_block_inside_string_interpolation() {
	let hir = lower(r#"func f(): string = "value=${{ let n = 1 n }}""#);
	let HirExpr::InterpolatedString(parts) = &hir.funcs[0].body else {
		panic!("expected interpolated string");
	};
	assert_eq!(parts[0], HirExpr::Str("value=".into()));
	let HirExpr::ExternCall {
		module,
		symbol,
		args,
	} = &parts[1]
	else {
		panic!("expected Display call for interpolated block");
	};
	assert_eq!((*module, *symbol), ("std/display", "display"));
	assert!(matches!(args.as_slice(), [HirExpr::Block { .. }]));
}

#[test]
fn lowers_leading_interpolation_without_a_coercion_prefix() {
	let hir = lower(r#"func f(n: int): string = "${n}!""#);
	assert_eq!(
		hir.funcs[0].body,
		HirExpr::InterpolatedString(vec![
			HirExpr::ExternCall {
				module: "std/display",
				symbol: "display",
				args: vec![HirExpr::Local("n".into())],
			},
			HirExpr::Str("!".into()),
		])
	);
}

#[test]
fn lowers_string_pattern_escapes_instead_of_panicking() {
	use nymph_hir::hir::{HirLit, HirPat};

	// String PATTERNS may also carry escapes (`StringPatternPart` has no
	// interpolation variant) — cooking now extends here too, replacing the old
	// escapes-always-panic arm.
	let hir = lower(
		r#"
		func f(s: string): int = match (s) {
			"a\nb" -> 1,
			_ -> 0,
		}
		"#,
	);
	let HirExpr::Match { arms, .. } = &hir.funcs[0].body else {
		panic!("expected Match");
	};
	assert!(matches!(&arms[0].pat, HirPat::Lit(HirLit::Str(s)) if s == "a\nb"));
}

// ── Slice 4H: range/for-loop expressions ────────────────────────────────────

#[test]
fn lowers_an_exclusive_range_for_loop_through_the_iterator_protocol() {
	let hir = lower(
		r#"
		func f(): int = {
			let mut total = 0
			for (i in 1..3) {
				total = total + i
			}
			total
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Expr(HirExpr::Block {
		stmts: for_stmts, ..
	}) = &stmts[1]
	else {
		panic!("expected desugared for-loop Block, got {:?}", stmts[1]);
	};
	assert_range_protocol(for_stmts, false);
}

#[test]
fn range_for_loop_enters_the_iterator_protocol() {
	let hir = lower("func f(): void = for (i in 1..3) { i }");
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected protocol block");
	};
	let HirStmt::Let { value, .. } = &stmts[0] else {
		panic!("expected iterator binding");
	};
	assert!(matches!(
		value,
		HirExpr::Call { callee, args }
			if args.is_empty() && matches!(callee.as_ref(), HirExpr::Field { name, .. } if name == "iter")
	));
}

#[test]
fn generic_iterable_bound_lowers_through_iter() {
	let hir = lower(
		"enum Option<T> { Some(value: T), None }
		 interface Iterator<Item> { mut func next(): Option<Item> }
		 interface Iterable<Item> { func iter(): Iterator<Item> }
		 func consume<T: Iterable<Item = int>>(items: T): void = for (item in items) { item }",
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected protocol block");
	};
	assert!(matches!(
		&stmts[0],
		HirStmt::Let { value: HirExpr::Call { callee, args }, .. }
			if args.is_empty() && matches!(callee.as_ref(), HirExpr::Field { name, .. } if name == "iter")
	));
}

#[test]
fn lowers_an_inclusive_range_for_loop_through_the_iterator_protocol() {
	let hir = lower(
		r#"
		func f(): int = {
			for (i in 1..=3) {
				i
			}
			0
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block");
	};
	let HirStmt::Expr(HirExpr::Block {
		stmts: for_stmts, ..
	}) = &stmts[0]
	else {
		panic!("expected desugared for-loop Block, got {:?}", stmts[0]);
	};
	assert_range_protocol(for_stmts, true);
}

fn assert_range_protocol(stmts: &[HirStmt], inclusive: bool) {
	assert_eq!(stmts.len(), 3, "iterator let, continuation let, while");
	let HirStmt::Let {
		name: iterator,
		mutable: false,
		value: HirExpr::Call { callee, args },
	} = &stmts[0]
	else {
		panic!("expected immutable iterator binding, got {:?}", stmts[0]);
	};
	assert!(args.is_empty());
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected `.iter` field call, got {callee:?}");
	};
	assert_eq!(name, "iter");
	let HirExpr::New { class, fields, .. } = recv.as_ref() else {
		panic!("expected NymphRange construction, got {recv:?}");
	};
	assert_eq!(class, "NymphRange");
	assert_eq!(fields.len(), 3);
	assert_eq!(fields[0], ("start".into(), HirExpr::Num(1.0, NumKind::Int)));
	assert_eq!(fields[1], ("end".into(), HirExpr::Num(3.0, NumKind::Int)));
	assert_eq!(fields[2], ("inclusive".into(), HirExpr::Bool(inclusive)));

	let HirStmt::Let {
		name: continuation,
		mutable: true,
		value: HirExpr::Bool(true),
	} = &stmts[1]
	else {
		panic!("expected mutable continuation binding, got {:?}", stmts[1]);
	};
	let HirStmt::Expr(HirExpr::While { cond, body, .. }) = &stmts[2] else {
		panic!("expected protocol while loop, got {:?}", stmts[2]);
	};
	assert_eq!(cond.as_ref(), &HirExpr::Local(continuation.clone()));
	let HirExpr::Block {
		stmts: body,
		tail: None,
	} = body.as_ref()
	else {
		panic!("expected while body block, got {body:?}");
	};
	let [HirStmt::Expr(HirExpr::Match { scrutinee, arms })] = body.as_slice() else {
		panic!("expected a single protocol match, got {body:?}");
	};
	assert!(matches!(
		scrutinee.as_ref(),
		HirExpr::Call { callee, args }
			if args.is_empty() && matches!(callee.as_ref(), HirExpr::Field { recv, name }
				if name == "next" && recv.as_ref() == &HirExpr::Local(iterator.clone()))
	));
	assert!(
		matches!(&arms[0].pat, HirPat::Variant { enum_name, variant, .. } if enum_name == "Option" && variant == "Some")
	);
	assert!(
		matches!(&arms[1].pat, HirPat::Variant { enum_name, variant, fields } if enum_name == "Option" && variant == "None" && fields.is_empty())
	);
	assert_eq!(
		arms[1].body,
		HirExpr::Assign {
			target: Box::new(HirExpr::Local(continuation.clone())),
			value: Box::new(HirExpr::Bool(false)),
		}
	);
}

#[test]
fn lowers_a_for_loop_with_a_parenthesized_range_bound() {
	// A parenthesized range bound is `ExprKind::Grouped` in the AST. The
	// checker's `check()` recurses through `Grouped` without recording an
	// annotation for the `Grouped` node's own id, so `lower_for`'s numeric-
	// element guard must peel through it to the innermost expression before
	// looking up the annotation — otherwise `annotations.get(min.id)` sees
	// `None` and panics "got element type None" on a perfectly valid program.
	// Exercise both a parenthesized literal bound and a parenthesized
	// binary-expression bound.
	let hir = lower(
		r#"
        func f(): int = {
            let mut total = 0
            for (i in (1)..5) {
                total = total + i
            }
            total
        }
        "#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block");
	};
	let HirStmt::Expr(HirExpr::Block {
		stmts: for_stmts, ..
	}) = &stmts[1]
	else {
		panic!("expected desugared for-loop Block, got {:?}", stmts[1]);
	};
	assert!(
		matches!(&for_stmts[2], HirStmt::Expr(HirExpr::While { .. })),
		"expected While, got {:?}",
		for_stmts[2]
	);

	let hir = lower(
		r#"
        func g(a: int, b: int, n: int): int = {
            let mut total = 0
            for (i in (a + b)..n) {
                total = total + i
            }
            total
        }
        "#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block");
	};
	let HirStmt::Expr(HirExpr::Block {
		stmts: for_stmts, ..
	}) = &stmts[1]
	else {
		panic!("expected desugared for-loop Block, got {:?}", stmts[1]);
	};
	assert!(
		matches!(&for_stmts[2], HirStmt::Expr(HirExpr::While { .. })),
		"expected While, got {:?}",
		for_stmts[2]
	);
}

#[test]
fn range_values_lower_to_canonical_structs_in_compatibility_pipeline() {
	let hir = lower(
		"struct Range<T>(start: T, end: T)\nstruct RangeFrom<T>(start: T)\nstruct RangeTo<T>(end: T)\nstruct RangeInclusive<T>(start: T, end: T)\nstruct RangeToInclusive<T>(end: T)\nfunc values(): void = { let a = 1..2\nlet b = 3..\nlet c = ..4\nlet d = 5..=6\nlet e = ..=7 }",
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected block")
	};
	let classes = stmts
		.iter()
		.map(|stmt| match stmt {
			HirStmt::Let {
				value: HirExpr::New { class, .. },
				..
			} => class.as_str(),
			other => panic!("expected canonical range construction, got {other:?}"),
		})
		.collect::<Vec<_>>();
	assert_eq!(
		classes,
		[
			"Range",
			"RangeFrom",
			"RangeTo",
			"RangeInclusive",
			"RangeToInclusive"
		]
	);
}

#[test]
#[should_panic(expected = "start-less")]
fn for_loop_over_a_startless_range_panics_in_lowering() {
	lower(
		r#"
		func f(): int = {
			for (i in ..10) {
				i
			}
			0
		}
		"#,
	);
}

#[test]
#[should_panic(expected = "unbounded")]
fn for_loop_over_an_unbounded_range_panics_in_lowering() {
	lower(
		r#"
		func f(): int = {
			for (i in 1..) {
				i
			}
			0
		}
		"#,
	);
}

#[test]
fn range_for_loop_accepts_a_non_binding_pattern_through_protocol_lowering() {
	lower(
		r#"
		func f(): int = {
			for (_ in 1..3) {
				0
			}
			0
		}
		"#,
	);
}

// ── Slice 4I, Task 2: `|>`, `in`/`!in`, `??` lowering ────────────────────────

#[test]
fn lowers_pipe_to_a_structural_call() {
	// DD1: `x |> f` lowers to `Call { callee: <lowered f>, args: [<lowered x>] }` —
	// no `Resolution` involved at all.
	let hir = lower(
		r#"
		func double(x: int): int = x * 2
		func f(a: int): int = a |> double
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Block {
		stmts,
		tail: Some(tail),
	} = &f.body
	else {
		panic!("expected sequenced pipe, got {:?}", f.body);
	};
	assert!(matches!(stmts.as_slice(), [HirStmt::Let { value: HirExpr::Local(n), .. }] if n == "a"));
	let HirExpr::Call { callee, args } = tail.as_ref() else {
		panic!("expected pipe call");
	};
	assert!(matches!(callee.as_ref(), HirExpr::Local(n) if n == "double"));
	assert_eq!(args.len(), 1);
	assert!(matches!(&args[0], HirExpr::Local(n) if n == "$pipe"));
}

#[test]
fn lowers_chained_pipe_left_associatively() {
	// `10 |> double |> inc` parses left-associative (`(10 |> double) |> inc`), so
	// the outer `Call`'s single argument is itself the inner pipe's `Call`.
	let hir = lower(
		r#"
		func double(x: int): int = x * 2
		func inc(x: int): int = x + 1
		func f(): int = 10 |> double |> inc
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Block {
		stmts,
		tail: Some(tail),
	} = &f.body
	else {
		panic!("expected outer pipe block, got {:?}", f.body);
	};
	let [
		HirStmt::Let {
			value: HirExpr::Block { .. },
			..
		},
	] = stmts.as_slice()
	else {
		panic!("expected the inner pipe to initialize the outer temporary");
	};
	let HirExpr::Call { callee, args } = tail.as_ref() else {
		panic!("expected outer pipe call");
	};
	assert!(matches!(callee.as_ref(), HirExpr::Local(n) if n == "inc"));
	assert_eq!(args.len(), 1);
	assert!(matches!(&args[0], HirExpr::Local(n) if n == "$pipe"));
}

#[test]
fn lowers_in_operator_with_swapped_receiver() {
	// DD2: `a in c` ≡ `c.contains(a)` — the RHS is the receiver, the LHS is the
	// sole argument (operand order swapped relative to every other operator).
	let hir = lower(
		r#"
		interface Contains<Item> { func contains(item: Item): boolean }
		struct Bag(n: int)
		impl Contains<Item = int> for Bag {
			func contains(item: int): boolean = true
		}
		func f(b: Bag, x: int): boolean = x in b
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Block {
		stmts,
		tail: Some(tail),
	} = &f.body
	else {
		panic!("expected sequenced membership, got {:?}", f.body);
	};
	assert!(matches!(stmts.as_slice(), [HirStmt::Let { value: HirExpr::Local(n), .. }] if n == "x"));
	let HirExpr::Call { callee, args } = tail.as_ref() else {
		panic!("expected membership call");
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "contains");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "b"));
	assert_eq!(args.len(), 1);
	assert!(matches!(&args[0], HirExpr::Local(n) if n == "$member"));
}

#[test]
fn lowers_not_in_operator_to_not_contains() {
	let hir = lower(
		r#"
		interface Contains<Item> {
			func contains(item: Item): boolean
			func not_contains(item: Item): boolean
		}
		struct Bag(n: int)
		impl Contains<Item = int> for Bag {
			func contains(item: int): boolean = true
			func not_contains(item: int): boolean = false
		}
		func f(b: Bag, x: int): boolean = x !in b
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Block {
		stmts,
		tail: Some(tail),
	} = &f.body
	else {
		panic!("expected sequenced membership, got {:?}", f.body);
	};
	assert!(matches!(stmts.as_slice(), [HirStmt::Let { value: HirExpr::Local(n), .. }] if n == "x"));
	let HirExpr::Call { callee, args } = tail.as_ref() else {
		panic!("expected membership call");
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "not_contains");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "b"));
	assert_eq!(args.len(), 1);
	assert!(matches!(&args[0], HirExpr::Local(n) if n == "$member"));
}

#[test]
fn lowers_user_unwrap_impl_to_an_eager_method_call() {
	// DD3 (corrected): Nymph has no optional runtime representation, so every
	// `??` resolution is `UserImpl` — an eager `recv.unwrap(fallback)` call, never
	// a native JS `??` and never short-circuiting.
	let hir = lower(
		r#"
		interface Unwrap<Output> { func unwrap(default: Output): Output }
		struct MaybeInt(present: boolean, value: int)
		impl Unwrap<Output = int> for MaybeInt {
			func unwrap(default: int): int = default
		}
		func f(m: MaybeInt, d: int): int = m ?? d
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Call { callee, args } = &f.body else {
		panic!("expected Call, got {:?}", f.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "unwrap");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "m"));
	assert_eq!(args.len(), 1);
	assert!(matches!(&args[0], HirExpr::Local(n) if n == "d"));
}

#[test]
#[should_panic(expected = "interface default method")]
fn unwrap_bounded_generic_default_still_panics_in_lowering() {
	// `GenericBound` → `UserImplDefaultMethod`: codegen cannot yet materialize an
	// interface default method generically, so this stays a loud lowering panic
	// (mirrors every other operator's `UserImplDefaultMethod` treatment).
	lower(
		"interface Unwrap<Output> { func unwrap(default: Output): Output }
		 func f<T: Unwrap<Output = int>>(a: T, b: int): int = a ?? b",
	);
}

// ── Slice 4J: `namespace func` statics, `mut func` methods ─────────────────

#[test]
fn lowers_a_struct_namespaced_function_into_statics() {
	let hir = lower(
		"struct Point(x: int) {
		   namespace func at(v: int): Point = Point(x = v)
		 }
		 func origin(): Point = Point.at(0)",
	);
	assert_eq!(hir.classes.len(), 1);
	let class = &hir.classes[0];
	assert_eq!(class.name, "Point");
	// The namespaced function lands in `statics`, not `methods`.
	assert!(class.methods.is_empty(), "no instance methods: {class:?}");
	assert_eq!(class.statics.len(), 1);
	assert_eq!(class.statics[0].name, "at");
	// The call site `Point.at(0)` needed zero lowering changes: it already
	// falls to the generic `Call` arm, resolving to `Field { recv: Local
	// ("Point"), name: "at" }`.
	let origin = hir.funcs.iter().find(|f| f.name == "origin").unwrap();
	let HirExpr::Call { callee, args } = &origin.body else {
		panic!("expected Call, got {:?}", origin.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "at");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "Point"));
	assert_eq!(args.len(), 1);
}

#[test]
fn lowers_an_enum_namespaced_function_into_statics() {
	let hir = lower(
		"enum Opt<T> {
		   Some(value: T),
		   None

		   namespace func empty(): self = None
		 }
		 func none_int(): Opt<int> = Opt.empty()",
	);
	let e = hir.enums.iter().find(|e| e.name == "Opt").expect("Opt");
	assert!(e.methods.is_empty(), "no instance methods: {e:?}");
	assert_eq!(e.statics.len(), 1);
	assert_eq!(e.statics[0].name, "empty");
}

#[test]
#[should_panic(expected = "collides with a variant")]
fn enum_namespaced_function_colliding_with_a_variant_name_panics() {
	// A namespaced fn sharing a variant's name would put two entries under the
	// same key on the enum's returned object (Slice 4J hazard) — loud, not a
	// silent last-wins.
	lower(
		"enum Color {
		   Red

		   namespace func Red(): Color = Color.Red
		 }",
	);
}

#[test]
fn lowers_impl_mut_methods_as_ordinary_instance_methods() {
	// The checker enforces nothing extra for a `mut func` — a plain method
	// mutating `this` fields checks identically — so lowering treats a
	// `mut func` exactly like an ordinary instance method.
	let hir = lower(
		"struct Counter(n: int) {
		   mut func bump(): void = { this.n = this.n + 1 }
		 }",
	);
	assert_eq!(hir.classes.len(), 1);
	let class = &hir.classes[0];
	assert_eq!(class.methods.len(), 1);
	assert_eq!(class.methods[0].name, "bump");
	assert!(class.statics.is_empty());
}

#[test]
fn namespaced_call_through_a_generic_parameter_lowers_hidden_type_object() {
	let hir = lower(
		"interface Default { func default(): self }
		 func make<T: Default>(): T = T.default()",
	);
	let make = hir
		.funcs
		.iter()
		.find(|function| function.name == "make")
		.unwrap();
	assert_eq!(make.params, ["$type$0"]);
	assert!(matches!(
		&make.body,
		HirExpr::Call { callee, args }
			if args.is_empty()
				&& matches!(callee.as_ref(), HirExpr::Field { recv, name }
					if name == "default" && matches!(recv.as_ref(), HirExpr::Local(local) if local == "$type$0"))
	));
}

#[test]
fn compatibility_receiverless_lowering_uses_the_checker_selected_shadowed_parameter() {
	let hir = lower(
		"interface Default { func default(): self }
		 struct Box<T: Default> {
		   namespace func make<T: Default>(): T = T.default()
		 }",
	);
	let make = hir.classes[0]
		.statics
		.iter()
		.find(|method| method.name == "make")
		.unwrap();
	assert_eq!(make.params, ["$type$0", "$type$1"]);
	assert!(matches!(
		&make.body,
		HirExpr::Call { callee, args }
			if args.is_empty()
				&& matches!(callee.as_ref(), HirExpr::Field { recv, name }
					if name == "default" && matches!(recv.as_ref(), HirExpr::Local(local) if local == "$type$1"))
	));
}

#[test]
fn compatibility_lowering_appends_forwarded_hidden_arguments_after_source_arguments() {
	let hir = lower(
		"interface Default { func default(): self }
		 func inner<T: Default>(value: T): T = T.default()
		 func outer<U: Default>(value: U): U = inner(value)",
	);
	let outer = hir
		.funcs
		.iter()
		.find(|function| function.name == "outer")
		.unwrap();
	assert_eq!(outer.params, ["value", "$type$0"]);
	assert!(matches!(
		&outer.body,
		HirExpr::Call { callee, args }
			if matches!(callee.as_ref(), HirExpr::Local(name) if name == "inner")
				&& matches!(args.as_slice(), [HirExpr::Local(value), HirExpr::Local(hidden)] if value == "value" && hidden == "$type$0")
	));
}

#[test]
fn namespaced_call_through_a_struct_owned_generic_lowers_hidden_type_object() {
	// Confirmed defect (code review, Slice 4J): `push_generics` used to track
	// only the CURRENT func/method's OWN generics, never the OWNING struct/
	// enum's — so a namespaced call through a struct-owned generic type
	// parameter used inside one of that struct's own methods/statics
	// type-checked with zero diagnostics (the checker resolves it against
	// EVERY active param scope, including the one `collect_adt_inherent`
	// pushes for the struct's own generics) yet was invisible to
	// `is_current_generic`, silently falling through to ordinary lowering and
	// emitting a bare, unbound `T.default()` in the output JS. `T` has no JS
	// binding at all, so this must panic loudly instead.
	lower(
		"interface Default { func default(): self }
		 struct Box<T: Default> {
		   namespace func make(): T = T.default()
		 }",
	);
}

#[test]
fn namespaced_call_through_an_enum_owned_generic_lowers_hidden_type_object() {
	// Same defect, enum-owned generic reached from an ordinary inherent method
	// (not just a `namespace` static) — `push_generics` must also see the
	// enum's own generics while lowering its inherent method bodies.
	let hir = lower(
		"interface Default { func default(): self }
		 enum Box<T: Default> {
		   Empty

		   func make(): T = T.default()
		 }",
	);
	let box_ = hir
		.enums
		.iter()
		.find(|item| item.name == "Box")
		.expect("Box");
	let make = box_
		.methods
		.iter()
		.find(|method| method.name == "make")
		.expect("make");
	let HirExpr::Call { callee, .. } = &make.body else {
		panic!("expected call, got {:?}", make.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected field callee, got {callee:?}");
	};
	assert_eq!(name, "default");
	assert_eq!(
		recv.as_ref(),
		&HirExpr::RuntimeTypeProjection {
			receiver: Box::new(HirExpr::This),
			path: vec![0],
		}
	);
}

// ── Slice 4K: `is`/`!is` desugar, `as` scalar/`Into` dispatch ──────────────

#[test]
fn lowers_is_to_a_two_arm_boolean_match() {
	// HH1: a one-arm pattern match plus a trailing `Wildcard` fallback — never a
	// third case, so runtime fallthrough is structurally impossible.
	let hir = lower("func f(x: int): boolean = x is 5");
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Match { scrutinee, arms } = &f.body else {
		panic!("expected Match, got {:?}", f.body);
	};
	assert!(matches!(scrutinee.as_ref(), HirExpr::Local(n) if n == "x"));
	assert_eq!(arms.len(), 2);
	assert_eq!(arms[0].pat, HirPat::Lit(HirLit::Num(5.0, NumKind::Int)));
	assert!(arms[0].guard.is_none());
	assert_eq!(arms[0].body, HirExpr::Bool(true));
	assert_eq!(arms[1].pat, HirPat::Wildcard);
	assert_eq!(arms[1].body, HirExpr::Bool(false));
}

#[test]
fn lowers_not_is_with_swapped_arm_bodies() {
	// `!is` is the same match shape with `true`/`false` swapped, not a double
	// negation wrapping an `is` match.
	let hir = lower("func f(x: int): boolean = x !is 5");
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Match { arms, .. } = &f.body else {
		panic!("expected Match, got {:?}", f.body);
	};
	assert_eq!(arms[0].body, HirExpr::Bool(false));
	assert_eq!(arms[1].body, HirExpr::Bool(true));
}

#[test]
fn is_pattern_bindings_do_not_leak_into_the_match_arm_body() {
	// The pattern's binding (`n`) is bound only inside the pattern itself, never
	// threaded into the arm body — the body is always a bare `Bool` literal,
	// structurally incapable of referencing it.
	let hir = lower(
		"struct Point(x: int, y: int)
		 func f(p: Point): boolean = p is Point(x = n, y = _)",
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Match { arms, .. } = &f.body else {
		panic!("expected Match, got {:?}", f.body);
	};
	assert_eq!(arms[0].body, HirExpr::Bool(true));
	let HirPat::Struct { fields } = &arms[0].pat else {
		panic!("expected Struct pattern, got {:?}", arms[0].pat);
	};
	assert!(
		fields
			.iter()
			.any(|(name, pat)| name == "x" && matches!(pat, HirPat::Binding { name, .. } if name == "n"))
	);
}

#[test]
fn identity_cast_lowers_to_the_bare_operand() {
	// `P as P` needs no runtime conversion at all — no `ScalarCast`, no `Into`
	// call, just the lowered operand unchanged.
	let hir = lower(
		"struct P(x: int)
		 func f(p: P): P = p as P",
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	assert!(matches!(&f.body, HirExpr::Local(n) if n == "p"));
}

#[test]
fn int_to_float_cast_lowers_to_an_explicit_destination_cast() {
	// `int`/`uint`/`float` share one JS `number` representation, so a cast that
	// only ever widens or reinterprets among them (never crossing into `uint`,
	// which now saturates via `Math.abs`) is a no-op too, not just same-type
	// identity.
	let hir = lower("func f(n: int): float = n as float");
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	assert!(matches!(
		&f.body,
		HirExpr::ScalarCast { kind: ScalarCastKind::ToFloat, operand }
			if matches!(operand.as_ref(), HirExpr::Local(n) if n == "n")
	));
}

#[test]
fn float_to_int_cast_lowers_to_a_saturating_to_int_scalar_cast() {
	let hir = lower("func f(x: float): int = x as int");
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::ScalarCast { kind, operand } = &f.body else {
		panic!("expected ScalarCast, got {:?}", f.body);
	};
	assert_eq!(*kind, ScalarCastKind::SaturatingToInt);
	assert!(matches!(operand.as_ref(), HirExpr::Local(n) if n == "x"));
}

#[test]
fn float_to_uint_cast_lowers_to_a_saturating_to_uint_scalar_cast() {
	let hir = lower("func f(x: float): uint = x as uint");
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::ScalarCast { kind, operand } = &f.body else {
		panic!("expected ScalarCast, got {:?}", f.body);
	};
	assert_eq!(*kind, ScalarCastKind::SaturatingToUInt);
	assert!(matches!(operand.as_ref(), HirExpr::Local(n) if n == "x"));
}

#[test]
fn int_to_uint_cast_lowers_to_a_saturating_to_uint_scalar_cast() {
	// `int as uint` used to be a bare-operand no-op (Slice 4K, HH2); the
	// abs-first saturating rule makes it a real runtime operation now.
	let hir = lower("func f(n: int): uint = n as uint");
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::ScalarCast { kind, operand } = &f.body else {
		panic!("expected ScalarCast, got {:?}", f.body);
	};
	assert_eq!(*kind, ScalarCastKind::SaturatingToUInt);
	assert!(matches!(operand.as_ref(), HirExpr::Local(n) if n == "n"));
}

#[test]
fn char_to_int_cast_lowers_to_a_code_point_of_scalar_cast() {
	let hir = lower("func f(c: char): int = c as int");
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::ScalarCast { kind, .. } = &f.body else {
		panic!("expected ScalarCast, got {:?}", f.body);
	};
	assert_eq!(*kind, ScalarCastKind::CharToInt);
}

#[test]
fn int_to_char_cast_lowers_to_a_char_from_num_scalar_cast() {
	let hir = lower("func f(n: int): char = n as char");
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::ScalarCast { kind, .. } = &f.body else {
		panic!("expected ScalarCast, got {:?}", f.body);
	};
	assert_eq!(*kind, ScalarCastKind::NumToChar);
}

#[test]
fn float_to_char_cast_lowers_to_a_float_to_char_scalar_cast() {
	let hir = lower("func f(x: float): char = x as char");
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::ScalarCast { kind, .. } = &f.body else {
		panic!("expected ScalarCast, got {:?}", f.body);
	};
	assert_eq!(*kind, ScalarCastKind::FloatToChar);
}

#[test]
fn cast_via_user_into_impl_lowers_to_an_into_method_call() {
	let hir = lower(
		"interface Into<Other> { func into(): Other }
		 struct P(x: int)
		 impl Into<string> for P { func into(): string = \"p\" }
		 func f(p: P): string = p as string",
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Call { callee, args } = &f.body else {
		panic!("expected Call, got {:?}", f.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "into");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "p"));
	assert!(args.is_empty());
}

#[test]
fn cast_via_into_impl_with_a_custom_method_name_lowers_to_a_call_to_that_name() {
	// Defect 1: `check_cast` used to hardcode the dispatched method name to
	// `"into"` regardless of what the resolved `Into`-named interface actually
	// declares. A local `interface Into<Other> { func convert(): Other }` must
	// lower `p as string` to a call to `convert`, never to a nonexistent `into`.
	let hir = lower(
		"interface Into<Other> { func convert(): Other }
		 struct P(x: int)
		 impl Into<string> for P { func convert(): string = \"p\" }
		 func f(p: P): string = p as string",
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").expect("f");
	let HirExpr::Call { callee, args } = &f.body else {
		panic!("expected Call, got {:?}", f.body);
	};
	let HirExpr::Field { recv, name } = callee.as_ref() else {
		panic!("expected Field callee, got {callee:?}");
	};
	assert_eq!(name, "convert");
	assert!(matches!(recv.as_ref(), HirExpr::Local(n) if n == "p"));
	assert!(args.is_empty());
}

// ── Closures (Slice 4L) ──────────────────────────────────────────────────────

#[test]
fn lowers_a_paren_closure_expression() {
	let hir = lower(
		r#"
		func f(): int = {
			let g = (x: int) -> x + 1
			g(5)
		}
		"#,
	);
	let HirExpr::Block { stmts, tail } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Let { name, value, .. } = &stmts[0] else {
		panic!("expected a Let statement, got {:?}", stmts[0]);
	};
	assert_eq!(name, "g");
	assert_eq!(
		value,
		&HirExpr::Closure {
			params: vec!["x".into()],
			body: Box::new(HirExpr::Binary {
				op: BinOp::Add,
				result: BuiltinResult::Int,
				lhs: Box::new(HirExpr::Local("x".into())),
				rhs: Box::new(HirExpr::Num(1.0, NumKind::Int)),
			}),
		}
	);
	assert_eq!(
		tail.as_deref(),
		Some(&HirExpr::Call {
			callee: Box::new(HirExpr::Local("g".into())),
			args: vec![HirExpr::Num(5.0, NumKind::Int)],
		})
	);
}

#[test]
fn lowers_a_single_ident_closure_as_a_pipe_rhs() {
	// `10 |> x -> x * 2` — DD1 lowers `|>` structurally to a `Call` whose callee
	// is the (lowered) RHS, so the single-ident closure form becomes the
	// callee here.
	let hir = lower("func f(): int = 10 |> x -> x * 2");
	assert!(matches!(
		&hir.funcs[0].body,
		HirExpr::Block { stmts, tail: Some(tail) }
			if matches!(stmts.as_slice(), [HirStmt::Let { value: HirExpr::Num(10.0, NumKind::Int), .. }])
				&& matches!(tail.as_ref(), HirExpr::Call { callee, args }
					if matches!(callee.as_ref(), HirExpr::Closure { params, .. }
						if matches!(params.as_slice(), [name] if name == "x"))
						&& matches!(args.as_slice(), [HirExpr::Local(name)] if name == "$pipe"))
	));
}

#[test]
fn lowers_a_multi_param_closure_with_a_block_body_sharing_one_scope() {
	// Mirrors `lower_func`/`lower_func_body`: the params and the block body's
	// own `let`s share ONE JS scope (no separate nested scope for the body),
	// exactly like a function's own body.
	let hir = lower(
		r#"
		func f(): int = {
			let g = (a: int, b: int) -> { let s = a + b  s * 2 }
			g(2, 3)
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Let { value, .. } = &stmts[0] else {
		panic!("expected a Let statement, got {:?}", stmts[0]);
	};
	let HirExpr::Closure { params, body } = value else {
		panic!("expected a Closure, got {value:?}");
	};
	assert_eq!(params, &vec!["a".to_string(), "b".to_string()]);
	let HirExpr::Block {
		stmts: body_stmts,
		tail: body_tail,
	} = body.as_ref()
	else {
		panic!("expected a Block closure body, got {body:?}");
	};
	let HirStmt::Let { name, value, .. } = &body_stmts[0] else {
		panic!("expected a Let statement, got {:?}", body_stmts[0]);
	};
	assert_eq!(name, "s");
	assert_eq!(
		value,
		&HirExpr::Binary {
			op: BinOp::Add,
			result: BuiltinResult::Int,
			lhs: Box::new(HirExpr::Local("a".into())),
			rhs: Box::new(HirExpr::Local("b".into())),
		}
	);
	assert_eq!(
		body_tail.as_deref(),
		Some(&HirExpr::Binary {
			op: BinOp::Mul,
			result: BuiltinResult::Int,
			lhs: Box::new(HirExpr::Local("s".into())),
			rhs: Box::new(HirExpr::Num(2.0, NumKind::Int)),
		})
	);
}

#[test]
fn closure_captures_a_shadow_renamed_outer_binding() {
	// JJ3: `let x = 1; let x = x + 1` renames the second binding to `x$1` (Y2).
	// A closure defined AFTER that redeclaration, reading the free variable
	// `x`, must resolve through the scope stack to the renamed `x$1` — exactly
	// the name a captured mutation would need to target under Node.
	let hir = lower(
		r#"
		func f(): int = {
			let x = 1
			let x = x + 1
			let g = () -> x
			g()
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Let { name, value, .. } = &stmts[2] else {
		panic!("expected a Let statement, got {:?}", stmts[2]);
	};
	assert_eq!(name, "g");
	assert_eq!(
		value,
		&HirExpr::Closure {
			params: vec![],
			body: Box::new(HirExpr::Local("x$1".into())),
		},
		"the closure body must capture the RENAMED outer binding"
	);
}

#[test]
fn return_inside_closure_block_body_lowers_to_the_closure() {
	let hir = lower(
		r#"
		func f(): int = {
			let g: (boolean) -> int = (b: boolean) -> { if (b) { return 1 }  2 }
			g(true)
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected function block");
	};
	let HirStmt::Let {
		value: HirExpr::Closure { body, .. },
		..
	} = &stmts[0]
	else {
		panic!("expected closure binding");
	};
	let HirExpr::Block { stmts, .. } = body.as_ref() else {
		panic!("expected closure block");
	};
	assert!(matches!(
		stmts.as_slice(),
		[HirStmt::Expr(HirExpr::If { .. })]
	));
}

#[test]
fn return_inside_anonymous_closure_body_lowers_to_that_callable() {
	let hir = lower(
		r#"
		func f(): int = {
			let g: (int) -> int = {
				if ($0 > 0) { return 1 }
				0
			}
			g(1)
		}
		"#,
	);
	assert!(matches!(hir.funcs[0].body, HirExpr::Block { .. }));
}

#[test]
fn return_inside_an_unbraced_closure_body_lowers() {
	let hir = lower(
		r#"
		func f(): int = {
			let g = (x: int) -> return x
			g(5)
		}
		"#,
	);
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected function block");
	};
	assert!(matches!(
		stmts[0],
		HirStmt::Let {
			value: HirExpr::Closure { .. },
			..
		}
	));
}

#[test]
fn legal_return_in_a_statement_position_match_is_unaffected_by_a_sibling_closure() {
	// Regression guard: an unrelated closure elsewhere in the same function
	// body must not affect a return inside a later statement-position match.
	let hir = lower(
		r#"
		func f(n: int): int = {
			let g = (x: int) -> x + 1
			match (n) {
				0 -> { return g(1) },
				_ -> { },
			}
			n
		}
		"#,
	);
	let HirExpr::Block { stmts, tail } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Let { name, .. } = &stmts[0] else {
		panic!("expected a Let statement, got {:?}", stmts[0]);
	};
	assert_eq!(name, "g");
	assert!(
		matches!(&stmts[1], HirStmt::Expr(HirExpr::Match { .. })),
		"expected a statement-position Match, got {:?}",
		stmts[1]
	);
	assert_eq!(tail.as_deref(), Some(&HirExpr::Local("n".into())));
}

#[test]
fn for_loop_over_a_list_typed_spread_param_lowers_through_iterable() {
	let hir = lower(
		r#"
		func make<Item>(...from: #[Item]): int = {
			let mut total = 0
			for (item in from) {
				total = total + 1
			}
			total
		}
		"#,
	);
	assert_eq!(hir.funcs[0].name, "make");
	let HirExpr::Block { stmts, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let HirStmt::Expr(HirExpr::Block {
		stmts: loop_stmts, ..
	}) = &stmts[1]
	else {
		panic!("expected protocol loop block, got {:?}", stmts[1]);
	};
	assert!(matches!(
		&loop_stmts[0],
		HirStmt::Let {
			value: HirExpr::Call { .. },
			..
		}
	));
}

#[test]
#[should_panic(expected = "does not support a spread closure parameter")]
fn spread_closure_param_panics_in_lowering() {
	// The checker never reads `ClosureParam::spread` (silently ignores it) —
	// lowering panics loudly on a spread closure parameter rather than
	// silently dropping the flag and emitting a plain (non-variadic) param.
	lower(
		r#"
		func f(): int = {
			let g = (...xs: int) -> xs
			g(5)
		}
		"#,
	);
}

// ── SS1: smart literal spread lowering ──────────────────────────────────────

#[test]
fn lowers_a_list_spread_over_a_native_list_source_to_a_native_splice() {
	// A boxed `#[T]` list exposes its native payload directly to the spread; no
	// protocol drain is needed.
	let hir = lower(
		r#"
		func f(): #[int] = {
			let xs = #[1, 2, 3]
			#[...xs, 4]
		}
		"#,
	);
	let HirExpr::Block { tail, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	assert_eq!(
		value_with_runtime_prototype(
			tail.as_deref().expect("a tail expression"),
			"NList",
			&["NInt"]
		),
		&HirExpr::ArraySpread {
			kind: HirArrayKind::List,
			elems: vec![
				HirArrayElem::Spread(HirExpr::Field {
					recv: Box::new(HirExpr::Local("xs".into())),
					name: "v".into(),
				}),
				HirArrayElem::Item(HirExpr::Num(4.0, NumKind::Int)),
			],
		}
	);
}

#[test]
fn lowers_a_list_spread_over_a_user_iterator_source_to_a_drain() {
	// A non-array `Iterator` source drains through the shared `$acc`/`$it`/`$go`
	// protocol machinery (Track A's own drain, extracted and reused) rather
	// than a native splice.
	let hir = lower(
		r#"
		enum Option<T> { Some(value: T), None }
		interface Iterator<Item> { mut func next(): Option<Item> }
		struct Counter(n: int, max: int)
		impl Iterator<int> for Counter {
			mut func next(): Option<int> = if (this.n > this.max) {
				None
			} else {
				let v = this.n
				this.n = this.n + 1
				Some(value = v)
			}
		}
		func f(): #[int] = {
			let mut c = Counter(n = 1, max = 3)
			#[...c, 99]
		}
		"#,
	);
	let f = hir
		.funcs
		.iter()
		.find(|f| f.name == "f")
		.expect("f in module");
	let HirExpr::Block { tail, .. } = &f.body else {
		panic!("expected Block, got {:?}", f.body);
	};
	let tail = tail.as_deref().expect("a tail expression");
	let tail = value_with_runtime_prototype(tail, "NList", &["NInt"]);
	let HirExpr::ArraySpread { elems, .. } = tail else {
		panic!("expected ArraySpread, got {tail:?}");
	};
	assert_eq!(elems.len(), 2);
	assert_eq!(
		elems[1],
		HirArrayElem::Item(HirExpr::Num(99.0, NumKind::Int))
	);
	let HirArrayElem::Spread(HirExpr::Block {
		stmts,
		tail: acc_tail,
	}) = &elems[0]
	else {
		panic!(
			"expected a drain Block for the non-native source, got {:?}",
			elems[0]
		);
	};
	// let $acc = []; let $it = ...; let mut $go = true; while (...) { .. }
	assert_eq!(stmts.len(), 4);
	assert!(matches!(
		&stmts[0],
		HirStmt::Let {
			name,
			value: HirExpr::Array { kind: HirArrayKind::Raw, items },
			..
		} if name == "$acc" && items.is_empty()
	));
	assert!(matches!(&stmts[1], HirStmt::Let { name, .. } if name == "$it"));
	assert!(matches!(&stmts[2], HirStmt::Let { name, mutable: true, .. } if name == "$go"));
	assert!(matches!(&stmts[3], HirStmt::Expr(HirExpr::While { .. })));
	assert_eq!(acc_tail.as_deref(), Some(&HirExpr::Local("$acc".into())));
}

#[test]
#[should_panic(expected = "no `IterMode` recorded for a non-list spread source")]
fn list_spread_over_a_range_source_panics_loudly_in_lowering() {
	// The checker types a `Range` spread source via its own short-circuit
	// (`infer_iterable_element`'s direct `ExprKind::Range` match), which never
	// consults `Iterator`/`Iterable` and so records no `IterMode` — an
	// out-of-scope edge that must never silently miscompile.
	lower(
		r#"
		func f(): #[int] = #[...0..5]
		"#,
	);
}

#[test]
fn lowers_a_map_spread_over_a_native_map_source_to_a_native_merge() {
	let hir = lower(
		r#"
		func f(): #{int: string} = {
			let m = #{1: "a"}
			#{...m, 2: "b"}
		}
		"#,
	);
	let HirExpr::Block { tail, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	assert_eq!(
		value_with_runtime_prototype(
			tail.as_deref().expect("a tail expression"),
			"NMap",
			&["NInt", "NString"]
		),
		&HirExpr::MapSpread(vec![
			HirMapElem::Spread(HirExpr::Local("m".into())),
			HirMapElem::Entry(HirExpr::Num(2.0, NumKind::Int), HirExpr::Str("b".into())),
		])
	);
}

#[test]
fn lowers_a_map_spread_over_a_native_list_of_pairs_source_directly() {
	// A boxed `#[#(K, V)]` list exposes its native payload without a drain,
	// splicing the list's tuple values into the map entries.
	let hir = lower(
		r#"
		func f(): #{int: string} = {
			let pairs = #[#(1, "a"), #(2, "b")]
			#{...pairs, 9: "z"}
		}
		"#,
	);
	let HirExpr::Block { tail, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	assert_eq!(
		value_with_runtime_prototype(
			tail.as_deref().expect("a tail expression"),
			"NMap",
			&["NInt", "NString"]
		),
		&HirExpr::MapSpread(vec![
			HirMapElem::Spread(HirExpr::Field {
				recv: Box::new(HirExpr::Local("pairs".into())),
				name: "v".into(),
			}),
			HirMapElem::Entry(HirExpr::Num(9.0, NumKind::Int), HirExpr::Str("z".into())),
		])
	);
}

#[test]
fn lowers_a_map_spread_over_a_non_map_iterable_of_pairs_to_a_drain() {
	let hir = lower(
		r#"
		enum Option<T> { Some(value: T), None }
		interface Iterator<Item> { mut func next(): Option<Item> }
		struct Pairs(n: int, max: int)
		impl Iterator<#(int, string)> for Pairs {
			mut func next(): Option<#(int, string)> = if (this.n > this.max) {
				None
			} else {
				let v = this.n
				this.n = this.n + 1
				Some(value = #(v, "x"))
			}
		}
		func f(): #{int: string} = {
			let mut p = Pairs(n = 1, max = 3)
			#{...p, 9: "z"}
		}
		"#,
	);
	let f = hir
		.funcs
		.iter()
		.find(|f| f.name == "f")
		.expect("f in module");
	let HirExpr::Block { tail, .. } = &f.body else {
		panic!("expected Block, got {:?}", f.body);
	};
	let tail = tail.as_deref().expect("a tail expression");
	let tail = value_with_runtime_prototype(tail, "NMap", &["NInt", "NString"]);
	let HirExpr::MapSpread(elems) = tail else {
		panic!("expected MapSpread, got {tail:?}");
	};
	assert_eq!(elems.len(), 2);
	assert_eq!(
		elems[1],
		HirMapElem::Entry(HirExpr::Num(9.0, NumKind::Int), HirExpr::Str("z".into()))
	);
	assert!(
		matches!(&elems[0], HirMapElem::Spread(HirExpr::Block { .. })),
		"expected a drain Block for the non-map source, got {:?}",
		elems[0]
	);
}

#[test]
fn tuple_spread_lowers_with_kind_boundaries_and_source_order() {
	let hir = lower(
		r#"
		func f(): #(int, boolean, string, uint) = {
			let xs = #(true, "x")
			#(1, ...#(), ...xs, 2u)
		}
		"#,
	);
	let HirExpr::Block { tail, .. } = &hir.funcs[0].body else {
		panic!("expected Block, got {:?}", hir.funcs[0].body);
	};
	let spread = value_with_runtime_prototype(
		tail.as_deref().expect("a tail expression"),
		"NTuple",
		&["NInt", "NBool", "NString", "NUint"],
	);
	let HirExpr::ArraySpread { kind, elems } = spread else {
		panic!("expected ArraySpread, got {spread:?}");
	};
	assert_eq!(*kind, HirArrayKind::Tuple);
	assert_eq!(elems.len(), 4);
	assert_eq!(
		elems[0],
		HirArrayElem::Item(HirExpr::Num(1.0, NumKind::Int))
	);
	let HirArrayElem::Spread(HirExpr::Field { recv, name }) = &elems[1] else {
		panic!("expected empty tuple spread, got {:?}", elems[1]);
	};
	assert_eq!(name, "v");
	assert_eq!(
		value_with_runtime_prototype(recv, "NTuple", &[]),
		&HirExpr::Array {
			kind: HirArrayKind::Tuple,
			items: vec![],
		}
	);
	assert_eq!(
		&elems[2..],
		&[
			HirArrayElem::Spread(HirExpr::Field {
				recv: Box::new(HirExpr::Local("xs".into())),
				name: "v".into(),
			}),
			HirArrayElem::Item(HirExpr::Num(2.0, NumKind::UInt)),
		]
	);
}

#[test]
fn mut_func_in_a_top_level_impl_lowers_as_an_instance_method() {
	// A `mut func` in a top-level `impl Type { … }` block is an ordinary
	// instance method (mut carries no extra lowering, same as in a type body).
	let hir = lower(
		"struct Counter(n: int)
		 impl Counter {
		   mut func bump(): void = { this.n = this.n + 1 }
		 }",
	);
	let class = hir.classes.iter().find(|c| c.name == "Counter").unwrap();
	assert_eq!(class.methods.len(), 1);
	assert_eq!(class.methods[0].name, "bump");
	assert!(class.statics.is_empty());
}

#[test]
fn namespace_funcs_in_top_level_impls_attach_to_structs_and_enums() {
	let hir = lower(
		"impl Counter { namespace func zero(): Counter = Counter(n = 0) }
		 struct Counter(n: int)
		 enum Choice<T> { Some(value: T), None }
		 impl<T> Choice<T> { namespace func empty(): Choice<T> = Choice.None }",
	);
	let class = hir.classes.iter().find(|c| c.name == "Counter").unwrap();
	assert_eq!(
		class
			.statics
			.iter()
			.map(|m| m.name.as_str())
			.collect::<Vec<_>>(),
		["zero"]
	);
	assert!(class.methods.is_empty());
	let enum_ = hir.enums.iter().find(|e| e.name == "Choice").unwrap();
	assert_eq!(
		enum_
			.statics
			.iter()
			.map(|m| m.name.as_str())
			.collect::<Vec<_>>(),
		["empty"]
	);
	assert!(enum_.methods.is_empty());
}

#[test]
fn ambient_struct_static_attaches_across_prelude_sources() {
	let owner = parse_module("struct Factory(value: int)", "owner");
	let attachment = parse_module(
		"impl Factory { namespace func make(value: int): Factory = Factory(value = value) }",
		"attachment",
	);
	let user = parse_module("func make(): Factory = Factory.make(3)", "user");
	let preludes = [owner.tree, attachment.tree];
	let checked = check_module_with_prelude(&user.tree, &preludes);
	assert!(
		checked.diags.is_empty(),
		"check failed: {:?}",
		checked.diags
	);
	let canonical_owner = RuntimeOwner::Compiler("canonical".into());
	let lowered = lower_hir_with_prelude_runtime_and_deps_with_owners(
		&user.tree,
		&preludes,
		&[canonical_owner.clone(), canonical_owner],
		2,
		&checked,
	);
	let class = lowered
		.prelude_runtime
		.classes
		.iter()
		.find(|c| c.name == "Factory")
		.unwrap();
	assert_eq!(
		class
			.statics
			.iter()
			.map(|m| m.name.as_str())
			.collect::<Vec<_>>(),
		["make"]
	);
}

#[test]
fn ambient_static_from_a_different_owner_is_not_attached_by_bare_type_name() {
	let owner = parse_module("struct Factory(value: int)", "owner");
	let extension = parse_module(
		"impl Factory { namespace func make(value: int): Factory = Factory(value = value) }",
		"extension",
	);
	let user = parse_module("func make(): Factory = Factory.make(3)", "user");
	let preludes = [owner.tree, extension.tree];
	let checked = check_module_with_prelude(&user.tree, &preludes);
	assert!(
		checked.diags.is_empty(),
		"check failed: {:?}",
		checked.diags
	);
	let lowered = lower_hir_with_prelude_runtime_and_deps_with_owners(
		&user.tree,
		&preludes,
		&[
			RuntimeOwner::Compiler("owner".into()),
			RuntimeOwner::Compiler("extension".into()),
		],
		2,
		&checked,
	);
	let class = lowered
		.prelude_runtime
		.classes
		.iter()
		.find(|class| class.name == "Factory")
		.unwrap();
	assert!(class.statics.is_empty());
}

#[test]
fn external_let_lowers_once_and_references_share_its_binding() {
	let hir = lower("external(max_float) let limit: float\nfunc pair() = #(limit, limit)");
	assert_eq!(hir.lets.len(), 1);
	assert!(matches!(
		hir.lets[0].value,
		HirExpr::ExternValue {
			module: "std/math/intrinsics",
			symbol: "max_float",
			marshal: nymph_hir::hir::MarshalKind::Float,
		}
	));
	let HirExpr::Array { items, .. } =
		value_with_runtime_prototype(&hir.funcs[0].body, "NTuple", &["NFloat", "NFloat"])
	else {
		panic!("expected tuple");
	};
	assert_eq!(
		items,
		&[
			HirExpr::Local("limit".into()),
			HirExpr::Local("limit".into())
		]
	);
}

#[test]
fn duplicate_external_lets_lower_to_one_canonical_snapshot_and_a_local_alias() {
	let hir = lower("external(max_float) let first: float\nexternal(max_float) let second: float");
	assert_eq!(hir.lets.len(), 2);
	assert!(matches!(hir.lets[0].value, HirExpr::ExternValue { .. }));
	assert_eq!(hir.lets[1].name, "second");
	assert_eq!(hir.lets[1].value, HirExpr::Local("first".into()));
}

#[test]
fn external_let_marshal_uses_resolved_declaration_type() {
	for source in [
		"type Scalar = float\nexternal(max_float) let limit: Scalar",
		"external(max_float) let limit: (float)",
	] {
		let hir = lower(source);
		assert!(matches!(
			hir.lets[0].value,
			HirExpr::ExternValue {
				marshal: nymph_hir::hir::MarshalKind::Float,
				..
			}
		));
	}
}

#[test]
fn ambient_external_let_is_demand_collected_once() {
	let prelude = parse_module("external(max_float) let limit: float", "prelude");
	let user = parse_module("func pair() = #(limit, limit)", "test");
	let checked = check_module_with_prelude(&user.tree, std::slice::from_ref(&prelude.tree));
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let lowered = lower_hir_with_prelude_runtime_and_deps(
		&user.tree,
		std::slice::from_ref(&prelude.tree),
		1,
		&checked,
	);
	assert!(lowered.module.lets.is_empty());
	assert_eq!(lowered.prelude_runtime.lets.len(), 1);
	assert!(matches!(
		lowered.prelude_runtime.lets[0].value,
		HirExpr::ExternValue { .. }
	));
}
