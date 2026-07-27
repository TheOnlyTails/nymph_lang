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

#[test]
fn stable_emission_links_demanded_ambient_option_runtime() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-option-runtime");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"func value(): int = match (Some(value = 42)) { Some(value) -> value, None -> 0 }\npublic func main(): void = { let result = value() }"
			.into(),
		SourceVersion(1),
	);

	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("stable emission links the exact demanded Option artifacts");
	assert_eq!(compiled.js.matches("const $m14$Option =").count(), 1);
	assert!(!compiled.js.contains("from \"@nymph/runtime/option\""));
	let js = compiled.js.replace(
		"import { NInt } from \"std/box\";",
		"class NInt { constructor(v) { this.v = v; } }",
	);
	let script = format!("{js}\nconsole.log($m0$value().v);\n");
	let path = std::env::temp_dir().join(format!("nymph-stable-option-{}.mjs", std::process::id()));
	std::fs::write(&path, script).unwrap();
	let output = std::process::Command::new("node")
		.arg(&path)
		.output()
		.unwrap();
	let _ = std::fs::remove_file(path);
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
}

#[test]
fn stable_result_construction_match_and_inherited_default_run() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-result-runtime");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"func ok(): Result<int, string> = Ok(value = 7)\nfunc error(): Result<int, string> = Error(error = \"x\")\nfunc value(result: Result<int, string>): int = match (result) { Ok(value) -> value, Error(...) -> -1 }\nfunc inherited(result: Result<int, string>): int = result.unwrap(9)\npublic func main(): void = {}"
			.into(),
		SourceVersion(1),
	);
	let lowered = session
		.lower_interface_module_for_test(
			project.clone(),
			main.clone(),
			main.clone(),
			EntryMode::Entry,
		)
		.expect("stable Result lowering succeeds");
	assert!(
		lowered.virtual_runtime.iter().any(|fragment| matches!(
			fragment.fragment.fragment(),
			nymph_sema::LoweredHirFragment::EnumShell(_)
		)),
		"{:#?}",
		lowered.virtual_runtime
	);

	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("stable emission links Result and its inherited Unwrap implementation");
	assert_eq!(
		compiled
			.js
			.matches("//#region @nymph/runtime/result")
			.count(),
		1,
		"{}",
		compiled.js
	);
	assert_eq!(
		compiled
			.js
			.matches("Symbol.for(\"$m16$Result.Ok\")")
			.count(),
		3,
		"{}",
		compiled.js
	);
	assert_eq!(compiled.js.matches("unwrap(").count(), 2, "{}", compiled.js);
	assert!(
		!compiled.js.contains("@nymph/runtime/option"),
		"{}",
		compiled.js
	);
	let js = compiled.js.replace(
		"import { NInt, NString } from \"std/box\";",
		"class NInt { constructor(v) { this.v = v; } } class NString { constructor(v) { this.v = v; } }",
	);
	let script = format!("{js}\nconsole.log($m0$value($m0$ok()).v, $m0$inherited($m0$error()).v);\n");
	let path = std::env::temp_dir().join(format!("nymph-stable-result-{}.mjs", std::process::id()));
	std::fs::write(&path, script).unwrap();
	let output = std::process::Command::new("node")
		.arg(&path)
		.output()
		.unwrap();
	let _ = std::fs::remove_file(path);
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(String::from_utf8_lossy(&output.stdout), "7 9\n");
}

#[test]
fn stable_native_list_runtime_is_exact_collision_safe_and_runs_after_dependency_warmup() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-list-runtime");
	let main = ModulePath::new("main").unwrap();
	let dependency = ModulePath::new("collections/list").unwrap();
	session.set_source(
		project.clone(),
		dependency.clone(),
		"public func values(): mut #[int] = #[1, 2, 3]".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/collections/list with (values)\nfunc exercise(): int = {\n  let mut items = values()\n  let before = match (items.get(1u)) { Some(value) -> value, None -> 0 }\n  items[1] = 7\n  let mut total = 0\n  for (item in items) { total = total + item }\n  before + items[1] + items.length() + total\n}\npublic func main(): void = {}"
			.into(),
		SourceVersion(1),
	);

	// Warm the dependency's exact runtime artifacts, then keep the dependency
	// AST/analysis guard active through stable assembly, emission, and bundling.
	session
		.lower_interface_module_for_test(project.clone(), main.clone(), dependency, EntryMode::Entry)
		.expect("dependency stable runtime facts warm successfully");
	session.panic_on_dependency_body_access_for_test(project.clone(), main.clone());
	let emitted = session
		.emit_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.expect("stable List modules emit");
	assert_eq!(
		emitted.module_sources["main"]
			.matches("from \"@nymph/runtime/collections/list\"")
			.count(),
		1
	);
	assert_eq!(
		emitted.module_sources["main"]
			.matches("from \"@nymph/runtime/option\"")
			.count(),
		1
	);
	assert_eq!(
		emitted.module_sources["main"]
			.matches("from \"@nymph/runtime/iter/iterable\"")
			.count(),
		1
	);
	assert!(emitted.module_sources.contains_key("collections/list"));
	assert!(
		emitted
			.module_sources
			.contains_key("@nymph/runtime/collections/list")
	);
	assert!(emitted.module_sources.contains_key("@nymph/runtime/option"));
	assert!(
		emitted
			.module_sources
			.contains_key("@nymph/runtime/iter/iterable")
	);
	assert!(
		!emitted
			.module_sources
			.contains_key("@nymph/runtime/collections/map")
	);
	assert!(!emitted.module_sources.contains_key("@nymph/runtime/result"));
	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("stable emission links only the native List runtime closure");

	assert_eq!(
		compiled
			.js
			.matches("//#region @nymph/runtime/option")
			.count(),
		1,
		"{}",
		compiled.js
	);
	assert_eq!(
		compiled.js.matches("//#region collections/list").count(),
		1,
		"{}",
		compiled.js
	);
	assert!(
		!compiled.js.contains("@nymph/runtime/collections/map"),
		"{}",
		compiled.js
	);
	assert!(
		!compiled.js.contains("@nymph/runtime/result"),
		"{}",
		compiled.js
	);

	let js = compiled.js.replace(
		"import { get as $m7$list$get$1, length as $m7$list$length } from \"std/collections/list\";",
		"const $m7$list$get = (xs, i) => i.v < xs.v.length ? { [Symbol.for('nymph.tag')]: Symbol.for('$m15$Option.Some'), value: xs.v[i.v] } : { [Symbol.for('nymph.tag')]: Symbol.for('$m15$Option.None') }; const $m7$list$get$1 = $m7$list$get; const $m7$list$length = (xs) => new NUint(xs.v.length);",
	).replace(
		"import { NBool, NInt, NList, NUint } from \"std/box\";",
		"class NBool { constructor(v) { this.v = v; } } class NInt { constructor(v) { this.v = v; } } class NUint { constructor(v) { this.v = v; } } class NList { constructor(v) { this.v = v; } index(i) { return this.v[i.v]; } } const $m15$Option = { Some: ({ value }) => ({ [Symbol.for('nymph.tag')]: Symbol.for('$m15$Option.Some'), value }), None: { [Symbol.for('nymph.tag')]: Symbol.for('$m15$Option.None') } };",
	);
	let exercise = compiled.entry_symbol("exercise");
	let script = format!("{js}\nconsole.log({exercise}().v);\n");
	let path = std::env::temp_dir().join(format!("nymph-stable-list-{}.mjs", std::process::id()));
	std::fs::write(&path, script).unwrap();
	let output = std::process::Command::new("node")
		.arg(&path)
		.output()
		.unwrap();
	let _ = std::fs::remove_file(path);
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(String::from_utf8_lossy(&output.stdout), "23\n");
}

#[test]
fn stable_emission_links_exact_ambient_math_demands_once_and_runs() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-math-runtime");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"func constant(): float = pi\nfunc root(): float = (16).sqrt()\nfunc power(): float = 16 ** 0.5\npublic func main(): void = {}"
			.into(),
		SourceVersion(1),
	);

	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("stable emission links the exact demanded ambient math artifacts");
	assert_eq!(
		compiled.js.matches("3.141592653589793").count(),
		1,
		"{}",
		compiled.js
	);
	assert_eq!(
		compiled.js.matches("function $m12$int$sqrt(").count(),
		1,
		"{}",
		compiled.js
	);
	assert!(
		compiled.js.contains("$m12$int$sqrt(new NInt(16))"),
		"{}",
		compiled.js
	);
	assert_eq!(
		compiled.js.matches(" ** new NFloat(.5).v").count(),
		2,
		"{}",
		compiled.js
	);
	let js = compiled.js.replace(
		"import { NFloat, NInt } from \"std/box\";",
		"class NFloat { constructor(v) { this.v = v; } } class NInt { constructor(v) { this.v = v; } }",
	);
	let script = format!("{js}\nconsole.log($m0$constant().v, $m0$root().v, $m0$power().v);\n");
	let path = std::env::temp_dir().join(format!("nymph-stable-math-{}.mjs", std::process::id()));
	std::fs::write(&path, script).unwrap();
	let output = std::process::Command::new("node")
		.arg(&path)
		.output()
		.unwrap();
	let _ = std::fs::remove_file(path);
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(
		String::from_utf8_lossy(&output.stdout),
		"3.141592653589793 4 4\n"
	);
}

#[test]
fn primitive_extension_bindings_do_not_collide_between_int_and_float() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("primitive-extension-collision");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"impl int { func identity(): int = this }\nimpl float { func identity(): float = this }\nfunc integer(): int = (1).identity()\nfunc decimal(): float = (1.5).identity()"
			.into(),
		SourceVersion(1),
	);

	let module = session
		.lower_interface_module_for_test(project, main.clone(), main, EntryMode::Library)
		.expect("primitive extensions lower without owner shells or binding collisions");
	let helpers = module
		.hir
		.funcs
		.iter()
		.filter(|function| function.name.contains("identity"))
		.collect::<Vec<_>>();
	assert_eq!(helpers.len(), 2);
	assert_ne!(helpers[0].name, helpers[1].name);
	assert!(helpers.iter().all(|function| function.params[0] == "$self"));
}
