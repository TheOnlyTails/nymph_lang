//! End-to-end: parse -> check -> lower -> emit -> run under Node, asserting stdout.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use nymph_codegen::emit;
use nymph_sema::{check_module, lower_hir};
use nymph_syntax::parse_module;

/// Compile a Nymph source module to a JS module string.
fn compile(src: &str) -> String {
	let parsed = parse_module(src, "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse errors in test source: {:?}",
		parsed.diagnostics
	);
	let checked = check_module(&parsed.tree);
	assert!(
		checked.diags.is_empty(),
		"check errors: {:?}",
		checked.diags
	);
	emit(&lower_hir(&parsed.tree))
}

/// Emit `src`, append a driver that logs `expr`, run under Node, return trimmed stdout.
fn run(src: &str, call: &str) -> String {
	let mut js = compile(src);
	js.push_str(&format!("\nconsole.log({call});\n"));

	// `process::id()` alone is not a unique filename: all tests in this binary
	// share one process and may run on parallel threads, racing on the same path.
	// Mix in a monotonic counter to keep each test's script isolated.
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir();
	let path = dir.join(format!("nymph_run_{}_{unique}.mjs", std::process::id()));
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(js.as_bytes()).unwrap();

	// This shell's environment may force ANSI color output (`FORCE_COLOR`), which
	// would corrupt the plain stdout values we assert on; pin Node to no-color.
	let output = Command::new("node")
		.arg(&path)
		.env("NO_COLOR", "1")
		.env_remove("FORCE_COLOR")
		.output()
		.expect("run node");
	let _ = std::fs::remove_file(&path);
	assert!(
		output.status.success(),
		"node failed:\n{}\n--- js ---\n{}",
		String::from_utf8_lossy(&output.stderr),
		js
	);
	String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn runs_arithmetic() {
	// Pure scalar arithmetic (Task 3/4 already cover emit+lower; this asserts it RUNS).
	let out = run("func add(a: int, b: int): int = a + b * 2", "add(3, 4)");
	assert_eq!(out, "11");
}

#[test]
fn runs_a_block_with_bindings() {
	let src = r#"
		func compute(): int = {
			let x = 10
			let y = x + 5
			y * 2
		}
	"#;
	let out = run(src, "compute()");
	assert_eq!(out, "30");
}

#[test]
fn runs_if_as_value() {
	// `if`/`else` in value position (nested), each branch a block with a tail value.
	let src = r#"
		func sign(n: int): int =
			if (n > 0) { 1 }
			else { if (n < 0) { -1 } else { 0 } }
	"#;
	assert_eq!(run(src, "sign(5)"), "1");
	assert_eq!(run(src, "sign(-3)"), "-1");
	assert_eq!(run(src, "sign(0)"), "0");
}

#[test]
fn runs_while_loop() {
	// A `while` loop with a mutable accumulator; assignment (`=`) drives it.
	let src = r#"
		func sum_to(n: int): int = {
			let mut total = 0
			let mut i = 1
			while (i <= n) {
				total = total + i
				i = i + 1
			}
			total
		}
	"#;
	assert_eq!(run(src, "sum_to(5)"), "15");
}

#[test]
fn compile_reports_check_errors() {
	// A type error surfaces as diagnostics, not JS.
	let result = nymph_codegen::compile("func f(): int = true", "test");
	assert!(result.is_err(), "type error should not produce JS");
}

#[test]
fn compile_produces_runnable_js() {
	let result = nymph_codegen::compile("func double(n: int): int = n * 2", "test");
	assert!(
		result.is_ok(),
		"well-typed program should compile: {result:?}"
	);
}
