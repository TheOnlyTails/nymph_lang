use std::{collections::BTreeSet, fs, path::Path};

use nymph_ast::Span;
use nymph_diagnostics::{
	EditGroup, SourceEdit, SourceId, SourceSnapshot, SourceVersion, TextReplacement,
};
use serde_json::Value;

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/legacy-migration");

fn field<'a>(value: &'a Value, name: &str) -> &'a str {
	value[name]
		.as_str()
		.unwrap_or_else(|| panic!("missing string field `{name}` in {value}"))
}

fn number(value: &Value, name: &str) -> usize {
	value[name]
		.as_u64()
		.unwrap_or_else(|| panic!("missing integer field `{name}` in {value}")) as usize
}

fn assert_guidance_link(reference: &str) {
	let (path, anchor) = reference
		.split_once('#')
		.unwrap_or_else(|| panic!("migration guidance needs an anchor: {reference}"));
	assert!(!anchor.is_empty());
	let path = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("../..")
		.join(path);
	let guidance =
		fs::read_to_string(path).unwrap_or_else(|_| panic!("missing migration guidance: {reference}"));
	assert!(
		guidance.lines().any(|line| {
			line
				.trim_start_matches('#')
				.trim()
				.to_ascii_lowercase()
				.replace(' ', "-")
				== anchor
		}),
		"missing migration guidance anchor: {reference}"
	);
}

fn fnv1a64(bytes: &[u8]) -> String {
	let mut hash = 0xcbf2_9ce4_8422_2325_u64;
	for byte in bytes {
		hash ^= u64::from(*byte);
		hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
	}
	format!("{hash:016x}")
}

#[test]
fn legacy_migration_corpus_is_frozen_and_every_class_has_an_expectation() {
	let root = Path::new(CORPUS);
	let manifest: Value =
		serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
	assert_eq!(manifest["version"], 1);
	assert_guidance_link(field(&manifest, "guidance"));

	let fixtures = manifest["fixtures"].as_array().unwrap();
	let fixture_ids = fixtures
		.iter()
		.map(|fixture| field(fixture, "id"))
		.collect::<BTreeSet<_>>();
	assert_eq!(
		fixture_ids.len(),
		fixtures.len(),
		"fixture ids must be unique"
	);

	for fixture in fixtures {
		let input_path = field(fixture, "input");
		assert!(
			input_path.ends_with(".nym.txt"),
			"legacy input entered an accepted .nym corpus: {input_path}"
		);
		let input = fs::read(root.join(input_path)).unwrap();
		let diagnostic = &fixture["diagnostic"];
		assert!(!field(diagnostic, "code").is_empty());
		let diagnostic_start = number(diagnostic, "start");
		let diagnostic_end = number(diagnostic, "end");
		assert!(diagnostic_start < diagnostic_end && diagnostic_end <= input.len());
		let input = String::from_utf8(input).unwrap();
		let load = |module: &str| (module == "main").then(|| input.clone());
		let diagnostics = nymph_compiler::check_project_library_with_embedded_std("main", &load);
		let has_error = diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.is_error());
		// These fixtures record policy decisions around otherwise valid source;
		// every fixture that contains retired language behavior must stay rejected.
		let policy_only = matches!(
			field(fixture, "id"),
			"integer-policy-choice" | "root-profile-and-echo-policy"
		);
		assert_eq!(
			has_error,
			!policy_only,
			"legacy fixture `{}` had an unexpected acceptance result: {diagnostics:?}",
			field(fixture, "id")
		);

		match field(fixture, "kind") {
			"safe" => {
				let result_path = field(fixture, "result");
				assert!(result_path.ends_with(".nym.txt"));
				let expected = fs::read_to_string(root.join(result_path)).unwrap();
				let replacements = fixture["replacements"].as_array().unwrap();
				assert!(!replacements.is_empty());
				let source = SourceId::new("migration-corpus", field(fixture, "id"));
				let edit = EditGroup::new(
					format!("Migrate {}", field(fixture, "id")),
					vec![
						SourceEdit::new(
							source.clone(),
							SourceVersion(1),
							replacements
								.iter()
								.map(|replacement| {
									TextReplacement::new(
										Span::new(number(replacement, "start"), number(replacement, "end")),
										field(replacement, "text"),
									)
								})
								.collect(),
						)
						.unwrap(),
					],
				)
				.unwrap();
				let actual = edit
					.apply(&[SourceSnapshot::new(source, SourceVersion(1), &input)])
					.unwrap()
					.pop()
					.unwrap()
					.text;
				assert_eq!(actual, expected, "safe fixture `{}`", field(fixture, "id"));

				let formatted = nymph_format::format(&actual, result_path)
					.unwrap_or_else(|error| panic!("safe fixture failed to format: {error:?}"));
				assert_eq!(
					nymph_format::format(&formatted, result_path).unwrap(),
					formatted,
					"safe fixture formatting was not idempotent"
				);
				let load = |module: &str| (module == "main").then(|| formatted.clone());
				let diagnostics = nymph_compiler::check_project_library_with_embedded_std("main", &load);
				assert!(
					diagnostics.is_empty(),
					"safe fixture `{}` failed checking: {diagnostics:?}",
					field(fixture, "id")
				);
				nymph_compiler::compile_project_library_with_embedded_std_and_options(
					"main",
					&load,
					&Default::default(),
				)
				.unwrap_or_else(|diagnostics| {
					panic!(
						"safe fixture `{}` failed lowering: {diagnostics:?}",
						field(fixture, "id")
					)
				});
			}
			"manual" => {
				assert!(fixture.get("result").is_none());
				assert!(fixture.get("replacements").is_none());
				assert_guidance_link(field(fixture, "guidance"));
				assert_eq!(number(&fixture["frozen"], "bytes"), input.len());
				assert_eq!(
					field(&fixture["frozen"], "fnv1a64"),
					fnv1a64(input.as_bytes())
				);
			}
			kind => panic!("unknown fixture kind `{kind}`"),
		}
	}

	let classes = manifest["migration_classes"].as_array().unwrap();
	let class_ids = classes
		.iter()
		.map(|class| field(class, "id"))
		.collect::<BTreeSet<_>>();
	assert_eq!(
		class_ids.len(),
		classes.len(),
		"migration class ids must be unique"
	);
	for class in classes {
		match (class.get("coverage"), class.get("manual_only")) {
			(Some(coverage), None) => assert!(
				fixture_ids.contains(coverage.as_str().unwrap()),
				"class `{}` names an unknown fixture",
				field(class, "id")
			),
			(None, Some(expectation)) => assert!(!expectation.as_str().unwrap().is_empty()),
			_ => panic!(
				"class `{}` must have fixture coverage or one manual-only expectation",
				field(class, "id")
			),
		}
	}

	let files = fs::read_dir(root.join("safe"))
		.unwrap()
		.chain(fs::read_dir(root.join("manual")).unwrap())
		.map(|entry| entry.unwrap().path())
		.collect::<BTreeSet<_>>();
	let declared = fixtures
		.iter()
		.flat_map(|fixture| [fixture.get("input"), fixture.get("result")])
		.flatten()
		.map(|path| root.join(path.as_str().unwrap()))
		.collect::<BTreeSet<_>>();
	assert_eq!(files, declared, "every frozen corpus file must be declared");
}
