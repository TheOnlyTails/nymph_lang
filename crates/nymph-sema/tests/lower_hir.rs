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
