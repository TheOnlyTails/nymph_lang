use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::NymphCommand;
use crate::compile_guard::{CompileOutcome, compile_guarded, unsupported_feature_message};
use crate::project_support::{ManifestSelection, ProjectOperation};

/// `nymph run [file]` — compile a Nymph source file and execute it under
/// `node`, forwarding stdout/stderr live and propagating node's exit status.
/// With no file, the nearest project's manifest `build.entry` is used.
///
/// The program's entry point is its top-level `main`: a parameterless,
/// non-generic function with a validated synchronous or task root result. `run`
/// appends a separate Node launcher after the emitted module (the module itself
/// stays a self-contained ES module with no self-executing code). The run file
/// is always compiled in *entry mode*: a missing
/// or mis-shaped `main` — missing entirely,
/// generic, taking parameters, or returning an unsupported root type — is
/// reported as an ordinary type-checker diagnostic (via
/// [`CompileOutcome::Diagnostics`]) alongside any other parse/type errors in
/// the same rendered report, rather than being detected by a separate
/// pre-compile scan. There is no way to run something other than `main` — if
/// a program needs to do something, that's what its `main` is for.
///
/// `nymph run -e "<expr>"` instead evaluates a single inline expression and
/// prints its value: the expression is wrapped in a throwaway nullary function
/// compiled as a *library* module (no `main` required), and the emitted JS is
/// rendered through Nymph's `Display` protocol, so a quick
/// `nymph run -e "1 + 2"` prints `3`.
/// This is the manual-testing counterpart to a full REPL.
///
/// A compile error, or the compiler backend panicking on an unsupported
/// language feature (see [`crate::compile_guard`]), renders a readable
/// message to stderr and exits nonzero without invoking node.
#[derive(clap::Args)]
pub(crate) struct RunCommand {
	/// Path to the `.nym` source file to run (defaults to project build.entry).
	#[arg(conflicts_with = "expr")]
	file: Option<PathBuf>,

	/// An expression to evaluate and print.
	#[arg(short = 'e', long = "expr")]
	expr: Option<String>,

	/// Run with the release compiler profile.
	#[arg(long)]
	release: bool,
}

impl NymphCommand for RunCommand {
	fn run(&self, manifest: &ManifestSelection) -> i32 {
		let profile = if self.release {
			nymph_compiler::BuildProfile::Release
		} else {
			nymph_compiler::BuildProfile::Development
		};
		if let Some(expr) = &self.expr {
			return run_inline_expr(expr, profile);
		}

		let operation = match ProjectOperation::resolve(self.file.as_deref(), manifest, profile) {
			Some(operation) => operation,
			None => return 1,
		};
		match operation.compile_entry() {
			Some(compiled) => execute(&node_launcher(&compiled)),
			None => 1,
		}
	}
}

fn node_launcher(compiled: &nymph_compiler::CompiledProject) -> String {
	use nymph_compiler::CompiledEntryRoot;

	let (kind, binding, task) = match compiled
		.entry_root
		.as_ref()
		.expect("entry compilation provides a validated root adapter")
	{
		CompiledEntryRoot::Void => ("void", None, false),
		CompiledEntryRoot::Option { binding } => ("option", Some(binding.as_str()), false),
		CompiledEntryRoot::Result { binding } => ("result", Some(binding.as_str()), false),
		CompiledEntryRoot::TaskVoid => ("void", None, true),
		CompiledEntryRoot::TaskOption { binding } => ("option", Some(binding.as_str()), true),
		CompiledEntryRoot::TaskResult { binding } => ("result", Some(binding.as_str()), true),
	};
	let binding = binding.unwrap_or("undefined");
	format!(
		r#"{}
const __nymphRootKind = "{kind}";
const __nymphRootEnum = {binding};
let __nymphRootExecution;
let __nymphSignalStatus;
let __nymphSignalCount = 0;
const __nymphSignal = (status) => {{
	__nymphSignalCount += 1;
	if (__nymphSignalCount > 1) process.exit(status);
	__nymphSignalStatus = status;
	__nymphRootExecution?.cancel();
}};
const __nymphSigint = () => __nymphSignal(130);
const __nymphSigterm = () => __nymphSignal(143);
process.on("SIGINT", __nymphSigint);
process.on("SIGTERM", __nymphSigterm);
__nymphRootExecution = nymphStartRoot(() => {}(), {task});
if (__nymphSignalStatus !== undefined) __nymphRootExecution.cancel();
const __nymphOutcome = await __nymphRootExecution.outcome;
process.off("SIGINT", __nymphSigint);
process.off("SIGTERM", __nymphSigterm);
const __nymphWriteDefect = (defect) => {{
	let report = "error: program defected\n";
	try {{ report = nymphRenderDefect(defect); }} catch {{}}
	try {{ process.stderr.write(report); }} catch {{
		try {{ process.stderr.write("error: program defected\n"); }} catch {{}}
	}}
	process.exitCode = 101;
}};
if (__nymphOutcome.tag === "cancelled") {{
	process.stderr.write("error: execution cancelled\n");
	process.exitCode = __nymphSignalStatus ?? 130;
}} else if (__nymphOutcome.tag === "defected") {{
	__nymphWriteDefect(__nymphOutcome.defect);
}} else {{
	try {{
		const value = __nymphOutcome.value;
		const tag = value?.[Symbol.for("nymph.tag")];
		if (__nymphRootKind === "option") {{
			if (tag === __nymphRootEnum.None[Symbol.for("nymph.tag")]) {{
				process.stderr.write("error: main returned None\n");
				process.exitCode = 1;
			}} else if (tag !== __nymphRootEnum.Some[Symbol.for("nymph.tag")]) {{
				throw new TypeError("main produced an invalid Option root value");
			}}
		}} else if (__nymphRootKind === "result") {{
			if (tag === __nymphRootEnum.Error[Symbol.for("nymph.tag")]) {{
				const rendered = nymphActivate(nymphProtocolDisplayStep, undefined, [value.error], -1);
				process.stderr.write(`error: ${{rendered.v}}\n`);
				process.exitCode = 1;
			}} else if (tag !== __nymphRootEnum.Ok[Symbol.for("nymph.tag")]) {{
				throw new TypeError("main produced an invalid Result root value");
			}}
		}}
	}} catch (defect) {{
		__nymphWriteDefect(defect);
	}}
}}
"#,
		compiled.js, compiled.entry_main
	)
}

/// Wrap an inline expression in throwaway evaluation and display functions,
/// compile them as a library module, and print the display string.
fn run_inline_expr(expr: &str, profile: nymph_compiler::BuildProfile) -> i32 {
	// These names are `$`-free but unlikely to collide with anything the
	// expression itself references (inline expressions are self-contained).
	// Keep rendering in Nymph so CLI output observes the same inspectable
	// `Display` behavior as interpolation and stdlib I/O.
	let source = format!(
		"func __nymph_repl() = {expr}\nfunc __nymph_repl_display(): string = \"${{__nymph_repl()}}\"\n"
	);
	let path = "<expr>";

	let js = match compile_guarded(&source, path, profile) {
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

	execute(&format!("{js}\nconsole.log(__nymph_repl_display().v);\n"))
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

	// Spawn with inherited stdio so output streams live. On Unix, forward the
	// first termination signal for Node to clean up, then force-stop it if a
	// second signal arrives.
	let status = run_node(&temp_path);
	let _ = std::fs::remove_file(&temp_path);

	match status {
		Ok(status) => status,
		Err(err) => {
			eprintln!("error: could not run node: {err}");
			1
		}
	}
}

#[cfg(unix)]
fn run_node(path: &std::path::Path) -> std::io::Result<i32> {
	use rustix::process::{Pid, Signal, kill_process};
	use signal_hook::consts::signal::{SIGINT, SIGTERM};
	use signal_hook::iterator::Signals;
	use std::os::unix::process::CommandExt;
	use std::sync::Arc;
	use std::sync::atomic::{AtomicI32, Ordering};

	let mut signals = Signals::new([SIGINT, SIGTERM])?;
	let handle = signals.handle();
	// Isolate Node from the terminal's foreground process group. Otherwise a
	// terminal signal would reach Node directly and then arrive a second time
	// through this forwarder, skipping cooperative cleanup.
	let mut child = Command::new("node").arg(path).process_group(0).spawn()?;
	let pid = Pid::from_raw(child.id() as i32).expect("Node child has a positive process ID");
	let forwarded_status = Arc::new(AtomicI32::new(0));
	let thread_status = Arc::clone(&forwarded_status);
	let forwarder = std::thread::spawn(move || {
		let mut count = 0;
		for signal in signals.forever() {
			count += 1;
			let signal = if signal == SIGINT {
				Signal::INT
			} else {
				Signal::TERM
			};
			if count == 1 {
				thread_status.store(
					if signal == Signal::INT { 130 } else { 143 },
					Ordering::Relaxed,
				);
				let _ = kill_process(pid, signal);
			} else {
				let _ = kill_process(pid, Signal::KILL);
			}
		}
	});
	let status = child.wait();
	handle.close();
	let _ = forwarder.join();
	status.map(|status| {
		status
			.code()
			.unwrap_or_else(|| forwarded_status.load(Ordering::Relaxed).max(1))
	})
}

#[cfg(not(unix))]
fn run_node(path: &std::path::Path) -> std::io::Result<i32> {
	Command::new("node")
		.arg(path)
		.status()
		.map(|status| status.code().unwrap_or(1))
}

#[cfg(all(test, unix))]
mod tests {
	use super::*;
	use rustix::process::{Pid, Signal, kill_process};
	use std::io::BufRead;
	use std::process::Stdio;

	fn pending_launcher(cancel: &str) -> std::path::PathBuf {
		let compiled = nymph_compiler::CompiledProject {
			js: format!(
				r#"
function main() {{}}
const hold = setInterval(() => {{}}, 1000);
function nymphStartRoot() {{
	let settle;
	const outcome = new Promise((resolve) => {{ settle = resolve; }});
	return {{ cancel() {{ {cancel} }}, get outcome() {{ process.stdout.write("ready\n"); return outcome; }} }};
}}
function nymphRenderDefect() {{ return "error: program defected\n"; }}
function nymphActivate() {{}}
function nymphProtocolDisplayStep() {{}}
"#
			),
			entry_main: "main".to_string(),
			entry_root: Some(nymph_compiler::CompiledEntryRoot::TaskVoid),
			entry_tag: 0,
		};
		let script = node_launcher(&compiled);
		let path = std::env::temp_dir().join(format!(
			"nymph_node_launcher_signal_{}_{}.mjs",
			std::process::id(),
			cancel.len()
		));
		std::fs::write(&path, script).unwrap();
		path
	}

	fn wait_for_stdout(child: &mut std::process::Child, expected: &str) {
		let stdout = child.stdout.take().unwrap();
		let mut reader = std::io::BufReader::new(stdout);
		let mut line = String::new();
		reader.read_line(&mut line).unwrap();
		assert_eq!(line, expected);
		child.stdout = Some(reader.into_inner());
	}

	fn assert_first_signal(signal: Signal, status: i32) {
		let path = pending_launcher("clearInterval(hold); settle({ tag: \"cancelled\" });");
		let mut child = Command::new("node")
			.arg(&path)
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.unwrap();
		wait_for_stdout(&mut child, "ready\n");
		kill_process(Pid::from_raw(child.id() as i32).unwrap(), signal).unwrap();
		let output = child.wait_with_output().unwrap();
		let _ = std::fs::remove_file(path);
		assert_eq!(output.status.code(), Some(status));
		assert!(output.stdout.is_empty());
		assert_eq!(output.stderr, b"error: execution cancelled\n");
	}

	#[test]
	fn node_launcher_maps_first_sigint_cancellation_to_130() {
		assert_first_signal(Signal::INT, 130);
	}

	#[test]
	fn node_launcher_maps_first_sigterm_cancellation_to_143() {
		assert_first_signal(Signal::TERM, 143);
	}

	#[test]
	fn node_launcher_maps_unsourced_cancellation_to_130() {
		let compiled = nymph_compiler::CompiledProject {
			js: r#"
function main() {}
function nymphStartRoot() { return { cancel() {}, outcome: Promise.resolve({ tag: "cancelled" }) }; }
function nymphRenderDefect() { return "error: program defected\n"; }
function nymphActivate() {}
function nymphProtocolDisplayStep() {}
"#
			.to_string(),
			entry_main: "main".to_string(),
			entry_root: Some(nymph_compiler::CompiledEntryRoot::TaskVoid),
			entry_tag: 0,
		};
		let output = Command::new("node")
			.arg("--input-type=module")
			.arg("--eval")
			.arg(node_launcher(&compiled))
			.output()
			.unwrap();
		assert_eq!(output.status.code(), Some(130));
		assert!(output.stdout.is_empty());
		assert_eq!(output.stderr, b"error: execution cancelled\n");
	}

	#[test]
	fn node_launcher_allows_a_second_signal_to_force_exit() {
		let path = pending_launcher("process.stdout.write(\"cancelled\\n\");");
		let mut child = Command::new("node")
			.arg(&path)
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.unwrap();
		wait_for_stdout(&mut child, "ready\n");
		let pid = Pid::from_raw(child.id() as i32).unwrap();
		kill_process(pid, Signal::TERM).unwrap();
		wait_for_stdout(&mut child, "cancelled\n");
		kill_process(pid, Signal::TERM).unwrap();
		let output = child.wait_with_output().unwrap();
		let _ = std::fs::remove_file(path);
		assert_eq!(output.status.code(), Some(143));
		assert!(output.stdout.is_empty());
		assert!(output.stderr.is_empty());
	}
}
