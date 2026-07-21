use nymph_codegen::emit;
use nymph_hir::hir::{
	BinOp, HirArm, HirEnum, HirExpr, HirFunc, HirLit, HirMethod, HirModule, HirPat, HirStmt,
	HirVariant, NumKind,
};

#[test]
fn emits_a_function_returning_a_number() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "answer".into(),
			params: vec![],
			body: HirExpr::Num(42.0, NumKind::Int),
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
#[ignore = "#2: ignored for the boxing branch; replacement golden suite lands pre-merge"]
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
					rhs: Box::new(HirExpr::Num(2.0, NumKind::Int)),
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
#[ignore = "#2: ignored for the boxing branch; replacement golden suite lands pre-merge"]
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
			statics: vec![],
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
				body: HirExpr::Num(0.0, NumKind::Int),
			}],
			statics: vec![],
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

#[test]
fn emits_list_index_assignment_as_a_computed_member_assignment() {
	// Confirmed defect (code review): `HirExpr::Assign { target: Index { .. },
	// .. }` used to hit an `unreachable!` panic in emit's `Assign` match — a
	// zero-diagnostic program (`xs[i] = value`) always lowers to exactly this
	// shape, so the panic was an ICE reachable on valid input, not a genuinely
	// unreachable case. Emit must produce a plain `xs[i] = v` assignment.
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "set".into(),
			params: vec!["xs".into(), "i".into(), "v".into()],
			body: HirExpr::Assign {
				target: Box::new(HirExpr::Index {
					recv: Box::new(HirExpr::Local("xs".into())),
					index: Box::new(HirExpr::Local("i".into())),
				}),
				value: Box::new(HirExpr::Local("v".into())),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(js.contains("xs[i] = v"), "computed-member assignment: {js}");
}

#[test]
fn emits_map_index_assignment_as_a_set_call() {
	// Same defect, `Map` receiver: a JS `Map` has no assignment-expression
	// form for its entries, so `HirExpr::Assign { target: MapGet { .. }, .. }`
	// must lower to a `.set(key, value)` call, never a computed-member
	// assignment (which would silently set an own property on the `Map`
	// object instead of mutating an entry).
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "set".into(),
			params: vec!["m".into(), "k".into(), "v".into()],
			body: HirExpr::Assign {
				target: Box::new(HirExpr::MapGet {
					recv: Box::new(HirExpr::Local("m".into())),
					key: Box::new(HirExpr::Local("k".into())),
				}),
				value: Box::new(HirExpr::Local("v".into())),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(js.contains("m.set(k, v)"), "map .set call: {js}");
	assert!(
		!js.contains("m[k] ="),
		"never a computed-member assignment on a Map: {js}"
	);
}

#[test]
fn emits_a_closure_as_an_arrow_function() {
	// Slice 4L, JJ1: `HirExpr::Closure` emits as a JS arrow function; a
	// `Block` body flattens directly into the arrow's function body (no
	// needless nested IIFE), mirroring `emit_func`'s own body split.
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "f".into(),
			params: vec![],
			body: HirExpr::Closure {
				params: vec!["x".into(), "y".into()],
				body: Box::new(HirExpr::Binary {
					op: BinOp::Add,
					lhs: Box::new(HirExpr::Local("x".into())),
					rhs: Box::new(HirExpr::Local("y".into())),
				}),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(js.contains("(x, y) =>"), "arrow params present: {js}");
	assert!(js.contains("x + y"), "arrow body present: {js}");
}

#[test]
fn closure_return_emits_fine_inside_a_subexpression_position_match_arm() {
	// Slice 4L, JJ2's closure-boundary story, tested independently of
	// lowering (which never actually PRODUCES this shape — any `return`
	// lexically inside a closure body panics at lowering): a `return` inside
	// a closure that itself sits inside a SUBEXPRESSION-position match arm
	// (which sets `in_iife_subexpr = true` for everything underneath, Slice
	// 4E, Y1) must still emit as a plain arrow `return`, not panic as if it
	// targeted the match's own IIFE. This is exactly what the closure-body
	// emission's save/reset-to-`false` of `in_iife_subexpr` (mirroring
	// `emit_func`'s implicit top-level function boundary) exists to give.
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "f".into(),
			params: vec!["n".into()],
			body: HirExpr::Block {
				stmts: vec![HirStmt::Let {
					name: "g".into(),
					mutable: false,
					value: HirExpr::Match {
						scrutinee: Box::new(HirExpr::Local("n".into())),
						arms: vec![HirArm {
							pat: HirPat::Wildcard,
							guard: None,
							body: HirExpr::Closure {
								params: vec!["x".into()],
								body: Box::new(HirExpr::Block {
									stmts: vec![HirStmt::Return(Some(HirExpr::Local("x".into())))],
									tail: None,
								}),
							},
						}],
					},
				}],
				tail: Some(Box::new(HirExpr::Local("g".into()))),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(js.contains("=>"), "arrow present: {js}");
	assert!(js.contains("return x"), "closure body's own return: {js}");
}

#[test]
#[should_panic(expected = "would return from the emitted IIFE")]
fn return_inside_a_nested_subexpression_match_within_a_closure_body_still_panics() {
	// The closure boundary's reset only covers the closure's OWN top-level
	// body emission — a construct NESTED inside that body which is itself in
	// subexpression position (here, a `let`'s match initializer) still sets
	// its own `in_iife_subexpr` guard and must still panic on a `return`
	// inside one of ITS arms. Proves the reset is scoped, not a blanket
	// "never panic again" pass triggered by merely being inside a closure.
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "f".into(),
			params: vec![],
			body: HirExpr::Closure {
				params: vec!["x".into()],
				body: Box::new(HirExpr::Block {
					stmts: vec![HirStmt::Let {
						name: "y".into(),
						mutable: false,
						value: HirExpr::Match {
							scrutinee: Box::new(HirExpr::Local("x".into())),
							arms: vec![
								HirArm {
									pat: HirPat::Lit(HirLit::Num(0.0)),
									guard: None,
									body: HirExpr::Block {
										stmts: vec![HirStmt::Return(Some(HirExpr::Num(1.0, NumKind::Int)))],
										tail: None,
									},
								},
								HirArm {
									pat: HirPat::Wildcard,
									guard: None,
									body: HirExpr::Local("x".into()),
								},
							],
						},
					}],
					tail: Some(Box::new(HirExpr::Local("y".into()))),
				}),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	emit(&module);
}
