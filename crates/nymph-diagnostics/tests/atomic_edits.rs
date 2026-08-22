use nymph_ast::Span;
use nymph_diagnostics::{
	Applicability, Diagnostic, EditError, EditGroup, SourceEdit, SourceId, SourceSnapshot,
	SourceVersion, TextReplacement,
};

fn source(project: &str, module: &str) -> SourceId {
	SourceId::new(project, module)
}

#[test]
fn machine_applicable_groups_apply_atomically_in_canonical_order() {
	let main = source("demo", "main");
	let helper = source("demo", "nested/helper");
	let main_text = "let café = 1\nlet result = café\n";
	let helper_text = "let answer =\n  41\n";
	let cafe_start = main_text.find("café").unwrap();
	let result_start = main_text.rfind("café").unwrap();
	let number_start = helper_text.find("41").unwrap();
	let group = EditGroup::new(
		"Use the canonical names",
		vec![
			SourceEdit::new(
				helper.clone(),
				SourceVersion(3),
				vec![TextReplacement::new(
					Span::new(number_start, number_start + 2),
					"42",
				)],
			)
			.unwrap(),
			SourceEdit::new(
				main.clone(),
				SourceVersion(7),
				vec![
					TextReplacement::new(
						Span::new(result_start, result_start + "café".len()),
						"coffee",
					),
					TextReplacement::new(Span::new(cafe_start, cafe_start + "café".len()), "coffee"),
				],
			)
			.unwrap(),
		],
	)
	.unwrap();

	assert_eq!(group.title(), "Use the canonical names");
	assert_eq!(group.applicability(), Applicability::MachineApplicable);
	assert_eq!(group.sources()[0].source(), &main);
	assert_eq!(group.sources()[0].version(), SourceVersion(7));
	assert_eq!(group.sources()[0].source().project(), "demo");
	assert_eq!(group.sources()[0].source().module(), "main");
	assert_eq!(
		group.sources()[0]
			.replacements()
			.iter()
			.map(TextReplacement::span)
			.collect::<Vec<_>>(),
		[
			Span::new(cafe_start, cafe_start + "café".len()),
			Span::new(result_start, result_start + "café".len()),
		]
	);
	assert!(
		group.sources()[0]
			.replacements()
			.iter()
			.all(|replacement| replacement.replacement() == "coffee")
	);
	let diagnostic = Diagnostic::warning(
		"MIGRATION-SAFE".into(),
		"canonical rename available",
		Span::new(cafe_start, cafe_start + "café".len()),
	)
	.with_edit(group.clone());
	assert_eq!(diagnostic.edits(), std::slice::from_ref(&group));

	let edited = group
		.apply(&[
			SourceSnapshot::new(helper.clone(), SourceVersion(3), helper_text),
			SourceSnapshot::new(main.clone(), SourceVersion(7), main_text),
		])
		.unwrap();
	assert_eq!(edited[0].source, main);
	assert_eq!(edited[0].text, "let coffee = 1\nlet result = coffee\n");
	assert_eq!(edited[1].source, helper);
	assert_eq!(edited[1].text, "let answer =\n  42\n");
}

#[test]
fn invalid_overlapping_and_stale_groups_are_rejected_before_any_text_is_returned() {
	let id = source("demo", "main");
	let helper = source("demo", "helper");
	let overlap = SourceEdit::new(
		id.clone(),
		SourceVersion(1),
		vec![
			TextReplacement::new(Span::new(0, 3), "one"),
			TextReplacement::new(Span::new(2, 4), "two"),
		],
	)
	.unwrap_err();
	assert!(matches!(overlap, EditError::OverlappingReplacements { .. }));

	let group = EditGroup::new(
		"Replace the name",
		vec![
			SourceEdit::new(
				id.clone(),
				SourceVersion(2),
				vec![TextReplacement::new(Span::new(0, 4), "value")],
			)
			.unwrap(),
			SourceEdit::new(
				helper.clone(),
				SourceVersion(4),
				vec![TextReplacement::new(Span::new(0, 4), "safe")],
			)
			.unwrap(),
		],
	)
	.unwrap();
	assert!(matches!(
		group.apply(&[
			SourceSnapshot::new(helper, SourceVersion(4), "name"),
			SourceSnapshot::new(id, SourceVersion(1), "name"),
		]),
		Err(EditError::StaleSource { .. })
	));
}

#[test]
fn spans_must_be_valid_utf8_boundaries_and_groups_must_name_exact_sources() {
	let id = source("demo", "main");
	let group = EditGroup::new(
		"Replace the emoji",
		vec![
			SourceEdit::new(
				id.clone(),
				SourceVersion(1),
				vec![TextReplacement::new(Span::new(2, 3), "x")],
			)
			.unwrap(),
		],
	)
	.unwrap();
	assert!(matches!(
		group.validate(&[SourceSnapshot::new(id.clone(), SourceVersion(1), "a😀z")]),
		Err(EditError::InvalidUtf8Boundary { .. })
	));
	assert!(matches!(
		group.validate(&[SourceSnapshot::new(
			source("other", "main"),
			SourceVersion(1),
			"a😀z"
		)]),
		Err(EditError::MissingSource { source }) if source == id
	));
}

#[test]
fn a_manual_diagnostic_has_no_edit_group() {
	let diagnostic = Diagnostic::error(
		"MIGRATION-MANUAL".into(),
		"this migration requires a semantic choice",
		Span::new(0, 5),
	)
	.with_help("rewrite this code manually");

	assert!(diagnostic.edits().is_empty());
}
