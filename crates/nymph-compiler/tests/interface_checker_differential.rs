#![cfg(feature = "test-support")]

use std::sync::{Arc, Mutex};

use nymph_compiler::project::{
	CompilerSession, ModulePath, ProjectId, SemanticPipeline, SemanticQueryEvent, SourceVersion,
};
use nymph_sema::EntryMode;

fn path(value: &str) -> ModulePath {
	ModulePath::new(value).unwrap()
}

fn fixture(pipeline: SemanticPipeline) -> CompilerSession {
	CompilerSession::with_semantic_pipeline_for_test(pipeline)
}

#[derive(Clone)]
struct MatrixFixture {
	category: &'static str,
	modules: &'static [(&'static str, &'static str)],
	entry: &'static str,
	mode: EntryMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Outcome {
	diagnostics: Vec<(String, String, String, String, usize, usize)>,
	types: Vec<(u32, String)>,
	definition_targets: Vec<(u32, String)>,
	resolutions: Vec<(u32, String, String, Option<String>, Option<String>)>,
	variants: Vec<(u32, String)>,
	pattern_variants: Vec<(usize, usize, String)>,
}

fn run_matrix_fixture(case: &MatrixFixture, pipeline: SemanticPipeline) -> Outcome {
	let mut session = fixture(pipeline);
	let project = ProjectId::new(format!("differential-{}", case.category));
	for (index, (module, source)) in case.modules.iter().enumerate() {
		session.set_source(
			project.clone(),
			path(module),
			(*source).into(),
			SourceVersion(index as i64 + 1),
		);
	}
	let diagnostics = session
		.tooling_diagnostics(project.clone(), path(case.entry), false)
		.iter()
		.map(|item| {
			(
				item.module.clone(),
				item.diag.code.to_string(),
				item.diag.message.to_string(),
				format!("{:?}", item.diag.severity),
				item.diag.span.start,
				item.diag.span.end,
			)
		})
		.collect();
	let _analysis = session
		.analyze_module(
			project.clone(),
			path(case.entry),
			path(case.entry),
			case.mode,
		)
		.unwrap_or_else(|| {
			panic!(
				"matrix entry `{}` must be reachable in {pipeline:?}; diagnostics: {diagnostics:?}",
				case.category
			)
		});
	let stable = session
		.stable_annotations_for_test(
			project.clone(),
			path(case.entry),
			path(case.entry),
			case.mode,
		)
		.expect("matrix entry has a stable annotation projection");
	let types = stable
		.types
		.iter()
		.map(|(id, ty)| (id.0, format!("{ty:?}")))
		.collect();
	let entry_source_len = case
		.modules
		.iter()
		.find_map(|(module, source)| (*module == case.entry).then_some(source.len()))
		.expect("matrix entry source exists");
	let definition_targets = stable
		.definition_targets
		.iter()
		.map(|(id, target)| (id.0, format!("{target:?}")))
		.collect();
	let resolutions = stable
		.resolutions
		.iter()
		.map(|(id, method, dispatch, target, implementation)| {
			(
				id.0,
				method.to_string(),
				format!("{dispatch:?}"),
				target.as_ref().map(|item| format!("{item:?}")),
				implementation.as_ref().map(|item| format!("{item:?}")),
			)
		})
		.collect();
	let variants = stable
		.variants
		.iter()
		.map(|(id, variant)| (id.0, format!("{variant:?}")))
		.collect();
	let pattern_variants = stable
		.pattern_variants
		.iter()
		.filter(|(span, _)| span.end <= entry_source_len)
		.map(|(span, variant)| (span.start, span.end, format!("{variant:?}")))
		.collect();
	Outcome {
		diagnostics,
		types,
		definition_targets,
		resolutions,
		variants,
		pattern_variants,
	}
}

#[test]
fn full_differential_fixture_matrix() {
	let cases = [
		MatrixFixture {
			category: "direct-transitive-complete-visibility",
			modules: &[
				(
					"c",
					"public struct Answer(value: int) { func get(): int = this.value }",
				),
				(
					"b",
					"import @/c with (Answer)\npublic func make_answer(): Answer = Answer(value = 42)",
				),
				(
					"main",
					"import @/b with (make_answer)\nfunc main(): int = make_answer().get()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "visibility-import-alias-namespace",
			modules: &[
				(
					"dep",
					"func hidden(): int = 1\npublic func shown(): int = 2",
				),
				(
					"main",
					"import @/dep as d with (shown)\nfunc main(): int = d.shown() + shown() + hidden()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "aliases",
			modules: &[
				(
					"dep",
					"public type Number = int\npublic func id(value: Number): Number = value",
				),
				("main", "import @/dep\nfunc main(): Number = id(4)"),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "adts-fields-variants-patterns",
			modules: &[
				(
					"dep",
					"public struct Point(x: int)\npublic enum Choice { One(value: int), None }",
				),
				(
					"main",
					"import @/dep\nfunc qualified(): int = { let p = Point(x = 1)\nlet c = Choice.One(value = p.x)\nmatch (c) { Choice.One(value = n) -> n, Choice.None -> 0 } }\nfunc bare(): int = { let c = One(value = 2)\nmatch (c) { One(value = n) -> n, None -> 0 } }\nfunc main(): int = qualified() + bare()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "inherent-namespace-mutating-statics-values",
			modules: &[
				(
					"dep",
					"public struct Box(value: int) { func get(): int = this.value\nnamespace func make(value: int): Box = Box(value = value)\nnamespace let zero: int = 0 }",
				),
				(
					"main",
					"import @/dep\nfunc main(): int = Box.make(Box.zero).get()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "interfaces-associated-direct-default",
			modules: &[
				(
					"dep",
					"public interface Read<Output> { func read(): Output\nfunc twice(): Output = this.read() }\npublic struct Cell(value: int) { impl Read<Output = int> { func read(): int = this.value } }",
				),
				(
					"main",
					"import @/dep\nfunc main(): int = Cell(value = 2).read() + Cell(value = 3).twice()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "generic-constraints-blanket-precedence",
			modules: &[
				(
					"dep",
					"public interface Get<T> { func get(): T }\npublic func take<T, U: Get<T = T>>(value: U): T = value.get()\npublic struct Item(value: int) { impl Get<T = int> { func get(): int = this.value } }",
				),
				(
					"main",
					"import @/dep\nfunc main(): int = take(Item(value = 3))",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "coherence-overlap-orphan",
			modules: &[
				(
					"dep",
					"public interface Mark { func mark(): int }\npublic struct Item",
				),
				(
					"main",
					"import @/dep\nimpl Mark for Item { func mark(): int = 1 }\nimpl Mark for Item { func mark(): int = 2 }\nfunc main(): int = 0",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "dependency-errors-poison-independent",
			modules: &[
				("dep", "public func broken(): Missing = nope"),
				(
					"main",
					"import @/dep\nfunc main(): int { broken(1); independent }",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "externals-marshal-abi",
			modules: &[
				(
					"dep",
					"public external(host_value) let value: int\npublic external(host_call) func call(value: int): int",
				),
				("main", "import @/dep\nfunc main(): int = call(value)"),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "entry-mode",
			modules: &[("main", "public func helper(): int = 1")],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "library-mode",
			modules: &[("main", "public func helper(): int = 1")],
			entry: "main",
			mode: EntryMode::Library,
		},
		MatrixFixture {
			category: "stable-definition-targets",
			modules: &[
				("dep", "public struct Remote(value: int)"),
				(
					"main",
					"import @/dep with (Remote)\nstruct Left(value: int)\nstruct Right(value: int)\nfunc helper(): int = 4\nfunc main(): int = Remote(value = 1).value + Left(value = 2).value + Right(value = 3).value + helper()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
	];

	for case in cases {
		let compatibility = run_matrix_fixture(&case, SemanticPipeline::CompatibilityFlattened);
		let interface = run_matrix_fixture(&case, SemanticPipeline::Interface);
		if case.category == "visibility-import-alias-namespace" {
			let mut compatibility_without_stable_targets = compatibility.clone();
			let mut interface_without_stable_targets = interface.clone();
			compatibility_without_stable_targets
				.definition_targets
				.clear();
			interface_without_stable_targets.definition_targets.clear();
			assert_eq!(
				compatibility_without_stable_targets,
				interface_without_stable_targets
			);
			assert!(
				interface.definition_targets.iter().any(|(_, target)| {
					target.contains("path: \"dep\"") && target.contains("name: \"shown\"")
				}),
				"namespace and with references retain dep's stable shown target",
			);
			continue;
		}
		assert_eq!(
			compatibility, interface,
			"differential category `{}` diverged",
			case.category,
		);
		if case.category == "direct-transitive-complete-visibility" {
			let (_, call_target) = interface
				.definition_targets
				.iter()
				.find(|(_, target)| target.contains("name: \"make_answer\""))
				.expect("category 1 records the direct B function call");
			assert!(
				call_target.contains("path: \"b\""),
				"make_answer call target must be owned by B: {call_target:?}",
			);
			let (_, _, _, method_target, implementation) = interface
				.resolutions
				.iter()
				.find(|(_, method, _, _, _)| method == "get")
				.expect("category 1 records the inherent C method call");
			assert!(
				method_target
					.as_ref()
					.is_some_and(|target| target.contains("path: \"c\"")),
				"get method target must be owned by C: {method_target:?}",
			);
			assert!(
				implementation
					.as_ref()
					.is_some_and(|target| target.contains("path: \"c\"")),
				"get implementation provenance must be owned by C: {implementation:?}",
			);
		}
		if case.category == "interfaces-associated-direct-default" {
			let read = interface
				.resolutions
				.iter()
				.find(|(_, method, _, _, _)| method == "read")
				.expect("category 6 records the direct impl method call");
			assert_eq!(read.2, "UserImpl");
			assert!(read.3.as_ref().is_some_and(|target| {
				target.contains("key: Implementation") && target.contains("name: \"read\"")
			}));
			assert!(
				read
					.4
					.as_ref()
					.is_some_and(|implementation| implementation.contains("key: Implementation"))
			);
			let twice = interface
				.resolutions
				.iter()
				.find(|(_, method, _, _, _)| method == "twice")
				.expect("category 6 records the selected interface default call");
			assert_eq!(twice.2, "UserImplDefaultMethod");
			assert!(twice.3.as_ref().is_some_and(|target| {
				target.contains("category: Method") && target.contains("name: \"twice\"")
			}));
			assert_eq!(twice.4, read.4);
			assert!(
				interface.types.iter().filter(|(_, ty)| ty == "Int").count() >= 3,
				"both associated-output method calls and their sum have int type"
			);
		}
	}
}

#[test]
fn stable_definition_targets_are_exact_and_owner_sensitive() {
	let case = MatrixFixture {
		category: "stable-definition-targets-exact",
		modules: &[
			("dep", "public struct Remote(value: int)"),
			(
				"main",
				"import @/dep with (Remote)\nstruct Left(value: int)\nstruct Right(value: int)\nfunc helper(): int = 4\nfunc main(): int = Remote(value = 1).value + Left(value = 2).value + Right(value = 3).value + helper()",
			),
		],
		entry: "main",
		mode: EntryMode::Entry,
	};
	let compatibility = run_matrix_fixture(&case, SemanticPipeline::CompatibilityFlattened);
	let interface = run_matrix_fixture(&case, SemanticPipeline::Interface);
	assert_eq!(compatibility, interface);
	for owner in ["Remote", "Left", "Right"] {
		assert!(interface.definition_targets.iter().any(|(_, target)| {
			target.contains("category: Field")
				&& target.contains("name: \"value\"")
				&& target.contains(&format!("name: \"{owner}\""))
		}));
	}
	assert!(interface.definition_targets.iter().any(|(_, target)| {
		target.contains("category: Function") && target.contains("name: \"helper\"")
	}));
}

#[test]
fn lexical_import_visibility_category_matches_compatibility() {
	let cases = [
		MatrixFixture {
			category: "no-with-direct-bare-and-default-namespace",
			modules: &[
				("dep", "public func shown(): int = 2"),
				(
					"main",
					"import @/dep\nfunc main(): int = shown() + dep.shown()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "no-with-does-not-expose-transitive-name",
			modules: &[
				("c", "public func transitive(): int = 1"),
				("b", "import @/c\npublic func direct(): int = transitive()"),
				(
					"main",
					"import @/b\nfunc main(): int = direct() + transitive()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "namespace-with-hidden",
			modules: &[
				(
					"dep",
					"func hidden(): int = 1\npublic func shown(): int = 2",
				),
				(
					"main",
					"import @/dep as d with (shown)\nfunc main(): int = d.shown() + shown() + hidden()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "with-alias",
			modules: &[
				("dep", "public func shown(): int = 2"),
				(
					"main",
					"import @/dep with (shown as local)\nfunc main(): int = local()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "default-namespace",
			modules: &[
				("dep", "public func shown(): int = 2"),
				("main", "import @/dep\nfunc main(): int = dep.shown()"),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
		MatrixFixture {
			category: "no-transitive-bare-name",
			modules: &[
				("c", "public func transitive(): int = 1"),
				(
					"b",
					"import @/c with (transitive)\npublic func direct(): int = transitive()",
				),
				(
					"main",
					"import @/b with (direct)\nfunc main(): int = direct() + transitive()",
				),
			],
			entry: "main",
			mode: EntryMode::Entry,
		},
	];
	for case in cases {
		let compatibility = run_matrix_fixture(&case, SemanticPipeline::CompatibilityFlattened);
		let interface = run_matrix_fixture(&case, SemanticPipeline::Interface);
		assert_eq!(
			compatibility.diagnostics, interface.diagnostics,
			"{}",
			case.category
		);
		assert_eq!(compatibility.types, interface.types, "{}", case.category);
		if case.category == "no-with-direct-bare-and-default-namespace" {
			assert!(interface.diagnostics.is_empty());
			assert!(interface.definition_targets.iter().any(|(_, target)| {
				target.contains("path: \"dep\"") && target.contains("name: \"shown\"")
			}));
		}
		if case.category == "no-with-does-not-expose-transitive-name" {
			assert!(interface.diagnostics.iter().any(|(_, code, message, ..)| {
				code == "2000" && message == "cannot find `transitive` in this scope"
			}));
		}
		assert!(
			interface.definition_targets.iter().all(|(_, target)| {
				!target.contains("name: \"hidden\"") && !target.contains("name: \"transitive\"")
			}),
			"{} leaked a non-lexical stable target",
			case.category,
		);
	}
}

#[test]
fn direct_and_transitive_imports_match_compatibility() {
	fn run(pipeline: SemanticPipeline) -> (Vec<(String, String, String)>, String) {
		let mut session = fixture(pipeline);
		let project = ProjectId::new("differential-transitive");
		session.set_source(
			project.clone(),
			path("c"),
			"public func answer(): int = 42".into(),
			SourceVersion(1),
		);
		session.set_source(
			project.clone(),
			path("b"),
			"import @/c with (answer)\npublic func b_answer(): int = answer()".into(),
			SourceVersion(1),
		);
		session.set_source(
			project.clone(),
			path("main"),
			"import @/b with (b_answer)\nfunc main(): int = b_answer()".into(),
			SourceVersion(1),
		);
		let diagnostics = session
			.tooling_diagnostics(project.clone(), path("main"), false)
			.iter()
			.map(|item| {
				(
					item.module.clone(),
					item.diag.code.to_string(),
					item.diag.message.to_string(),
				)
			})
			.collect();
		let ty = session
			.analyze_module(project, path("main"), path("main"), EntryMode::Entry)
			.unwrap()
			.type_at(35)
			.unwrap();
		(diagnostics, ty)
	}

	assert_eq!(
		run(SemanticPipeline::Interface),
		run(SemanticPipeline::CompatibilityFlattened)
	);
}

#[test]
fn interface_pipeline_never_reads_a_dependency_body() {
	let events = Arc::new(Mutex::new(Vec::<SemanticQueryEvent>::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_detailed_event_callback_for_test(
		SemanticPipeline::Interface,
		move |event| sink.lock().unwrap().push(event),
	);
	let project = ProjectId::new("no-dependency-body");
	session.set_source(
		project.clone(),
		path("dep"),
		"public func answer(): int = 42".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		path("main"),
		"import @/dep\nfunc main() = answer()".into(),
		SourceVersion(1),
	);
	session.warm_interface_dependency_environments_for_test(
		project.clone(),
		path("main"),
		path("main"),
		EntryMode::Entry,
	);
	events.lock().unwrap().clear();
	session.panic_on_dependency_body_access_for_test(project.clone(), path("main"));
	assert!(
		session
			.analyze_module(project, path("main"), path("main"), EntryMode::Entry)
			.is_some()
	);
	let observed = events.lock().unwrap();
	assert!(observed.iter().any(|event| {
		event.query == "interface_module_analysis" && event.module.as_deref() == Some("main")
	}));
	assert!(!observed.iter().any(|event| {
		event.module.as_deref() == Some("dep")
			&& (event.query == "parse"
				|| event.query == "interface_module_analysis"
				|| event.query.starts_with("compat_"))
	}));
}
