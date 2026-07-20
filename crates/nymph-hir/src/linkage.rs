//! The external-linkage registry (Gap 3, L0/L1): a table mapping an
//! `external(name)` MARKER (the identifier a stdlib `.nym` declaration writes
//! inside the `external(..)` parens — e.g. `external(length) func length():
//! uint`) to the real JS module specifier + exported symbol name that
//! implements it.
//!
//! This is the one shared home both `nymph-sema` (the lowering gate and the
//! `HirExpr::ExternCall` construction site) and `nymph-codegen` (the emit-time
//! `symbol(...)` call + `import` synthesis) consult — `nymph-hir` is a leaf
//! crate (deps: `ecow`, `rustc-hash` only) that both already depend on, so it
//! needs no new dependency edge in either direction.
//!
//! # Key: marker name, RECEIVER-TAG-DISAMBIGUATED where needed
//!
//! L0 keyed this registry by bare marker name alone, on the premise that
//! every stdlib collection used a distinct marker name per intrinsic. L1
//! breaks that premise: `list.nym` (both its `mut #[T]` and plain `#[T]`
//! impls) AND `map.nym`'s `mut #{K: V}` impl all declare their OWN
//! `external(get)` — same bare marker, three DIFFERENT JS implementations
//! (`list.ts`'s `get` vs a future `map.ts`'s `get`). A bare-name lookup would
//! mislink `map`'s `get` to `list`'s the instant `get` gained ANY entry, so
//! [`lookup`] now also takes the caller's RECEIVER TAG (the same tag
//! `nymph-sema`'s `inherent_self_type_tag` already computes to key a
//! materialized prelude method's mangled name — `"list"`/`"mut_list"`/
//! `"map"`/`"mut_map"`/… ) and an entry only matches when its own
//! [`Linked::receiver_tag`] is either `None` (an UNAMBIGUOUS marker, safe
//! against any receiver — `length`/`first`/`last`/`pop` today, each declared
//! by exactly one collection) or exactly equal to the caller's tag (an
//! AMBIGUOUS marker like `get`, disambiguated per receiver).
//!
//! `List`'s `mut #[T]` and plain `#[T]` impls both declare `get` against the
//! SAME `list.ts` symbol (mutability doesn't change how reading an element
//! works), so `get` gets two registry rows — `"list"` and `"mut_list"` — both
//! pointing at `list.ts`'s `get`; `Map`'s `get` is deliberately NOT
//! registered at all yet (no `map.ts` runtime exists), so a `Map` receiver's
//! `get` keeps failing `lookup` and stays a loud external defer, exactly as
//! before this slice — see `nymph-codegen/tests/run_node.rs`'s
//! `real_map_get_stays_a_loud_external_defer`.
use rustc_hash::FxHashMap;

/// Where an `external(name)` marker is actually implemented in real JS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Linked {
	/// The module specifier a `symbol(...)` call must be imported from (e.g.
	/// `"std/collections/list"`) — the specifier `nymph-codegen`'s emitted
	/// `import { .. } from ".."` line names, and the specifier
	/// `nymph-compiler`'s bundle injection registers a virtual module under.
	pub module: &'static str,
	/// The exported JS symbol name to call, `$_this`-first (e.g. `"length"`,
	/// so `xs.length()` lowers to a call shaped `length(xs)`).
	pub symbol: &'static str,
	/// `None` for an UNAMBIGUOUS marker (matches any receiver — safe because
	/// exactly one collection declares it). `Some(tag)` for an AMBIGUOUS
	/// marker shared by multiple receiver types with DIFFERENT JS
	/// implementations — only matches a [`lookup`] whose caller-supplied tag
	/// is exactly `tag` (mirrors `nymph-sema`'s `inherent_self_type_tag`:
	/// `"list"`/`"mut_list"`/`"map"`/`"mut_map"`/the six primitive tags/…).
	pub receiver_tag: Option<&'static str>,
}

/// The linkage table. L0 seeded `length` (`stdlib/src/collections/list.ts`'s
/// `export const length = ($_this) => $_this.length`) — a plain `uint`/JS
/// `number` return needing no `Option`/`Result` runtime ABI. L1 adds the
/// Option-returning `List` intrinsics (`get`/`first`/`last`/`pop`) now that
/// the Option ABI seam is wired (see `nymph-codegen::strip_ts_to_js`'s
/// `import_rewrites` and `nymph-compiler::intrinsics`'s injected
/// `std/option` virtual module) — `get` alone needs `receiver_tag`
/// disambiguation (see this module's own doc comment); `first`/`last`/`pop`
/// are `List`-only in the real stdlib today, so they stay unambiguous.
///
/// L2 links the REST of `list.nym`'s markers. Non-`mut`, non-collision
/// markers (`slice`/`sorted`/`chunked`/`distinct`/`splice`; `push` on the
/// `mut` side) stay `receiver_tag: None`, exactly like `length` — no other
/// stdlib collection/primitive declares the same bare marker name today.
/// Every OTHER new row needs `Some(tag)` because a second, DIFFERENT prelude
/// impl declares the identical bare marker against a different JS
/// implementation (see this module's own doc comment on why a bare-name
/// lookup would mislink):
/// - `concat`/`reversed`/`drop`/`take`/`contains` collide with
///   `string.nym`'s inherent `impl string { .. }` (tag `"string"`), so each
///   needs `Some("list")` to stay `List`-only.
/// - `insert`/`clear`/`remove` collide with `map.nym`'s `impl<K,V> mut
///   #{K:V} { .. }` (tag `"mut_map"`), so each needs `Some("mut_list")`.
/// - `to_string` (the marker behind `Into<string> for #[T]`'s `into`)
///   collides with `map.nym`'s own `Into<string> for #{K:V}`'s `to_string`
///   (tag `"map"`), so it needs `Some("list")` too — registered for
///   forward-safety even though no path calls `#[T]::into()` today (a
///   direct `.into()` is a checker error, and string interpolation bypasses
///   `into` entirely; see `nymph-compiler/tests/std_linkage.rs`).
pub const REGISTRY: &[(&str, Linked)] = &[
	(
		"length",
		Linked {
			module: "std/collections/list",
			symbol: "length",
			receiver_tag: None,
		},
	),
	(
		"get",
		Linked {
			module: "std/collections/list",
			symbol: "get",
			receiver_tag: Some("list"),
		},
	),
	(
		"get",
		Linked {
			module: "std/collections/list",
			symbol: "get",
			receiver_tag: Some("mut_list"),
		},
	),
	(
		"first",
		Linked {
			module: "std/collections/list",
			symbol: "first",
			receiver_tag: None,
		},
	),
	(
		"last",
		Linked {
			module: "std/collections/list",
			symbol: "last",
			receiver_tag: None,
		},
	),
	(
		"pop",
		Linked {
			module: "std/collections/list",
			symbol: "pop",
			receiver_tag: None,
		},
	),
	(
		"slice",
		Linked {
			module: "std/collections/list",
			symbol: "slice",
			receiver_tag: None,
		},
	),
	(
		"sorted",
		Linked {
			module: "std/collections/list",
			symbol: "sorted",
			receiver_tag: None,
		},
	),
	(
		"chunked",
		Linked {
			module: "std/collections/list",
			symbol: "chunked",
			receiver_tag: None,
		},
	),
	(
		"distinct",
		Linked {
			module: "std/collections/list",
			symbol: "distinct",
			receiver_tag: None,
		},
	),
	(
		"concat",
		Linked {
			module: "std/collections/list",
			symbol: "concat",
			receiver_tag: Some("list"),
		},
	),
	(
		"reversed",
		Linked {
			module: "std/collections/list",
			symbol: "reversed",
			receiver_tag: Some("list"),
		},
	),
	(
		"drop",
		Linked {
			module: "std/collections/list",
			symbol: "drop",
			receiver_tag: Some("list"),
		},
	),
	(
		"take",
		Linked {
			module: "std/collections/list",
			symbol: "take",
			receiver_tag: Some("list"),
		},
	),
	(
		"contains",
		Linked {
			module: "std/collections/list",
			symbol: "contains",
			receiver_tag: Some("list"),
		},
	),
	(
		"to_string",
		Linked {
			module: "std/collections/list",
			symbol: "to_string",
			receiver_tag: Some("list"),
		},
	),
	(
		"push",
		Linked {
			module: "std/collections/list",
			symbol: "push",
			receiver_tag: None,
		},
	),
	(
		"splice",
		Linked {
			module: "std/collections/list",
			symbol: "splice",
			receiver_tag: None,
		},
	),
	(
		"insert",
		Linked {
			module: "std/collections/list",
			symbol: "insert",
			receiver_tag: Some("mut_list"),
		},
	),
	(
		"clear",
		Linked {
			module: "std/collections/list",
			symbol: "clear",
			receiver_tag: Some("mut_list"),
		},
	),
	(
		"remove",
		Linked {
			module: "std/collections/list",
			symbol: "remove",
			receiver_tag: Some("mut_list"),
		},
	),
	// Free-function externals (the print/io slice): a top-level `external`
	// func (no enclosing `impl`, so no receiver at all) links exactly like
	// any other marker, just with `receiver_tag: None` — `lookup(marker,
	// None)` (a top-level `Declaration::ExternalFunc` has no receiver-type
	// context to supply) matches it directly. `symbol == marker` here
	// (`print` -> `print`), unlike the `$_this`-first collection symbols
	// above; `stdlib/src/io.ts`'s exports are receiver-less by construction.
	(
		"print",
		Linked {
			module: "std/io",
			symbol: "print",
			receiver_tag: None,
		},
	),
	(
		"println",
		Linked {
			module: "std/io",
			symbol: "println",
			receiver_tag: None,
		},
	),
	// L3: `map.nym`'s markers. Unlike the task-prose "non-mut/mut" split,
	// `map.nym`'s ACTUAL impl blocks put `size`/`get`/`insert`/`remove`/
	// `clear`/`get_or_insert` ALL in `impl<K,V> mut #{K:V}` (tag `"mut_map"`,
	// per `inherent_self_type_tag`'s IMPL-mutability-keyed tagging — a
	// non-mut receiver calling one of these still tags `mut_map`, since the
	// tag comes from the impl block, not the call-site receiver) —
	// `contains_key`/`keys`/`values`/`entries`/`merge` (via `Plus::plus`)
	// live in the non-mut `impl<K,V> #{K:V}` (tag `"map"`). `get`/`insert`/
	// `remove`/`clear` collide with `list.nym`'s own markers of the same
	// name (registered above under `"list"`/`"mut_list"`), so each needs
	// `Some("mut_map")` to stay `Map`-only; `size`/`get_or_insert` are
	// unique bare markers, so `None` (matches any receiver, mirroring
	// `length`). `to_string` (the marker behind `Into<string> for
	// #{K:V}`'s `into`) collides with `list.nym`'s own `to_string`
	// (registered `Some("list")` above), so it needs `Some("map")`.
	// `contains_key` alone serves BOTH the inherent `contains_key` method
	// AND `Contains<Item=K> for #{K:V}`'s `contains` — both declare
	// `external(contains_key)`, so one row covers both call sites.
	(
		"get",
		Linked {
			module: "std/collections/map",
			symbol: "get",
			receiver_tag: Some("mut_map"),
		},
	),
	(
		"insert",
		Linked {
			module: "std/collections/map",
			symbol: "insert",
			receiver_tag: Some("mut_map"),
		},
	),
	(
		"remove",
		Linked {
			module: "std/collections/map",
			symbol: "remove",
			receiver_tag: Some("mut_map"),
		},
	),
	(
		"clear",
		Linked {
			module: "std/collections/map",
			symbol: "clear",
			receiver_tag: Some("mut_map"),
		},
	),
	(
		"size",
		Linked {
			module: "std/collections/map",
			symbol: "size",
			receiver_tag: None,
		},
	),
	(
		"get_or_insert",
		Linked {
			module: "std/collections/map",
			symbol: "get_or_insert",
			receiver_tag: None,
		},
	),
	(
		"contains_key",
		Linked {
			module: "std/collections/map",
			symbol: "contains_key",
			receiver_tag: None,
		},
	),
	(
		"keys",
		Linked {
			module: "std/collections/map",
			symbol: "keys",
			receiver_tag: None,
		},
	),
	(
		"values",
		Linked {
			module: "std/collections/map",
			symbol: "values",
			receiver_tag: None,
		},
	),
	(
		"entries",
		Linked {
			module: "std/collections/map",
			symbol: "entries",
			receiver_tag: None,
		},
	),
	(
		"merge",
		Linked {
			module: "std/collections/map",
			symbol: "merge",
			receiver_tag: None,
		},
	),
	(
		"to_string",
		Linked {
			module: "std/collections/map",
			symbol: "to_string",
			receiver_tag: Some("map"),
		},
	),
];

/// Look up an `external(name)` marker's linkage for a receiver whose
/// `inherent_self_type_tag` is `receiver_tag` (`None` when the caller has no
/// receiver-type context at all, e.g. a top-level `external` function outside
/// any impl block). `None` return means the marker is not yet linked FOR THIS
/// RECEIVER — callers must keep treating it as a loud defer, never silently
/// emit a call to nothing. An entry with [`Linked::receiver_tag`] set to
/// `None` matches any caller-supplied tag (an unambiguous marker); an entry
/// with a `Some` tag matches only the identical caller-supplied tag.
#[must_use]
pub fn lookup(name: &str, receiver_tag: Option<&str>) -> Option<&'static Linked> {
	REGISTRY
		.iter()
		.find(|(marker, linked)| {
			*marker == name
				&& match linked.receiver_tag {
					None => true,
					Some(tag) => receiver_tag == Some(tag),
				}
		})
		.map(|(_, linked)| linked)
}

/// Every distinct registry MODULE, each paired with the DEDUPED symbols it
/// must export — used by the driver (`nymph-compiler`) to know which virtual
/// modules to inject into the bundle graph and which symbols each one needs
/// to keep (after stripping/filtering the `.ts` source) so an unrelated,
/// still-unlinked import (e.g. `Option`, when nothing kept still references
/// it) never becomes a fatal bundle-resolution failure. Deduped because an
/// ambiguous marker like `get` now contributes MULTIPLE registry rows (one
/// per receiver tag) that all name the SAME `(module, symbol)` pair —
/// without deduping, `strip_ts_to_js`'s `keep` list would carry a duplicate
/// entry, harmless there (`Vec::contains`) but not the honest "one row per
/// real export" shape this function promises.
#[must_use]
pub fn modules() -> Vec<(&'static str, Vec<&'static str>)> {
	let mut by_module: FxHashMap<&'static str, Vec<&'static str>> = FxHashMap::default();
	for (_, linked) in REGISTRY {
		let symbols = by_module.entry(linked.module).or_default();
		if !symbols.contains(&linked.symbol) {
			symbols.push(linked.symbol);
		}
	}
	let mut out: Vec<_> = by_module.into_iter().collect();
	for (_, symbols) in &mut out {
		symbols.sort_unstable();
	}
	out.sort_unstable_by_key(|(module, _)| *module);
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn length_is_linked_to_list_ts_for_any_receiver() {
		let linked = lookup("length", Some("list")).expect("`length` must be linked in L0");
		assert_eq!(linked.module, "std/collections/list");
		assert_eq!(linked.symbol, "length");
		// Unambiguous marker — matches even a caller with no receiver context
		// or an unrelated tag, exactly like before this slice.
		assert!(lookup("length", None).is_some());
		assert!(lookup("length", Some("map")).is_some());
	}

	#[test]
	fn get_links_to_list_only_for_a_list_receiver() {
		let list = lookup("get", Some("list")).expect("`get` must be linked for a `list` receiver");
		assert_eq!(list.module, "std/collections/list");
		assert_eq!(list.symbol, "get");
		let mut_list =
			lookup("get", Some("mut_list")).expect("`get` must be linked for a `mut_list` receiver");
		assert_eq!(mut_list.module, "std/collections/list");
		assert_eq!(mut_list.symbol, "get");
	}

	#[test]
	fn get_links_to_map_only_for_a_mut_map_receiver() {
		// L3: `map.nym` declares `get` in its `impl<K,V> mut #{K:V}` block, so
		// it links under `"mut_map"` — a DIFFERENT JS implementation than
		// `list`'s `get`, disambiguated by the receiver tag (a bare-name
		// lookup would have mislinked one to the other). A non-mut `"map"`
		// receiver never reaches `get` (the impl itself is `mut`-only), and a
		// tag-less caller stays unresolved too.
		let mut_map =
			lookup("get", Some("mut_map")).expect("`get` must be linked for a `mut_map` receiver");
		assert_eq!(mut_map.module, "std/collections/map");
		assert_eq!(mut_map.symbol, "get");
		assert!(lookup("get", Some("map")).is_none());
		assert!(lookup("get", None).is_none());
	}

	#[test]
	fn an_unlinked_marker_is_none() {
		// Every collection marker declared today is now linked — retarget to
		// a genuinely still-unlinked surface (string/iterator/range) instead.
		assert!(lookup("chars", Some("string")).is_none());
		assert!(lookup("len", Some("list")).is_none());
	}

	#[test]
	fn modules_groups_by_specifier_and_dedupes_symbols() {
		let mods = modules();
		assert_eq!(
			mods,
			vec![
				(
					"std/collections/list",
					vec![
						"chunked",
						"clear",
						"concat",
						"contains",
						"distinct",
						"drop",
						"first",
						"get",
						"insert",
						"last",
						"length",
						"pop",
						"push",
						"remove",
						"reversed",
						"slice",
						"sorted",
						"splice",
						"take",
						"to_string",
					]
				),
				(
					"std/collections/map",
					vec![
						"clear",
						"contains_key",
						"entries",
						"get",
						"get_or_insert",
						"insert",
						"keys",
						"merge",
						"remove",
						"size",
						"to_string",
						"values",
					]
				),
				("std/io", vec!["print", "println"]),
			]
		);
	}

	#[test]
	fn print_and_println_are_linked_free_function_externals() {
		// A free-function external has no receiver at all — `None` must match,
		// exactly like the top-level `Declaration::ExternalFunc` call site
		// (`lookup_free_fn_external`, `nymph-sema`) always passes.
		let print = lookup("print", None).expect("`print` must be linked as a free-function external");
		assert_eq!(print.module, "std/io");
		assert_eq!(print.symbol, "print");
		let println =
			lookup("println", None).expect("`println` must be linked as a free-function external");
		assert_eq!(println.module, "std/io");
		assert_eq!(println.symbol, "println");
	}
}
