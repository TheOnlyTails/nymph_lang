use nymph_hir::hir::{BinOp, HirExpr, HirModule};
use nymph_sema::lower_hir;
use nymph_syntax::parse_module;

fn lower(src: &str) -> HirModule {
	let parsed = parse_module(src, "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse failed: {src}"
	);
	lower_hir(&parsed.tree)
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
	let hir = lower("func g(): int = h(1)");
	assert_eq!(
		hir.funcs[0].body,
		HirExpr::Call {
			callee: Box::new(HirExpr::Local("h".into())),
			args: vec![HirExpr::Num(1.0)],
		}
	);
}
