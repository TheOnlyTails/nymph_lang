#![cfg(feature = "test-support")]

use nymph_compiler::project::{CompilerSession, ModulePath, ProjectId, SourceVersion};
use nymph_sema::EntryMode;

#[test]
fn assembles_source_order_shells_values_functions_and_members() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("assembly");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"struct Point(x: int) { func get(): int = this.x }\nlet answer = 42\nfunc read(): int = answer"
			.into(),
		SourceVersion(1),
	);
	let module = session
		.lower_interface_module_for_test(project, main.clone(), main, EntryMode::Library)
		.expect("stable module assembly succeeds");
	assert_eq!(module.hir.classes.len(), 1);
	assert_eq!(module.hir.classes[0].methods.len(), 1);
	assert_eq!(module.hir.lets.len(), 1);
	assert_eq!(module.hir.funcs.len(), 1);
	assert_eq!(module.own_definitions.len(), 4);
}

#[test]
fn demand_closure_is_iterative_and_deduplicates_mutual_recursion() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("recursive-assembly");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"func even(n: int): boolean = if (n == 0) { true } else { odd(n - 1) }\nfunc odd(n: int): boolean = if (n == 0) { false } else { even(n - 1) }".into(),
		SourceVersion(1),
	);
	let module = session
		.lower_interface_module_for_test(project, main.clone(), main, EntryMode::Library)
		.expect("mutual recursion must not create a Salsa cycle");
	assert_eq!(module.fragments.len(), 2);
}

#[test]
fn stable_emission_links_exact_project_modules_and_bundles() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("stable-emission");
	let main = ModulePath::new("main").unwrap();
	let helper = ModulePath::new("helper").unwrap();
	session.set_source(
		project.clone(),
		helper,
		"public func value(): int = 42".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/helper with (value)\npublic func main(): void = { let result = value() }".into(),
		SourceVersion(1),
	);

	let emitted = session
		.emit_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.expect("stable project emission succeeds");
	assert_eq!(emitted.entry_tag, 1);
	assert!(emitted.module_sources["main"].contains("import { $m0$value } from \"helper\";"));
	assert!(emitted.module_sources["helper"].contains("export { $m0$value };"));

	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("stable project bundling succeeds");
	assert_eq!(compiled.entry_main, "main");
	assert_eq!(compiled.entry_symbol("value"), "$m1$value");
	assert!(compiled.js.contains("42"), "{}", compiled.js);
}
