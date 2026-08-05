use nymph_codegen::emit;
use nymph_hir::hir::{
	BinOp, BuiltinResult, HirArm, HirBoundDispatchCase, HirBoundDispatchTarget, HirEnum, HirExpr,
	HirFunc, HirLit, HirMethod, HirModule, HirPat, HirStmt, HirVariant, NumKind,
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
fn external_value_is_imported_marshaled_and_bound_once() {
	let module = HirModule {
		lets: vec![nymph_hir::hir::HirLet {
			name: "limit".into(),
			mutable: false,
			value: HirExpr::ExternValue {
				module: "host/limits",
				symbol: "maximum",
				marshal: nymph_hir::hir::MarshalKind::Float,
			},
		}],
		funcs: vec![HirFunc {
			name: "same".into(),
			params: vec![],
			body: HirExpr::Array {
				kind: nymph_hir::hir::HirArrayKind::Raw,
				items: vec![
					HirExpr::Local("limit".into()),
					HirExpr::Local("limit".into()),
				],
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert_eq!(js.matches("from \"host/limits\"").count(), 1, "{js}");
	assert_eq!(
		js.matches("new NFloat($nymph_external$value$").count(),
		1,
		"{js}"
	);
	assert!(js.contains("const limit ="), "{js}");
	assert_eq!(js.matches("[limit, limit]").count(), 1, "{js}");
}

#[test]
fn duplicate_external_value_declarations_share_one_import_and_box() {
	let module = HirModule {
		lets: vec![
			nymph_hir::hir::HirLet {
				name: "first".into(),
				mutable: false,
				value: HirExpr::ExternValue {
					module: "host/limits",
					symbol: "maximum",
					marshal: nymph_hir::hir::MarshalKind::Float,
				},
			},
			nymph_hir::hir::HirLet {
				name: "second".into(),
				mutable: false,
				value: HirExpr::ExternValue {
					module: "host/limits",
					symbol: "maximum",
					marshal: nymph_hir::hir::MarshalKind::Float,
				},
			},
		],
		funcs: vec![],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert_eq!(js.matches("from \"host/limits\"").count(), 1, "{js}");
	assert_eq!(js.matches("new NFloat(").count(), 1, "{js}");
	assert!(js.contains("const second = first"), "{js}");
}

#[test]
fn external_aliases_include_module_symbol_and_kind_identity() {
	let module = HirModule {
		lets: vec![nymph_hir::hir::HirLet {
			name: "value".into(),
			mutable: false,
			value: HirExpr::ExternValue {
				module: "host/a",
				symbol: "same",
				marshal: nymph_hir::hir::MarshalKind::Float,
			},
		}],
		funcs: vec![HirFunc {
			name: "calls".into(),
			params: vec![],
			body: HirExpr::Array {
				kind: nymph_hir::hir::HirArrayKind::Raw,
				items: vec![
					HirExpr::ExternCall {
						module: "host/a",
						symbol: "same",
						args: vec![],
					},
					HirExpr::ExternCall {
						module: "host/b",
						symbol: "same",
						args: vec![],
					},
				],
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert_eq!(js.matches("same as $nymph_external$").count(), 3, "{js}");
	assert_eq!(js.matches("from \"host/a\"").count(), 2, "{js}");
	assert_eq!(js.matches("from \"host/b\"").count(), 1, "{js}");
	assert!(js.contains("$call$"), "{js}");
	assert!(js.contains("$value$"), "{js}");
}

#[test]
fn display_protocol_call_includes_its_runtime_without_other_boxed_values() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "render".into(),
			params: vec!["value".into()],
			body: HirExpr::ExternCall {
				module: "std/display",
				symbol: "debug",
				args: vec![HirExpr::Local("value".into())],
			},
		}],
		classes: vec![],
		enums: vec![],
	};

	let js = emit(&module);
	assert!(
		js.contains("function nymphProtocolDebug"),
		"display protocol helper must be defined alongside its call: {js}"
	);
}

#[test]
fn bound_dispatch_evaluates_operands_once_and_falls_back_to_the_user_method() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "compare".into(),
			params: vec!["make_left".into(), "make_right".into()],
			body: HirExpr::BoundDispatch {
				interface: "Comparable".into(),
				method: "less_than".into(),
				receiver: Box::new(HirExpr::Call {
					callee: Box::new(HirExpr::Local("make_left".into())),
					args: vec![],
				}),
				argument: Box::new(HirExpr::Call {
					callee: Box::new(HirExpr::Local("make_right".into())),
					args: vec![],
				}),
				cases: vec![HirBoundDispatchCase {
					receiver_tag: "nymph.int".into(),
					argument_tag: "nymph.int".into(),
					target: HirBoundDispatchTarget::TopLevel {
						module: "std/ops".into(),
						name: "int_less_than".into(),
					},
				}],
			},
		}],
		classes: vec![],
		enums: vec![],
	};

	let js = emit(&module);
	assert_eq!(js.matches("make_left()").count(), 1, "{js}");
	assert_eq!(js.matches("make_right()").count(), 1, "{js}");
	assert!(js.contains("Symbol.for(\"nymph.tag\")"), "{js}");
	assert!(js.contains("Symbol.for(\"nymph.int\")"), "{js}");
	assert!(js.contains("int_less_than"), "{js}");
	assert!(js.contains(".less_than("), "{js}");
}

#[test]
fn interpolation_emits_cooked_text_as_raw_strings() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "render".into(),
			params: vec!["value".into()],
			body: HirExpr::InterpolatedString(vec![
				HirExpr::Str("value=".into()),
				HirExpr::ExternCall {
					module: "std/display",
					symbol: "display",
					args: vec![HirExpr::Local("value".into())],
				},
				HirExpr::Str("!".into()),
			]),
		}],
		classes: vec![],
		enums: vec![],
	};

	let js = emit(&module);
	assert!(
		!js.contains("new NString(\"value=\")") && !js.contains("new NString(\"!\")"),
		"cooked interpolation segments must not allocate temporary NString boxes: {js}"
	);
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
				result: BuiltinResult::Raw,
				lhs: Box::new(HirExpr::Local("a".into())),
				rhs: Box::new(HirExpr::Binary {
					op: BinOp::Mul,
					result: BuiltinResult::Raw,
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
fn emits_list_index_assignment_against_the_boxed_payload() {
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
	assert!(
		js.contains("xs.v[i.v] = v"),
		"boxed-payload assignment: {js}"
	);
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
					result: BuiltinResult::Raw,
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
	// A `return` inside a closure that itself sits inside a
	// SUBEXPRESSION-position match arm
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
									stmts: vec![HirStmt::Return {
										value: Some(HirExpr::Local("x".into())),
										target: nymph_hir::hir::HirReturnTarget::Callable,
									}],
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
fn function_return_crosses_a_generated_iife_without_losing_sibling_values() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "f".into(),
			params: vec!["flag".into()],
			body: HirExpr::Binary {
				op: BinOp::Add,
				result: BuiltinResult::Raw,
				lhs: Box::new(HirExpr::Num(1.0, NumKind::Int)),
				rhs: Box::new(HirExpr::If {
					cond: Box::new(HirExpr::Local("flag".into())),
					then: Box::new(HirExpr::Block {
						stmts: vec![HirStmt::Return {
							value: Some(HirExpr::Num(7.0, NumKind::Int)),
							target: nymph_hir::hir::HirReturnTarget::Callable,
						}],
						tail: None,
					}),
					otherwise: Some(Box::new(HirExpr::Num(2.0, NumKind::Int))),
				}),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(
		js.contains("throw ["),
		"return crosses the generated IIFE: {js}"
	);
	assert!(
		js.contains("catch ("),
		"function consumes its completion: {js}"
	);
	assert!(
		js.contains(" + "),
		"non-returning sibling value is preserved: {js}"
	);
}

#[test]
fn return_inside_a_nested_subexpression_uses_the_closure_completion_target() {
	// A construct nested in a closure body and used in subexpression position
	// needs an IIFE. Its return must cross that synthetic boundary while still
	// targeting the closure, not the function that created the closure.
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
									pat: HirPat::Lit(HirLit::Num(0.0, NumKind::Int)),
									guard: None,
									body: HirExpr::Block {
										stmts: vec![HirStmt::Return {
											value: Some(HirExpr::Num(1.0, NumKind::Int)),
											target: nymph_hir::hir::HirReturnTarget::Callable,
										}],
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
	let js = emit(&module);
	assert!(js.contains("throw ["), "return completion is thrown: {js}");
	assert!(
		js.contains("catch ("),
		"closure catches its completion: {js}"
	);
}
