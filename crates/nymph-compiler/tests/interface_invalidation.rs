use std::sync::{Arc, Mutex};

use nymph_compiler::project::{CompilerSession, ModulePath, ProjectId, SourceVersion};
use nymph_sema::{EntryMode, ModuleEnvironment};

fn event_session() -> (CompilerSession, Arc<Mutex<Vec<String>>>) {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	(
		CompilerSession::with_event_callback_and_tombstone_threshold(
			move |name| sink.lock().unwrap().push(name.to_string()),
			256,
		),
		events,
	)
}

#[test]
fn body_only_edit_preserves_compat_interface_value() {
	let (mut session, events) = event_session();
	let project = ProjectId::new("oracle");
	let path = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"public func answer(): int = 1".into(),
		SourceVersion(1),
	);
	let before = session
		.compat_module_interface(
			project.clone(),
			path.clone(),
			path.clone(),
			EntryMode::Library,
		)
		.unwrap();
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		path.clone(),
		"public func answer(): int = 2".into(),
		SourceVersion(2),
	);
	let after = session
		.compat_module_interface(project.clone(), path.clone(), path, EntryMode::Library)
		.unwrap();
	assert_eq!(before, after);
	let observed = events.lock().unwrap();
	assert!(
		observed
			.iter()
			.any(|event| event == "compat_module_analysis")
	);
	assert!(
		observed
			.iter()
			.any(|event| event == "compat_declared_headers")
	);
	assert!(
		observed
			.iter()
			.any(|event| event == "compat_module_environment")
	);
	assert!(
		!observed
			.iter()
			.any(|event| event == "compat_module_interface"),
		"equal environment should backdate the interface producer"
	);
}

#[test]
fn interface_producer_is_backdated_after_body_only_edit() {
	let (mut session, events) = event_session();
	let project = ProjectId::new("backdating");
	let path = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"private func helper(): int = 1\npublic func answer(): int = helper()".into(),
		SourceVersion(1),
	);
	let _ = session.compat_module_interface(
		project.clone(),
		path.clone(),
		path.clone(),
		EntryMode::Library,
	);
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		path.clone(),
		"private func helper(): int = 2\npublic func answer(): int = helper()".into(),
		SourceVersion(2),
	);
	let _ = session.compat_module_interface(
		project.clone(),
		path.clone(),
		path.clone(),
		EntryMode::Library,
	);
	let first = events.lock().unwrap().clone();
	assert!(
		first
			.iter()
			.any(|event| event == "compat_module_environment")
	);
	assert!(!first.iter().any(|event| event == "compat_module_interface"));
	events.lock().unwrap().clear();
	let _ = session.compat_module_interface(project, path.clone(), path, EntryMode::Library);
	assert!(
		events.lock().unwrap().is_empty(),
		"backdated value was recomputed"
	);
}

#[test]
fn recovered_environment_is_not_lowerable() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("recovery");
	let path = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"public func broken(x: Missing): Missing = x".into(),
		SourceVersion(1),
	);
	let environment = session
		.compat_module_environment(
			project.clone(),
			path.clone(),
			path.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert!(matches!(&*environment, ModuleEnvironment::Recovered(_)));
	assert!(
		session
			.compat_environment_is_lowerable(project, path.clone(), path, EntryMode::Library)
			.is_err()
	);
}

#[test]
fn conversion_failure_is_a_separate_internal_diagnostic_and_blocks_lowering() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("conversion");
	let path = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		path.clone(),
		"public type Mystery = _".into(),
		SourceVersion(1),
	);
	let diagnostics = session
		.compat_module_diagnostics(
			project.clone(),
			path.clone(),
			path.clone(),
			EntryMode::Library,
		)
		.unwrap();
	assert_eq!(diagnostics.len(), 1);
	assert_eq!(diagnostics[0].diag.code, "INTERNAL-INTERFACE-CONVERSION");
	assert!(
		session
			.compat_environment_is_lowerable(project, path.clone(), path, EntryMode::Library)
			.is_err()
	);
}

#[test]
fn compiler_builtins_have_private_identity_and_stable_runtime_owners() {
	let session = CompilerSession::new();
	let artifacts = session.builtin_runtime_artifacts();
	assert!(!artifacts.is_empty());
	assert!(
		artifacts
			.iter()
			.all(|artifact| artifact.definition.module.project == "compiler")
	);
	assert!(
		artifacts
			.iter()
			.all(|artifact| !artifact.checked_body.is_empty())
	);
	assert_eq!(
		artifacts,
		CompilerSession::new().builtin_runtime_artifacts()
	);
}
