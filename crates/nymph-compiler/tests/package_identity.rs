#![cfg(feature = "test-support")]

use nymph_compiler::{CompilerSession, ModulePath, ProjectId, SourceVersion};
use nymph_sema::{EntryMode, ModuleEnvironment};

fn path(value: &str) -> ModulePath {
	ModulePath::new(value).unwrap()
}

fn source(
	session: &mut CompilerSession,
	package: nymph_compiler::PackageId,
	module: &str,
	text: &str,
	version: i64,
) {
	session
		.set_package_source(package, path(module), text.into(), SourceVersion(version))
		.unwrap();
}

fn definition(
	session: &CompilerSession,
	package: nymph_compiler::PackageId,
	entry: &str,
	module: &str,
	name: &str,
) -> nymph_sema::DefinitionId {
	let environment = session
		.package_module_environment_for_test(package, path(entry), path(module), EntryMode::Library)
		.unwrap();
	let ModuleEnvironment::Complete(interface) = environment.as_ref() else {
		panic!("valid package source must have a complete interface")
	};
	interface
		.exports
		.iter()
		.chain(
			interface
				.support_definitions
				.iter()
				.map(|support| &support.definition),
		)
		.find(|definition| definition.name == name)
		.unwrap()
		.id
		.clone()
}

#[test]
fn package_nodes_survive_source_edits_and_aliases_share_only_their_exact_target() {
	let project = ProjectId::new("package-identity");
	let mut session = CompilerSession::without_builtin_sources();
	let root = session.root_package(project.clone());
	let shared = session.mint_package(project.clone());
	session
		.set_package_alias(root.clone(), "left", shared.clone())
		.unwrap();
	session
		.set_package_alias(root.clone(), "right", shared.clone())
		.unwrap();
	source(
		&mut session,
		shared.clone(),
		"types",
		"public struct Value(value: int)",
		1,
	);
	source(
		&mut session,
		root.clone(),
		"main",
		"import left/types as left_types with (Value as Left)\nimport right/types as right_types with (Value as Right)\npublic func same(left: Left, right: Right): boolean = true",
		1,
	);

	let diagnostics = session.check_project(project.clone(), path("main"), EntryMode::Library);
	assert!(diagnostics.is_empty(), "{diagnostics:#?}");
	let before = definition(&session, shared.clone(), "main", "types", "Value");
	source(
		&mut session,
		shared.clone(),
		"types",
		"public struct Value(value: int)\nprivate func helper(): int = 1",
		2,
	);
	let after = definition(&session, shared.clone(), "main", "types", "Value");
	assert_eq!(
		before, after,
		"ordinary source edits preserve exact package and declaration IDs"
	);

	let emitted = session
		.emit_interface_project_for_test(project, path("main"), EntryMode::Library)
		.unwrap();
	assert_eq!(
		emitted
			.module_sources
			.keys()
			.filter(|specifier| specifier.starts_with("package::"))
			.count(),
		1,
		"two aliases to one graph node emit one exact package module"
	);
}

#[test]
fn independent_copies_and_resolution_replacements_have_distinct_exact_owners() {
	let project = ProjectId::new("package-copies");
	let mut session = CompilerSession::without_builtin_sources();
	let root = session.root_package(project.clone());
	let first = session.mint_package(project.clone());
	let untouched = session.mint_package(project.clone());
	for package in [first.clone(), untouched.clone()] {
		source(
			&mut session,
			package,
			"types",
			"public struct Value(value: int)",
			1,
		);
	}
	session
		.set_package_alias(root.clone(), "selected", first.clone())
		.unwrap();
	session
		.set_package_alias(root.clone(), "untouched", untouched.clone())
		.unwrap();
	source(
		&mut session,
		root.clone(),
		"main",
		"import selected/types as selected_types with (Value as Selected)\nimport untouched/types as untouched_types with (Value as Untouched)\npublic func use(a: Selected, b: Untouched): boolean = true",
		1,
	);
	let diagnostics = session.check_project(project.clone(), path("main"), EntryMode::Library);
	assert!(diagnostics.is_empty(), "{diagnostics:#?}");
	let first_id = definition(&session, first, "main", "types", "Value");
	let untouched_id = definition(&session, untouched.clone(), "main", "types", "Value");
	assert_ne!(
		first_id, untouched_id,
		"same name, version, path, and source do not merge copies"
	);
	assert_ne!(
		session
			.binding_name_for_test(
				project.clone(),
				path("main"),
				first_id.clone(),
				EntryMode::Library,
			)
			.unwrap(),
		session
			.binding_name_for_test(
				project.clone(),
				path("main"),
				untouched_id.clone(),
				EntryMode::Library,
			)
			.unwrap(),
		"independent package owners receive distinct canonical bindings"
	);
	for id in [&first_id, &untouched_id] {
		let runtime = session
			.runtime_definition_consumer_for_test(
				project.clone(),
				path("main"),
				id.clone(),
				EntryMode::Library,
			)
			.unwrap();
		assert_eq!(&runtime.source_owner, &id.module);
	}
	let emitted = session
		.emit_interface_project_for_test(project.clone(), path("main"), EntryMode::Library)
		.unwrap();
	assert_eq!(
		emitted
			.module_sources
			.keys()
			.filter(|specifier| specifier.starts_with("package::"))
			.count(),
		2,
		"independent package copies retain distinct canonical module specifiers"
	);

	let replacement = session.mint_package(project.clone());
	source(
		&mut session,
		replacement.clone(),
		"types",
		"public struct Value(value: int)",
		1,
	);
	session
		.set_package_alias(root, "selected", replacement.clone())
		.unwrap();
	let diagnostics = session.check_project(project, path("main"), EntryMode::Library);
	assert!(diagnostics.is_empty(), "{diagnostics:#?}");
	let replacement_id = definition(&session, replacement, "main", "types", "Value");
	let still_untouched = definition(&session, untouched, "main", "types", "Value");
	assert_ne!(first_id, replacement_id);
	assert_eq!(untouched_id, still_untouched);
}

#[test]
fn internal_and_private_access_use_exact_package_and_module_owners() {
	let project = ProjectId::new("package-visibility");
	let mut session = CompilerSession::without_builtin_sources();
	let root = session.root_package(project.clone());
	let dependency = session.mint_package(project.clone());
	session
		.set_package_alias(root.clone(), "dep", dependency.clone())
		.unwrap();
	source(
		&mut session,
		dependency.clone(),
		"types",
		"internal struct Shared(value: int)\nprivate struct Secret(value: int)",
		1,
	);
	source(
		&mut session,
		dependency,
		"consumer",
		"import @/types with (Shared)\npublic func value(input: Shared): int = input.value",
		1,
	);
	source(
		&mut session,
		root,
		"main",
		"import dep/consumer with (value)\nimport dep/types with (Shared, Secret)\npublic func use(): int = value(Shared(value = 1))",
		1,
	);

	let diagnostics = session.check_project(project.clone(), path("main"), EntryMode::Library);
	assert_eq!(
		diagnostics
			.iter()
			.filter(|diagnostic| diagnostic.diag.code == "IMPORT-PRIVATE-NAME")
			.count(),
		2
	);
	let without_prelude =
		session.check_project_without_prelude(project, path("main"), EntryMode::Library);
	assert_eq!(
		without_prelude
			.iter()
			.filter(|diagnostic| diagnostic.diag.code == "IMPORT-PRIVATE-NAME")
			.count(),
		2
	);
}

#[test]
fn struct_field_visibility_uses_public_package_and_module_contexts() {
	let project = ProjectId::new("struct-field-visibility");
	let mut session = CompilerSession::without_builtin_sources();
	let root = session.root_package(project.clone());
	let dependency = session.mint_package(project.clone());
	session
		.set_package_alias(root.clone(), "dep", dependency.clone())
		.unwrap();
	source(
		&mut session,
		dependency.clone(),
		"types",
		"public struct Record(public shown: int, internal shared: int, private secret: int)",
		1,
	);
	source(
		&mut session,
		dependency,
		"consumer",
		"import @/types with (Record)\npublic func shared(record: Record): int = record.shared\npublic func secret(record: Record): int = record.secret",
		1,
	);
	source(
		&mut session,
		root,
		"main",
		"import dep/types with (Record)\npublic func clone(record: Record): Record = Record(...record, shown = 1)\npublic func shared(record: Record): int = record.shared\npublic func fresh(): Record = Record(shown = 1, shared = 2, secret = 3)",
		1,
	);

	let diagnostics = session.check_project(project, path("main"), EntryMode::Library);
	assert_eq!(
		diagnostics
			.iter()
			.filter(|diagnostic| diagnostic
				.diag
				.message
				.contains("not available in this context"))
			.count(),
		3,
		"internal is visible only in-package; private only in its module: {diagnostics:#?}"
	);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic
			.diag
			.message
			.contains("cannot be constructed fresh because it has hidden fields")
	}));
}

#[test]
fn struct_clone_rejects_same_named_owner_from_another_exact_package() {
	let project = ProjectId::new("struct-exact-owner");
	let mut session = CompilerSession::without_builtin_sources();
	let root = session.root_package(project.clone());
	let left = session.mint_package(project.clone());
	let right = session.mint_package(project.clone());
	session
		.set_package_alias(root.clone(), "left", left.clone())
		.unwrap();
	session
		.set_package_alias(root.clone(), "right", right.clone())
		.unwrap();
	for package in [left, right] {
		source(
			&mut session,
			package,
			"types",
			"public struct Box(public value: int)",
			1,
		);
	}
	source(
		&mut session,
		root,
		"main",
		"import left/types with (Box as Left)\nimport right/types with (Box as Right)\nfunc bad(source: Right): Left = Left(...source, value = 1)",
		1,
	);
	let diagnostics = session.check_project(project, path("main"), EntryMode::Library);
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.message.contains("mismatched types"))
	);
}
