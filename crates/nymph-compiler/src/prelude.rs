//! The ambient `core` prelude, embedded and parsed once (Slice: core/std
//! split — Slice A injects all of `core` as the ambient prelude; Slice B
//! resolves `import std/…` via a pluggable provider).
//!
//! `core` is the compiler-coupled subset of the stdlib that every Nymph
//! program gets for free, with no `import` required: the operator interfaces
//! (`Plus`, `Comparable`, `Equals`, …, plus the inline `Order` enum),
//! `Default`, `Option`, `Result`, the `Option`/`Result` conversions
//! (`convert.nym`), `Iterator`, `Iterable`, and the `Range` family. Each of
//! these `stdlib/src/**` files is embedded into the binary via `include_str!`
//! and parsed once, lazily, behind a [`OnceLock`]; every call to
//! [`crate::check`]/[`crate::compile`] (and their entry-mode counterparts, and
//! the project driver's per-module prelude slice) reuses the same parsed
//! `Vec<Module>` (checking clones and offsets each entry per call — see
//! `nymph_sema::check_module_with_prelude` — so sharing the parse is safe).
//!
//! `nymph-sema` deliberately does *not* depend on `nymph-syntax` (a heavyweight
//! parser dependency an otherwise dependency-light checker crate has no other
//! reason to pull in); `nymph-compiler` already depends on both, so it — not
//! `nymph-sema` — owns embedding and parsing the prelude sources.
//!
//! The intra-core `@/…` imports each of these files carries (e.g.
//! `option.nym`'s `import @/default`) are inert once flattened: the checker
//! drops every `Declaration::Import` when flattening a prelude module (see
//! `nymph_sema::prelude::check_module_with_prelude_impl`), so they never spawn
//! a project-level module lookup — core is passed straight to check/lower as
//! `&[Module]`, bypassing the project driver's import resolution entirely.
//! This is also what makes the `Option`/`Result` cycle (`convert.nym` imports
//! both, one-way; `option.nym`/`result.nym` do not import each other — see
//! MT4) harmless here: the whole bundle shares one flattened scope.

use std::sync::OnceLock;

use nymph_ast::decl::Module;

/// One core source file: its `std/…` display name (kept for span-scrub
/// compatibility — see `nymph_sema::prelude::scrub_prelude_labels`) and its
/// `include_str!`-embedded source.
///
/// Order matters only for readability/debuggability (name resolution over a
/// flattened prelude slice is order-independent — the checker builds a
/// def-map over the combined members), but `convert` is still listed after
/// `option`/`result` since it imports both.
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

/// Embedded ambient-core sources keyed in their compiler-private namespace.
/// Keys are canonical, extension-less paths relative to the core root.
pub(crate) fn core_sources() -> impl ExactSizeIterator<Item = (&'static str, &'static str)> {
	CORE_SOURCES
		.iter()
		.map(|(display, source)| (display.strip_prefix("std/").unwrap(), *source))
}

static CORE_PRELUDE: OnceLock<Vec<Module>> = OnceLock::new();

/// Parse one embedded core source, panicking (via `debug_assert`, same as the
/// prior single-module `ops_prelude`) if it fails to parse — every entry in
/// [`CORE_SOURCES`] is real, checked-in stdlib source, never user input.
fn parse_core_source(display_name: &str, source: &str) -> Module {
	let parsed = nymph_syntax::parse_module(source, display_name);
	debug_assert!(
		parsed.diagnostics.iter().all(|d| !d.is_error()),
		"the embedded {display_name} core module failed to parse: {:?}",
		parsed.diagnostics
	);
	parsed.tree
}

/// The parsed `core` prelude — every module in [`CORE_SOURCES`], parsed once
/// and cached. Every call site that used to pass the single `std/ops` module
/// as `[Module; 1]` now passes this whole slice.
pub(crate) fn core_prelude() -> &'static [Module] {
	CORE_PRELUDE
		.get_or_init(|| {
			CORE_SOURCES
				.iter()
				.map(|(name, source)| parse_core_source(name, source))
				.collect()
		})
		.as_slice()
}
