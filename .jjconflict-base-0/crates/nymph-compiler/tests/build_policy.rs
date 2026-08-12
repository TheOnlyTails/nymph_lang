use std::sync::{Arc, Mutex};

use nymph_compiler::{
	BuildProfile, CompilerOptions, CompilerSession, LintLevel, ModulePath, ProjectId, SourceVersion,
};
use nymph_sema::EntryMode;

#[test]
fn profile_defaults_to_development_and_lints_belong_to_the_exact_root_package() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("workspace");
	let root = session.root_package(project.clone());
	let dependency = session.mint_package(project.clone());
	let other_project = ProjectId::new("other-workspace");
	let other_root = session.root_package(other_project.clone());

	assert_eq!(session.build_profile(), BuildProfile::Development);
	session.set_project_lints(
		project.clone(),
		[("echo-in-release".to_string(), LintLevel::Deny)],
	);
	assert_eq!(
		session.lint_level(project.clone(), root, "echo-in-release", LintLevel::Warn),
		LintLevel::Deny
	);
	assert_eq!(
		session.lint_level(
			project.clone(),
			dependency,
			"echo-in-release",
			LintLevel::Warn
		),
		LintLevel::Warn
	);
	assert_eq!(
		session.lint_level(
			other_project,
			other_root,
			"echo-in-release",
			LintLevel::Allow
		),
		LintLevel::Allow
	);
}

#[test]
fn profile_changes_reexecute_only_policy_dependent_diagnostics() {
	let events = Arc::new(Mutex::new(Vec::new()));
	let captured = events.clone();
	let mut session = CompilerSession::with_event_callback_and_tombstone_threshold(
		move |event| captured.lock().unwrap().push(event.to_string()),
		256,
	);
	let project = ProjectId::new("policy-invalidation");
	let module = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		module.clone(),
		"public func value(): int = 1".to_string(),
		SourceVersion(1),
	);
	assert!(
		session
			.check_project(project.clone(), module.clone(), EntryMode::Library)
			.is_empty()
	);
	events.lock().unwrap().clear();

	session.set_build_profile(BuildProfile::Release);
	assert!(
		session
			.check_project(project, module, EntryMode::Library)
			.is_empty()
	);
	let events = events.lock().unwrap();
	assert!(
		events
			.iter()
			.any(|event| event == "policy_project_diagnostics")
	);
	assert!(
		!events
			.iter()
			.any(|event| event == "interface_project_diagnostics")
	);
	assert!(!events.iter().any(|event| event == "project_graph"));
	assert!(!events.iter().any(|event| event == "parse"));
	assert!(
		!events
			.iter()
			.any(|event| event == "interface_module_analysis")
	);
}

#[test]
fn configured_one_shot_and_retained_diagnostics_are_identical_in_both_profiles() {
	let source = "public func value(): Missing = 1";
	for profile in [BuildProfile::Development, BuildProfile::Release] {
		let options = CompilerOptions {
			profile,
			lints: [("echo-in-release".to_string(), LintLevel::Deny)]
				.into_iter()
				.collect(),
		};
		let one_shot = nymph_compiler::check_project_library_with_embedded_std_and_options(
			"main",
			&|module| (module == "main").then(|| source.to_string()),
			&options,
		);
		let mut retained = CompilerSession::new();
		let project = ProjectId::new("retained-parity");
		let module = ModulePath::new("main").unwrap();
		retained.set_build_profile(profile);
		retained.set_project_lints(project.clone(), options.lints.clone());
		retained.set_source(
			project.clone(),
			module.clone(),
			source.to_string(),
			SourceVersion(1),
		);
		let retained = retained.check_project(project, module, EntryMode::Library);

		assert_eq!(one_shot.as_slice(), retained.as_ref());
	}
}

#[test]
fn echo_release_lint_honors_allow_warn_deny_and_exact_root_ownership() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("echo-policy");
	let root = session.root_package(project.clone());
	let dependency = session.mint_package(project.clone());
	session
		.set_package_alias(root, "dep", dependency.clone())
		.unwrap();
	session
		.set_package_source(
			dependency,
			ModulePath::new("lib").unwrap(),
			"public func dep(): int = echo 1".into(),
			SourceVersion(1),
		)
		.unwrap();
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"import dep/lib with (dep)\npublic func root(): int = echo dep()".into(),
		SourceVersion(1),
	);
	assert!(
		session
			.check_project(project.clone(), main.clone(), EntryMode::Library)
			.is_empty()
	);
	session.set_build_profile(BuildProfile::Release);
	let warned = session.check_project(project.clone(), main.clone(), EntryMode::Library);
	assert_eq!(
		warned
			.iter()
			.filter(|diagnostic| diagnostic.diag.code == "echo-in-release")
			.count(),
		1
	);
	assert!(warned.iter().all(|diagnostic| !diagnostic.diag.is_error()));

	session.set_project_lints(
		project.clone(),
		[("echo-in-release".into(), LintLevel::Allow)],
	);
	assert!(
		session
			.check_project(project.clone(), main.clone(), EntryMode::Library)
			.is_empty()
	);
	session.set_project_lints(
		project.clone(),
		[("echo-in-release".into(), LintLevel::Deny)],
	);
	let denied = session.check_project(project, main, EntryMode::Library);
	assert_eq!(denied.len(), 1);
	assert!(denied[0].diag.is_error());
}

#[test]
fn managed_resource_warnings_honor_allow_and_deny() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("managed-policy");
	let module = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		module.clone(),
		"struct Resource\n\
		 impl Close<!()> for Resource { func close(): void = {} }\n\
		 struct Borrowed(resource: Resource)\n\
		 async func risky(): void = {\n\
		   let use resource = Resource()\n\
		   let child = async { resource.close() }.spawn()\n\
		 }"
		.into(),
		SourceVersion(1),
	);
	let warned = session.check_project(project.clone(), module.clone(), EntryMode::Library);
	assert!(
		warned
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "managed-field")
	);
	assert!(
		warned
			.iter()
			.any(|diagnostic| diagnostic.diag.code == "managed-child-capture")
	);

	session.set_project_lints(
		project.clone(),
		[
			("managed-field".into(), LintLevel::Allow),
			("managed-child-capture".into(), LintLevel::Allow),
		],
	);
	assert!(
		session
			.check_project(project.clone(), module.clone(), EntryMode::Library)
			.is_empty()
	);
	session.set_project_lints(
		project.clone(),
		[
			("managed-field".into(), LintLevel::Allow),
			("managed-child-capture".into(), LintLevel::Deny),
		],
	);
	let denied = session.check_project(project, module, EntryMode::Library);
	assert_eq!(denied.len(), 1);
	assert!(denied[0].diag.is_error());
	assert_eq!(denied[0].diag.code, "managed-child-capture");
}

#[test]
fn echo_source_uris_reach_development_output_and_release_removes_all_observer_bytes() {
	let source = "public func observed(): int = echo 1";
	let load = |module: &str| (module == "main").then(|| source.to_string());
	let development =
		nymph_compiler::compile_project_library_with_embedded_std_options_and_source_uris(
			"main",
			&load,
			&CompilerOptions::default(),
			&|module| Some(format!("file:///workspace/{module}.nym")),
		)
		.unwrap();
	assert!(development.js.contains("file:///workspace/main.nym"));
	assert!(development.js.contains("nymphEcho"));

	let release = nymph_compiler::compile_project_library_with_embedded_std_and_options(
		"main",
		&load,
		&CompilerOptions {
			profile: BuildProfile::Release,
			lints: [("echo-in-release".into(), LintLevel::Allow)]
				.into_iter()
				.collect(),
		},
	)
	.unwrap();
	for erased in [
		"nymphEcho",
		"nymphEchoBoxes",
		"nymphEchoStructuralShapes",
		"main.nym",
		"file:///",
	] {
		assert!(!release.js.contains(erased), "release retained {erased}");
	}
}

#[test]
fn a_release_user_binding_named_nymph_echo_does_not_demand_the_observer() {
	let source = "public func nymphEcho(): int = 1\npublic func value(): int = nymphEcho()";
	let load = |module: &str| (module == "main").then(|| source.to_string());
	let release = nymph_compiler::compile_project_library_with_embedded_std_and_options(
		"main",
		&load,
		&CompilerOptions {
			profile: BuildProfile::Release,
			lints: Default::default(),
		},
	)
	.unwrap();
	for observer in [
		"nymphEchoBoxes",
		"nymphEchoStructuralShapes",
		"function nymphEcho(value, site)",
		"<opaque external>",
	] {
		assert!(
			!release.js.contains(observer),
			"release retained {observer}"
		);
	}
}
