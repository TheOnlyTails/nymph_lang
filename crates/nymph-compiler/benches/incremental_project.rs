use std::{
	collections::{BTreeMap, BTreeSet, HashSet},
	hint::black_box,
	sync::{Arc, Mutex},
};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use nymph_compiler::project::{
	CompilerSession, GraphFixture, GraphShape, ModulePath, ProjectId, SemanticQueryEvent,
	SourceVersion,
};
use nymph_compiler::{check_project_library, compile_project_library};
use nymph_sema::{DeclarationKey, DefinitionId, EntryMode};

type Events = Arc<Mutex<Vec<SemanticQueryEvent>>>;
const PRIVATE_MODULE: &str = "mixed/branch_000/level_003";

#[derive(Clone, Copy, Debug)]
enum Request {
	Diagnostics,
	AnalysisTypeAt,
	EmitProject,
	FullCompile,
}

struct RetainedState {
	session: CompilerSession,
	project: ProjectId,
	entry: ModulePath,
	target: ModulePath,
	type_offset: usize,
	sources: BTreeMap<String, String>,
}

fn ordinary_sources() -> BTreeMap<String, String> {
	let mut sources = GraphShape::Mixed { width: 4, depth: 4 }
		.generate()
		.sources()
		.clone();
	sources
		.get_mut(PRIVATE_MODULE)
		.unwrap()
		.push_str("private func private_work(): int = 1\n");
	sources
}

fn private_post_edit_sources() -> BTreeMap<String, String> {
	let mut sources = ordinary_sources();
	apply_private_edit(&mut sources);
	sources
}

fn apply_private_edit(sources: &mut BTreeMap<String, String>) -> String {
	let source = sources.get_mut(PRIVATE_MODULE).unwrap();
	let after = source.replace(
		"private func private_work(): int = 1",
		"private func private_work(): int = 2",
	);
	assert_ne!(
		*source, after,
		"private edit marker must occur exactly once"
	);
	*source = after.clone();
	after
}

// A real forwarding chain. Selective imports ensure each edge is semantically
// consumed; inferred return types make the API signature edit propagate.
fn public_edit_sources() -> BTreeMap<String, String> {
	BTreeMap::from([
		("api".into(), "public func value() = 1\n".into()),
		(
			"direct".into(),
			"import @/api with (value)\npublic func direct() = value()\n".into(),
		),
		(
			"transitive".into(),
			"import @/direct with (direct)\npublic func transitive() = direct()\n".into(),
		),
		(
			"main".into(),
			"import @/transitive with (transitive)\npublic func root_value() = transitive()\n".into(),
		),
		("unrelated_a".into(), "public func a(): int = 1\n".into()),
		(
			"unrelated_b".into(),
			"import @/unrelated_a with (a)\npublic func b() = a()\n".into(),
		),
	])
}

fn install_with(sources: BTreeMap<String, String>, session: CompilerSession) -> RetainedState {
	let mut state = RetainedState {
		session,
		project: ProjectId::new("incremental-project-benchmark"),
		entry: ModulePath::new("main").unwrap(),
		target: ModulePath::new("main").unwrap(),
		type_offset: 0,
		sources,
	};
	for (path, source) in &state.sources {
		state.session.set_source(
			state.project.clone(),
			ModulePath::new(path).unwrap(),
			source.clone(),
			SourceVersion(1),
		);
	}
	let marker = "root_value(): int = 0";
	state.type_offset = state.sources["main"]
		.find(marker)
		.map_or(0, |start| start + marker.rfind('0').unwrap());
	state
}

fn install(sources: BTreeMap<String, String>) -> RetainedState {
	// Timed states deliberately have no event callback: callback locking and
	// event allocation must not contaminate Criterion measurements.
	install_with(sources, CompilerSession::new())
}

fn instrumented(sources: BTreeMap<String, String>) -> (RetainedState, Events) {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	let session = CompilerSession::with_detailed_event_callback_for_test(move |event| {
		sink.lock().unwrap().push(event);
	});
	(install_with(sources, session), events)
}

fn request(state: &RetainedState, request: Request) -> usize {
	match request {
		Request::Diagnostics => {
			let diagnostics = state.session.check_project(
				state.project.clone(),
				state.entry.clone(),
				EntryMode::Library,
			);
			assert!(
				diagnostics.is_empty(),
				"benchmark fixture failed: {diagnostics:?}"
			);
			diagnostics.len()
		}
		Request::AnalysisTypeAt => {
			let analysis = state
				.session
				.analyze_module(
					state.project.clone(),
					state.entry.clone(),
					state.target.clone(),
					EntryMode::Library,
				)
				.expect("target must be reachable");
			let ty = analysis.type_at(state.type_offset);
			assert_eq!(ty.as_deref(), Some("int"));
			ty.unwrap().len()
		}
		Request::EmitProject => state
			.session
			.emit_interface_project_for_test(
				state.project.clone(),
				state.entry.clone(),
				EntryMode::Library,
			)
			.unwrap_or_else(|d| panic!("benchmark fixture failed: {d:?}"))
			.module_sources
			.len(),
		Request::FullCompile => state
			.session
			.compile_interface_project_for_test(
				state.project.clone(),
				state.entry.clone(),
				EntryMode::Library,
			)
			.unwrap_or_else(|d| panic!("benchmark fixture failed: {d:?}"))
			.js
			.len(),
	}
}

fn fresh(_: Request) -> RetainedState {
	install(ordinary_sources())
}

fn warm(kind: Request) -> RetainedState {
	let state = install(ordinary_sources());
	black_box(request(&state, kind));
	state
}

fn private_edit() -> RetainedState {
	let mut state = install(ordinary_sources());
	black_box(request(&state, Request::FullCompile));
	let after = apply_private_edit(&mut state.sources);
	state.session.set_source(
		state.project.clone(),
		ModulePath::new(PRIVATE_MODULE).unwrap(),
		after,
		SourceVersion(2),
	);
	state
}

fn fresh_private_post_edit() -> RetainedState {
	install(private_post_edit_sources())
}

fn public_edit() -> RetainedState {
	let mut state = install(public_edit_sources());
	black_box(request(&state, Request::Diagnostics));
	let after = "public func value() = 1.0\n".to_string();
	state.sources.insert("api".into(), after.clone());
	state.session.set_source(
		state.project.clone(),
		ModulePath::new("api").unwrap(),
		after,
		SourceVersion(2),
	);
	state
}

fn clear(events: &Events) {
	events.lock().unwrap().clear();
}

fn scoped_events(events: &[SemanticQueryEvent]) -> impl Iterator<Item = &SemanticQueryEvent> {
	events.iter().filter(|event| event.module.is_some())
}

fn print_events(label: &str, events: &[SemanticQueryEvent]) {
	for (scope, scoped) in [("scoped", true), ("global", false)] {
		let mut summary = BTreeMap::<(String, String, String), usize>::new();
		for event in events
			.iter()
			.filter(|event| event.module.is_some() == scoped)
		{
			let definition = event
				.definition
				.as_ref()
				.map_or_else(|| "-".into(), |id| format!("{id:?}"));
			*summary
				.entry((
					event.query.clone(),
					event.module.clone().unwrap_or_else(|| "<global>".into()),
					definition,
				))
				.or_default() += 1;
		}
		for ((query, module, definition), count) in summary {
			eprintln!(
				"AUDIT {label} scope={scope} query={query} module={module} definition={definition} count={count}"
			);
		}
	}
}

fn audit_preflight() {
	// Warm diagnostics and analysis must be fully backdated.
	for kind in [Request::Diagnostics, Request::AnalysisTypeAt] {
		let (state, events) = instrumented(ordinary_sources());
		request(&state, kind);
		clear(&events);
		request(&state, kind);
		let observed = events.lock().unwrap().clone();
		print_events(&format!("warm-{kind:?}"), &observed);
		assert!(
			observed.is_empty(),
			"warm {kind:?} executed queries: {observed:#?}"
		);
	}

	// The fresh and retained post-edit cases share the exact source constructor,
	// and must produce identical prebundle and bundled output.
	let fresh = install(private_post_edit_sources());
	let mut retained = install(ordinary_sources());
	request(&retained, Request::FullCompile);
	let after = apply_private_edit(&mut retained.sources);
	retained.session.set_source(
		retained.project.clone(),
		ModulePath::new(PRIVATE_MODULE).unwrap(),
		after,
		SourceVersion(2),
	);
	assert_eq!(fresh.sources, retained.sources);
	let fresh_emit = fresh
		.session
		.emit_interface_project_for_test(
			fresh.project.clone(),
			fresh.entry.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let retained_emit = retained
		.session
		.emit_interface_project_for_test(
			retained.project.clone(),
			retained.entry.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(fresh_emit.module_sources, retained_emit.module_sources);
	let fresh_bundle = fresh
		.session
		.compile_interface_project_for_test(
			fresh.project.clone(),
			fresh.entry.clone(),
			EntryMode::Library,
		)
		.unwrap();
	let retained_bundle = retained
		.session
		.compile_interface_project_for_test(
			retained.project.clone(),
			retained.entry.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(fresh_bundle.js, retained_bundle.js);

	// Audit private invalidation using the same exact-definition pattern as the
	// incremental_baseline tests.
	let (mut private, private_events) = instrumented(ordinary_sources());
	request(&private, Request::FullCompile);
	let initial = private_events.lock().unwrap().clone();
	let target_definition = scoped_events(&initial)
		.filter(|event| {
			event.query == "runtime_definition"
				&& event.module.as_deref() == Some(PRIVATE_MODULE)
				&& matches!(
					&event.definition,
					Some(DefinitionId {
						key: DeclarationKey::TopLevel { name, .. },
						..
					}) if name == "private_work"
				)
		})
		.filter_map(|event| event.definition.clone())
		.collect::<HashSet<_>>();
	assert_eq!(
		target_definition.len(),
		1,
		"initial compile must identify exactly one private_work definition: {initial:#?}"
	);
	let target_definition = target_definition.into_iter().next().unwrap();
	let project_modules = private.sources.keys().cloned().collect::<HashSet<_>>();
	clear(&private_events);
	let after = apply_private_edit(&mut private.sources);
	private.session.set_source(
		private.project.clone(),
		ModulePath::new(PRIVATE_MODULE).unwrap(),
		after,
		SourceVersion(2),
	);
	request(&private, Request::FullCompile);
	let edited = private_events.lock().unwrap().clone();
	print_events("private-edit", &edited);
	let analyzed = scoped_events(&edited)
		.filter(|event| event.query == "interface_module_analysis")
		.filter_map(|event| event.module.clone())
		.collect::<BTreeSet<_>>();
	assert_eq!(
		analyzed,
		BTreeSet::from([PRIVATE_MODULE.to_string()]),
		"only the edited private module may rerun analysis: {edited:#?}"
	);
	for query in ["runtime_definition", "lower_runtime_definition"] {
		let rerun = scoped_events(&edited)
			.filter(|event| {
				event.query == query
					&& event
						.module
						.as_ref()
						.is_some_and(|module| project_modules.contains(module))
			})
			.collect::<Vec<_>>();
		assert_eq!(
			rerun.len(),
			1,
			"expected only private_work to rerun {query}: {edited:#?}"
		);
		assert_eq!(rerun[0].definition.as_ref(), Some(&target_definition));
	}

	let (mut public, public_events) = instrumented(public_edit_sources());
	request(&public, Request::Diagnostics);
	clear(&public_events);
	let after = "public func value() = 1.0\n".to_string();
	public.sources.insert("api".into(), after.clone());
	public.session.set_source(
		public.project.clone(),
		ModulePath::new("api").unwrap(),
		after,
		SourceVersion(2),
	);
	request(&public, Request::Diagnostics);
	let observed = public_events.lock().unwrap().clone();
	print_events("public-signature", &observed);
	let rechecked = scoped_events(&observed)
		.filter(|e| e.query == "interface_module_analysis")
		.filter_map(|e| e.module.clone())
		.collect::<BTreeSet<_>>();
	assert_eq!(
		rechecked,
		BTreeSet::from([
			"api".into(),
			"direct".into(),
			"main".into(),
			"transitive".into()
		])
	);
}

fn baseline_compatible_clean_compile(fixture: &GraphFixture) -> usize {
	compile_project_library(fixture.entry(), &|key| fixture.load(key))
		.unwrap_or_else(|diagnostics| panic!("historical fixture failed: {diagnostics:?}"))
		.js
		.len()
}

fn baseline_compatible_clean_check(fixture: &GraphFixture) -> usize {
	let diagnostics = check_project_library(fixture.entry(), &|key| fixture.load(key));
	assert!(
		diagnostics.is_empty(),
		"historical fixture failed: {diagnostics:?}"
	);
	diagnostics.len()
}

fn incremental_project(c: &mut Criterion) {
	audit_preflight();
	let cases: &[(&str, fn() -> RetainedState, Request)] = &[
		(
			"diagnostics/fresh",
			|| fresh(Request::Diagnostics),
			Request::Diagnostics,
		),
		(
			"diagnostics/warm",
			|| warm(Request::Diagnostics),
			Request::Diagnostics,
		),
		(
			"analysis-type-at/fresh",
			|| fresh(Request::AnalysisTypeAt),
			Request::AnalysisTypeAt,
		),
		(
			"analysis-type-at/warm",
			|| warm(Request::AnalysisTypeAt),
			Request::AnalysisTypeAt,
		),
		(
			"emit-project/fresh",
			|| fresh(Request::EmitProject),
			Request::EmitProject,
		),
		(
			"emit-project/warm",
			|| warm(Request::EmitProject),
			Request::EmitProject,
		),
		(
			"full-compile/fresh",
			|| fresh(Request::FullCompile),
			Request::FullCompile,
		),
		(
			"full-compile/warm",
			|| warm(Request::FullCompile),
			Request::FullCompile,
		),
		(
			"private-body/fresh-post-edit",
			fresh_private_post_edit,
			Request::FullCompile,
		),
		(
			"private-body/incremental",
			private_edit,
			Request::FullCompile,
		),
		(
			"public-signature/incremental-diagnostics",
			public_edit,
			Request::Diagnostics,
		),
	];
	let mut group = c.benchmark_group("retained-session");
	for &(label, setup, kind) in cases {
		group.bench_function(BenchmarkId::from_parameter(label), |b| {
			b.iter_batched_ref(
				setup,
				|state| black_box(request(black_box(state), kind)),
				BatchSize::PerIteration,
			);
		});
	}
	group.finish();

	// Preserve the exact historical Mixed 4×4 fixture and stateless facade
	// operation boundary solely for clean-build regression comparison. It is
	// intentionally not an incremental acceptance case.
	let mut historical = c.benchmark_group("baseline-compatible");
	historical.bench_function("mixed-4x4/diagnostics", |b| {
		b.iter_batched(
			|| GraphShape::Mixed { width: 4, depth: 4 }.generate(),
			|fixture| black_box(baseline_compatible_clean_check(black_box(&fixture))),
			BatchSize::SmallInput,
		);
	});
	historical.bench_function("mixed-4x4/full-compile", |b| {
		b.iter_batched(
			|| GraphShape::Mixed { width: 4, depth: 4 }.generate(),
			|fixture| black_box(baseline_compatible_clean_compile(black_box(&fixture))),
			BatchSize::SmallInput,
		);
	});
	historical.finish();
}

criterion_group!(benches, incremental_project);
criterion_main!(benches);
