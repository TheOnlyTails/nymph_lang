use std::sync::Arc;

use nymph_sema::{
	BodyNodeId, EntryMode, ModuleEnvironment, ModuleIdentity, ModuleInterface, ModuleOrigin,
	RecoveredInterfaceType, RecoveredModuleInterface, RuntimeDefinition, RuntimePayload,
	SemanticEnvironment, check_module_with_environment, declared_headers, extract_module_interface,
	recover_module_environment, runtime_definitions,
};

fn identity() -> ModuleIdentity {
	ModuleIdentity {
		origin: ModuleOrigin::Project("stable-contract".into()),
		project: "stable-contract".into(),
		path: "main".into(),
	}
}

fn checked(
	source: &str,
) -> (
	Arc<nymph_ast::decl::Module>,
	nymph_sema::Checked,
	nymph_sema::DeclaredHeaders,
) {
	let parsed = nymph_syntax::parse_module(source, "main.nym");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let module = Arc::new(parsed.tree);
	let environment = SemanticEnvironment::from_modules(identity(), &[]).unwrap();
	let result =
		check_module_with_environment(module.clone(), identity(), &environment, EntryMode::Library);
	let checked = nymph_sema::Checked {
		diags: result.diagnostics.to_vec(),
		facts: result.analysis.checked.as_ref().clone(),
	};
	let headers = declared_headers(identity(), &module);
	(module, checked, headers)
}

fn complete_snapshot(source: &str) -> (ModuleInterface, RuntimeDefinition) {
	let (module, checked, headers) = checked(source);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	let interface = extract_module_interface(identity(), &module, &checked, &headers).unwrap();
	let answer = runtime_definitions(&module, &checked.facts, &interface)
		.unwrap()
		.into_iter()
		.find(|definition| definition.definition == interface.exports[0].id)
		.expect("the exported function has one exact runtime artifact");
	(interface, answer)
}

fn recovered_snapshot(source: &str) -> RecoveredModuleInterface {
	let (module, checked, headers) = checked(source);
	assert!(
		checked
			.diags
			.iter()
			.any(nymph_diagnostics::Diagnostic::is_error)
	);
	let ModuleEnvironment::Recovered(interface) =
		recover_module_environment(identity(), &module, &checked, &headers)
	else {
		panic!("invalid headers must produce the canonical recovered product")
	};
	interface
}

#[test]
fn complete_interface_fingerprint_and_runtime_artifact_are_stable_snapshots() {
	let source = "public func answer(): int = 42";
	let (first_interface, first_runtime) = complete_snapshot(source);
	let (second_interface, second_runtime) = complete_snapshot(source);

	assert_eq!(first_interface, second_interface);
	assert_eq!(first_runtime, second_runtime);
	assert_eq!(first_interface.exports[0].name, "answer");
	assert_eq!(first_interface.fingerprint, 1_079_886_840_365_780_136);
	assert_eq!(
		first_interface.fingerprint,
		first_interface.structural_fingerprint()
	);
	let RuntimePayload::NymphBody(body) = &first_runtime.payload else {
		panic!("answer must retain its checked Nymph body")
	};
	assert_eq!(body.stable.root.id, BodyNodeId(0));

	let (with_unrelated_interface, with_unrelated_runtime) = complete_snapshot(
		"private func unrelated(): int = { let value = 1 value }\npublic func answer(): int = 42",
	);
	assert_ne!(first_interface, with_unrelated_interface);
	assert_ne!(
		first_interface.fingerprint,
		with_unrelated_interface.fingerprint
	);
	assert_eq!(with_unrelated_interface.support_definitions.len(), 1);
	assert_eq!(
		with_unrelated_interface.support_definitions[0]
			.definition
			.name,
		"unrelated"
	);
	assert_eq!(
		first_interface.exports[0].id,
		with_unrelated_interface.exports[0].id
	);
	assert_eq!(first_runtime, with_unrelated_runtime);
}

#[test]
fn recovered_interface_and_fingerprint_are_stable_snapshots() {
	let source = "public func broken(value: Missing): Missing = value\npublic func valid(): int = 1";
	let first = recovered_snapshot(source);
	let second = recovered_snapshot(source);

	assert_eq!(first, second);
	assert_eq!(first.fingerprint, 5_588_967_939_573_553_042);
	assert_eq!(first.fingerprint, first.structural_fingerprint());
	assert_eq!(
		first
			.exports
			.iter()
			.map(|definition| definition.name.as_str())
			.collect::<Vec<_>>(),
		["broken", "valid"]
	);
	assert!(matches!(
		first.exports[0].return_type,
		Some(RecoveredInterfaceType::Poison)
	));
	assert!(matches!(
		first.exports[1].return_type,
		Some(RecoveredInterfaceType::Known(_))
	));
}
