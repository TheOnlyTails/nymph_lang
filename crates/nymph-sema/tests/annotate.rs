//! The checker returns a [`Checked`] result: diagnostics plus a [`NodeId`]-keyed
//! annotation side-table for the lowering pass. This slice wires the plumbing; the
//! recording of specific node kinds is exercised as later slices add lowering.

use nymph_ast::expr::{Expr, ExprKind};
use nymph_sema::check_module;
use nymph_syntax::parse_module;

/// Parse `source`, asserting it parses cleanly (these tests exercise the checker).
fn parse(source: &str) -> nymph_ast::decl::Module {
	let parsed = parse_module(source, "test");
	let parse_errors: Vec<_> = parsed
		.diagnostics
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect();
	assert!(
		parse_errors.is_empty(),
		"source failed to parse: {parse_errors:?}\n---\n{source}"
	);
	parsed.tree
}

#[test]
fn checked_result_exposes_diags_and_annotations() {
	let module = parse("func f(): int = 1 + 2");
	let checked = check_module(&module);

	// A well-typed program has no diagnostics, and the annotation side-table is
	// reachable off the same result (populated as recording is wired in later).
	assert!(
		checked.diags.is_empty(),
		"well-typed program should have no diagnostics: {:?}",
		checked.diags
	);
	let _ = checked.annotations.is_empty();
}

/// Walk every expression node reachable in a function body, collecting ids.
fn collect_ids(expr: &Expr, out: &mut Vec<nymph_ast::NodeId>) {
	out.push(expr.id);
	if let ExprKind::BinaryOp { lhs, rhs, .. } = &expr.kind {
		collect_ids(lhs, out);
		collect_ids(rhs, out);
	}
}

/// Walk every expression node reachable from `expr`, including list/tuple/map
/// literal elements. Broader than [`collect_ids`] above (which only recurses into
/// `BinaryOp`) so it can walk collection literals too.
fn collect_expr_ids(expr: &Expr, out: &mut Vec<nymph_ast::NodeId>) {
	use nymph_ast::expr::ListItem;
	out.push(expr.id);
	match &expr.kind {
		ExprKind::BinaryOp { lhs, rhs, .. } => {
			collect_expr_ids(lhs, out);
			collect_expr_ids(rhs, out);
		}
		ExprKind::List(items) | ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(e) | ListItem::Spread(e) => collect_expr_ids(e, out),
				}
			}
		}
		_ => {}
	}
}

#[test]
fn literals_and_operators_are_annotated() {
	let module = parse("func f(): int = 1 + 2");
	let checked = check_module(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);

	// Reach the `1 + 2` body: the single func's body expression.
	let nymph_ast::decl::Declaration::Func { body, .. } = &module.members[0] else {
		panic!("expected a func declaration, got {:?}", module.members[0]);
	};

	let mut ids = Vec::new();
	collect_ids(body, &mut ids);
	assert_eq!(
		ids.len(),
		3,
		"expected `+`, `1`, `2` — three expression nodes"
	);

	// Every one of the three nodes (the binary op and both int literals) was
	// recorded with a resolved type.
	for id in ids {
		assert!(
			checked.annotations.get(id).is_some(),
			"node {id:?} should be annotated",
		);
	}
}

#[test]
fn records_type_of_collection_literals() {
	// A list literal's node should carry a recorded type after checking.
	let module = parse("func f(): #[int] = #[1, 2, 3]");
	let checked = check_module(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);

	// Reach the `#[1, 2, 3]` body: the single func's body expression.
	let nymph_ast::decl::Declaration::Func { body, .. } = &module.members[0] else {
		panic!("expected a func declaration, got {:?}", module.members[0]);
	};

	let mut ids = Vec::new();
	collect_expr_ids(body, &mut ids);
	let annotated = ids
		.iter()
		.filter(|id| checked.annotations.get(**id).is_some())
		.count();
	assert_eq!(
		annotated,
		ids.len(),
		"every inferred expression node should be annotated"
	);
}

#[test]
fn records_variant_pattern_resolution() {
	let module = parse(
		r#"
		enum Opt { Some(value: int), None }
		func f(o: Opt): int = match (o) {
			Some(value) -> value,
			None -> 0,
		}
		"#,
	);
	let checked = check_module(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);

	// Reach the match arms and assert each variant pattern was recorded by its span.
	let nymph_ast::decl::Declaration::Func { body, .. } = &module.members[1] else {
		panic!("expected a func declaration");
	};
	let ExprKind::Match { arms, .. } = &body.kind else {
		panic!("expected a match, got {:?}", body.kind);
	};
	let mut found = 0;
	for arm in arms {
		if let Some(res) = checked.annotations.pattern_variant_of(arm.pattern.1) {
			assert_eq!(res.enum_name, "Opt");
			assert!(res.variant == "Some" || res.variant == "None");
			found += 1;
		}
	}
	assert_eq!(found, 2, "both Some(value) and None patterns recorded");
}

#[test]
fn records_variant_resolution() {
	// Bare construction, bare nullary reference, and qualified construction each
	// record their resolved (enum, variant) so lowering can emit the tag ABI.
	let module = parse(
		r#"
		enum Opt { Some(value: int), None }
		func f(): Opt = Some(value = 1)
		func g(): Opt = None
		func h(): Opt = Opt.Some(value = 2)
		"#,
	);
	let checked = check_module(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);

	let mut found = 0;
	for member in &module.members {
		let nymph_ast::decl::Declaration::Func { body, .. } = member else {
			continue;
		};
		// Each func body IS a variant expression; its top-level node carries the
		// resolution.
		if let Some(res) = checked.annotations.variant_of(body.id) {
			assert_eq!(res.enum_name, "Opt");
			assert!(
				res.variant == "Some" || res.variant == "None",
				"unexpected variant {:?}",
				res.variant
			);
			found += 1;
		}
	}
	assert_eq!(found, 3, "Some, None, and Opt.Some each recorded once");
}
