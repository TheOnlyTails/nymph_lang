pub(crate) mod build;
pub(crate) mod check;
pub(crate) mod doc;
pub(crate) mod format;
pub(crate) mod new;
pub(crate) mod repl;
pub(crate) mod run;

/// Shared behavior for subcommands not yet implemented in this slice: print a
/// clear message to stderr and exit nonzero rather than silently no-op'ing.
pub(crate) fn not_implemented(name: &str) -> i32 {
	eprintln!("nymph {name} is not implemented yet");
	2
}
