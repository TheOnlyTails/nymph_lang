use std::io::{BufRead, IsTerminal, Write};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::NymphCommand;
use crate::compile_guard::{guarded, unsupported_feature_message};
use crate::project_support::{self, ManifestSelection, fs_loader};

/// Start a persistent, project-aware Nymph read-eval-print loop.
///
/// In a terminal, `> ` is the primary prompt and `... ` indicates incomplete
/// syntax. Redirected stdin emits no banner or prompts, so transcript output is
/// deterministic and suitable for scripts.
#[derive(clap::Args)]
pub(crate) struct ReplCommand {}

impl NymphCommand for ReplCommand {
	fn run(&self, manifest: &ManifestSelection) -> i32 {
		let context = match project_support::resolve_repl(manifest) {
			Ok(context) => context,
			Err(error) => {
				eprintln!("error: {error}");
				return 1;
			}
		};
		let load_root = context.src_root.clone();
		let mut session = context
			.src_root
			.map_or_else(nymph_compiler::ReplSession::loose, |root| {
				nymph_compiler::ReplSession::new(fs_loader(root))
			});

		let stdin = std::io::stdin();
		let interactive = stdin.is_terminal();
		let mut input = stdin.lock();
		let mut buffer = String::new();
		let mut line = String::new();
		if interactive {
			println!("Nymph REPL — Ctrl-D to exit");
		}

		loop {
			if interactive {
				print!("{}", if buffer.is_empty() { "> " } else { "... " });
				if std::io::stdout().flush().is_err() {
					return 1;
				}
			}
			line.clear();
			let read = match input.read_line(&mut line) {
				Ok(read) => read,
				Err(error) => {
					eprintln!("error: could not read REPL input: {error}");
					return 1;
				}
			};
			if read == 0 {
				if interactive {
					println!();
				}
				return 0;
			}
			buffer.push_str(&line);
			if nymph_compiler::repl_input_status(&buffer) == nymph_compiler::ReplInputStatus::Incomplete {
				continue;
			}
			if buffer.trim().is_empty() {
				buffer.clear();
				continue;
			}

			let staged = match guarded(|| session.stage(&buffer)) {
				Ok(Ok(staged)) => staged,
				Ok(Err(error)) => {
					render_error(&error, load_root.as_deref());
					buffer.clear();
					continue;
				}
				Err(payload) => {
					eprintln!("{}", unsupported_feature_message(&payload));
					buffer.clear();
					continue;
				}
			};
			let runtime = execute(&staged.execution_js());
			if runtime.success {
				print!("{}", runtime.stdout);
				let _ = std::io::stdout().flush();
				session.commit(staged);
			} else {
				eprintln!("error: REPL submission failed at runtime");
				if let Some(message) = runtime_error_message(&runtime.stderr) {
					eprintln!("{message}");
				}
			}
			buffer.clear();
		}
	}
}

fn runtime_error_message(stderr: &str) -> Option<&str> {
	stderr
		.lines()
		.map(str::trim)
		.find(|line| line.contains("Error:") && !line.starts_with("at "))
		.or_else(|| stderr.lines().map(str::trim).find(|line| !line.is_empty()))
}

fn render_error(error: &nymph_compiler::ReplStageError, src_root: Option<&std::path::Path>) {
	let Some((diagnostics, staged_module, staged_source)) = error.diagnostics() else {
		return;
	};
	let disk = src_root.map(|root| fs_loader(root.to_path_buf()));
	for item in diagnostics {
		let (filename, source) = if item.module == staged_module {
			("<repl>".to_string(), staged_source.to_string())
		} else {
			let source = disk
				.as_ref()
				.and_then(|load| load(&item.module))
				.unwrap_or_default();
			let filename = src_root.map_or_else(
				|| format!("{}.nym", item.module),
				|root| {
					nymph_compiler::ModulePath::new(&item.module).map_or_else(
						|_| format!("{}.nym", item.module),
						|module| module.source_file(root).display().to_string(),
					)
				},
			);
			(filename, source)
		};
		eprint!(
			"{}",
			nymph_diagnostics::render(&filename, &source, std::slice::from_ref(&item.diag))
		);
	}
}

struct RuntimeOutput {
	success: bool,
	stdout: String,
	stderr: String,
}

fn execute(js: &str) -> RuntimeOutput {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let path = std::env::temp_dir().join(format!(
		"nymph_cli_repl_{}_{unique}.mjs",
		std::process::id()
	));
	if let Err(error) = std::fs::write(&path, js) {
		return RuntimeOutput {
			success: false,
			stdout: String::new(),
			stderr: format!("error: could not write {}: {error}\n", path.display()),
		};
	}
	let output = Command::new("node").arg(&path).output();
	let _ = std::fs::remove_file(path);
	match output {
		Ok(output) => RuntimeOutput {
			success: output.status.success(),
			stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
			stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
		},
		Err(error) => RuntimeOutput {
			success: false,
			stdout: String::new(),
			stderr: format!("error: could not run node: {error}\n"),
		},
	}
}
