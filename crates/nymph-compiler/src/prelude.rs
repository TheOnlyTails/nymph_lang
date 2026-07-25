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

use ecow::EcoString;
use nymph_ast::decl::{Declaration, Module};
use nymph_hir::hir::{HirClass, HirEnum, HirModule, HirVariant};
use rustc_hash::FxHashMap;

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

static CORE_PRELUDE: OnceLock<Vec<Module>> = OnceLock::new();
static CORE_RUNTIME_TYPE_OWNERS: OnceLock<FxHashMap<EcoString, &'static str>> = OnceLock::new();
static CORE_RUNTIME_DECLARATION_SEEDS: OnceLock<HirModule> = OnceLock::new();

pub(crate) fn core_runtime_module_owners() -> impl Iterator<Item = EcoString> {
	CORE_SOURCES
		.iter()
		.map(|(owner, _)| EcoString::from(*owner))
}

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

/// The canonical source module for every runtime enum and struct in core.
///
/// Ownership comes from the top-level declaration's source, not from any
/// separate `impl` that may add behavior to the type elsewhere in core.
pub(crate) fn core_runtime_type_owners() -> &'static FxHashMap<EcoString, &'static str> {
	CORE_RUNTIME_TYPE_OWNERS.get_or_init(|| {
		let mut owners = FxHashMap::default();

		for ((owner, _), module) in CORE_SOURCES.iter().zip(core_prelude()) {
			for declaration in &module.members {
				let name = match declaration {
					Declaration::Enum { name, .. } | Declaration::Struct { name, .. } => &name.0,
					_ => continue,
				};

				if let Some(previous_owner) = owners.insert(name.clone(), *owner) {
					panic!(
						"duplicate core runtime type declaration `{name}` in `{previous_owner}` and `{owner}`"
					);
				}
			}
		}

		owners
	})
}

/// Canonical declaration shapes derived directly from the parsed core source.
/// Bodies are deliberately absent: consumer lowering supplies only demanded
/// methods, while these seeds ensure an import-only demand still has its
/// source-owned enum variants or struct fields available for emission.
pub(crate) fn core_runtime_declaration_seeds() -> &'static HirModule {
	CORE_RUNTIME_DECLARATION_SEEDS.get_or_init(|| {
		let mut module = HirModule {
			lets: Vec::new(),
			funcs: Vec::new(),
			classes: Vec::new(),
			enums: Vec::new(),
		};
		for source in core_prelude() {
			for declaration in &source.members {
				match declaration {
					Declaration::Enum { name, variants, .. } => module.enums.push(HirEnum {
						name: name.0.clone(),
						variants: variants
							.iter()
							.map(|variant| HirVariant {
								name: variant.0.name.0.clone(),
								fields: variant
									.0
									.fields
									.iter()
									.map(|field| field.0.name.0.clone())
									.collect(),
							})
							.collect(),
						methods: Vec::new(),
						statics: Vec::new(),
					}),
					Declaration::Struct { name, fields, .. } => module.classes.push(HirClass {
						name: name.0.clone(),
						fields: fields.iter().map(|field| field.0.name.0.clone()).collect(),
						methods: Vec::new(),
						statics: Vec::new(),
					}),
					_ => {}
				}
			}
		}
		module
	})
}

#[cfg(test)]
mod tests {
	use super::{core_runtime_declaration_seeds, core_runtime_type_owners};

	#[test]
	fn core_runtime_declaration_seeds_preserve_source_variants_and_fields_only() {
		let seeds = core_runtime_declaration_seeds();
		let option = seeds
			.enums
			.iter()
			.find(|item| item.name == "Option")
			.expect("Option seed");
		assert_eq!(
			option
				.variants
				.iter()
				.map(|variant| (variant.name.as_str(), variant.fields.len()))
				.collect::<Vec<_>>(),
			[("Some", 1), ("None", 0)]
		);
		assert!(option.methods.is_empty() && option.statics.is_empty());
		let range = seeds
			.classes
			.iter()
			.find(|item| item.name == "Range")
			.expect("Range seed");
		assert_eq!(
			range
				.fields
				.iter()
				.map(AsRef::as_ref)
				.collect::<Vec<&str>>(),
			["start", "end"]
		);
		assert!(range.methods.is_empty() && range.statics.is_empty());
	}

	#[test]
	fn core_runtime_types_have_their_declaration_module_as_owner() {
		let owners = core_runtime_type_owners();
		let expected = [
			("Order", "std/ops"),
			("Option", "std/option"),
			("Result", "std/result"),
			("Mapped", "std/iter"),
			("Filtered", "std/iter"),
			("Take", "std/iter"),
			("Drop", "std/iter"),
			("ListIter", "std/iter/iterable"),
			("Bound", "std/range"),
			("Range", "std/range"),
			("RangeFrom", "std/range"),
			("RangeTo", "std/range"),
			("RangeInclusive", "std/range"),
			("RangeToInclusive", "std/range"),
		];

		for (type_name, owner) in expected {
			assert_eq!(owners.get(type_name), Some(&owner), "owner for {type_name}");
		}
	}
}
