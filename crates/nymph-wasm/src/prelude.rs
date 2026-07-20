//! The stdlib operator-interface prelude, embedded and parsed once.
//!
//! Mirrors `nymph-compiler`'s `prelude` module (see
//! `crates/nymph-compiler/src/prelude.rs`): `stdlib/src/ops/mod.nym` — the
//! operator interfaces (`Plus`, `Comparable`, `Equals`, …) and their
//! primitive/blanket impls — is embedded via `include_str!` and parsed once,
//! lazily, behind a [`OnceLock`]; every call to [`crate::compile`]/
//! [`crate::check`] reuses the same parsed [`Module`] (checking clones and
//! offsets it per call, so sharing the parse is safe).
//!
//! Follow-up (not this slice): once the core/std split lands, embed the full
//! core, not just `std/ops` — this ops-only prelude matches the current
//! `nymph-compiler` facade.

use std::sync::OnceLock;

use nymph_ast::decl::Module;

/// The real `stdlib/src/ops/mod.nym` source, embedded at compile time.
const OPS_PRELUDE_SOURCE: &str = include_str!("../../../stdlib/src/ops/mod.nym");

static OPS_PRELUDE: OnceLock<Module> = OnceLock::new();

/// The parsed `std/ops` prelude module, parsed once and cached.
pub(crate) fn ops_prelude() -> &'static Module {
	OPS_PRELUDE.get_or_init(|| {
		let parsed = nymph_syntax::parse_module(OPS_PRELUDE_SOURCE, "std/ops");
		debug_assert!(
			parsed.diagnostics.iter().all(|d| !d.is_error()),
			"the embedded std/ops prelude failed to parse: {:?}",
			parsed.diagnostics
		);
		parsed.tree
	})
}
