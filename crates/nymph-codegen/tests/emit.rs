use nymph_codegen::emit;
use nymph_hir::hir::{BinOp, HirEnum, HirExpr, HirFunc, HirMethod, HirModule, HirVariant};

#[test]
fn emits_a_function_returning_a_number() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "answer".into(),
			params: vec![],
			body: HirExpr::Num(42.0),
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	// A single-expression body becomes an arrow-style function returning the value.
	assert!(js.contains("answer"), "function name present: {js}");
	assert!(js.contains("42"), "literal present: {js}");
}

#[test]
fn emits_arithmetic_and_params() {
	// function add(a, b) { return a + b * 2; }
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "add".into(),
			params: vec!["a".into(), "b".into()],
			body: HirExpr::Binary {
				op: BinOp::Add,
				lhs: Box::new(HirExpr::Local("a".into())),
				rhs: Box::new(HirExpr::Binary {
					op: BinOp::Mul,
					lhs: Box::new(HirExpr::Local("b".into())),
					rhs: Box::new(HirExpr::Num(2.0)),
				}),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(js.contains("function add(a, b)"), "{js}");
	assert!(js.contains("a + b * 2"), "{js}");
}

#[test]
fn emits_call_and_string() {
	// function greet() { return log("hi"); }
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "greet".into(),
			params: vec![],
			body: HirExpr::Call {
				callee: Box::new(HirExpr::Local("log".into())),
				args: vec![HirExpr::Str("hi".into())],
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(
		js.contains("log('hi')") || js.contains("log(\"hi\")"),
		"{js}"
	);
}

#[test]
fn emits_method_less_enum_without_a_prototype() {
	// X1: a method-less enum must keep today's exact shape — no `proto`, no
	// `Object.create` — regardless of the new methodful codepath existing.
	let module = HirModule {
		lets: vec![],
		funcs: vec![],
		classes: vec![],
		enums: vec![HirEnum {
			name: "Opt".into(),
			variants: vec![
				HirVariant {
					name: "Some".into(),
					fields: vec!["value".into()],
				},
				HirVariant {
					name: "None".into(),
					fields: vec![],
				},
			],
			methods: vec![],
		}],
	};
	let js = emit(&module);
	assert!(!js.contains("proto"), "no prototype object: {js}");
	assert!(!js.contains("Object.create"), "no Object.create: {js}");
	assert!(js.contains("Object.freeze"), "nullary stays frozen: {js}");
	assert!(
		js.contains("Object.assign"),
		"field variant tags factory: {js}"
	);
	assert!(
		js.contains("...fields"),
		"field variant spreads fields: {js}"
	);
}

#[test]
fn emits_enum_with_methods_prototype_shape() {
	// X1: an enum WITH methods gets a shared `proto` object and every variant
	// is created via `Object.create(proto)`, while the tag ABI (Object.freeze /
	// factory-tagging Object.assign) stays intact.
	let module = HirModule {
		lets: vec![],
		funcs: vec![],
		classes: vec![],
		enums: vec![HirEnum {
			name: "Color".into(),
			variants: vec![
				HirVariant {
					name: "Red".into(),
					fields: vec![],
				},
				HirVariant {
					name: "Custom".into(),
					fields: vec!["n".into()],
				},
			],
			methods: vec![HirMethod {
				name: "idx".into(),
				params: vec![],
				body: HirExpr::Num(0.0),
			}],
		}],
	};
	let js = emit(&module);
	assert!(js.contains("proto"), "prototype object present: {js}");
	assert!(
		js.contains("Object.create(proto)"),
		"variants built via Object.create(proto): {js}"
	);
	assert!(js.contains("idx"), "method name present: {js}");
	assert!(js.contains("Object.freeze"), "nullary stays frozen: {js}");
	assert!(
		js.contains("...fields"),
		"field variant spreads fields: {js}"
	);
}
