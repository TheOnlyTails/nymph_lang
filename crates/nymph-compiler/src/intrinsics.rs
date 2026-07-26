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
//! that import resolve to the compiler's canonical, source-derived Option:
//! 1. `IMPORT_REWRITES` tells `strip_ts_to_js` to keep that import (when a
//!    kept export still references it) and rewrite its specifier to the bare
//!    virtual key `"std/option"` — a real sources-map key `bundle::
//!    VirtualFsPlugin` can resolve (unlike the raw relative `"../option"`,
//!    which it can't — `resolve_id` only matches exact specifier strings).
//! 2. `Driver::compile_all` emits the demanded `option.nym` implementation
//!    once under that key. Intrinsics and source consumers therefore share
//!    both global variant tags and the same method-bearing prototype.

use std::sync::OnceLock;

use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;

/// One registry MODULE specifier's `include_str!`-embedded `.ts` source —
/// mirrors `prelude.rs`'s `CORE_SOURCES` table one level down (runtime JS,
/// not checker-facing Nymph source). Add an entry here whenever
/// `nymph_hir::linkage::REGISTRY` gains a module this table doesn't cover yet
/// — `intrinsic_module_sources` panics loudly (never silently skips) if one
/// is missing.
const INTRINSIC_TS_SOURCES: &[(&str, &str)] = &[
	(
		"std/display",
		include_str!("../../../stdlib/src/display.ts"),
	),
	(
		"std/equality",
		include_str!("../../../stdlib/src/ops/equality.ts"),
	),
	(
		"std/comparison",
		include_str!("../../../stdlib/src/ops/comparison.ts"),
	),
	("std/hash", include_str!("../../../stdlib/src/hash.ts")),
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
	(
		"std/math/intrinsics",
		include_str!("../../../stdlib/src/math/mod.ts"),
	),
	// The ambient `string` methods (linked so `"…".contains(…)` etc. lower to
	// native JS). `string.ts` imports `Option` via `"./option"` (it sits at the
	// stdlib root, so `./` not `../`) — see `IMPORT_REWRITES` below.
	("std/string", include_str!("../../../stdlib/src/string.ts")),
];

/// Every relative import specifier an intrinsic `.ts` source might write,
/// paired with the bare virtual module key it resolves to in the bundle
/// graph — passed to `strip_ts_to_js` as `import_rewrites` for every module
/// in [`INTRINSIC_TS_SOURCES`]. `"../option"` is `list.ts`'s own relative
/// specifier; the project compiler supplies its source-derived runtime owner.
/// A future intrinsic module needing a different relative import would add
/// its own row here.
const IMPORT_REWRITES: &[(&str, &str)] = &[
	("std/option", "std/option"),
	("std/box", "std/box"),
	("./display", "std/display"),
];

static INTRINSIC_MODULE_SOURCES: OnceLock<FxHashMap<String, String>> = OnceLock::new();

/// Build the virtual module sources every LINKED external's registry module
/// needs: for each distinct module `nymph_hir::linkage::modules()` names, its
/// embedded `.ts` source stripped of TypeScript syntax and FILTERED down to
/// only the symbols that module actually links (never the full file — see
/// `nymph_codegen::strip_ts_to_js`'s doc comment for why injecting the whole
/// file is fatal to bundling: an unrelated, still-unlinked `import` inside it
/// would be a dangling specifier rolldown resolves eagerly, before
/// tree-shaking ever gets a chance to drop it). The project driver separately
/// supplies the canonical `std/option` module referenced by rewritten imports.
///
/// Keyed by the SAME module specifier the registry names (e.g.
/// `"std/collections/list"`) — the specifier an emitted `import { .. } from
/// ".."` line names, and what `bundle::VirtualFsPlugin` resolves module
/// sources against. Callers merge this into the driver's own
/// `module_sources` map before bundling. `VirtualFsPlugin` only loads a source
/// when something imports it, and rolldown tree-shakes unreferenced entries.
#[must_use]
pub(crate) fn intrinsic_module_sources() -> FxHashMap<String, String> {
	INTRINSIC_MODULE_SOURCES
		.get_or_init(build_intrinsic_module_sources)
		.clone()
}

fn build_intrinsic_module_sources() -> FxHashMap<String, String> {
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
	// Uniform value boxing (slice #2): the importable `std/box` runtime module
	// carrying the primitive wrapper classes (`NInt`/`NString`/…). Injected
	// UNCONDITIONALLY, like the registry modules above — `VirtualFsPlugin`
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

/// Core runtime types named by imports retained in intrinsic JS. This is
/// derived from stripped output rather than a handwritten Option/Result list,
/// so adding another source-level intrinsic import automatically creates the
/// corresponding canonical declaration demand.
pub(crate) fn runtime_type_imports(
	sources: &FxHashMap<String, String>,
	owners: &FxHashMap<ecow::EcoString, &'static str>,
) -> FxHashSet<ecow::EcoString> {
	let mut demands = FxHashSet::default();
	let canonical_specifiers: FxHashSet<_> = owners.values().copied().collect();
	for source in sources.values() {
		for line in source.lines() {
			let line = line.trim();
			let Some((_, quoted_specifier)) = line.rsplit_once(" from ") else {
				continue;
			};
			let quoted_specifier = quoted_specifier.trim_end_matches(';');
			let specifier = quoted_specifier
				.strip_prefix('"')
				.and_then(|specifier| specifier.strip_suffix('"'))
				.or_else(|| {
					quoted_specifier
						.strip_prefix('\'')
						.and_then(|specifier| specifier.strip_suffix('\''))
				});
			let Some(specifier) = specifier else {
				continue;
			};
			if !canonical_specifiers.contains(specifier) {
				continue;
			}
			if !(line.starts_with("import {") && line.ends_with("\";")) {
				panic!("malformed retained canonical runtime import: `{line}`");
			}
			let Some((bindings, specifier)) = line.split_once("} from \"") else {
				panic!("malformed retained canonical runtime import: `{line}`");
			};
			let specifier = specifier.trim_end_matches(';').trim_end_matches('"');
			for binding in bindings
				.trim_start()
				.trim_start_matches("import {")
				.split(',')
			{
				let name = binding.trim();
				let Some((canonical, _)) = owners
					.iter()
					.find(|(candidate, owner)| candidate.as_str() == name && **owner == specifier)
				else {
					panic!(
						"unsupported retained canonical runtime import binding `{name}` from `{specifier}`"
					);
				};
				demands.insert((*canonical).clone());
			}
		}
	}
	demands
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
		assert!(
			list_js.contains("from \"std/box\""),
			"list results must use the canonical box module: {list_js}"
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
		// import must survive, rewritten to the canonical `std/option`
		// key (never the original, unresolvable `"../option"`).
		assert!(
			map_js.contains("import { Option } from \"std/option\";"),
			"expected the `Option` import to survive, rewritten to `std/option`, got:\n{map_js}"
		);
		assert!(
			!map_js.contains("\"../option\""),
			"expected the original, unresolvable `../option` specifier to be gone, got:\n{map_js}"
		);
		assert!(
			map_js.contains("from \"std/box\""),
			"map results must use the canonical box module: {map_js}"
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
	fn injects_the_hash_intrinsic() {
		let sources = intrinsic_module_sources();
		let hash_js = sources
			.get("std/hash")
			.expect("expected the hash runtime module to be injected");
		assert!(hash_js.contains("export const hash"), "{hash_js}");
		assert!(
			hash_js.contains("from \"std/box\""),
			"hash must share the box runtime's structural implementation: {hash_js}"
		);
	}

	#[test]
	fn does_not_fabricate_the_canonical_option_module() {
		let sources = intrinsic_module_sources();
		assert!(
			!sources.contains_key("std/option"),
			"the project compiler must be the sole owner of canonical std/option"
		);
	}

	#[test]
	#[should_panic(expected = "malformed retained canonical runtime import")]
	fn malformed_retained_canonical_runtime_import_panics_loudly() {
		let owners = crate::prelude::core_runtime_type_owners();
		let sources = FxHashMap::from_iter([(
			"intrinsic".to_string(),
			"import { Option } from 'std/option';\n".to_string(),
		)]);

		let _ = runtime_type_imports(&sources, owners);
	}

	#[test]
	#[should_panic(expected = "unsupported retained canonical runtime import binding")]
	fn aliased_retained_canonical_runtime_import_panics_loudly() {
		let owners = crate::prelude::core_runtime_type_owners();
		let sources = FxHashMap::from_iter([(
			"intrinsic".to_string(),
			"import { Option as O } from \"std/option\";\n".to_string(),
		)]);

		let _ = runtime_type_imports(&sources, owners);
	}

	#[test]
	#[should_panic(expected = "unsupported retained canonical runtime import binding")]
	fn empty_retained_canonical_runtime_import_binding_panics_loudly() {
		let owners = crate::prelude::core_runtime_type_owners();
		let sources = FxHashMap::from_iter([(
			"intrinsic".to_string(),
			"import { Option, } from \"std/option\";\n".to_string(),
		)]);

		let _ = runtime_type_imports(&sources, owners);
	}

	#[test]
	#[should_panic(expected = "unsupported retained canonical runtime import binding")]
	fn unknown_retained_canonical_runtime_import_name_panics_loudly() {
		let owners = crate::prelude::core_runtime_type_owners();
		let sources = FxHashMap::from_iter([(
			"intrinsic".to_string(),
			"import { NotAType } from \"std/option\";\n".to_string(),
		)]);

		let _ = runtime_type_imports(&sources, owners);
	}

	#[test]
	fn unsupported_noncanonical_intrinsic_import_is_ignored() {
		let owners = crate::prelude::core_runtime_type_owners();
		let sources = FxHashMap::from_iter([(
			"intrinsic".to_string(),
			"import { value as alias, } from \"ordinary/intrinsic\";\n".to_string(),
		)]);

		assert!(runtime_type_imports(&sources, owners).is_empty());
	}

	#[test]
	fn malformed_noncanonical_import_containing_a_runtime_owner_is_ignored() {
		let owners = crate::prelude::core_runtime_type_owners();
		let sources = FxHashMap::from_iter([(
			"intrinsic".to_string(),
			"import { Option } from 'vendor/std/option';\n".to_string(),
		)]);

		assert!(runtime_type_imports(&sources, owners).is_empty());
	}
}
