use std::{
	fs,
	path::{Path, PathBuf},
	time::{SystemTime, UNIX_EPOCH},
};

use nymph_compiler::{
	config::load_compiler_project_config,
	db::{NymphDatabase, TypeErrors},
	queries::{bundle_project, load_source_file, transpile_standalone_file, typecheck_file},
};

#[test]
fn bundle_project_emits_stdlib_structure() {
	let db = NymphDatabase::default();
	let stdlib_root = PathBuf::from("../stdlib")
		.canonicalize()
		.expect("stdlib directory not found");
	let output_dir = unique_temp_dir("nymph-bundler-stdlib");
	let config = load_compiler_project_config(&db, stdlib_root.clone(), output_dir.clone())
		.expect("expected stdlib config to load");

	let result = bundle_project(&db, config);

	assert!(
		!result.emitted_modules.is_empty(),
		"expected emitted stdlib modules"
	);
	assert!(
		output_dir.join("option.js").exists(),
		"expected bundled output for option.nym"
	);
	assert!(
		output_dir.join("math/mod.js").exists(),
		"expected bundled output for math/mod.nym"
	);
	assert!(
		output_dir.join("math/mod.external.ts").exists(),
		"expected copied external companion for math/mod.nym"
	);

	let linked_list = result
		.emitted_modules
		.iter()
		.find(|module| {
			module
				.source_path
				.ends_with(Path::new("collections/linked_list.nym"))
		})
		.expect("expected linked_list module to be emitted");
	assert!(
		linked_list
			.code
			.contains("import * as option from '../option.js';"),
		"expected namespace import in linked_list output"
	);
	assert!(
		linked_list
			.code
			.contains("import { Option } from '../option.js';"),
		"expected named import in linked_list output"
	);

	let math_module = fs::read_to_string(output_dir.join("math/mod.js"))
		.expect("expected bundled math/mod.js to be readable");
	assert!(
		math_module.contains("from './mod.external.ts'"),
		"expected math/mod.js to reference renamed external companion"
	);

	let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn transpile_standalone_file_uses_source_path_js_output() {
	let db = NymphDatabase::default();
	let stdlib_root = PathBuf::from("../stdlib")
		.canonicalize()
		.expect("stdlib directory not found");
	let source_path = stdlib_root.join("src/option.nym");
	let file = load_source_file(&db, source_path.to_string_lossy().to_string());
	let config =
		load_compiler_project_config(&db, stdlib_root, unique_temp_dir("nymph-standalone-config"))
			.expect("expected stdlib config to load");

	let result = transpile_standalone_file(&db, file, config)
		.expect("expected standalone transpilation to succeed");

	assert_eq!(result.source_path, source_path);
	assert_eq!(result.output_path, source_path.with_extension("js"));
	assert!(
		result.code.contains("from './result.js'"),
		"expected standalone JS output to include project import rewrites"
	);
	assert!(
		result.code.contains("from './default.js'"),
		"expected standalone JS output to include multiple rewritten imports"
	);

	let _ = fs::remove_dir_all(config.output_dir(&db));
}

#[test]
fn standalone_file_uses_core_prelude_without_manual_imports() {
	let db = NymphDatabase::default();
	let project_root = write_minimal_prelude_project("nymph-core-prelude-project", false);
	let output_dir = unique_temp_dir("nymph-core-prelude-output");
	let source_path = project_root.join("src/main.nym");
	fs::write(
		&source_path,
		r#"type EqInt = Equals<int>
func wrap(value: Result<Option<int>, string>) -> value
"#,
	)
	.expect("test source should be writable");

	let file = load_source_file(&db, source_path.to_string_lossy().to_string());
	let config = load_compiler_project_config(&db, project_root.clone(), output_dir.clone())
		.expect("expected fixture config to load");

	let result = transpile_standalone_file(&db, file, config)
		.expect("expected standalone transpilation to succeed");
	let errors = transpile_standalone_file::accumulated::<TypeErrors>(&db, file, config);
	let error_messages = errors
		.iter()
		.map(|error| error.0.to_string())
		.collect::<Vec<_>>();
	assert!(
		errors.is_empty(),
		"expected no type errors, found {error_messages:?}"
	);
	assert!(
		result
			.code
			.contains("import { Option } from './option.js';"),
		"expected implicit Option import in emitted JS"
	);
	assert!(
		result
			.code
			.contains("import { Result } from './result.js';"),
		"expected implicit Result import in emitted JS"
	);
	assert!(
		result
			.code
			.contains("import { Equals } from './ops/mod.js';"),
		"expected emitted JS to import the available ops prelude name"
	);
	assert!(
		!result.code.contains("./math/complex.js"),
		"did not expect Complex to be imported implicitly"
	);
	assert!(
		!result.code.contains("Plus"),
		"did not expect missing ops names to be imported implicitly"
	);

	let _ = fs::remove_dir_all(project_root);
	let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn standalone_file_can_use_unqualified_prelude_enum_variants() {
	let db = NymphDatabase::default();
	let project_root = write_minimal_prelude_project("nymph-prelude-variants-project", false);
	let output_dir = unique_temp_dir("nymph-prelude-variants-output");
	let source_path = project_root.join("src/main.nym");
	fs::write(&source_path, "let value = Some(true)\n").expect("test source should be writable");

	let file = load_source_file(&db, source_path.to_string_lossy().to_string());
	let config = load_compiler_project_config(&db, project_root.clone(), output_dir.clone())
		.expect("expected fixture config to load");

	let result = transpile_standalone_file(&db, file, config)
		.expect("expected standalone transpilation to succeed");
	let errors = transpile_standalone_file::accumulated::<TypeErrors>(&db, file, config);
	let error_messages = errors
		.iter()
		.map(|error| error.0.to_string())
		.collect::<Vec<_>>();
	assert!(
		errors.is_empty(),
		"expected no type errors, found {error_messages:?}"
	);
	assert!(
		result.code.contains("const { None, Some } = Option;"),
		"expected emitted JS to bind Option variants into scope"
	);
	assert!(
		result.code.contains("const value = Some(true);"),
		"expected emitted JS to preserve the unqualified constructor call"
	);

	let _ = fs::remove_dir_all(project_root);
	let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn implicit_prelude_module_does_not_emit_recursive_prelude_imports() {
	let db = NymphDatabase::default();
	let project_root = write_minimal_prelude_project("nymph-prelude-module-project", false);
	let output_dir = unique_temp_dir("nymph-prelude-module-output");
	let source_path = project_root.join("src/ops/mod.nym");
	let file = load_source_file(&db, source_path.to_string_lossy().to_string());
	let config = load_compiler_project_config(&db, project_root.clone(), output_dir.clone())
		.expect("expected fixture config to load");

	let result = transpile_standalone_file(&db, file, config)
		.expect("expected prelude module transpilation to succeed");

	assert!(
		!result.code.contains("default.js"),
		"did not expect implicit prelude imports inside a prelude module"
	);
	assert!(
		!result.code.contains("option.js"),
		"did not expect implicit prelude imports inside a prelude module"
	);
	assert!(
		!result.code.contains("result.js"),
		"did not expect implicit prelude imports inside a prelude module"
	);

	let _ = fs::remove_dir_all(project_root);
	let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn standalone_file_can_disable_implicit_prelude_via_config() {
	let db = NymphDatabase::default();
	let project_root = write_minimal_prelude_project("nymph-disabled-prelude-project", true);
	let output_dir = unique_temp_dir("nymph-disabled-prelude-output");
	let source_path = project_root.join("src/main.nym");
	fs::write(
		&source_path,
		r#"type EqInt = Equals<int>
func wrap(value: Result<Option<int>, string>) -> value
"#,
	)
	.expect("test source should be writable");

	let file = load_source_file(&db, source_path.to_string_lossy().to_string());
	let config = load_compiler_project_config(&db, project_root.clone(), output_dir.clone())
		.expect("expected fixture config to load");

	let _ = typecheck_file(&db, file, config);
	let errors = typecheck_file::accumulated::<TypeErrors>(&db, file, config);
	let error_messages = errors
		.iter()
		.map(|error| error.0.to_string())
		.collect::<Vec<_>>();
	assert!(
		error_messages
			.iter()
			.any(|message| message.contains("Unknown type: Equals")),
		"expected disabled implicit prelude to require explicit imports, got {error_messages:?}"
	);

	let _ = fs::remove_dir_all(project_root);
	let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn standalone_file_still_requires_explicit_complex_import() {
	let db = NymphDatabase::default();
	let project_root = write_minimal_prelude_project("nymph-complex-import-project", false);
	let output_dir = unique_temp_dir("nymph-complex-import-output");
	let source_path = project_root.join("src/main.nym");
	fs::write(
		&source_path,
		"func magnitude(value: Complex) -> value.abs()\n",
	)
	.expect("test source should be writable");

	let file = load_source_file(&db, source_path.to_string_lossy().to_string());
	let config = load_compiler_project_config(&db, project_root.clone(), output_dir.clone())
		.expect("expected fixture config to load");

	let _ = typecheck_file(&db, file, config);
	let errors = typecheck_file::accumulated::<TypeErrors>(&db, file, config);
	let error_messages = errors
		.iter()
		.map(|error| error.0.to_string())
		.collect::<Vec<_>>();
	assert!(
		errors
			.iter()
			.any(|error| error.0.to_string().contains("Unknown type: Complex")),
		"expected Complex to remain unavailable without an explicit import, got {error_messages:?}"
	);

	let _ = fs::remove_dir_all(project_root);
	let _ = fs::remove_dir_all(output_dir);
}

fn write_minimal_prelude_project(prefix: &str, disable_implicit_prelude: bool) -> PathBuf {
	let project_root = unique_temp_dir(prefix);
	let build_section = if disable_implicit_prelude {
		"\n[build]\ndisable_implicit_prelude = true\n"
	} else {
		""
	};
	fs::write(
		project_root.join("nymph.toml"),
		format!("name = \"fixture\"\nversion = \"0.1.0\"\nlicense = \"MIT\"{build_section}"),
	)
	.expect("project config should be writable");
	fs::create_dir_all(project_root.join("src/ops")).expect("ops directory should be creatable");
	fs::write(
		project_root.join("src/default.nym"),
		"public interface Default {}\n",
	)
	.expect("default module should be writable");
	fs::write(
		project_root.join("src/option.nym"),
		"public enum Option<T> {\n  Some(value: T),\n  None\n}\n",
	)
	.expect("option module should be writable");
	fs::write(
		project_root.join("src/result.nym"),
		"public enum Result<T, E> {\n  Ok(value: T),\n  Error(error: E)\n}\n",
	)
	.expect("result module should be writable");
	fs::write(
		project_root.join("src/ops/mod.nym"),
		"public interface Equals<Other> {}\n",
	)
	.expect("ops module should be writable");

	project_root
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
	let unique = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.expect("system clock should be after unix epoch")
		.as_nanos();
	let path = std::env::temp_dir().join(format!("{prefix}-{unique}"));
	fs::create_dir_all(&path).expect("temp output directory should be creatable");
	path
}
