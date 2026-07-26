#![cfg(feature = "test-support")]

use nymph_compiler::{
	check_project_library, compile_project_library,
	project::{GraphShape, with_phase_counts},
};

fn assert_baseline(shape: GraphShape) {
	let fixture = shape.generate();
	assert_eq!(fixture.unresolved_imports(), Vec::<String>::new());

	let module_count = fixture.sources().len() as u64;
	let (diags, counts) = with_phase_counts(|| {
		check_project_library(fixture.entry(), &|key| fixture.load(key))
	});
	assert!(diags.is_empty(), "fixture should check cleanly: {diags:?}");
	assert_eq!(counts.graph, 1);
	assert_eq!(counts.rewrite, module_count);
	assert_eq!(counts.check, module_count);
}

#[test]
fn wide_graph_resolves_and_has_stable_phase_counts() {
	assert_baseline(GraphShape::Wide { leaves: 16 });
}

#[test]
fn deep_graph_resolves_and_has_stable_phase_counts() {
	assert_baseline(GraphShape::Deep { depth: 16 });
}

#[test]
fn mixed_graph_resolves_and_has_stable_phase_counts() {
	assert_baseline(GraphShape::Mixed { width: 4, depth: 4 });
}

#[test]
fn generated_sources_compile_to_nonempty_output() {
	for shape in [
		GraphShape::Wide { leaves: 16 },
		GraphShape::Deep { depth: 16 },
		GraphShape::Mixed { width: 4, depth: 4 },
	] {
		let fixture = shape.generate();
		let compiled = compile_project_library(fixture.entry(), &|key| fixture.load(key))
			.unwrap_or_else(|diags| panic!("fixture should compile cleanly: {diags:?}"));
		assert!(!compiled.js.is_empty());
	}
}
