use nymph_codegen::{
	EchoEmission, emit, emit_for_project_module_with_imports_and_echo,
	emit_for_transactional_project_module_checked,
};
use nymph_hir::hir::{
	BinOp, BuiltinResult, HirArm, HirBoundDispatchCase, HirBoundDispatchTarget, HirEnum, HirExpr,
	HirFunc, HirLit, HirMethod, HirModule, HirPat, HirStmt, HirTaskContext, HirTaskOperation,
	HirVariant, NumKind, OperationMode,
};

#[test]
fn echo_development_emits_a_site_and_release_erases_every_observer_byte() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "observe".into(),
			params: vec!["value".into()],
			body: HirExpr::Echo {
				operand: Box::new(HirExpr::Local("value".into())),
				site: nymph_hir::hir::EchoSite {
					module: "main".into(),
					start: 14,
					end: 18,
				},
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let development = emit_for_project_module_with_imports_and_echo(
		&module,
		"main",
		&[],
		EchoEmission::Development {
			source_name: "/workspace/src/main.nym".into(),
			source_uri: Some("file:///workspace/src/main.nym".into()),
			source: "func f() = {\n\techo 1\n}".into(),
		},
	);
	assert!(development.contains("nymphEcho"), "{development}");
	assert!(development.contains("main.nym"), "{development}");
	assert!(
		development.contains("file:///workspace/src/main.nym"),
		"{development}"
	);

	let release =
		emit_for_project_module_with_imports_and_echo(&module, "main", &[], EchoEmission::Release);
	assert!(!release.contains("nymphEcho"), "{release}");
	assert!(!release.contains("nymphEchoBoxes"), "{release}");
	assert!(!release.contains("nymphEchoStructuralShapes"), "{release}");
	assert!(!release.contains("main.nym"), "{release}");
	assert!(!release.contains("file:///"), "{release}");
	let operand_only = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "observe".into(),
			params: vec!["value".into()],
			body: HirExpr::Local("value".into()),
		}],
		classes: vec![],
		enums: vec![],
	};
	assert_eq!(
		release,
		emit_for_project_module_with_imports_and_echo(
			&operand_only,
			"main",
			&[],
			EchoEmission::Release,
		)
	);
}

#[test]
fn emits_a_function_returning_an_exact_integer() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "answer".into(),
			params: vec![],
			body: HirExpr::Int(42),
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	// A single-expression body becomes an arrow-style function returning the value.
	assert!(js.contains("answer"), "function name present: {js}");
	assert!(js.contains("new NInt(42n)"), "exact literal present: {js}");
}

#[test]
fn emits_exact_integer_boundaries_and_checked_power() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![
			HirFunc {
				name: "signed_min".into(),
				params: vec![],
				body: HirExpr::Int(i64::MIN),
			},
			HirFunc {
				name: "unsigned_max".into(),
				params: vec![],
				body: HirExpr::UInt(u64::MAX),
			},
			HirFunc {
				name: "power".into(),
				params: vec!["base".into(), "exponent".into()],
				body: HirExpr::Binary {
					op: BinOp::Pow,
					result: BuiltinResult::Int,
					mode: OperationMode::Checked,
					lhs: Box::new(HirExpr::Local("base".into())),
					rhs: Box::new(HirExpr::Local("exponent".into())),
				},
			},
		],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(js.contains("new NInt(-9223372036854775808n)"), "{js}");
	assert!(js.contains("new NUint(18446744073709551615n)"), "{js}");
	assert!(js.contains("new NInt(nymphCheckedPower("), "{js}");
	assert!(js.contains(".liveLocals[0].v"), "{js}");
	assert!(js.contains(".liveLocals[1].v"), "{js}");
}

#[test]
fn strict_transactional_emission_rejects_the_exact_unaudited_external_inventory() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "call_host".into(),
			params: vec![],
			body: HirExpr::ExternCall {
				module: "host/state",
				symbol: "mutate",
				args: vec![],
				call_mode: nymph_hir::hir::ExternalCallMode::Ordinary,
				argument_marshals: vec![],
				return_marshal: None,
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	assert_eq!(
		emit_for_transactional_project_module_checked(&module, "test", &[], &[]),
		Err(("host/state".to_string(), "mutate".to_string()))
	);
}

#[test]
fn external_value_is_imported_marshaled_and_bound_once() {
	let module = HirModule {
		lets: vec![nymph_hir::hir::HirLet {
			name: "limit".into(),
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
				value: HirExpr::ExternValue {
					module: "host/limits",
					symbol: "maximum",
					marshal: nymph_hir::hir::MarshalKind::Float,
				},
			},
			nymph_hir::hir::HirLet {
				name: "second".into(),
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
						call_mode: nymph_hir::hir::ExternalCallMode::Ordinary,
						argument_marshals: vec![],
						return_marshal: None,
					},
					HirExpr::ExternCall {
						module: "host/b",
						symbol: "same",
						args: vec![],
						call_mode: nymph_hir::hir::ExternalCallMode::Ordinary,
						argument_marshals: vec![],
						return_marshal: None,
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
fn external_calls_emit_exact_mode_and_opaque_marshalling_abis() {
	let external = |name: &str, call_mode, identity| HirFunc {
		name: name.into(),
		params: vec!["value".into()],
		body: HirExpr::ExternCall {
			module: "host/resources",
			symbol: Box::leak(name.to_string().into_boxed_str()),
			args: vec![HirExpr::Local("value".into())],
			call_mode,
			argument_marshals: vec![Some(nymph_hir::hir::MarshalKind::Opaque(identity))],
			return_marshal: Some(nymph_hir::hir::MarshalKind::Opaque(identity)),
		},
	};
	let module = HirModule {
		lets: vec![],
		funcs: vec![
			external("ordinary", nymph_hir::hir::ExternalCallMode::Ordinary, 117),
			external(
				"cancellable",
				nymph_hir::hir::ExternalCallMode::Cancellable,
				117,
			),
		],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(js.matches("nymphUnboxOpaque(117n").count() == 2, "{js}");
	assert_eq!(js.matches("nymphBoxOpaque(117n").count(), 2, "{js}");
	assert_eq!(
		js.matches("nymphCurrentExecutionSignal()").count(),
		2,
		"the runtime definition plus exactly one cancellable call must be present: {js}"
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
				hidden_arguments: vec![HirExpr::Call {
					callee: Box::new(HirExpr::Local("make_hidden".into())),
					args: vec![],
				}],
				cases: vec![HirBoundDispatchCase {
					receiver_tag: "nymph.int".into(),
					argument_tag: "nymph.int".into(),
					target: HirBoundDispatchTarget::TopLevel {
						module: "std/ops".into(),
						name: "int_less_than".into(),
					},
				}],
				mode: nymph_hir::hir::HirCallMode::Push,
				source: 7,
			},
		}],
		classes: vec![],
		enums: vec![],
	};

	let js = emit(&module);
	assert_eq!(js.matches("undefined, [], 0,").count(), 3, "{js}");
	assert_eq!(js.matches("= make_hidden;").count(), 1, "{js}");
	assert!(js.contains("Symbol.for(\"nymph.tag\")"), "{js}");
	assert!(js.contains("Symbol.for(\"nymph.int\")"), "{js}");
	assert!(js.contains("nymphPush(int_less_than, undefined,"), "{js}");
	assert!(
		js.contains("nymphPush(") && js.contains(".less_than,"),
		"{js}"
	);
	assert!(js.contains("], 7, 1,"), "{js}");
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
				HirExpr::ProtocolDisplay(Box::new(HirExpr::Local("value".into()))),
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
				mode: OperationMode::Direct,
				lhs: Box::new(HirExpr::Local("a".into())),
				rhs: Box::new(HirExpr::Binary {
					op: BinOp::Mul,
					result: BuiltinResult::Raw,
					mode: OperationMode::Direct,
					lhs: Box::new(HirExpr::Local("b".into())),
					rhs: Box::new(HirExpr::Num(2.0, NumKind::Raw)),
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
fn emits_method_less_enum_with_a_canonical_prototype() {
	// The canonical runtime type object exists for every enum, and variants use
	// it even when the source enum declares no instance methods.
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
	assert!(
		js.contains("const proto = {}"),
		"canonical prototype object: {js}"
	);
	assert!(
		js.contains("Object.create(proto)"),
		"variants use canonical prototype: {js}"
	);
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
				body: HirExpr::Int(0),
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
fn emits_a_closure_as_an_activation_state() {
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
					mode: OperationMode::Direct,
					lhs: Box::new(HirExpr::Local("x".into())),
					rhs: Box::new(HirExpr::Local("y".into())),
				}),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(js.matches("nymphCallable(function(").count() >= 2, "{js}");
	assert!(
		js.contains(".bind(this)"),
		"closure captures its lexical receiver: {js}"
	);
	assert!(
		js.contains(".liveLocals[0] +") && js.contains(".liveLocals[1]"),
		"{js}"
	);
}

#[test]
fn task_hir_emits_cold_recipes_and_explicit_suspension_operations() {
	let operation = |name: &str, operation: HirTaskOperation, operands: Vec<&str>| HirFunc {
		name: name.into(),
		params: operands.iter().map(|name: &&str| (*name).into()).collect(),
		body: HirExpr::TaskOperation {
			operation,
			operands: operands
				.into_iter()
				.map(|name| HirExpr::Local(name.into()))
				.collect(),
		},
	};
	let module = HirModule {
		lets: vec![],
		funcs: vec![
			HirFunc {
				name: "make".into(),
				params: vec![],
				body: HirExpr::TaskRecipe {
					body: Box::new(HirExpr::Block {
						stmts: vec![HirStmt::Expr(HirExpr::TaskOperation {
							operation: HirTaskOperation::Checkpoint,
							operands: vec![],
						})],
						tail: Some(Box::new(HirExpr::Int(42))),
					}),
					context: HirTaskContext::Nested,
				},
			},
			operation("drive", HirTaskOperation::Drive, vec!["task"]),
			operation("spawn", HirTaskOperation::Spawn, vec!["task"]),
			operation("observe", HirTaskOperation::Observe, vec!["handle"]),
			operation("cancel", HirTaskOperation::Cancel, vec!["handle"]),
			operation("select", HirTaskOperation::Select, vec!["first", "second"]),
			operation("race", HirTaskOperation::Race, vec!["first", "second"]),
		],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	for helper in [
		"nymphTaskRecipe",
		"nymphTaskDrive",
		"nymphTaskSpawn",
		"nymphHandleObserve",
		"nymphHandleCancel",
		"nymphCheckpoint",
		"nymphTaskSelect",
		"nymphTaskRace",
	] {
		assert!(js.contains(helper), "missing {helper}: {js}");
	}
	assert!(
		js.contains("nymphTaskRecipe(") && js.contains(", true)"),
		"{js}"
	);
	assert!(
		js.contains("return nymphSuspend(") && js.contains("return nymphCheckpoint();"),
		"{js}"
	);

	let script =
		format!("{js}\nconst result = await nymphRunTask(make()); console.log(String(result.v));");
	let output = std::process::Command::new("node")
		.arg("--input-type=module")
		.arg("--eval")
		.arg(script)
		.output()
		.expect("Node must be available for emitted task HIR tests");
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
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
					cleanup: None,
				}],
				tail: Some(Box::new(HirExpr::Local("g".into()))),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(js.matches("nymphCallable(function(").count() >= 2, "{js}");
	assert!(
		js.contains("return nymphReturn("),
		"closure return terminal: {js}"
	);
}

#[test]
fn managed_hir_registers_one_cleanup_and_emits_lexical_unwind() {
	let module = HirModule {
		lets: vec![],
		funcs: vec![HirFunc {
			name: "managed".into(),
			params: vec!["resource".into(), "close".into()],
			body: HirExpr::Block {
				stmts: vec![HirStmt::Let {
					name: "managed_resource".into(),
					value: HirExpr::Local("resource".into()),
					cleanup: Some(HirExpr::Call {
						callee: Box::new(HirExpr::Local("close".into())),
						args: vec![],
					}),
				}],
				tail: Some(Box::new(HirExpr::Local("managed_resource".into()))),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert_eq!(js.matches("nymphRegisterCleanup(() =>").count(), 1, "{js}");
	assert!(js.contains("nymphEnterCleanupScope()"), "{js}");
	assert!(js.contains("nymphUnwindCleanupScopes(1)"), "{js}");
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
				mode: OperationMode::Direct,
				lhs: Box::new(HirExpr::Num(1.0, NumKind::Raw)),
				rhs: Box::new(HirExpr::If {
					cond: Box::new(HirExpr::Local("flag".into())),
					then: Box::new(HirExpr::Block {
						stmts: vec![HirStmt::Return {
							value: Some(HirExpr::Num(7.0, NumKind::Raw)),
							target: nymph_hir::hir::HirReturnTarget::Callable,
						}],
						tail: None,
					}),
					otherwise: Some(Box::new(HirExpr::Num(2.0, NumKind::Raw))),
				}),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(
		js.contains("= 7;"),
		"return value is retained in a frame slot: {js}"
	);
	assert!(
		js.contains("return nymphReturn("),
		"return is a state terminal: {js}"
	);
	assert!(
		js.contains("resumeState"),
		"function has explicit states: {js}"
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
						value: HirExpr::Match {
							scrutinee: Box::new(HirExpr::Local("x".into())),
							arms: vec![
								HirArm {
									pat: HirPat::Lit(HirLit::Int(0)),
									guard: None,
									body: HirExpr::Block {
										stmts: vec![HirStmt::Return {
											value: Some(HirExpr::Int(1)),
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
						cleanup: None,
					}],
					tail: Some(Box::new(HirExpr::Local("y".into()))),
				}),
			},
		}],
		classes: vec![],
		enums: vec![],
	};
	let js = emit(&module);
	assert!(
		js.contains("new NInt(1n)"),
		"return value is retained in a frame slot: {js}"
	);
	assert!(
		js.contains("return nymphReturn("),
		"return is a closure terminal: {js}"
	);
	assert!(js.matches("nymphCallable(function(").count() >= 2, "{js}");
}
