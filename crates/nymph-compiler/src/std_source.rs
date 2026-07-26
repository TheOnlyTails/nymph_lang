//! The embedded `std` source tree — the non-`core` stdlib modules a program
//! reaches via `import std/…`. `core` (the operator interfaces, `Option`,
//! `Result`, the collection/`string` methods, …) is ambient with no import —
//! see [`crate::prelude`]. `std` is everything else: things you opt into.
//!
//! Each module is embedded via `include_str!` (exactly like `core`), so
//! `import std/…` resolves from a shipped binary, not just a dev checkout —
//! this is the provider the CLI threads into `compile_project_with_std`.
//!
//! Keyed by the path the driver hands the std provider: everything after the
//! `std::` module-key prefix (`import std/io` → provider path `"io"`,
//! `import std/collections/tree` → `"collections/tree"`).

/// One embedded `std` module: its provider path and `include_str!`-embedded
/// source. Add a row here when a stdlib module becomes reachable via
/// `import std/…` (i.e. it is NOT in `prelude::CORE_SOURCES`).
const STD_SOURCES: &[(&str, &str)] = &[
	("io", include_str!("../../../stdlib/src/io.nym")),
	(
		"collections/set",
		include_str!("../../../stdlib/src/collections/set.nym"),
	),
	(
		"collections/linked_list",
		include_str!("../../../stdlib/src/collections/linked_list.nym"),
	),
	(
		"collections/tree",
		include_str!("../../../stdlib/src/collections/tree.nym"),
	),
	(
		"math/complex",
		include_str!("../../../stdlib/src/math/complex.nym"),
	),
];

pub(crate) fn embedded_std_sources() -> impl Iterator<Item = (&'static str, &'static str)> {
	STD_SOURCES.iter().copied()
}

/// A std-source provider over the embedded `std` tree — pass to
/// [`crate::compile_project_with_std`] / [`crate::check_project_with_std`].
/// Given the provider path (`"io"`, `"collections/tree"`, …), returns the
/// embedded module source, or `None` for a path that names no `std` module (the
/// driver then reports it as an unresolved `import std/…`).
#[must_use]
pub fn embedded_std_provider(path: &str) -> Option<String> {
	STD_SOURCES
		.iter()
		.find(|(name, _)| *name == path)
		.map(|(_, source)| (*source).to_string())
}
