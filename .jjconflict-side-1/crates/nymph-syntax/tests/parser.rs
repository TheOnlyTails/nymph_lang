//! Integration tests for the parser, exercising the refined Nymph syntax end to end
//! (lex → parse).

use nymph_ast::{
	Span,
	decl::{Declaration, FuncDeclaration, FuncKind, ImplMember, LetKind},
	expr::{
		CallArg, Expr, ExprKind, ListItem, Pattern, RangeKind, RangePatternKind, Statement, StringPart,
	},
	ops::BinaryOperator,
	token::Token,
	ty::{Effect, GenericArgValue, GenericParamKind, Type},
};
use nymph_syntax::{lex, parse_expression, parse_module};

fn expr(src: &str) -> Expr {
	let result = parse_expression(src);
	assert!(
		result.diagnostics.is_empty(),
		"unexpected diagnostics for {src:?}: {:?}",
		result.diagnostics
	);
	result.tree
}

fn module_ok(src: &str) -> Vec<Declaration> {
	let result = parse_module(src, "test");
	assert!(
		result.diagnostics.is_empty(),
		"unexpected diagnostics: {:?}",
		result.diagnostics
	);
	result.tree.members
}

#[test]
fn effect_forms_parse_through_declarations_generics_and_callable_types() {
	let members = module_ok(
		"effect Database\n\
		 effect Network\n\
		 func pure(): void + !() = {}\n\
		 func inferred(): !_ = {}\n\
		 func apply<T, !E>(callback: (T) -> void + !E): !Database + !Network + !E = {}\n\
		 type Applied = Wrapper<int, !Database + !Network>",
	);
	assert!(matches!(members[0], Declaration::Effect { .. }));
	assert!(matches!(members[1], Declaration::Effect { .. }));

	let Declaration::Func { meta: pure, .. } = &members[2] else {
		panic!("expected pure function");
	};
	assert!(pure.effects.as_ref().unwrap().0.effects.is_empty());

	let Declaration::Func { meta: inferred, .. } = &members[3] else {
		panic!("expected inferred function");
	};
	assert!(matches!(
		inferred.effects.as_ref().unwrap().0.effects.as_slice(),
		[nymph_ast::Spanned(Effect::Infer, _)]
	));

	let Declaration::Func { meta: apply, .. } = &members[4] else {
		panic!("expected generic function");
	};
	assert_eq!(apply.generics[0].0.kind, GenericParamKind::Type);
	assert_eq!(apply.generics[1].0.kind, GenericParamKind::Effect);
	let Type::Function { effects, .. } = &apply.params[0].0.type_.0 else {
		panic!("expected effectful callable parameter");
	};
	assert!(matches!(
		effects.as_ref().unwrap().0.effects.as_slice(),
		[nymph_ast::Spanned(Effect::Named(name), _)] if name.0 == "E"
	));
	assert_eq!(apply.effects.as_ref().unwrap().0.effects.len(), 3);

	let Declaration::TypeAlias { value, .. } = &members[5] else {
		panic!("expected type alias");
	};
	let Type::Reference { generics, .. } = &value.0 else {
		panic!("expected applied type");
	};
	assert!(matches!(generics[0].0.value, GenericArgValue::Type(_)));
	assert!(matches!(generics[1].0.value, GenericArgValue::Effect(_)));
}

#[test]
fn malformed_effect_row_reports_its_cause_and_recovers_to_the_next_declaration() {
	let result = parse_module(
		"effect Database\nfunc broken(): void + !Database + = {}\neffect Later",
		"test",
	);
	assert!(
		result.diagnostics.iter().any(|diagnostic| diagnostic
			.message
			.contains("expected an effect beginning with `!` after `+`")),
		"missing causal effect-row diagnostic: {:?}",
		result.diagnostics
	);
	assert!(matches!(
		result.tree.members.last(),
		Some(Declaration::Effect { name, .. }) if name.0 == "Later"
	));
}

#[test]
fn literals_and_identifiers() {
	assert!(matches!(expr("42").kind, ExprKind::Int(_)));
	assert!(matches!(expr("3.14").kind, ExprKind::Float(_)));
	assert!(matches!(expr("true").kind, ExprKind::Boolean(_)));
	assert!(matches!(expr("'a'").kind, ExprKind::Char(_)));
	assert!(matches!(expr("foo").kind, ExprKind::Identifier(_)));
	assert!(matches!(expr("this").kind, ExprKind::This));
}

#[test]
fn echo_parses_as_a_dedicated_prefix_and_pipeline_expression() {
	let prefixed = expr("echo value");
	let ExprKind::Echo { operand, keyword } = prefixed.kind else {
		panic!("expected echo expression");
	};
	assert!(matches!(operand.kind, ExprKind::Identifier(ref name) if name.0 == "value"));
	assert_eq!(keyword, Span::new(0, 4));

	let piped = expr("value |> echo");
	let ExprKind::Echo { operand, keyword } = piped.kind else {
		panic!("expected pipeline-position echo expression");
	};
	assert!(matches!(operand.kind, ExprKind::Identifier(ref name) if name.0 == "value"));
	assert_eq!(keyword, Span::new(9, 13));
}

#[test]
fn lexer_accepts_integer_storage_boundaries() {
	let signed_min_magnitude = lex("9223372036854775808");
	assert!(signed_min_magnitude.diagnostics.is_empty());
	assert!(matches!(
		signed_min_magnitude.tokens.as_slice(),
		[nymph_ast::Spanned(Token::Int(9_223_372_036_854_775_808), _)]
	));

	let unsigned_max = lex("18446744073709551615u");
	assert!(unsigned_max.diagnostics.is_empty());
	assert!(matches!(
		unsigned_max.tokens.as_slice(),
		[nymph_ast::Spanned(Token::UInt(u64::MAX), _)]
	));
}

#[test]
fn lexer_diagnoses_integer_literals_larger_than_u64() {
	for source in [
		"18446744073709551616",
		"18446744073709551616u",
		"0x10000000000000000",
	] {
		let result = lex(source);
		assert!(
			result
				.diagnostics
				.iter()
				.any(|diag| diag.message.contains("out of range")),
			"expected an out-of-range diagnostic for {source}, got {:?}",
			result.diagnostics
		);
	}
}

#[test]
fn signed_integer_patterns_respect_i64_boundaries() {
	let ExprKind::PatternOp { rhs, .. } = expr("value is -9223372036854775808").kind else {
		panic!("expected pattern operator");
	};
	assert!(matches!(rhs.0, Pattern::Int(value) if value.0 == i64::MIN));

	for source in [
		"value is 9223372036854775808",
		"value is -9223372036854775809",
	] {
		let result = parse_expression(source);
		assert!(
			result
				.diagnostics
				.iter()
				.any(|diagnostic| diagnostic.message.contains("out of range")),
			"expected an out-of-range diagnostic for {source}, got {:?}",
			result.diagnostics
		);
	}
}

#[test]
fn arithmetic_precedence() {
	// 1 + 2 * 3 parses as 1 + (2 * 3)
	let ExprKind::BinaryOp { op, rhs, .. } = expr("1 + 2 * 3").kind else {
		panic!("expected a binary op");
	};
	assert_eq!(op, BinaryOperator::Plus);
	assert!(matches!(
		rhs.kind,
		ExprKind::BinaryOp {
			op: BinaryOperator::Times,
			..
		}
	));
}

#[test]
fn power_is_right_associative() {
	// 2 ** 3 ** 2 parses as 2 ** (3 ** 2)
	let ExprKind::BinaryOp { op, rhs, .. } = expr("2 ** 3 ** 2").kind else {
		panic!("expected a binary op");
	};
	assert_eq!(op, BinaryOperator::Power);
	assert!(matches!(
		rhs.kind,
		ExprKind::BinaryOp {
			op: BinaryOperator::Power,
			..
		}
	));
}

#[test]
fn shift_recombination() {
	let ExprKind::BinaryOp { op, .. } = expr("a << b").kind else {
		panic!("expected a binary op");
	};
	assert_eq!(op, BinaryOperator::LeftShift);
}

#[test]
fn collections_and_ranges() {
	assert!(matches!(expr("#[1, 2, 3]").kind, ExprKind::List(_)));
	assert!(matches!(expr("#(1, true, 'a')").kind, ExprKind::Tuple(_)));
	assert!(matches!(expr(r#"#{"a": 1}"#).kind, ExprKind::Map(_)));
	assert!(matches!(
		expr("1..10").kind,
		ExprKind::Range(RangeKind::Exclusive { .. })
	));
	assert!(matches!(
		expr("1..=10").kind,
		ExprKind::Range(RangeKind::Inclusive { .. })
	));
	assert!(matches!(
		expr("1..").kind,
		ExprKind::Range(RangeKind::From(_))
	));
	assert!(matches!(
		expr("..10").kind,
		ExprKind::Range(RangeKind::To(_))
	));
	assert!(matches!(
		expr("..=10").kind,
		ExprKind::Range(RangeKind::ToInclusive(_))
	));
}

#[test]
fn range_forms_parse_consistently_in_expression_and_pattern_contexts() {
	let expression_forms = [
		("1..", "from"),
		("1..10", "exclusive"),
		("1..=10", "inclusive"),
		("..10", "to"),
		("..=10", "to-inclusive"),
	];
	for (source, expected) in expression_forms {
		let kind = expr(source).kind;
		let matches = matches!(
			(&kind, expected),
			(ExprKind::Range(RangeKind::From(_)), "from")
				| (ExprKind::Range(RangeKind::Exclusive { .. }), "exclusive")
				| (ExprKind::Range(RangeKind::Inclusive { .. }), "inclusive")
				| (ExprKind::Range(RangeKind::To(_)), "to")
				| (ExprKind::Range(RangeKind::ToInclusive(_)), "to-inclusive")
		);
		assert!(matches, "unexpected range kind for {source:?}: {kind:?}");
	}

	let pattern_forms = [
		("1..", "from"),
		("1..10", "exclusive"),
		("1..=10", "inclusive"),
		("..10", "to"),
		("..=10", "to-inclusive"),
	];
	for (range, expected) in pattern_forms {
		let source = format!("value is {range}");
		let ExprKind::PatternOp { rhs, .. } = expr(&source).kind else {
			panic!("expected pattern operator for {source:?}");
		};
		let matches = matches!(
			(&rhs.0, expected),
			(Pattern::Range(RangePatternKind::From(_)), "from")
				| (
					Pattern::Range(RangePatternKind::Exclusive { .. }),
					"exclusive"
				) | (
				Pattern::Range(RangePatternKind::Inclusive { .. }),
				"inclusive"
			) | (Pattern::Range(RangePatternKind::To(_)), "to")
				| (
					Pattern::Range(RangePatternKind::ToInclusive(_)),
					"to-inclusive"
				)
		);
		assert!(matches, "unexpected range pattern kind for {source:?}");
	}
}

#[test]
fn missing_inclusive_range_upper_bound_has_a_dedicated_diagnostic() {
	for (source, expected_span) in [("1..=", (1, 4)), ("value is 1..=", (10, 13))] {
		let result = parse_expression(source);
		assert_eq!(
			result.diagnostics.len(),
			1,
			"expected one diagnostic for {source:?}, got {:?}",
			result.diagnostics
		);
		let diagnostic = &result.diagnostics[0];
		assert_eq!(diagnostic.code, "1022", "wrong diagnostic for {source:?}");
		assert_eq!(
			diagnostic.message, "an inclusive range needs an upper bound",
			"wrong diagnostic for {source:?}"
		);
		assert_eq!(
			(diagnostic.span.start, diagnostic.span.end),
			expected_span,
			"wrong diagnostic span for {source:?}"
		);
	}
}

#[test]
fn calls_and_member_access() {
	// foo.bar(1).baz chains member/call postfix operators.
	assert!(matches!(
		expr("foo.bar(1).baz").kind,
		ExprKind::MemberAccess { .. }
	));
	assert!(matches!(
		expr("list?.first").kind,
		ExprKind::MemberAccess { optional: true, .. }
	));
	assert!(matches!(
		expr("value?").kind,
		ExprKind::PostfixOp { label: None, .. }
	));
	assert!(matches!(
		expr("value?@target").kind,
		ExprKind::PostfixOp { label: Some(_), .. }
	));
}

#[test]
fn call_arguments_preserve_named_values_and_spreads_explicitly() {
	let ExprKind::Call { args, .. } = expr("Point(...source, y = value)").kind else {
		panic!("expected call");
	};
	assert!(matches!(
		&args[0].0,
		CallArg::Spread { value } if matches!(value.kind, ExprKind::Identifier(_))
	));
	assert!(matches!(
		&args[1].0,
		CallArg::Value { name: Some(name), value }
			if name.0 == "y" && matches!(value.kind, ExprKind::Identifier(_))
	));
}

#[test]
fn optional_chain_postfix_forms_and_composition() {
	let method = expr("option?.method(1)");
	let ExprKind::Call { func, .. } = method.kind else {
		panic!("expected optional method call");
	};
	assert!(matches!(
		func.kind,
		ExprKind::MemberAccess { optional: true, .. }
	));
	assert!(matches!(
		expr("option?.[index]").kind,
		ExprKind::IndexAccess { optional: true, .. }
	));
	let chained = expr("option?.child?.name");
	let ExprKind::MemberAccess {
		parent,
		optional: true,
		..
	} = chained.kind
	else {
		panic!("expected outer optional member access");
	};
	assert!(matches!(
		parent.kind,
		ExprKind::MemberAccess { optional: true, .. }
	));
}

#[test]
fn closures() {
	assert!(matches!(expr("x -> x + 1").kind, ExprKind::Closure { .. }));
	assert!(matches!(
		expr("(a, b) -> a + b").kind,
		ExprKind::Closure { .. }
	));
	assert!(matches!(expr("() -> 0").kind, ExprKind::Closure { .. }));
	// A grouped expression, not a closure.
	assert!(matches!(expr("(1 + 2)").kind, ExprKind::Grouped(_)));
}

#[test]
fn control_flow_expressions() {
	assert!(matches!(
		expr("if (x > 0) x else -x").kind,
		ExprKind::If { .. }
	));
	assert!(matches!(
		expr("match (n) { 0 -> true, _ -> false }").kind,
		ExprKind::Match { .. }
	));
	assert!(matches!(
		expr("{ let x = 1 x + 1 }").kind,
		ExprKind::Block { .. }
	));
	assert!(matches!(
		expr("break 42").kind,
		ExprKind::Break {
			value: Some(_),
			label: None,
		}
	));
	assert!(matches!(
		expr("continue").kind,
		ExprKind::Continue { label: None, .. }
	));
}

#[test]
fn labeled_control_syntax() {
	assert!(matches!(
		expr("for@outer (x in xs) {}").kind,
		ExprKind::For { label: Some(_), .. }
	));
	assert!(matches!(
		expr("outer@{ return@outer 1 }").kind,
		ExprKind::Block { label: Some(_), .. }
	));
	assert!(matches!(
		expr("break@outer 1").kind,
		ExprKind::Break { label: Some(_), .. }
	));
	assert!(matches!(
		expr("continue@outer").kind,
		ExprKind::Continue { label: Some(_), .. }
	));
	assert!(matches!(
		expr("outer@() -> 1").kind,
		ExprKind::Closure { label: Some(_), .. }
	));
	assert!(matches!(
		expr("() -> outer@{ return@outer 1 }").kind,
		ExprKind::Closure { label: Some(_), .. }
	));
	assert!(matches!(
		expr("outer@(x)  ->  outer@{ return@outer x }").kind,
		ExprKind::Closure { label: Some(_), .. }
	));
}

#[test]
fn immutable_state_loop_syntax_keeps_named_replacements() {
	let parsed = expr(
		"loop@outer (let left = 1, let right: int = left + 1) { continue@outer(left = right, right = left) }",
	);
	let ExprKind::StateLoop {
		bindings,
		body,
		label: Some(label),
	} = parsed.kind
	else {
		panic!("expected a labeled state loop")
	};
	assert_eq!(label.0, "outer");
	assert_eq!(bindings.len(), 2);
	assert!(matches!(
		&bindings[0].meta.name.0,
		Pattern::Binding { name, .. } if name.0 == "left"
	));
	assert!(matches!(
		&bindings[1].meta.name.0,
		Pattern::Binding { name, .. } if name.0 == "right"
	));
	let ExprKind::Block { body, .. } = body.kind else {
		panic!("expected a loop body block")
	};
	let Statement::Expr(Expr {
		kind: ExprKind::Continue {
			label: Some(label),
			replacements,
		},
		..
	}) = &body[0].0
	else {
		panic!("expected a labeled named continue")
	};
	assert_eq!(label.0, "outer");
	assert_eq!(replacements.len(), 2);
	assert_eq!(replacements[0].name.0, "left");
	assert_eq!(replacements[1].name.0, "right");
}

#[test]
fn label_edges_must_be_adjacent() {
	for source in [
		"for @outer (x in xs) {}",
		"for@ outer (x in xs) {}",
		"break @outer 1",
		"break@ outer 1",
		"continue @outer",
		"continue@ outer",
		"return @outer 1",
		"return@ outer 1",
		"value? @outer",
		"value?@ outer",
		"outer @{ 1 }",
		"outer@ { 1 }",
		"outer @(x) -> x",
		"outer@ (x) -> x",
		"(x) -> outer @{ x }",
		"(x) -> outer@ { x }",
	] {
		let result = parse_expression(source);
		assert!(
			result
				.diagnostics
				.iter()
				.any(|diagnostic| diagnostic.message.contains("cannot contain whitespace")),
			"expected focused label-whitespace diagnostic for {source:?}: {:?}",
			result.diagnostics
		);
	}
}

#[test]
fn mismatched_dual_closure_labels_mark_both_labels() {
	let source = "outer@(x) -> inner@{ x }";
	let result = parse_expression(source);
	let diagnostic = result
		.diagnostics
		.iter()
		.find(|diagnostic| diagnostic.message.contains("closure labels must match"))
		.expect("expected mismatched-label diagnostic");
	assert_eq!(diagnostic.span, Span::new(0, 5));
	assert_eq!(diagnostic.labels.len(), 1);
	assert_eq!(diagnostic.labels[0].span, Span::new(13, 18));
}

#[test]
fn string_interpolation() {
	let e = expr(r#""Hello, ${name}!""#);
	let ExprKind::String(parts) = e.kind else {
		panic!("expected string");
	};
	assert_eq!(parts.len(), 3);
	assert!(matches!(
		parts[1].0,
		nymph_ast::expr::StringPart::InterpolatedExpr(_)
	));
}

#[test]
fn empty_string_interpolation_has_a_focused_diagnostic() {
	for source in [r#""${}""#, r#""${   }""#] {
		let result = parse_expression(source);
		assert_eq!(
			result.diagnostics.len(),
			1,
			"diagnostics for {source:?}: {:?}",
			result.diagnostics
		);
		let diagnostic = &result.diagnostics[0];
		assert_eq!(
			diagnostic.message,
			"string interpolation requires an expression"
		);
		assert_eq!(diagnostic.span, nymph_ast::Span::new(1, source.len() - 1));
	}
}

#[test]
fn trailing_string_interpolation_content_has_a_focused_diagnostic() {
	for (source, trailing_span) in [
		(r#""${a b}""#, nymph_ast::Span::new(5, 6)),
		(r#""${a; b}""#, nymph_ast::Span::new(4, 5)),
	] {
		let result = parse_expression(source);
		assert_eq!(
			result.diagnostics.len(),
			1,
			"diagnostics for {source:?}: {:?}",
			result.diagnostics
		);
		let diagnostic = &result.diagnostics[0];
		assert_eq!(
			diagnostic.message,
			"unexpected trailing content in string interpolation"
		);
		assert_eq!(diagnostic.span, trailing_span);
	}
}

#[test]
fn string_interpolation_accepts_one_complete_expression() {
	for source in [
		r#""${(a + b)}""#,
		r#""${x -> x + 1}""#,
		r#""${outer(inner(a + b))}""#,
	] {
		expr(source);
	}
}

#[test]
fn interpolation_parses_balanced_block_and_match_expressions() {
	for source in [r#""${{ let x = 1 x }}""#, r#""${match (x) { _ -> x }}""#] {
		let result = parse_expression(source);
		assert!(
			result.diagnostics.is_empty(),
			"brace-containing interpolation failed to parse: {source:?}: {:?}",
			result.diagnostics
		);
	}
}

#[test]
fn nested_string_interpolation_reaches_the_parser_with_absolute_spans() {
	let source = r#""outer ${"inner ${{ 1 }}"} tail""#;
	let result = parse_expression(source);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let ExprKind::String(parts) = result.tree.kind else {
		panic!("expected outer string");
	};
	let StringPart::InterpolatedExpr(inner_string) = &parts[1].0 else {
		panic!("expected outer interpolation");
	};
	assert_eq!(parts[1].1, Span::new(7, 26));
	assert_eq!(inner_string.span, Span::new(9, 25));
	let ExprKind::String(inner_parts) = &inner_string.kind else {
		panic!("expected inner string");
	};
	let StringPart::InterpolatedExpr(block) = &inner_parts[1].0 else {
		panic!("expected nested interpolation");
	};
	assert_eq!(inner_parts[1].1, Span::new(16, 24));
	assert_eq!(block.span, Span::new(18, 23));
}

#[test]
fn invalid_interpolation_recovers_to_later_string_fragments() {
	let result = parse_expression(r#""${a b} text ${c d} tail""#);
	assert_eq!(
		result
			.diagnostics
			.iter()
			.map(|diagnostic| (diagnostic.message.as_str(), diagnostic.span))
			.collect::<Vec<_>>(),
		vec![
			(
				"unexpected trailing content in string interpolation",
				Span::new(5, 6)
			),
			(
				"unexpected trailing content in string interpolation",
				Span::new(17, 18)
			),
		]
	);
	let ExprKind::String(parts) = result.tree.kind else {
		panic!("expected recovered string");
	};
	assert_eq!(
		parts.len(),
		4,
		"all later fragments should survive recovery"
	);

	let StringPart::InterpolatedExpr(first) = &parts[0].0 else {
		panic!("expected first interpolation, got {:?}", parts[0]);
	};
	assert!(matches!(&first.kind, ExprKind::Identifier(name) if name.0 == "a"));
	assert_eq!(first.span, Span::new(3, 4));
	assert_eq!(parts[0].1, Span::new(1, 7));

	assert!(matches!(&parts[1].0, StringPart::Text(text) if text == " text "));
	assert_eq!(parts[1].1, Span::new(7, 13));

	let StringPart::InterpolatedExpr(second) = &parts[2].0 else {
		panic!("expected second interpolation, got {:?}", parts[2]);
	};
	assert!(matches!(&second.kind, ExprKind::Identifier(name) if name.0 == "c"));
	assert_eq!(second.span, Span::new(15, 16));
	assert_eq!(parts[2].1, Span::new(13, 19));

	assert!(matches!(&parts[3].0, StringPart::Text(text) if text == " tail"));
	assert_eq!(parts[3].1, Span::new(19, 24));
}

#[test]
fn semicolons_remain_rejected_outside_interpolation() {
	let top_level = parse_expression("a; b");
	assert_eq!(top_level.tree.span, Span::new(0, 1));
	assert_eq!(top_level.diagnostics.len(), 1);
	assert_eq!(
		top_level.diagnostics[0].message,
		"unexpected trailing tokens after expression"
	);
	assert_eq!(top_level.diagnostics[0].span, Span::new(1, 2));

	let block = parse_expression("{ a; b }");
	assert!(matches!(block.tree.kind, ExprKind::Block { .. }));
	assert_eq!(
		block
			.diagnostics
			.iter()
			.map(|diagnostic| (diagnostic.message.as_str(), diagnostic.span))
			.collect::<Vec<_>>(),
		vec![("expected an expression, found `;`", Span::new(3, 4))]
	);

	let declaration = parse_module("let a = 1; let b = 2", "test");
	assert_eq!(
		declaration.tree.members.len(),
		2,
		"recovery should retain the later declaration"
	);
	assert_eq!(declaration.diagnostics.len(), 1);
	assert_eq!(
		declaration.diagnostics[0].message,
		"expected a declaration, found `;`"
	);
	assert_eq!(declaration.diagnostics[0].span, Span::new(9, 10));
}

#[test]
fn pattern_operators() {
	assert!(matches!(
		expr("x is Some(value)").kind,
		ExprKind::PatternOp { .. }
	));
	assert!(matches!(
		expr("x !is None").kind,
		ExprKind::PatternOp { .. }
	));
	assert!(matches!(expr("x as float").kind, ExprKind::TypeOp { .. }));
}

#[test]
fn merged_bang_keywords_parse_in_outer_and_nested_interpolation() {
	for operator in ["!in", "!is"] {
		for source in [
			r#""${{x} OPERATOR y}""#.replace("OPERATOR", operator),
			r#""${"nested ${{x} OPERATOR y}"}""#.replace("OPERATOR", operator),
		] {
			let result = parse_expression(&source);
			assert!(
				result.diagnostics.is_empty(),
				"{operator} failed in {source:?}: {:?}",
				result.diagnostics
			);
		}
	}
}

#[test]
fn binding_subpattern_parses_in_every_pattern_position() {
	let ExprKind::PatternOp { rhs, .. } = expr("value is whole = #(head, tail)").kind else {
		panic!("expected pattern operator");
	};
	assert!(matches!(
		rhs.0,
		Pattern::Binding { ref name, ref inner }
			if name.0 == "whole" && matches!(inner.0, Pattern::Tuple(_))
	));

	for source in [
		"value is item = 1",
		"value is #(pair = #(left, right), list = #[head, ...tail])",
		"value is grouped = (1 | 2)",
		"value is left = 1 | right = 2",
		"match (value) { whole = 1 -> whole, _ -> 0 }",
		"{ let whole = #(left, right) = #(1, 2) whole }",
		"(whole = #(left, right)) -> left",
	] {
		let result = parse_expression(source);
		assert!(
			result.diagnostics.is_empty(),
			"unexpected diagnostics for {source:?}: {:?}",
			result.diagnostics
		);
	}

	module_ok("struct Box(value: int)\nfunc get(whole = Box(value): Box): int = value");
}

#[test]
fn annotated_let_binding_subpattern_is_disambiguated_from_the_initializer() {
	for source in [
		"let whole = #(a, b): #(int, int) = #(1, 2)",
		"func f(value: #(int, int)): int = { let whole = #(a, b): #(int, int) = value a + b }",
	] {
		let result = parse_module(source, "test");
		assert!(
			result.diagnostics.is_empty(),
			"unexpected diagnostics for {source:?}: {:?}",
			result.diagnostics
		);
	}

	// The first `=` remains the initializer delimiter for an ordinary annotated let.
	module_ok("let whole: #(int, int) = #(1, 2)");
}

#[test]
fn binding_subpattern_inside_struct_field_retains_field_selection() {
	let ExprKind::PatternOp { rhs, .. } = expr("value is Box(value = captured = 1)").kind else {
		panic!("expected pattern operator");
	};
	let Pattern::Struct { fields, .. } = rhs.0 else {
		panic!("expected struct pattern");
	};
	assert!(matches!(
		&fields[0].0,
		nymph_ast::expr::StructPatternField::Value { name, value }
			if name.0 == "value"
				&& matches!(&value.0, Pattern::Binding { name, inner }
					if name.0 == "captured" && matches!(inner.0, Pattern::Int(_)))
	));
}

#[test]
fn let_and_func_declarations() {
	let members = module_ok("let x = 1\nfunc add(a: int, b: int): int = a + b");
	assert_eq!(members.len(), 2);
	assert!(matches!(members[0], Declaration::Let { .. }));
	assert!(matches!(members[1], Declaration::Func { .. }));
}

#[test]
fn func_with_block_body() {
	// Block body is just `= { ... }` since blocks are expressions.
	let members = module_ok("func main(): void = {\n  let x = 1\n  x\n}");
	assert!(matches!(members[0], Declaration::Func { .. }));
}

#[test]
fn struct_declaration() {
	let members = module_ok(
		"public struct Person(name: string, age: int) {\n  func is_minor(): boolean = this.age < 18\n}",
	);
	assert!(matches!(members[0], Declaration::Struct { .. }));
}

#[test]
fn enum_declaration() {
	let members = module_ok(
		"public enum Option<T> {\n  Some(value: T),\n  None\n\n  func is_some(): boolean = match (this) { Some(...) -> true, None -> false }\n}",
	);
	let Declaration::Enum {
		variants,
		members: enum_members,
		..
	} = &members[0]
	else {
		panic!("expected enum");
	};
	assert_eq!(variants.len(), 2);
	assert_eq!(enum_members.len(), 1);
}

#[test]
fn interface_and_impl() {
	let members = module_ok(
		"interface Plus<Other, Output> {\n  func plus(other: Other): Output\n}\n\nimpl Plus<Other = int, Output = int> for int {\n  external func plus(other: int): int\n}",
	);
	assert!(matches!(members[0], Declaration::Interface { .. }));
	assert!(matches!(members[1], Declaration::ImplFor { .. }));
}

#[test]
fn import_declaration() {
	let members = module_ok("import @/math with (sin as sine, cos)");
	let Declaration::Import { path, idents, .. } = &members[0] else {
		panic!("expected import");
	};
	assert_eq!(path.len(), 1);
	assert_eq!(idents.as_ref().unwrap().len(), 2);
}

#[test]
fn import_bare() {
	let members = module_ok("import @/math");
	let Declaration::Import {
		path,
		alias,
		idents,
		..
	} = &members[0]
	else {
		panic!("expected import");
	};
	assert_eq!(path.len(), 1);
	assert_eq!(path[0].0, "math");
	assert!(alias.is_none());
	assert!(idents.is_none());
}

#[test]
fn import_with_alias() {
	let members = module_ok("import @/math as m");
	let Declaration::Import {
		path,
		alias,
		idents,
		..
	} = &members[0]
	else {
		panic!("expected import");
	};
	assert_eq!(path.len(), 1);
	assert_eq!(alias.as_ref().unwrap().0, "m");
	assert!(idents.is_none());
}

#[test]
fn import_with_idents_only() {
	let members = module_ok("import @/math with (sin, cos as c)");
	let Declaration::Import { alias, idents, .. } = &members[0] else {
		panic!("expected import");
	};
	assert!(alias.is_none());
	let idents = idents.as_ref().unwrap();
	assert_eq!(idents.len(), 2);
	assert_eq!(idents[0].0.0, "sin");
	assert!(idents[0].1.is_none());
	assert_eq!(idents[1].0.0, "cos");
	assert_eq!(idents[1].1.as_ref().unwrap().0, "c");
}

#[test]
fn import_alias_and_idents_combined() {
	let members = module_ok("import @/math as m with (sin as sine, cos)");
	let Declaration::Import {
		path,
		alias,
		idents,
		..
	} = &members[0]
	else {
		panic!("expected import");
	};
	assert_eq!(path.len(), 1);
	assert_eq!(alias.as_ref().unwrap().0, "m");
	let idents = idents.as_ref().unwrap();
	assert_eq!(idents.len(), 2);
	assert_eq!(idents[0].1.as_ref().unwrap().0, "sine");
}

/// Recursively push every expression node id reachable from `expr`. Kept minimal:
/// it only needs to reach the nodes produced by
/// `expression_node_ids_are_unique_and_dense`'s source (binary ops, a call, args,
/// literals), not exhaustively every `ExprKind` variant.
fn collect_expr_ids(expr: &Expr, out: &mut Vec<u32>) {
	out.push(expr.id.0);
	match &expr.kind {
		ExprKind::BinaryOp { lhs, rhs, .. } => {
			collect_expr_ids(lhs, out);
			collect_expr_ids(rhs, out);
		}
		ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(expr) | ListItem::Spread(expr) => collect_expr_ids(expr, out),
				}
			}
		}
		ExprKind::Call { func, args, .. } => {
			collect_expr_ids(func, out);
			for arg in args {
				collect_expr_ids(arg.0.value(), out);
			}
		}
		ExprKind::PrefixOp { value, .. }
		| ExprKind::PostfixOp { value, .. }
		| ExprKind::Grouped(value) => {
			collect_expr_ids(value, out);
		}
		_ => {}
	}
}

/// Push every expression node id reachable from a declaration's body/value.
fn collect_decl_expr_ids(decl: &Declaration, out: &mut Vec<u32>) {
	match decl {
		Declaration::Func { body, .. } => collect_expr_ids(body, out),
		Declaration::Let { value, .. } => collect_expr_ids(value, out),
		_ => {}
	}
}

#[test]
fn expression_node_ids_are_unique_and_dense() {
	// A body with several nested expressions: binary ops, a call, a literal.
	let src = "func f() = 1 + g(2) * 3";
	let members = module_ok(src);

	let mut ids = Vec::new();
	for member in &members {
		collect_decl_expr_ids(member, &mut ids);
	}

	let mut sorted = ids.clone();
	sorted.sort_unstable();
	sorted.dedup();
	assert_eq!(sorted.len(), ids.len(), "node ids must be unique");
	assert_ne!(ids.len(), 0, "expected several expression nodes");
	// Dense from 0: the parser numbers in construction order starting at 0.
	assert_eq!(*sorted.first().unwrap(), 0);
	assert_eq!(*sorted.last().unwrap(), (ids.len() as u32) - 1);
}

// ── Function kinds and `namespace Name { … }` ──────────────────────────────

/// Every member `func` kind that a struct/enum body carries, in order.
fn struct_member_kinds(decl: &Declaration) -> Vec<FuncKind> {
	let Declaration::Struct { members, .. } = decl else {
		panic!("expected a struct, got {decl:?}");
	};
	members
		.iter()
		.filter_map(|m| match &m.0 {
			ImplMember::Func { meta, .. } => Some(meta.kind.clone()),
			_ => None,
		})
		.collect()
}

#[test]
fn struct_body_carries_destination_func_kinds_and_splits_out_impls() {
	let members = module_ok(
		"struct Counter(n: int) {
		   func get(): int = this.n
		   namespace func zero(): Counter = Counter(n = 0)
		   impl Default { func default(): Counter = Counter(n = 0) }
		 }",
	);
	assert_eq!(
		struct_member_kinds(&members[0]),
		vec![FuncKind::Instance, FuncKind::Namespace],
	);
	let Declaration::Struct { impls, .. } = &members[0] else {
		unreachable!();
	};
	assert_eq!(impls.len(), 1, "the nested impl is not a flat member");
}

#[test]
fn top_level_namespace_block_still_parses() {
	// A named `namespace Name { … }` remains a top-level declaration, holding
	// regular funcs.
	let members = module_ok("namespace Math { func double(x: int): int = x * 2 }");
	assert!(
		matches!(&members[0], Declaration::Namespace { name, .. } if name.0 == "Math"),
		"expected a namespace declaration, got {:?}",
		members[0],
	);
}

#[test]
fn top_level_namespace_func_is_diagnosed() {
	let result = parse_module("namespace func f(): int = 1", "test");
	assert!(
		result
			.diagnostics
			.iter()
			.any(|d| d.message.contains("only valid inside")),
		"expected an outside-a-type diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn external_funcs_carry_their_kind() {
	let kind_of = |src: &str| -> FuncKind {
		let members = module_ok(src);
		match &members[0] {
			Declaration::ExternalFunc(_, _, FuncDeclaration { kind, .. }) => kind.clone(),
			other => panic!("expected an external func, got {other:?}"),
		}
	};
	assert_eq!(kind_of("external(js_f) func f(): int"), FuncKind::Instance);
	assert_eq!(
		kind_of("external(js_h) namespace func h(): int"),
		FuncKind::Namespace
	);
}

#[test]
fn external_lets_default_and_preserve_explicit_markers() {
	let members = module_ok(
		"public external let default_name: float\nprivate external(host_name) let explicit_name: float",
	);
	let markers: Vec<_> = members
		.iter()
		.map(|decl| match decl {
			Declaration::ExternalLet(_, marker, meta) => (
				marker.as_str(),
				meta.name.0.as_binding().unwrap().0.as_str(),
			),
			other => panic!("expected an external let, got {other:?}"),
		})
		.collect();
	assert_eq!(
		markers,
		[
			("default_name", "default_name"),
			("host_name", "explicit_name")
		]
	);
}

#[test]
fn namespace_let_is_a_static_member_binding() {
	let members = module_ok(
		"struct Config(v: int) {
		   namespace let DEFAULT_V: int = 42
		   let plain: int = 1
		 }",
	);
	let Declaration::Struct { members, .. } = &members[0] else {
		panic!("expected a struct, got {:?}", members[0]);
	};
	let kinds: Vec<LetKind> = members
		.iter()
		.filter_map(|m| match &m.0 {
			ImplMember::Let { meta, .. } => Some(meta.kind),
			_ => None,
		})
		.collect();
	assert_eq!(kinds, vec![LetKind::Namespace, LetKind::Instance]);
}

#[test]
fn let_use_is_a_contextual_managed_binding_kind() {
	let parsed = parse_module(
		"func use(value: int): int = value\nfunc main(): int = { let use resource = 1\n use(resource) }",
		"test",
	);
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let Declaration::Func { body, .. } = &parsed.tree.members[1] else {
		panic!("expected main function")
	};
	let nymph_ast::expr::ExprKind::Block { body, .. } = &body.kind else {
		panic!("expected block body")
	};
	let nymph_ast::expr::Statement::Let { meta, .. } = &body[0].0 else {
		panic!("expected managed let")
	};
	assert_eq!(meta.kind, LetKind::Use);
}

#[test]
fn let_use_outside_a_local_block_is_diagnosed_and_recovered() {
	let parsed = parse_module(
		"let use resource = acquire()\nfunc main(): void = {}",
		"test",
	);
	assert!(
		parsed.diagnostics.iter().any(|diagnostic| diagnostic
			.message
			.contains("only allowed for lexical local bindings")),
		"{:?}",
		parsed.diagnostics
	);
	assert_eq!(parsed.tree.members.len(), 2);
}

#[test]
fn top_level_namespace_let_is_diagnosed() {
	let result = parse_module("namespace let x: int = 1", "test");
	assert!(
		result
			.diagnostics
			.iter()
			.any(|d| d.message.contains("only valid inside")),
		"expected an outside-a-type diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn namespace_block_rejects_a_namespace_let() {
	let result = parse_module("namespace N { namespace let x: int = 1 }", "test");
	assert!(
		result
			.diagnostics
			.iter()
			.any(|d| d.message.contains("only contain regular")),
		"expected a namespace-only-regular diagnostic, got {:?}",
		result.diagnostics,
	);
}

#[test]
fn enum_views_and_single_variant_types_parse() {
	let result = parse_module(
		"enum Source<T> { A, B(value: T) }\n\
		 enum View<T> { ...Source, Source.B, C }\n\
		 func selected(value: Source<int>.B): View<int> = value",
		"test",
	);
	assert!(
		result
			.diagnostics
			.iter()
			.all(|diagnostic| !diagnostic.is_error()),
		"unexpected diagnostics: {:?}",
		result.diagnostics
	);
	let Declaration::Enum { embeddings, .. } = &result.tree.members[1] else {
		panic!("expected enum declaration");
	};
	assert_eq!(embeddings.len(), 2);
	assert!(embeddings[0].0.variant.is_none());
	assert_eq!(
		embeddings[1].0.variant.as_ref().map(|name| name.0.as_str()),
		Some("B")
	);
}

#[test]
fn async_function_block_and_await_have_dedicated_ast_nodes() {
	let members = module_ok("async func load(): int = async { 1 }.await");
	let Declaration::Func { meta, body, .. } = &members[0] else {
		panic!("expected async function declaration");
	};
	assert!(meta.is_async);
	let ExprKind::Await { value, .. } = &body.kind else {
		panic!("expected await expression");
	};
	assert!(matches!(value.kind, ExprKind::AsyncBlock { .. }));
}

#[test]
fn malformed_async_declaration_recovers_at_the_next_declaration() {
	let parsed = parse_module("async nope\nfunc recovered(): int = 1", "test");
	assert!(
		parsed
			.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.is_error())
	);
	assert!(parsed.tree.members.iter().any(|member| {
		matches!(member, Declaration::Func { meta, .. } if meta.name.0 == "recovered")
	}));
}

#[test]
fn external_async_function_is_rejected_without_publishing_a_task_signature() {
	let parsed = parse_module(
		"external async func load(): int\nfunc recovered(): int = 1",
		"test",
	);
	assert!(parsed.diagnostics.iter().any(|diagnostic| {
		diagnostic
			.message
			.contains("`external async func` is unsupported")
	}));
	let Declaration::ExternalFunc(_, _, meta) = &parsed.tree.members[0] else {
		panic!("expected recovered external function");
	};
	assert!(!meta.is_async);
	assert!(matches!(
		&parsed.tree.members[1],
		Declaration::Func { meta, .. } if meta.name.0 == "recovered"
	));
}
