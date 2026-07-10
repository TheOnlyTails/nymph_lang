use nymph_codegen::emit;
use nymph_hir::hir::{HirExpr, HirFunc, HirModule};

#[test]
fn emits_a_function_returning_a_number() {
	let module = HirModule {
		funcs: vec![HirFunc {
			name: "answer".into(),
			params: vec![],
			body: HirExpr::Num(42.0),
		}],
	};
	let js = emit(&module);
	// A single-expression body becomes an arrow-style function returning the value.
	assert!(js.contains("answer"), "function name present: {js}");
	assert!(js.contains("42"), "literal present: {js}");
}
