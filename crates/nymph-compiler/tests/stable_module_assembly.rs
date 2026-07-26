#![cfg(feature = "test-support")]

use std::sync::{Arc, Mutex};

use nymph_compiler::project::{
	CompilerSession, ModulePath, ProjectId, SemanticQueryEvent, SourceVersion,
};
use nymph_sema::{
	DeclarationCategory, DeclarationKey, DefinitionId, EntryMode, ModuleIdentity, ModuleOrigin,
};

fn event_session() -> (CompilerSession, Arc<Mutex<Vec<SemanticQueryEvent>>>) {
	let events = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	(
		CompilerSession::with_detailed_event_callback_for_test(move |event| {
			sink.lock().unwrap().push(event)
		}),
		events,
	)
}

fn executed(events: &[SemanticQueryEvent], query: &str, module: Option<&str>) -> bool {
	events.iter().any(|event| {
		event.query == query && module.is_none_or(|module| event.module.as_deref() == Some(module))
	})
}

#[test]
fn member_names_reserve_only_exact_ambient_display_and_debug_slots() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("protocol-member-names");
	let main = ModulePath::new("main").unwrap();
	let unrelated_module = ModulePath::new("unrelated").unwrap();
	session.set_source(
		project.clone(),
		unrelated_module,
		"interface Display { func display(): string }\ninterface Debug { func debug(): string }".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"public func main(): void = {}".into(),
		SourceVersion(1),
	);
	let member = |module: ModuleIdentity, interface: &str, name: &str| {
		let owner = DefinitionId::new(
			module.clone(),
			DeclarationKey::top_level(DeclarationCategory::Interface, interface),
		);
		DefinitionId::new(
			module,
			DeclarationKey::member(owner, DeclarationCategory::Method, name),
		)
	};
	let ambient = ModuleIdentity {
		origin: ModuleOrigin::Compiler,
		project: "compiler".into(),
		path: "ops".into(),
	};
	let unrelated = ModuleIdentity {
		origin: ModuleOrigin::Project("protocol-member-names".into()),
		project: "protocol-member-names".into(),
		path: "unrelated".into(),
	};
	for (definition, expected) in [
		(
			member(ambient.clone(), "Display", "display"),
			"$nymph$display",
		),
		(member(ambient, "Debug", "debug"), "$nymph$debug"),
		(member(unrelated.clone(), "Display", "display"), "display"),
		(member(unrelated, "Debug", "debug"), "debug"),
	] {
		assert_eq!(
			session
				.member_name_for_test(
					project.clone(),
					main.clone(),
					definition,
					EntryMode::Library
				)
				.unwrap()
				.as_str(),
			expected
		);
	}
}

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
fn body_edit_reexecutes_only_the_changed_modules_stable_chain() {
	let (mut session, events) = event_session();
	let project = ProjectId::new("stable-emission-invalidation");
	let main = ModulePath::new("main").unwrap();
	let sibling = ModulePath::new("sibling").unwrap();
	session.set_source(
		project.clone(),
		sibling,
		"public func sibling(): int = 7".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/sibling with (sibling)\nfunc answer(): int = 41\npublic func main(): void = { let result = answer() + sibling() }".into(),
		SourceVersion(1),
	);
	session
		.compile_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.expect("initial stable compilation succeeds");
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/sibling with (sibling)\nfunc answer(): int = 42\npublic func main(): void = { let result = answer() + sibling() }".into(),
		SourceVersion(2),
	);
	session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("stable recompilation succeeds");
	let observed = events.lock().unwrap();
	for query in [
		"runtime_definition_index",
		"runtime_definition_ids",
		"lower_runtime_definition",
		"lower_interface_module",
		"emitted_interface_module",
	] {
		assert!(
			executed(&observed, query, Some("main")),
			"{query}: {observed:#?}"
		);
		assert!(
			!executed(&observed, query, Some("sibling")),
			"{query}: {observed:#?}"
		);
	}
	assert!(executed(&observed, "emitted_interface_project", None));
	assert!(executed(&observed, "compiled_interface_project", None));
}

#[test]
fn recompiling_without_edits_executes_no_stable_query_bodies() {
	let (mut session, events) = event_session();
	let project = ProjectId::new("stable-emission-clean");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"public func main(): void = {}".into(),
		SourceVersion(1),
	);
	session
		.compile_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.unwrap();
	events.lock().unwrap().clear();
	session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.unwrap();
	let observed = events.lock().unwrap();
	assert!(
		observed.is_empty(),
		"a clean stable compile should be fully memoized: {observed:#?}"
	);
}

#[test]
fn failed_semantic_checking_prevents_stable_lowering_emission_and_bundling() {
	let (mut session, events) = event_session();
	let project = ProjectId::new("stable-emission-errors");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"public func main(): int = missing".into(),
		SourceVersion(1),
	);
	assert!(
		session
			.compile_interface_project_for_test(project, main, EntryMode::Entry)
			.is_err()
	);
	let observed = events.lock().unwrap();
	for query in [
		"lower_interface_module",
		"emitted_interface_module",
		"emitted_interface_project",
		"compiled_interface_project",
	] {
		assert!(!executed(&observed, query, None), "{query}: {observed:#?}");
	}
}

#[test]
fn equal_emitted_module_stops_project_and_bundle_invalidation() {
	let (mut session, events) = event_session();
	let project = ProjectId::new("stable-emission-backdating");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"func unused(value: int): void = {}\npublic func main(): void = {}".into(),
		SourceVersion(1),
	);
	let before = session
		.compile_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.unwrap();
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		main.clone(),
		"func unused(value: float): void = {}\npublic func main(): void = {}".into(),
		SourceVersion(2),
	);
	let after = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.unwrap();
	assert_eq!(before.js, after.js);
	let observed = events.lock().unwrap();
	assert!(
		executed(&observed, "emitted_interface_module", Some("main")),
		"{observed:#?}"
	);
	assert!(
		!executed(&observed, "emitted_interface_project", None),
		"{observed:#?}"
	);
	assert!(
		!executed(&observed, "compiled_interface_project", None),
		"{observed:#?}"
	);
}

#[test]
fn stable_importable_std_identity_is_distinct_from_colliding_project_names_and_runs() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-importable-identity");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"import std/collections/tree with (Tree as StdTree)\nstruct Option(value: int)\nstruct List(value: int)\nstruct Range(value: int)\nstruct Tree(value: int)\nlet pi = 42\nfunc answer(): int = { let local = Option(value = pi) let foreign = StdTree.Leaf(value = local.value) match (foreign) { Leaf(value) -> value, Node(...) -> 0 } }\npublic func main(): void = {}".into(),
		SourceVersion(1),
	);

	let emitted = session
		.emit_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.expect("stable identities keep project declarations separate from std and ambient owners");
	let source = &emitted.module_sources["main"];
	assert_eq!(
		source.matches("from \"collections/tree\"").count(),
		1,
		"{source}"
	);
	assert!(emitted.module_sources.contains_key("collections/tree"));
	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("stable importable std module bundles");
	let answer = compiled.entry_symbol("answer");
	let js = compiled.js.replace(
		"import { NInt } from \"std/box\";",
		"class NInt { constructor(v) { this.v = v; } }",
	);
	let path = std::env::temp_dir().join(format!(
		"nymph-stable-importable-{}.mjs",
		std::process::id()
	));
	std::fs::write(&path, format!("{js}\nconsole.log({answer}().v);\n")).unwrap();
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
	let list_imports = emitted.module_sources["main"]
		.lines()
		.filter(|line| line.ends_with("from \"@nymph/runtime/collections/list\";"))
		.collect::<Vec<_>>();
	assert!(!list_imports.is_empty());
	assert_eq!(
		list_imports.len(),
		list_imports
			.iter()
			.copied()
			.collect::<std::collections::HashSet<_>>()
			.len()
	);
	assert_eq!(
		emitted.module_sources["main"]
			.matches("from \"@nymph/runtime/option\"")
			.count(),
		1
	);
	let iterable_imports = emitted.module_sources["main"]
		.lines()
		.filter(|line| line.ends_with("from \"@nymph/runtime/iter/iterable\";"))
		.collect::<Vec<_>>();
	assert!(!iterable_imports.is_empty());
	assert_eq!(
		iterable_imports.len(),
		iterable_imports
			.iter()
			.copied()
			.collect::<std::collections::HashSet<_>>()
			.len()
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
fn stable_native_map_runtime_is_exact_collision_safe_and_runs_after_dependency_warmup() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-map-runtime");
	let main = ModulePath::new("main").unwrap();
	let dependency = ModulePath::new("collections/map").unwrap();
	session.set_source(
		project.clone(),
		dependency.clone(),
		"public func values(): mut #{int: int} = #{1: 10, 2: 20}".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/collections/map with (values)\nfunc exercise(): int = {\n  let mut items = values()\n  let before = match (items.get(1)) { Some(value) -> value, None -> 0 }\n  let indexed = items[2]\n  items.insert(3, 30)\n  items.insert(2, 7)\n  items[3] = 4\n  let mut total = 0\n  for (#(key, value) in items) { total = total + key + value }\n  before + indexed + items[2] + items[3] + items.size() + total\n}\npublic func main(): void = {}"
			.into(),
		SourceVersion(1),
	);

	session
		.lower_interface_module_for_test(project.clone(), main.clone(), dependency, EntryMode::Entry)
		.expect("dependency stable runtime facts warm successfully");
	session.panic_on_dependency_body_access_for_test(project.clone(), main.clone());
	let emitted = session
		.emit_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.expect("stable Map modules emit");
	let map_imports = emitted.module_sources["main"]
		.lines()
		.filter(|line| line.ends_with("from \"@nymph/runtime/collections/map\";"))
		.collect::<Vec<_>>();
	assert!(!map_imports.is_empty());
	assert_eq!(
		map_imports.len(),
		map_imports
			.iter()
			.copied()
			.collect::<std::collections::HashSet<_>>()
			.len()
	);
	assert_eq!(
		emitted.module_sources["main"]
			.matches("from \"@nymph/runtime/option\"")
			.count(),
		1
	);
	let iterable_imports = emitted.module_sources["main"]
		.lines()
		.filter(|line| line.ends_with("from \"@nymph/runtime/iter/iterable\";"))
		.collect::<Vec<_>>();
	assert!(!iterable_imports.is_empty());
	assert_eq!(
		iterable_imports.len(),
		iterable_imports
			.iter()
			.copied()
			.collect::<std::collections::HashSet<_>>()
			.len()
	);
	assert!(emitted.module_sources.contains_key("collections/map"));
	assert!(
		emitted
			.module_sources
			.contains_key("@nymph/runtime/collections/map")
	);
	assert!(emitted.module_sources.contains_key("@nymph/runtime/option"));
	assert!(
		emitted
			.module_sources
			.contains_key("@nymph/runtime/iter/iterable")
	);
	assert!(!emitted.module_sources.contains_key("@nymph/runtime/result"));
	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("stable emission links only the native Map runtime closure");
	assert_eq!(
		compiled.js.matches("//#region std/collections/map").count(),
		1,
		"{}",
		compiled.js
	);
	assert_eq!(
		compiled
			.js
			.matches("//#region @nymph/runtime/option")
			.count(),
		1,
		"{}",
		compiled.js
	);
	assert_eq!(compiled.js.matches("//#region collections/map").count(), 1);
	assert!(!compiled.js.contains("collections/list"), "{}", compiled.js);
	assert!(
		!compiled.js.contains("@nymph/runtime/result"),
		"{}",
		compiled.js
	);

	let exercise = compiled.entry_symbol("exercise");
	let script = format!("{}\nconsole.log({exercise}().v);\n", compiled.js);
	let path = std::env::temp_dir().join(format!("nymph-stable-map-{}.mjs", std::process::id()));
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
	assert_eq!(String::from_utf8_lossy(&output.stdout), "71\n");
}

#[test]
fn stable_native_range_runtime_is_exact_collision_safe_and_runs_after_dependency_warmup() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-range-runtime");
	let main = ModulePath::new("main").unwrap();
	let dependency = ModulePath::new("ranges/source").unwrap();
	session.set_source(
		project.clone(),
		dependency.clone(),
		"public func int_start(): int = 1\npublic func uint_start(): uint = 2u".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/ranges/source with (int_start, uint_start)\ninterface Plus<Other, Output> { func plus(other: Other): Output func plus_default(other: Other): Output = this.plus(other) }\nstruct Box<T>(value: T)\nimpl<T> Plus<Other = Box<T>, Output = T> for Box<T> { func plus(other: Box<T>): T = other.value }\nfunc exercise(): int = {\n  let mut total = 0\n  for (value in int_start()..4) { total = total + value }\n  for (value in int_start()..=3) { total = total + value }\n  for (value in uint_start()..5u) { total = total + (value as int) }\n  for (value in uint_start()..=4u) { total = total + (value as int) }\n  Box(value = 0).plus_default(Box(value = total + 1))\n}\npublic func main(): void = {}"
			.into(),
		SourceVersion(1),
	);

	session
		.lower_interface_module_for_test(project.clone(), main.clone(), dependency, EntryMode::Entry)
		.expect("dependency stable runtime facts warm successfully");
	session.panic_on_dependency_body_access_for_test(project.clone(), main.clone());
	let emitted = session
		.emit_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.expect("stable Range and operator modules emit");
	let source = &emitted.module_sources["main"];
	assert_eq!(source.matches("from \"std/box\"").count(), 1);
	assert_eq!(source.matches("new NymphRange").count(), 4);
	assert!(!source.contains("@nymph/runtime/option"));
	assert!(!source.contains("@nymph/runtime/iter"));
	assert!(!source.contains("@nymph/runtime/collections/list"));
	assert!(!source.contains("@nymph/runtime/collections/map"));
	assert!(!source.contains("@nymph/runtime/result"));
	assert!(emitted.module_sources.contains_key("ranges/source"));
	assert!(!emitted.module_sources.contains_key("@nymph/runtime/option"));
	assert!(!emitted.module_sources.contains_key("@nymph/runtime/iter"));
	assert!(
		!emitted
			.module_sources
			.contains_key("@nymph/runtime/collections/list")
	);
	assert!(
		!emitted
			.module_sources
			.contains_key("@nymph/runtime/collections/map")
	);
	assert!(!emitted.module_sources.contains_key("@nymph/runtime/result"));

	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("stable emission links only the Range, iterator, Option, and operator closure");
	assert_eq!(
		compiled.js.matches("NymphRange").count(),
		5,
		"{}",
		compiled.js
	);
	assert!(!compiled.js.contains("//#region @nymph/runtime/option"));
	assert!(!compiled.js.contains("//#region @nymph/runtime/iter"));
	assert_eq!(
		compiled.js.matches("plus_default(").count(),
		2,
		"{}",
		compiled.js
	);
	assert!(!compiled.js.contains("collections/list"), "{}", compiled.js);
	assert!(!compiled.js.contains("collections/map"), "{}", compiled.js);
	assert!(
		!compiled.js.contains("@nymph/runtime/result"),
		"{}",
		compiled.js
	);

	let exercise = compiled.entry_symbol("exercise");
	let script = format!("{}\nconsole.log({exercise}().v);\n", compiled.js);
	let path = std::env::temp_dir().join(format!("nymph-stable-range-{}.mjs", std::process::id()));
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
	assert_eq!(String::from_utf8_lossy(&output.stdout), "31\n");
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
		compiled.js.matches("function $m12$int$i0$sqrt(").count(),
		1,
		"{}",
		compiled.js
	);
	assert!(
		compiled.js.contains("$m12$int$i0$sqrt(new NInt(16))"),
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
fn stable_compare_to_closes_same_module_runtime_demands_and_runs() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-compare-to-runtime");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"func compare() = 1.compare_to(2)\npublic func main(): void = {}".into(),
		SourceVersion(1),
	);

	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("compare_to links its same-module order_from_sign helper");
	assert_eq!(
		compiled
			.js
			.lines()
			.filter(|line| line.starts_with("function ") && line.contains("order_from_sign("))
			.count(),
		1,
		"{}",
		compiled.js
	);
	assert_eq!(
		compiled.js.matches("order_from_sign(").count(),
		2,
		"{}",
		compiled.js
	);
	let compare = compiled.entry_symbol("compare");
	let js = compiled.js.replace(
		"import { NInt } from \"std/box\";",
		"class NInt { constructor(v) { this.v = v; } }",
	);
	let script = format!("{js}\nconsole.log({compare}()[Symbol.for('nymph.tag')].description);\n");
	let path = std::env::temp_dir().join(format!("nymph-stable-compare-{}.mjs", std::process::id()));
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
	assert!(String::from_utf8_lossy(&output.stdout).contains("Order.LessThan"));
}

#[test]
fn stable_project_module_exports_first_class_external_alias_and_runs() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-project-external-value");
	let provider = ModulePath::new("provider").unwrap();
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		provider,
		"public external(println) func println<T: Display>(value: T): void".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/provider with (println as host_println)\nfunc value(): int = { let f = host_println f(1) 0 }\npublic func main(): void = {}".into(),
		SourceVersion(1),
	);

	let emitted = session
		.emit_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.expect("ordinary module emits its external ABI alias");
	assert!(emitted.module_sources["provider"].contains(" as $m0$println"));
	assert!(emitted.module_sources["provider"].contains("export { $m0$println };"));
	assert!(emitted.module_sources["main"].contains("import { $m0$println } from \"provider\";"));
	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("consumer imports the external alias as a first-class value");
	let value = compiled.entry_symbol("value");
	let js = compiled.js.replace(
		"import { NInt } from \"std/box\";",
		"class NInt { constructor(v) { this.v = v; } }",
	);
	let path = std::env::temp_dir().join(format!(
		"nymph-stable-external-value-{}.mjs",
		std::process::id()
	));
	std::fs::write(&path, format!("{js}\nconsole.log({value}().v);\n")).unwrap();
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
	assert_eq!(String::from_utf8_lossy(&output.stdout), "1\n0\n");
}

#[test]
fn stable_importable_module_emits_demanded_external_member_alias() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("stable-importable-external-member");
	let provider = ModulePath::new("provider").unwrap();
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		provider,
		"impl int { external(display) func rendered(): string }".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/provider\nfunc value(): string = (1).rendered()\npublic func main(): void = {}"
			.into(),
		SourceVersion(1),
	);

	let emitted = session
		.emit_interface_project_for_test(project.clone(), main.clone(), EntryMode::Entry)
		.expect("importable module emits a demanded external member alias");
	let provider = &emitted.module_sources["provider"];
	assert!(provider.contains(" as $m0$int$i0$rendered"), "{provider}");
	assert!(
		provider.contains("export { $m0$int$i0$rendered"),
		"{provider}"
	);
	assert!(
		emitted.module_sources["main"].contains("import { $m0$int$i0$rendered } from \"provider\";")
	);
	let compiled = session
		.compile_interface_project_for_test(project, main, EntryMode::Entry)
		.expect("consumer bundles the importable external member");
	let value = compiled.entry_symbol("value");
	let path = std::env::temp_dir().join(format!(
		"nymph-stable-importable-external-member-{}.mjs",
		std::process::id()
	));
	std::fs::write(
		&path,
		format!("{}\nconsole.log({value}().v);\n", compiled.js),
	)
	.unwrap();
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
	assert_eq!(String::from_utf8_lossy(&output.stdout), "1\n");
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
