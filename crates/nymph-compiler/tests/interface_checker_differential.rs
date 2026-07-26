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

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
	diagnostics: Vec<(String, String, String, String, usize, usize)>,
	types: Vec<(usize, String)>,
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
	let source = case
		.modules
		.iter()
		.find(|(name, _)| *name == case.entry)
		.unwrap()
		.1;
	let analysis = session
		.analyze_module(project, path(case.entry), path(case.entry), case.mode)
		.expect("matrix entry must be reachable");
	let types = (0..=source.len())
		.filter_map(|offset| analysis.type_at(offset).map(|ty| (offset, ty)))
		.collect();
	let annotations = &analysis.semantic.annotations;
	let definition_targets = annotations
		.definition_targets()
		.map(|(id, target)| (id.0, format!("{target:?}")))
		.collect();
	let resolutions = annotations
		.infos()
		.filter(|(id, _)| id.0 < 1 << 30)
		.filter_map(|(id, info)| {
			info.resolution.as_ref().map(|resolution| {
				(
					id.0,
					resolution.method.to_string(),
					format!("{:?}", resolution.dispatch),
					resolution.target.as_ref().map(|item| format!("{item:?}")),
					resolution
						.implementation
						.as_ref()
						.map(|item| format!("{item:?}")),
				)
			})
		})
		.collect();
	let variants = annotations
		.variants()
		.filter(|(id, _)| id.0 < 1 << 30)
		.map(|(id, variant)| (id.0, format!("{variant:?}")))
		.collect();
	let pattern_variants = annotations
		.pattern_variants()
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
				("c", "public func answer(): int = 42"),
				(
					"b",
					"import @/c with (answer)\npublic func b_answer(): int = answer()",
				),
				(
					"main",
					"import @/b with (b_answer)\nfunc main(): int = c.answer() + b_answer()",
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
					"import @/dep\nfunc main(): int { let p = Point(x = 1); let c = Choice.One(value = p.x); match c { Choice.One(value = n) => n, Choice.None => 0 } }",
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
					"import @/dep\nfunc main(): int = Cell(value = 2).read()",
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
	];

	for case in cases {
		assert_eq!(
			run_matrix_fixture(&case, SemanticPipeline::CompatibilityFlattened),
			run_matrix_fixture(&case, SemanticPipeline::Interface),
			"differential category `{}` diverged",
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
