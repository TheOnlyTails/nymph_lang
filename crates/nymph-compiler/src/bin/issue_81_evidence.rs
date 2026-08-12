use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;
use std::process::Command;
use std::time::{Duration, Instant};

use nymph_compiler::project::{
	BenchmarkProfile, CompilerSession, GraphShape, ModulePath, ProjectId, SourceVersion,
	begin_benchmark_profile, finish_benchmark_profile,
};
use nymph_sema::EntryMode;
use serde_json::{Value, json};

#[derive(Clone, Copy)]
enum Request {
	Diagnostics,
	Compile,
}

fn shape(name: &str) -> GraphShape {
	match name {
		"single" => GraphShape::Single,
		"wide" => GraphShape::Wide { leaves: 16 },
		"deep" => GraphShape::Deep { depth: 16 },
		"mixed" => GraphShape::Mixed { width: 4, depth: 4 },
		_ => panic!("unknown shape {name:?}; expected single|wide|deep|mixed"),
	}
}

fn install(
	shape: GraphShape,
) -> (
	CompilerSession,
	ProjectId,
	ModulePath,
	BTreeMap<String, String>,
) {
	let fixture = shape.generate();
	let sources = fixture.sources().clone();
	let mut session = CompilerSession::new();
	let project = ProjectId::new("issue-81-evidence");
	let entry = ModulePath::new(fixture.entry()).unwrap();
	for (path, source) in &sources {
		session.set_source(
			project.clone(),
			ModulePath::new(path).unwrap(),
			source.clone(),
			SourceVersion(1),
		);
	}
	(session, project, entry, sources)
}

fn request(
	session: &CompilerSession,
	project: &ProjectId,
	entry: &ModulePath,
	kind: Request,
) -> usize {
	match kind {
		Request::Diagnostics => {
			let diagnostics = session.check_project(project.clone(), entry.clone(), EntryMode::Library);
			assert!(
				diagnostics.is_empty(),
				"fixture diagnostics: {diagnostics:#?}"
			);
			diagnostics.len()
		}
		Request::Compile => session
			.compile_interface_project_for_test(project.clone(), entry.clone(), EntryMode::Library)
			.unwrap_or_else(|diagnostics| panic!("fixture diagnostics: {diagnostics:#?}"))
			.js
			.len(),
	}
}

fn profile_json(profile: BenchmarkProfile) -> Value {
	json!({
		"phases": profile.phases.into_iter().map(|phase| json!({
			"name": phase.name,
			"inclusive_ns": phase.inclusive_nanos,
			"executions": phase.executions,
		})).collect::<Vec<_>>(),
		"prewarm_configured_workers": profile.prewarm_configured_workers,
		"prewarm_max_active": profile.prewarm_max_active,
	})
}

fn sample(shape_name: &str, request_name: &str, instrumented: bool) {
	let kind = match request_name {
		"diagnostics" => Request::Diagnostics,
		"compile" => Request::Compile,
		_ => panic!("unknown request {request_name:?}; expected diagnostics|compile"),
	};
	let (session, project, entry, _) = install(shape(shape_name));
	if instrumented {
		begin_benchmark_profile();
	}
	let cold_started = Instant::now();
	black_box(request(&session, &project, &entry, kind));
	let cold_ns = cold_started.elapsed().as_nanos() as u64;
	let profile = instrumented.then(finish_benchmark_profile);

	let warm_started = Instant::now();
	let mut warm_iterations = 0_u64;
	while warm_iterations < 10_000 || warm_started.elapsed() < Duration::from_millis(200) {
		for _ in 0..1_000 {
			black_box(request(&session, &project, &entry, kind));
		}
		warm_iterations += 1_000;
	}
	let warm_total_ns = warm_started.elapsed().as_nanos() as u64;
	println!(
		"{}",
		json!({
			"kind": "sample",
			"shape": shape_name,
			"request": request_name,
			"instrumented": instrumented,
			"rayon_workers": std::env::var("RAYON_NUM_THREADS").expect("RAYON_NUM_THREADS is required").parse::<usize>().unwrap(),
			"cold_wall_ns": cold_ns,
			"warm_iterations": warm_iterations,
			"warm_total_ns": warm_total_ns,
			"warm_ns_per_iteration": warm_total_ns as f64 / warm_iterations as f64,
			"profile": profile.map(profile_json),
		})
	);
}

fn sorted_diagnostics(
	session: &CompilerSession,
	project: &ProjectId,
	entry: &ModulePath,
) -> Vec<String> {
	let mut diagnostics = session
		.check_project(project.clone(), entry.clone(), EntryMode::Library)
		.iter()
		.map(|item| format!("{}::{:?}", item.module, item.diag))
		.collect::<Vec<_>>();
	diagnostics.sort();
	diagnostics
}

fn snapshot(shape_name: &str) {
	let (session, project, entry, sources) = install(shape(shape_name));
	let diagnostics = sorted_diagnostics(&session, &project, &entry);
	let graph_order = session
		.graph_order(project.clone(), entry.clone(), EntryMode::Library)
		.into_iter()
		.map(|path| path.as_str().to_string())
		.collect::<Vec<_>>();
	let mut definitions = BTreeSet::new();
	for path in sources.keys() {
		for artifact in session
			.runtime_definitions_for_test(
				project.clone(),
				entry.clone(),
				ModulePath::new(path).unwrap(),
				EntryMode::Library,
			)
			.expect("reachable fixture module")
		{
			definitions.insert(format!("{:?}", artifact.definition));
		}
	}
	let emitted = session
		.emit_interface_project_for_test(project.clone(), entry.clone(), EntryMode::Library)
		.unwrap();
	let module_sources = emitted
		.module_sources
		.iter()
		.map(|(module, source)| (module.clone(), source.clone()))
		.collect::<BTreeMap<_, _>>();
	let compiled = session
		.compile_interface_project_for_test(project, entry, EntryMode::Library)
		.unwrap();
	let invocation = format!(
		"{}\nconsole.log({}());\n",
		compiled.js,
		compiled.entry_symbol("root_value")
	);
	let node = Command::new("node")
		.arg("-e")
		.arg(invocation)
		.output()
		.expect("run Node");
	assert!(
		node.status.success(),
		"Node stderr: {}",
		String::from_utf8_lossy(&node.stderr)
	);
	println!(
		"{}",
		json!({
			"kind": "snapshot",
			"shape": shape_name,
			"diagnostics_sorted": diagnostics,
			"graph_order": graph_order,
			"module_order": module_sources.keys().collect::<Vec<_>>(),
			"stable_definition_ids": definitions,
			"module_sources": module_sources,
			"final_js_blake3": blake3::hash(compiled.js.as_bytes()).to_hex().to_string(),
			"node_stdout": String::from_utf8(node.stdout).unwrap(),
		})
	);
}

fn main() {
	let args = std::env::args().skip(1).collect::<Vec<_>>();
	match args.as_slice() {
		[command, shape, request, instrumentation] if command == "sample" => {
			sample(shape, request, instrumentation == "instrumented");
		}
		[command, shape] if command == "snapshot" => snapshot(shape),
		_ => panic!(
			"usage: issue_81_evidence sample SHAPE diagnostics|compile instrumented|uninstrumented\n       issue_81_evidence snapshot SHAPE"
		),
	}
}
