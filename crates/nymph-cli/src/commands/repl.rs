use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};

use crate::NymphCommand;
use crate::compile_guard::{guarded, unsupported_feature_message};
use crate::project_support::{self, ManifestSelection};

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
				nymph_compiler::ReplSession::new(nymph_project::fs_loader(root))
			});
		let mut worker = match ReplWorker::start() {
			Ok(worker) => worker,
			Err(error) => {
				eprintln!("error: could not start REPL runtime: {error}");
				return 1;
			}
		};

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
				if !buffer.trim().is_empty() {
					eprintln!("error: incomplete REPL submission at end of input");
					return 1;
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
			let runtime = worker.execute(&staged);
			if runtime.success {
				session.commit(staged, &runtime.retained_modules);
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
	let disk = src_root.map(|root| nymph_project::fs_loader(root.to_path_buf()));
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
	stderr: String,
	retained_modules: Vec<String>,
}

struct ReplWorker {
	child: Child,
	input: Option<ChildStdin>,
	output: BufReader<ChildStderr>,
}

impl ReplWorker {
	fn start() -> std::io::Result<Self> {
		let mut child = Command::new("node")
			.args([
				"--experimental-vm-modules",
				"--no-warnings",
				"--input-type=module",
				"--eval",
				include_str!("repl_worker.mjs"),
			])
			.stdin(Stdio::piped())
			.stdout(Stdio::inherit())
			.stderr(Stdio::piped())
			.spawn()?;
		let input = child.stdin.take().expect("piped worker stdin");
		let output = BufReader::new(child.stderr.take().expect("piped worker stderr"));
		Ok(Self {
			child,
			input: Some(input),
			output,
		})
	}

	fn execute(&mut self, staged: &nymph_compiler::StagedReplSubmission) -> RuntimeOutput {
		self.execute_request(staged.entry(), staged.render_symbol(), staged.modules())
	}

	fn execute_request(
		&mut self,
		entry: &str,
		render: Option<&str>,
		modules: &std::collections::BTreeMap<String, String>,
	) -> RuntimeOutput {
		const PREFIX: &str = "\u{1e}nymph-repl:";
		let request = serde_json::json!({
			"entry": entry,
			"render": render,
			"modules": modules,
		});
		let Some(input) = &mut self.input else {
			return RuntimeOutput {
				success: false,
				stderr: "REPL runtime worker is closed".to_string(),
				retained_modules: Vec::new(),
			};
		};
		if writeln!(input, "{request}")
			.and_then(|()| input.flush())
			.is_err()
		{
			return RuntimeOutput {
				success: false,
				stderr: "REPL runtime worker closed its input".to_string(),
				retained_modules: Vec::new(),
			};
		}
		loop {
			let mut line = String::new();
			match self.output.read_line(&mut line) {
				Ok(0) => {
					return RuntimeOutput {
						success: false,
						stderr: "REPL runtime worker exited unexpectedly".to_string(),
						retained_modules: Vec::new(),
					};
				}
				Ok(_) => {
					let Some(response) = line.strip_prefix(PREFIX) else {
						eprint!("{line}");
						continue;
					};
					match serde_json::from_str::<serde_json::Value>(response) {
						Ok(response) => {
							let success = response["ok"].as_bool().unwrap_or(false);
							let stderr = response["error"].as_str().unwrap_or_default().to_string();
							let retained_modules = response["retained"]
								.as_array()
								.into_iter()
								.flatten()
								.filter_map(|key| key.as_str().map(str::to_string))
								.collect();
							return RuntimeOutput {
								success,
								stderr,
								retained_modules,
							};
						}
						Err(error) => {
							return RuntimeOutput {
								success: false,
								stderr: format!("invalid REPL runtime response: {error}"),
								retained_modules: Vec::new(),
							};
						}
					}
				}
				Err(error) => {
					return RuntimeOutput {
						success: false,
						stderr: format!("could not read REPL runtime response: {error}"),
						retained_modules: Vec::new(),
					};
				}
			}
		}
	}
}

impl Drop for ReplWorker {
	fn drop(&mut self) {
		drop(self.input.take());
		if !matches!(self.child.try_wait(), Ok(Some(_))) {
			let _ = self.child.kill();
		}
		let _ = self.child.wait();
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use super::ReplWorker;

	#[test]
	fn failed_first_load_modules_are_evicted_and_can_be_retried() {
		let mut worker = ReplWorker::start().unwrap();
		let failed = BTreeMap::from([
			(
				"dependency".to_string(),
				"throw new Error('first load fails');".to_string(),
			),
			(
				"entry".to_string(),
				"import 'dependency'; export function render() { return { v: 'unreachable' }; }"
					.to_string(),
			),
		]);
		assert!(
			!worker
				.execute_request("entry", Some("render"), &failed)
				.success
		);

		let retry = BTreeMap::from([
			("dependency".to_string(), "export const value = 42;".to_string()),
			(
				"entry".to_string(),
				"import { value } from 'dependency'; export function render() { return { v: String(value) }; }"
					.to_string(),
			),
		]);
		assert!(
			worker
				.execute_request("entry", Some("render"), &retry)
				.success
		);
	}

	#[test]
	fn strict_worker_rejects_async_render_results() {
		let mut worker = ReplWorker::start().unwrap();
		let modules = BTreeMap::from([(
			"entry".to_string(),
			"export function render() { return Promise.resolve({ v: 'late' }); }".to_string(),
		)]);
		let output = worker.execute_request("entry", Some("render"), &modules);
		assert!(!output.success);
		assert!(
			output
				.stderr
				.contains("asynchronous REPL rendering is disabled")
		);
	}

	#[test]
	fn successful_requests_do_not_retain_unlinked_supplied_modules() {
		let mut worker = ReplWorker::start().unwrap();
		let first = BTreeMap::from([
			("entry".to_string(), "export const value = 1;".to_string()),
			("unused".to_string(), "export const old = 1;".to_string()),
		]);
		let output = worker.execute_request("entry", None, &first);
		assert!(output.success, "{}", output.stderr);
		assert!(!output.retained_modules.contains(&"unused".to_string()));

		let second = BTreeMap::from([
			(
				"entry_2".to_string(),
				"import { fresh } from 'unused'; export const value = fresh;".to_string(),
			),
			("unused".to_string(), "export const fresh = 2;".to_string()),
		]);
		let output = worker.execute_request("entry_2", None, &second);
		assert!(output.success, "{}", output.stderr);
	}

	#[test]
	#[cfg(target_os = "linux")]
	fn dropping_worker_reaps_the_node_process() {
		let worker = ReplWorker::start().unwrap();
		let pid = worker.child.id();
		drop(worker);
		assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
	}
}
