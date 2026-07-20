use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::NymphCommand;
use crate::compile_guard::{CompileOutcome, Entry, compile_guarded, unsupported_feature_message};
use crate::project_support::{self, fs_loader, render_project_diagnostics};

/// `nymph run <file>` — compile a Nymph source file and execute it under
/// `node`, forwarding stdout/stderr live and propagating node's exit status.
///
/// The program's entry point is its top-level `main`: a parameterless,
/// non-generic function declaring no return type other than `void`. `run`
/// appends a bare `main();` call after the emitted module (the module itself
/// stays a self-contained ES module with no self-executing code) and invokes
/// it. The run file is always compiled in *entry mode*
/// (`nymph_compiler::compile_entry`, via [`compile_guarded`]'s
/// [`Entry::Entry`]): a missing or mis-shaped `main` — missing entirely,
/// generic, taking parameters, or declaring a non-`void` return type — is
/// reported as an ordinary type-checker diagnostic (via
/// [`CompileOutcome::Diagnostics`]) alongside any other parse/type errors in
/// the same rendered report, rather than being detected by a separate
/// pre-compile scan. There is no way to run something other than `main` — if
/// a program needs to do something, that's what its `main` is for.
///
/// `nymph run -e "<expr>"` instead evaluates a single inline expression and
/// prints its value: the expression is wrapped in a throwaway nullary function
/// compiled as a *library* module (no `main` required), and the emitted JS is
/// invoked through `console.log`, so a quick `nymph run -e "1 + 2"` prints `3`.
/// This is the manual-testing counterpart to a full REPL.
///
/// A compile error, or the compiler backend panicking on an unsupported
/// language feature (see [`crate::compile_guard`]), renders a readable
/// message to stderr and exits nonzero without invoking node.
#[derive(clap::Args)]
pub(crate) struct RunCommand {
	/// Path to the `.nym` source file to run.
	#[arg(required_unless_present = "expr", conflicts_with = "expr")]
	file: Option<PathBuf>,

	/// An expression to evaluate and print.
	#[arg(short = 'e', long = "expr")]
	expr: Option<String>,
}

impl NymphCommand for RunCommand {
	fn run(&self) -> i32 {
		if let Some(expr) = &self.expr {
			return run_inline_expr(expr);
		}

		let file = self
			.file
			.as_ref()
			.expect("clap requires a file when --expr is absent");

		if let Some(project) = project_support::detect(file) {
			let load = fs_loader(project.src_root);
			return match nymph_compiler::compile_project(&project.entry_key, &load) {
				Ok(compiled) => execute(&format!("{}\n{}();\n", compiled.js, compiled.entry_main)),
				Err(diags) => {
					eprint!("{}", render_project_diagnostics(&diags, &load));
					1
				}
			};
		}

		let source = match std::fs::read_to_string(file) {
			Ok(source) => source,
			Err(err) => {
				eprintln!("error: could not read {}: {err}", file.display());
				return 1;
			}
		};
		let path = file.display().to_string();

		let js = match compile_guarded(&source, &path, Entry::Entry) {
			CompileOutcome::Ok(js) => js,
			CompileOutcome::Diagnostics(diagnostics) => {
				eprint!(
					"{}",
					nymph_diagnostics::render(&path, &source, &diagnostics)
				);
				return 1;
			}
			CompileOutcome::Panicked(payload) => {
				eprintln!("{}", unsupported_feature_message(&payload));
				return 1;
			}
		};

		execute(&format!("{js}\nmain();\n"))
	}
}

/// Wrap an inline expression in a throwaway nullary function, compile it as a
/// library module, and print its value through `console.log`.
fn run_inline_expr(expr: &str) -> i32 {
	// `__nymph_repl` is `$`-free but unlikely to collide with anything the
	// expression itself references (inline expressions are self-contained).
	let source = format!("func __nymph_repl() = {expr}\n");
	let path = "<expr>";

	let js = match compile_guarded(&source, path, Entry::Library) {
		CompileOutcome::Ok(js) => js,
		CompileOutcome::Diagnostics(diagnostics) => {
			// Spans point into the synthesized wrapper, so render against it —
			// the caret still lands on the offending part of the expression.
			eprint!("{}", nymph_diagnostics::render(path, &source, &diagnostics));
			return 1;
		}
		CompileOutcome::Panicked(payload) => {
			eprintln!("{}", unsupported_feature_message(&payload));
			return 1;
		}
	};

	execute(&format!("{js}\nconsole.log(__nymph_repl());\n"))
}

/// Write `js` to a unique temp `.mjs` and run it under `node`, forwarding
/// stdio live and returning node's exit code.
fn execute(js: &str) -> i32 {
	// `.mjs` makes Node treat the temp file as an ES module, matching the
	// pattern in `crates/nymph-codegen/tests/run_node.rs`. The pid + a
	// monotonic counter keep the path unique across concurrent invocations.
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let temp_path =
		std::env::temp_dir().join(format!("nymph_cli_run_{}_{unique}.mjs", std::process::id()));

	if let Err(err) = std::fs::write(&temp_path, js) {
		eprintln!(
			"error: could not write temp file {}: {err}",
			temp_path.display()
		);
		return 1;
	}

	// `.status()` inherits this process's stdio, so node's output streams
	// live rather than being buffered and reprinted.
	let status = Command::new("node").arg(&temp_path).status();
	let _ = std::fs::remove_file(&temp_path);

	match status {
		Ok(status) => status.code().unwrap_or(1),
		Err(err) => {
			eprintln!("error: could not run node: {err}");
			1
		}
	}
}
