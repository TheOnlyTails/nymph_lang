#![cfg(feature = "test-support")]

use nymph_compiler::project::{
	CompilerSession, GraphShape, ModulePath, ProjectId, SourceVersion, begin_benchmark_profile,
	finish_benchmark_profile,
};
use nymph_sema::EntryMode;

#[test]
fn diagnostics_prewarm_never_exceeds_its_configured_pool() {
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
	begin_benchmark_profile();
	let diagnostics = session.check_project(project, entry, EntryMode::Library);
	let profile = finish_benchmark_profile();
	assert!(diagnostics.is_empty());
	assert!(
		profile.prewarm_configured_workers > 0,
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
