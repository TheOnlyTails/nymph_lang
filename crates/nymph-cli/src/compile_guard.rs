//! A panic-safe wrapper that converts compiler panics into typed CLI failures.
//!
//! Lowering panics on purpose for deferred-but-type-checked language features
//! during stable runtime lowering — a program can pass parsing
//! and type-checking and still hit a `panic!` once the compiler tries to
//! lower it to HIR. Left unguarded, that panic would cross `build`/`run` as a
//! raw Rust panic: a backtrace-shaped stderr dump and exit code 101, nothing
//! like the rest of the CLI's diagnostics. [`compile_guarded`] catches it,
//! suppresses the default panic hook's backtrace print for the duration of
//! the call, and turns it into a normal, readable [`CompileOutcome::Panicked`]
//! instead.

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};

use nymph_diagnostics::Diagnostic;

/// The result of compiling a program through the panic-guarded path.
pub(crate) enum CompileOutcome {
	/// Parsed, checked, lowered, and emitted successfully.
	Ok(String),
	/// Parsing or checking failed; these are ordinary diagnostics, not a bug.
	Diagnostics(Vec<Diagnostic>),
	/// Lowering (or another pipeline stage) panicked — a deferred-but-typed
	/// feature the backend doesn't support yet. Carries the panic message.
	Panicked(String),
}

thread_local! {
	/// Scratch slot the panic hook installed by [`compile_guarded`] writes the
	/// panic's message into.
	///
	/// This capture happens *inside the hook* rather than by downcasting the
	/// payload `catch_unwind` hands back, because the two don't always agree:
	/// with this pipeline's dependency stack, a panic's payload can arrive at
	/// `catch_unwind`'s `Err` already repackaged into some other `Any` type by
	/// the time it gets there (observed: a panic that the hook sees as a
	/// plain `&'static str` shows up at the `catch_unwind` boundary as a value
	/// that downcasts as neither `&str` nor `String`). The hook, however,
	/// always sees the original payload at the moment the panic is raised —
	/// so that's where the message is captured.
	static CAPTURED_PANIC_MESSAGE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Compile `source` (anchored at `path` for diagnostics), catching any panic
/// from inside the pipeline and reporting it as data instead of unwinding
/// into the caller. This single-module path compiles a library; entry builds
/// use the project compiler through [`guarded`].
pub(crate) fn compile_guarded(source: &str, path: &str) -> CompileOutcome {
	CAPTURED_PANIC_MESSAGE.with(|cell| *cell.borrow_mut() = None);

	// The default hook prints "thread '...' panicked at ...:LINE:COL:\n<msg>\n
	// note: run with `RUST_BACKTRACE=1` ..." straight to stderr the moment the
	// panic happens, before `catch_unwind` even gets a chance to react to it.
	// Swap in a hook that captures the message instead of printing it, for
	// the duration of the guarded call only — our own message below is the
	// one the user sees.
	let previous_hook = panic::take_hook();
	panic::set_hook(Box::new(|info| {
		let message = panic_payload_to_string(info.payload());
		CAPTURED_PANIC_MESSAGE.with(|cell| *cell.borrow_mut() = Some(message));
	}));

	let result = panic::catch_unwind(AssertUnwindSafe(|| nymph_compiler::compile(source, path)));

	panic::set_hook(previous_hook);

	match result {
		Ok(Ok(js)) => CompileOutcome::Ok(js),
		Ok(Err(diagnostics)) => CompileOutcome::Diagnostics(diagnostics),
		Err(_) => {
			let message = CAPTURED_PANIC_MESSAGE
				.with(|cell| cell.borrow_mut().take())
				.unwrap_or_else(|| "<no panic message captured>".to_string());
			CompileOutcome::Panicked(message)
		}
	}
}

/// Run a compile call `f`, catching any pipeline panic (a deferred-but-typed
/// feature the backend doesn't support yet) and returning its captured message
/// as `Err` instead of unwinding into the caller. The generic core the
/// single-module [`compile_guarded`] wraps — exposed so the project-driver path
/// (`compile_project_with_std`, used for `nymph.toml` projects AND bare
/// single-file entries) gets the same "readable message, not a raw panic"
/// guarantee.
pub(crate) fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
	CAPTURED_PANIC_MESSAGE.with(|cell| *cell.borrow_mut() = None);

	let previous_hook = panic::take_hook();
	panic::set_hook(Box::new(|info| {
		let message = panic_payload_to_string(info.payload());
		CAPTURED_PANIC_MESSAGE.with(|cell| *cell.borrow_mut() = Some(message));
	}));

	let result = panic::catch_unwind(AssertUnwindSafe(f));

	panic::set_hook(previous_hook);

	result.map_err(|_| {
		CAPTURED_PANIC_MESSAGE
			.with(|cell| cell.borrow_mut().take())
			.unwrap_or_else(|| "<no panic message captured>".to_string())
	})
}

/// Render a panic payload the way `panic!`/`assert!` produce it: a
/// `&'static str` for a string-literal message, or a `String` for a
/// formatted one (the two shapes `std` itself ever panics with).
fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
	if let Some(s) = payload.downcast_ref::<&str>() {
		(*s).to_string()
	} else if let Some(s) = payload.downcast_ref::<String>() {
		s.clone()
	} else {
		"<non-string panic payload>".to_string()
	}
}

/// The stderr message for [`CompileOutcome::Panicked`], shared by `build` and
/// `run` so both commands report an unsupported-feature panic identically.
pub(crate) fn unsupported_feature_message(payload: &str) -> String {
	format!("error: this program uses a feature the compiler backend does not support yet: {payload}")
}
