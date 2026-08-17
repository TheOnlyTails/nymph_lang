//! The ambient `core` prelude, embedded, parsed once, and injected into every
//! module. Explicit `std/…` imports resolve through a pluggable provider.
//!
//! `core` is the compiler-coupled subset of the stdlib that every Nymph
//! program gets for free, with no `import` required: the operator interfaces
//! (`Plus`, `Comparable`, `Equals`, …, plus the inline `Order` enum),
//! `Default`, `Option`, `Result`, the `Option`/`Result` conversions
//! (`convert.nym`), `Iterator`, `Iterable`, and the `Range` family. Each of
//! these `stdlib/src/**` files is embedded into the binary via `include_str!`.
//! The project driver keeps each ambient module as its own Salsa input and
//! query key, while exact canonical source bytes reuse an independently cached
//! immutable parse.
//!
//! `nymph-sema` deliberately does *not* depend on `nymph-syntax` (a heavyweight
//! parser dependency an otherwise dependency-light checker crate has no other
//! reason to pull in); `nymph-compiler` already depends on both, so it — not
//! `nymph-sema` — owns embedding and parsing the prelude sources.
//!
//! Intra-core `@/…` imports use normal project module resolution. Cycles are
//! represented in the module graph and semantic environments rather than by
//! combining source scopes.

/// One core source file: its canonical `std/…` display name and its
/// `include_str!`-embedded source.
///
/// Order is retained for deterministic display and diagnostics; semantic
/// resolution uses each module's complete environment.
const CORE_SOURCES: &[(&str, &str)] = &[
	("std/ops", include_str!("../../../stdlib/src/ops/mod.nym")),
	(
		"std/default",
		include_str!("../../../stdlib/src/default.nym"),
	),
	("std/option", include_str!("../../../stdlib/src/option.nym")),
	("std/result", include_str!("../../../stdlib/src/result.nym")),
	(
		"std/convert",
		include_str!("../../../stdlib/src/convert.nym"),
	),
	("std/iter", include_str!("../../../stdlib/src/iter/mod.nym")),
	(
		"std/iter/iterable",
		include_str!("../../../stdlib/src/iter/iterable.nym"),
	),
	(
		"std/range",
		include_str!("../../../stdlib/src/range/mod.nym"),
	),
	("std/math", include_str!("../../../stdlib/src/math/mod.nym")),
	// Methods on the built-in/primitive types are ambient too: `"…".length()`,
	// `#[…].sort()`, `#{…}.get()` must "just work" with no `import`, exactly
	// like arithmetic on `int`. Primitives (via `ops`/`math`/`string`) and the
	// built-in literal collections (`#[T]` list, `#{K:V}` map) are ambient; NAMED
	// types (`Set`, `LinkedList`, `Tree`, `Complex`) and io's free functions stay
	// behind `import std/…` (see `crate::std_source`).
	("std/string", include_str!("../../../stdlib/src/string.nym")),
	(
		"std/collections/list",
		include_str!("../../../stdlib/src/collections/list.nym"),
	),
	(
		"std/collections/map",
		include_str!("../../../stdlib/src/collections/map.nym"),
	),
];

pub(crate) const CORE_SOURCE_COUNT: usize = CORE_SOURCES.len();

/// Embedded ambient-core sources keyed in their compiler-private namespace.
/// Keys are canonical, extension-less paths relative to the core root.
pub(crate) fn core_sources() -> impl ExactSizeIterator<Item = (&'static str, &'static str)> {
	CORE_SOURCES
		.iter()
		.map(|(display, source)| (display.strip_prefix("std/").unwrap(), *source))
}

/// Return the stable slot and exact embedded bytes for one canonical ambient
/// module. Callers must compare the bytes before reusing canonical derived data:
/// test support can replace an ambient source while retaining the same key.
pub(crate) fn core_source(path: &str) -> Option<(usize, &'static str)> {
	CORE_SOURCES
		.iter()
		.enumerate()
		.find_map(|(index, (display, source))| {
			(display.strip_prefix("std/") == Some(path)).then_some((index, *source))
		})
}
