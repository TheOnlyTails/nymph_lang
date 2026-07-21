//! Virtual "intrinsic" JS modules for every LINKED external (Gap 3, L0/L1) —
//! the runtime-source counterpart of `prelude.rs`'s embedded `.nym` checker
//! prelude, but for the real `.ts`/JS implementation a linked external's
//! emitted call actually resolves against at bundle time.
//!
//! `nymph_hir::linkage::REGISTRY` (a leaf-crate table both the sema gate and
//! codegen emit already consult) decides WHICH `external(name)` markers link,
//! and to which module specifier + exported symbol. It does NOT embed any
//! `.ts` SOURCE itself — a leaf crate (deps: `ecow`, `rustc-hash` only) must
//! not `include_str!` the stdlib tree. This module is the other half: it
//! supplies the actual embedded `.ts` source for each distinct module the
//! registry names, and strips + filters it (via `nymph_codegen::strip_ts_to_js`)
//! into the virtual sources [`Driver::compile_all`] injects into the bundle
//! graph alongside every real project/std module.
//!
//! L1 extension (the Option ABI seam): `list.ts`'s `get`/`first`/`last`/`pop`
//! all return `Option<T>`, built by calling the SAME `Option` their `.ts`
//! source imports (`import { Option } from "../option"`). Two things make
//! that import resolve, and resolve to something that INTEROPERATES with the
//! compiler's own per-module Option class:
//! 1. `IMPORT_REWRITES` tells `strip_ts_to_js` to keep that import (when a
//!    kept export still references it) and rewrite its specifier to the bare
//!    virtual key `"std/option"` — a real sources-map key `bundle::
//!    VirtualFsPlugin` can resolve (unlike the raw relative `"../option"`,
//!    which it can't — `resolve_id` only matches exact specifier strings).
//! 2. [`OPTION_MODULE_JS`] is injected under that exact key, UNCONDITIONALLY
//!    (see this function's own doc comment on why an unreferenced entry is
//!    harmless). Its `Some`/`None` build the identical `{ [TAG]: <symbol>,
//!    ..fields }` / frozen-`{ [TAG]: <symbol> }` shape `nymph-codegen`'s
//!    `emit_enum` builds for every user-written `Option` — and, since
//!    `emit_enum`'s per-variant discriminant is `Symbol.for(label)` (a
//!    GLOBAL, not `Symbol(label)`), the exact same `Symbol.for("Option.Some")`
//!    / `Symbol.for("Option.None")` values compare equal across every
//!    independently-built Option, module boundaries included — see
//!    `nymph-codegen::emit`'s `emit_enum` doc comment for the ABI itself.

use rustc_hash::FxHashMap;

/// One registry MODULE specifier's `include_str!`-embedded `.ts` source —
/// mirrors `prelude.rs`'s `CORE_SOURCES` table one level down (runtime JS,
/// not checker-facing Nymph source). Add an entry here whenever
/// `nymph_hir::linkage::REGISTRY` gains a module this table doesn't cover yet
/// — `intrinsic_module_sources` panics loudly (never silently skips) if one
/// is missing.
const INTRINSIC_TS_SOURCES: &[(&str, &str)] = &[
	(
		"std/collections/list",
		include_str!("../../../stdlib/src/collections/list.ts"),
	),
	(
		"std/collections/map",
		include_str!("../../../stdlib/src/collections/map.ts"),
	),
	// The print/io slice's free-function externals (`nymph_hir::linkage`'s
	// `"print"`/`"println"` rows) — `io.ts` has no `import` of its own, so
	// unlike `list.ts` it needs no `IMPORT_REWRITES` entry.
	("std/io", include_str!("../../../stdlib/src/io.ts")),
	// The ambient `string` methods (linked so `"…".contains(…)` etc. lower to
	// native JS). `string.ts` imports `Option` via `"./option"` (it sits at the
	// stdlib root, so `./` not `../`) — see `IMPORT_REWRITES` below.
	("std/string", include_str!("../../../stdlib/src/string.ts")),
];

/// Every relative import specifier an intrinsic `.ts` source might write,
/// paired with the bare virtual module key it resolves to in the bundle
/// graph — passed to `strip_ts_to_js` as `import_rewrites` for every module
/// in [`INTRINSIC_TS_SOURCES`]. `"../option"` is `list.ts`'s own specifier
/// for `stdlib/src/option.ts` (which doesn't exist as a real file — see
/// [`OPTION_MODULE_JS`]); a future intrinsic module needing a different
/// relative import would add its own row here.
const IMPORT_REWRITES: &[(&str, &str)] = &[
	("../option", "std/option"),
	// `string.ts` sits at the stdlib root, so its `Option` import is `./option`.
	("./option", "std/option"),
];

/// The virtual `std/option` module every Option-returning `List` intrinsic
/// (`get`/`first`/`last`/`pop`) imports as `import { Option } from
/// "std/option"` once `strip_ts_to_js` rewrites its specifier (see
/// [`IMPORT_REWRITES`]). Hand-written, not compiler-emitted from
/// `stdlib/src/option.nym` — under the global (`Symbol.for`) discriminant ABI
/// `nymph-codegen`'s `emit_enum` now uses, any independently-built `{ [TAG]:
/// Symbol.for("Option.Some"), ...fields }` / `Object.freeze({ [TAG]:
/// Symbol.for("Option.None") })` value interoperates with the compiler's OWN
/// per-module `Option` class for `match`/tag-comparison purposes — full
/// re-emission of `option.nym`'s inline methods (`is_some`/`map`/…) is not
/// needed for THAT to hold. The one thing a value built here can't do is
/// answer a METHOD call (`.is_some()`) directly, since it carries no
/// prototype — not exercised by any linked `List` intrinsic today (each only
/// ever CONSTRUCTS a `Some`/`None`, never calls a method on the result
/// itself); the constructed value round-trips through a `match` in USER code
/// exactly like any other `Option`, which is this slice's whole proof
/// obligation (see `nymph-compiler/tests/std_linkage.rs`).
const OPTION_MODULE_JS: &str = "\
const TAG = Symbol.for(\"nymph.tag\");
const SOME_TAG = Symbol.for(\"Option.Some\");
const NONE_TAG = Symbol.for(\"Option.None\");
export const Option = {
\tSome: (fields) => ({ [TAG]: SOME_TAG, ...fields }),
\tNone: Object.freeze({ [TAG]: NONE_TAG }),
};
";

/// Build the virtual module sources every LINKED external's registry module
/// needs: for each distinct module `nymph_hir::linkage::modules()` names, its
/// embedded `.ts` source stripped of TypeScript syntax and FILTERED down to
/// only the symbols that module actually links (never the full file — see
/// `nymph_codegen::strip_ts_to_js`'s doc comment for why injecting the whole
/// file is fatal to bundling: an unrelated, still-unlinked `import` inside it
/// would be a dangling specifier rolldown resolves eagerly, before
/// tree-shaking ever gets a chance to drop it), PLUS the [`OPTION_MODULE_JS`]
/// virtual module every Option-returning intrinsic's (rewritten) import
/// resolves against.
///
/// Keyed by the SAME module specifier the registry names (e.g.
/// `"std/collections/list"`) — the specifier an emitted `import { .. } from
/// ".."` line names, and what `bundle::VirtualFsPlugin` resolves module
/// sources against. Callers merge this into the driver's own
/// `module_sources` map before bundling. `"std/option"` is injected
/// UNCONDITIONALLY (not gated on whether any linked module's stripped output
/// actually references it) — mirrors `Driver::compile_all`'s own reasoning
/// for injecting every registry module unconditionally: `VirtualFsPlugin`
/// only loads a source when something actually imports it, and rolldown
/// tree-shakes an unreferenced one away regardless, so an unused entry costs
/// nothing.
#[must_use]
pub(crate) fn intrinsic_module_sources() -> FxHashMap<String, String> {
	let mut sources: FxHashMap<String, String> = nymph_hir::linkage::modules()
		.into_iter()
		.map(|(module, symbols)| {
			let source = INTRINSIC_TS_SOURCES
				.iter()
				.find(|(specifier, _)| *specifier == module)
				.unwrap_or_else(|| {
					panic!(
						"intrinsic_module_sources: no `.ts` source registered in \
						 INTRINSIC_TS_SOURCES for linkage module `{module}` — every \
						 module nymph_hir::linkage::REGISTRY names needs one"
					)
				})
				.1;
			(
				module.to_string(),
				nymph_codegen::strip_ts_to_js(source, &symbols, IMPORT_REWRITES),
			)
		})
		.collect();
	sources.insert("std/option".to_string(), OPTION_MODULE_JS.to_string());
	// Uniform value boxing (slice #2): the importable `std/box` runtime module
	// carrying the primitive wrapper classes (`NInt`/`NString`/…). Injected
	// UNCONDITIONALLY, for the same reason as `std/option` above — `VirtualFsPlugin`
	// only loads a source when something imports it, and rolldown tree-shakes an
	// unreferenced one away. Slice #2's emit inlines the wrapper definitions per
	// module (`nymph_codegen::box_preamble`) rather than importing them, so nothing
	// references this yet; it exists so slice #7's emitted `import { NInt } from
	// "std/box"` resolves against a real bundle-graph module.
	sources.insert(
		nymph_codegen::BOX_MODULE_KEY.to_string(),
		nymph_codegen::box_module_source(),
	);
	sources
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn injects_a_list_module_with_every_linked_symbol_and_a_resolvable_option_import() {
		let sources = intrinsic_module_sources();
		let list_js = sources
			.get("std/collections/list")
			.expect("expected the linked-symbol registry module to be injected");
		for symbol in ["length", "get", "first", "last", "pop"] {
			assert!(
				list_js.contains(symbol),
				"expected the linked `{symbol}` export to survive stripping, got:\n{list_js}"
			);
		}
		// `get`/`first`/`last`/`pop` all construct `Option.Some`/`Option.None`
		// — the import must survive, rewritten to the injected virtual
		// `std/option` key (never the original, unresolvable `"../option"`).
		assert!(
			list_js.contains("import { Option } from \"std/option\";"),
			"expected the `Option` import to survive, rewritten to `std/option`, got:\n{list_js}"
		);
		assert!(
			!list_js.contains("\"../option\""),
			"expected the original, unresolvable `../option` specifier to be gone, got:\n{list_js}"
		);
	}

	#[test]
	fn injects_a_map_module_with_every_linked_symbol_and_a_resolvable_option_import() {
		let sources = intrinsic_module_sources();
		let map_js = sources
			.get("std/collections/map")
			.expect("expected the linked-symbol registry module to be injected");
		for symbol in [
			"size",
			"get",
			"insert",
			"remove",
			"clear",
			"get_or_insert",
			"contains_key",
			"keys",
			"values",
			"entries",
			"merge",
			"to_string",
		] {
			assert!(
				map_js.contains(symbol),
				"expected the linked `{symbol}` export to survive stripping, got:\n{map_js}"
			);
		}
		// `get`/`remove` both construct `Option.Some`/`Option.None` — the
		// import must survive, rewritten to the injected virtual `std/option`
		// key (never the original, unresolvable `"../option"`).
		assert!(
			map_js.contains("import { Option } from \"std/option\";"),
			"expected the `Option` import to survive, rewritten to `std/option`, got:\n{map_js}"
		);
		assert!(
			!map_js.contains("\"../option\""),
			"expected the original, unresolvable `../option` specifier to be gone, got:\n{map_js}"
		);
		// L3's ABI fix: `get`/`remove` must build `Option.Some({ value: .. })`
		// — a named-field object, not a bare positional value — to
		// interoperate with the checker's generated `Some(value)` pattern
		// binding (see `map.ts`'s own doc comment / L1's `list.ts` template).
		assert!(
			map_js.contains("Option.Some({ value:") || map_js.contains("Option.Some({ value }"),
			"expected `get`/`remove` to build a named-field `Option.Some`, got:\n{map_js}"
		);
	}

	#[test]
	fn injects_the_option_module_with_globally_tagged_some_and_none() {
		let sources = intrinsic_module_sources();
		let option_js = sources
			.get("std/option")
			.expect("expected a virtual `std/option` module to be injected");
		assert!(
			option_js.contains("Symbol.for(\"Option.Some\")")
				&& option_js.contains("Symbol.for(\"Option.None\")"),
			"expected globally-tagged (`Symbol.for`, not bare `Symbol`) variant \
			 discriminants matching `nymph-codegen::emit_enum`'s own ABI, got:\n{option_js}"
		);
	}
}
