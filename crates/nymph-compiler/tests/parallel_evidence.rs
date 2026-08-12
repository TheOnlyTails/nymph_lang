#![cfg(feature = "test-support")]

use nymph_compiler::project::{
	CompilerSession, GraphShape, ModulePath, ProjectId, SourceVersion, begin_benchmark_profile,
	finish_benchmark_profile,
};
use nymph_sema::EntryMode;

fn install() -> (CompilerSession, ProjectId, ModulePath) {
	let fixture = GraphShape::Wide { leaves: 16 }.generate();
	let mut session = CompilerSession::new();
	let project = ProjectId::new("prewarm-bound");
	let entry = ModulePath::new(fixture.entry()).unwrap();
	for (path, source) in fixture.sources() {
		session.set_source(
			project.clone(),
			ModulePath::new(path).unwrap(),
			source.clone(),
			SourceVersion(1),
		);
	}
	(session, project, entry)
}

#[test]
fn profiling_preserves_output_and_prewarm_never_exceeds_its_pool() {
	let (plain, project, entry) = install();
	let expected = plain
		.compile_interface_project_for_test(project.clone(), entry.clone(), EntryMode::Library)
		.unwrap();
	let (profiled, project, entry) = install();
	begin_benchmark_profile();
	let actual = profiled
		.compile_interface_project_for_test(project, entry, EntryMode::Library)
		.unwrap();
	let profile = finish_benchmark_profile();
	assert_eq!(actual.js, expected.js);
	assert_eq!(actual.entry_main, expected.entry_main);
	assert_eq!(actual.entry_tag, expected.entry_tag);
	assert!(
		profile.prewarm_configured_workers > 0,
		"profile: {profile:#?}"
	);
	assert!(
		profile.prewarm_configured_workers
			<= std::thread::available_parallelism().map_or(1, usize::from),
		"profile: {profile:#?}"
	);
	assert!(profile.prewarm_max_active > 0, "profile: {profile:#?}");
	assert!(
		profile.prewarm_max_active <= profile.prewarm_configured_workers,
		"profile: {profile:#?}"
	);
	eprintln!(
		"configured_workers={} observed_max_active={}",
		profile.prewarm_configured_workers, profile.prewarm_max_active
	);
}
