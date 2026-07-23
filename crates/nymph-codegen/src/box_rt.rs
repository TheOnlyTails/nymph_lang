//! The uniform-value-boxing runtime representation (slice #2, the keystone).
//!
//! Every Nymph primitive, list, and tuple value compiles to a *boxed value* — a per-type wrapper
//! ES class carrying its payload in `.v` and its type discriminant on its
//! prototype under the shared `Symbol.for("nymph.tag")` key (ADR-0002). This
//! module is the single source of truth for those wrapper class definitions, in
//! two forms:
//!
//! * [`BOX_MODULE_SOURCE`] — an `export`ed ES module, injected into the bundle
//!   graph under [`BOX_MODULE_KEY`] exactly like the intrinsic runtime modules
//!   (`nymph-compiler::intrinsics`). This is slice #7's "boxes must be importable
//!   modules" seam: a later slice's emitted code will `import { NInt } from
//!   "std/box"` instead of relying on the inline preamble.
//! * [`BOX_PREAMBLE`] — the identical class definitions WITHOUT `export`,
//!   prepended inline by [`crate::emit`] into any single emitted module that
//!   constructs a box. The single-module facade (`emit`/`compile`, what
//!   `run_node` drives) runs one `.mjs` directly under Node with no bundler to
//!   resolve a bare `"std/box"` specifier, so the definitions must travel inside
//!   the module itself. Cross-module identity does NOT depend on sharing one
//!   class object: the discriminant is the GLOBAL `Symbol.for("nymph.<type>")`
//!   installed on each prototype, never class identity (`instanceof`), so two
//!   independently-defined `NInt` classes in two bundled modules still report the
//!   same `x[TAG]` — the same `Symbol.for`-keyed ABI `emit_enum` already relies on
//!   for per-module `Option`.
//!
//! Collection wrappers expose the payload-level operations required by slice #6;
//! every built-in box carries the structural equals/hash protocol used by `NMap`.

/// The virtual module specifier the box wrapper classes are importable under.
/// Stable across the whole boxing branch (later slices' emitted `import`s name
/// it verbatim), and the key `nymph-compiler` injects [`BOX_MODULE_SOURCE`]
/// into the bundle graph under.
pub const BOX_MODULE_KEY: &str = "std/box";

/// The base class name every per-type wrapper extends; holds the `.v` payload.
const BASE: &str = "NBox";

const HASH_MAP_RUNTIME: &str = r#"const NYMPH_TAG = Symbol.for("nymph.tag");

function nymphHashString(value) {
	let hash = 0x811c9dc5;
	for (let i = 0; i < value.length; i++) {
		hash ^= value.charCodeAt(i);
		hash = Math.imul(hash, 0x01000193);
	}
	return hash | 0;
}

function nymphHashCombine(hash, value) {
	return Math.imul(hash ^ value, 0x01000193) | 0;
}

function nymphTagName(value) {
	return value?.[NYMPH_TAG]?.description;
}

function nymphIsPayloadBox(value) {
	return [
		"nymph.int", "nymph.uint", "nymph.float", "nymph.char",
		"nymph.bool", "nymph.string", "nymph.list", "nymph.tuple",
	].includes(nymphTagName(value));
}

function nymphEquals(left, right) {
	if (left === right) return true;
	if (left == null || right == null || typeof left !== "object" || typeof right !== "object") {
		return false;
	}
	if (left[NYMPH_TAG] !== right[NYMPH_TAG]) return false;
	if (nymphTagName(left) === "nymph.map") {
		if (left.size !== right.size) return false;
		for (const [key, value] of left) {
			if (!right.has(key) || !nymphEquals(value, right.get(key))) return false;
		}
		return true;
	}
	if (Array.isArray(left.v) && Array.isArray(right.v)) {
		return left.v.length === right.v.length && left.v.every((value, i) => nymphEquals(value, right.v[i]));
	}
	if (nymphIsPayloadBox(left) || nymphIsPayloadBox(right)) return left.v === right.v;
	if (left[NYMPH_TAG] === undefined && left.constructor !== right.constructor) return false;
	const leftKeys = Object.keys(left).sort();
	const rightKeys = Object.keys(right).sort();
	return leftKeys.length === rightKeys.length
		&& leftKeys.every((key, i) => key === rightKeys[i] && nymphEquals(left[key], right[key]));
}

function nymphHash(value) {
	if (value == null) return 0;
	if (typeof value !== "object") return nymphHashString(`${typeof value}:${String(value)}`);
	let hash = nymphHashString(nymphTagName(value) ?? value.constructor?.name ?? "object");
	if (nymphTagName(value) === "nymph.map") {
		let entriesHash = 0;
		for (const [key, entryValue] of value) {
			entriesHash = (entriesHash + nymphHashCombine(nymphKeyHash(key), nymphHash(entryValue))) | 0;
		}
		return nymphHashCombine(hash, entriesHash);
	}
	if (Array.isArray(value.v)) {
		for (const item of value.v) hash = nymphHashCombine(hash, nymphHash(item));
		return hash;
	}
	if (nymphIsPayloadBox(value)) return nymphHashCombine(hash, nymphHashString(String(value.v)));
	for (const key of Object.keys(value).sort()) {
		hash = nymphHashCombine(hash, nymphHashString(key));
		hash = nymphHashCombine(hash, nymphHash(value[key]));
	}
	return hash;
}

function nymphKeyHash(key) {
	return typeof key?.hash === "function" ? key.hash().v | 0 : nymphHash(key);
}

function nymphKeyEquals(left, right) {
	return typeof left?.equals === "function" ? left.equals(right).v : nymphEquals(left, right);
}

const HAMT_NOT_FOUND = Symbol("nymph.hamt.not_found");

function hamtPopcount(value) {
	value -= (value >>> 1) & 0x55555555;
	value = (value & 0x33333333) + ((value >>> 2) & 0x33333333);
	return (((value + (value >>> 4)) & 0x0f0f0f0f) * 0x01010101) >>> 24;
}

function hamtMerge(left, leftHash, right, rightHash, shift) {
	const leftIndex = (leftHash >>> shift) & 31;
	const rightIndex = (rightHash >>> shift) & 31;
	const leftBit = 1 << leftIndex;
	const rightBit = 1 << rightIndex;
	if (leftBit === rightBit) {
		return { kind: "bitmap", bitmap: leftBit, children: [hamtMerge(left, leftHash, right, rightHash, shift + 5)] };
	}
	return {
		kind: "bitmap",
		bitmap: leftBit | rightBit,
		children: leftIndex < rightIndex ? [left, right] : [right, left],
	};
}

function hamtGet(node, hash, key, shift) {
	if (node == null) return HAMT_NOT_FOUND;
	if (node.kind === "leaf") return node.hash === hash && nymphKeyEquals(node.key, key) ? node.value : HAMT_NOT_FOUND;
	if (node.kind === "collision") {
		if (node.hash !== hash) return HAMT_NOT_FOUND;
		const entry = node.entries.find(([candidate]) => nymphKeyEquals(candidate, key));
		return entry ? entry[1] : HAMT_NOT_FOUND;
	}
	const bit = 1 << ((hash >>> shift) & 31);
	if ((node.bitmap & bit) === 0) return HAMT_NOT_FOUND;
	const index = hamtPopcount(node.bitmap & (bit - 1));
	return hamtGet(node.children[index], hash, key, shift + 5);
}

function hamtSet(node, hash, key, value, shift) {
	if (node == null) return [{ kind: "leaf", hash, key, value }, true];
	if (node.kind === "leaf") {
		if (node.hash === hash && nymphKeyEquals(node.key, key)) {
			node.value = value;
			return [node, false];
		}
		if (node.hash === hash) {
			return [{ kind: "collision", hash, entries: [[node.key, node.value], [key, value]] }, true];
		}
		return [hamtMerge(node, node.hash, { kind: "leaf", hash, key, value }, hash, shift), true];
	}
	if (node.kind === "collision") {
		if (node.hash !== hash) {
			return [hamtMerge(node, node.hash, { kind: "leaf", hash, key, value }, hash, shift), true];
		}
		const entry = node.entries.find(([candidate]) => nymphKeyEquals(candidate, key));
		if (entry) {
			entry[1] = value;
			return [node, false];
		}
		node.entries.push([key, value]);
		return [node, true];
	}
	const bit = 1 << ((hash >>> shift) & 31);
	const index = hamtPopcount(node.bitmap & (bit - 1));
	if ((node.bitmap & bit) === 0) {
		node.bitmap |= bit;
		node.children.splice(index, 0, { kind: "leaf", hash, key, value });
		return [node, true];
	}
	const [child, inserted] = hamtSet(node.children[index], hash, key, value, shift + 5);
	node.children[index] = child;
	return [node, inserted];
}

function hamtDelete(node, hash, key, shift) {
	if (node == null) return [null, undefined, false];
	if (node.kind === "leaf") {
		return node.hash === hash && nymphKeyEquals(node.key, key)
			? [null, node.value, true]
			: [node, undefined, false];
	}
	if (node.kind === "collision") {
		if (node.hash !== hash) return [node, undefined, false];
		const index = node.entries.findIndex(([candidate]) => nymphKeyEquals(candidate, key));
		if (index < 0) return [node, undefined, false];
		const [[, value]] = node.entries.splice(index, 1);
		if (node.entries.length === 1) {
			const [[remainingKey, remainingValue]] = node.entries;
			return [{ kind: "leaf", hash, key: remainingKey, value: remainingValue }, value, true];
		}
		return [node, value, true];
	}
	const bit = 1 << ((hash >>> shift) & 31);
	if ((node.bitmap & bit) === 0) return [node, undefined, false];
	const index = hamtPopcount(node.bitmap & (bit - 1));
	const [child, value, removed] = hamtDelete(node.children[index], hash, key, shift + 5);
	if (!removed) return [node, undefined, false];
	if (child == null) {
		node.bitmap &= ~bit;
		node.children.splice(index, 1);
		if (node.children.length === 0) return [null, value, true];
		if (node.children.length === 1 && node.children[0].kind !== "bitmap") return [node.children[0], value, true];
	} else {
		node.children[index] = child;
	}
	return [node, value, true];
}

function* hamtEntries(node) {
	if (node == null) return;
	if (node.kind === "leaf") {
		yield [node.key, node.value];
		return;
	}
	if (node.kind === "collision") {
		yield* node.entries;
		return;
	}
	for (const child of node.children) yield* hamtEntries(child);
}

class NymphHamt {
	constructor(entries = []) {
		this.root = null;
		this.size = 0;
		for (const entry of entries) {
			const pair = Array.isArray(entry.v) ? entry.v : entry;
			this.set(pair[0], pair[1]);
		}
	}
	get(key) {
		const value = hamtGet(this.root, nymphKeyHash(key), key, 0);
		return value === HAMT_NOT_FOUND ? undefined : value;
	}
	has(key) { return hamtGet(this.root, nymphKeyHash(key), key, 0) !== HAMT_NOT_FOUND; }
	set(key, value) {
		const [root, inserted] = hamtSet(this.root, nymphKeyHash(key), key, value, 0);
		this.root = root;
		if (inserted) this.size++;
		return this;
	}
	delete(key) {
		const [root, , removed] = hamtDelete(this.root, nymphKeyHash(key), key, 0);
		this.root = root;
		if (removed) this.size--;
		return removed;
	}
	clear() { this.root = null; this.size = 0; }
	*entries() { yield* hamtEntries(this.root); }
	*keys() { for (const [key] of this.entries()) yield key; }
	*values() { for (const [, value] of this.entries()) yield value; }
	[Symbol.iterator]() { return this.entries(); }
}

const NYMPH_OPTION_SOME = Symbol.for("Option.Some");
const NYMPH_OPTION_NONE = Object.freeze({ [NYMPH_TAG]: Symbol.for("Option.None") });

class NymphListIterator {
	constructor(items) {
		this.items = items;
		this.index = 0;
	}
	next() {
		if (this.index >= this.items.length) return NYMPH_OPTION_NONE;
		return { [NYMPH_TAG]: NYMPH_OPTION_SOME, value: this.items[this.index++] };
	}
}

class NymphMapIterator {
	constructor(entries) {
		this.entries = entries;
	}
	next() {
		const entry = this.entries.next();
		if (entry.done) return NYMPH_OPTION_NONE;
		return { [NYMPH_TAG]: NYMPH_OPTION_SOME, value: new NTuple(entry.value) };
	}
}
"#;

/// Each built-in box wrapper: `(JS class name, the `nymph.<type>` discriminant
/// suffix)`. The class name is what emitted `new N…(…)` construction and later
/// slices' imports reference; the suffix builds the global
/// `Symbol.for("nymph.<type>")` tag installed on the class prototype (which
/// `match`/hash/display read as "what type is this?"). The string wrapper is
/// `NString` (not the prototype sketch's `NStr`) because slices #7/#8 reference
/// `NString`.
const BOX_CLASSES: &[(&str, &str)] = &[
	("NInt", "int"),
	("NUint", "uint"),
	("NFloat", "float"),
	("NChar", "char"),
	("NBool", "bool"),
	("NString", "string"),
	("NList", "list"),
	("NTuple", "tuple"),
	("NMap", "map"),
];

/// The wrapper-class JS body, shared by both emitted forms. When `export` is
/// true each declaration is `export`ed (the importable [`BOX_MODULE_SOURCE`]);
/// when false it is a bare declaration (the inline [`BOX_PREAMBLE`]). The tag is
/// installed on the prototype under `Symbol.for("nymph.tag")` written inline
/// (not via a `const TAG` binding) so the block is self-contained — it never
/// collides with, nor depends on, a module's own `const TAG` (emitted for
/// enums) and never trips `wrap_module_js`'s used-but-undeclared `[TAG]` probe.
fn class_defs(export: bool) -> String {
	let kw = if export { "export " } else { "" };
	let mut out = String::new();
	out.push_str(&format!("{kw}class {BASE} {{\n"));
	out.push_str("\tconstructor(v) {\n\t\tthis.v = v;\n\t}\n");
	out.push_str("\tequals(other) {\n\t\treturn new NBool(nymphEquals(this, other));\n\t}\n");
	out.push_str("\thash() {\n\t\treturn new NInt(nymphHash(this));\n\t}\n");
	out.push_str("}\n");
	out.push_str(HASH_MAP_RUNTIME);
	if export {
		out.push_str("export { nymphKeyEquals as protocolEquals, nymphHash as structuralHash };\n");
	}
	for (class, _) in BOX_CLASSES {
		if *class == "NMap" {
			out.push_str(&format!(
				"{kw}class {class} extends {BASE} {{\n\tconstructor(entries) {{\n\t\tsuper(new NymphHamt(entries));\n\t}}\n\tget size() {{ return this.v.size; }}\n\tget(key) {{ return this.v.get(key); }}\n\thas(key) {{ return this.v.has(key); }}\n\tset(key, value) {{ this.v.set(key, value); return this; }}\n\tdelete(key) {{ return this.v.delete(key); }}\n\tclear() {{ this.v.clear(); }}\n\tkeys() {{ return this.v.keys(); }}\n\tvalues() {{ return this.v.values(); }}\n\tentries() {{ return this.v.entries(); }}\n\titer() {{ return new NymphMapIterator(this.v.entries()); }}\n\t[Symbol.iterator]() {{ return this.v[Symbol.iterator](); }}\n}}\n"
			));
		} else if *class == "NTuple" {
			// Native Map's constructor reads pair entries through numeric properties.
			// Keep that boundary compatible while the tuple's canonical storage stays `.v`.
			out.push_str(&format!(
				"{kw}class {class} extends {BASE} {{\n\tindex(key) {{\n\t\treturn this.v[key.v];\n\t}}\n\tget 0() {{\n\t\treturn this.v[0];\n\t}}\n\tget 1() {{\n\t\treturn this.v[1];\n\t}}\n}}\n"
			));
		} else if *class == "NList" {
			out.push_str(&format!(
				"{kw}class {class} extends {BASE} {{\n\tindex(key) {{\n\t\treturn this.v[key.v];\n\t}}\n\titer() {{\n\t\treturn new NymphListIterator(this.v);\n\t}}\n}}\n"
			));
		} else {
			out.push_str(&format!("{kw}class {class} extends {BASE} {{}}\n"));
		}
	}
	for (class, tag) in BOX_CLASSES {
		out.push_str(&format!(
			"{class}.prototype[Symbol.for(\"nymph.tag\")] = Symbol.for(\"nymph.{tag}\");\n"
		));
	}
	out
}

/// The inline, non-`export`ed box class definitions prepended into a single
/// emitted module that constructs a box (see the module docs and [`crate::emit`]).
#[must_use]
pub fn box_preamble() -> String {
	class_defs(false)
}

/// The `export`ed `std/box` runtime module source, injected into the bundle
/// graph under [`BOX_MODULE_KEY`] (see the module docs).
#[must_use]
pub fn box_module_source() -> String {
	class_defs(true)
}

/// The JS class name a [`nymph_hir::hir::NumKind`] boxes as (`NInt`/`NUint`/
/// `NFloat`). Panics on [`nymph_hir::hir::NumKind::Raw`], which is never a boxed
/// value — the caller must handle the raw case before reaching here.
#[must_use]
pub fn num_box_class(kind: nymph_hir::hir::NumKind) -> &'static str {
	use nymph_hir::hir::NumKind;
	match kind {
		NumKind::Int => "NInt",
		NumKind::UInt => "NUint",
		NumKind::Float => "NFloat",
		NumKind::Raw => {
			unreachable!("NumKind::Raw is an unboxed internal number, not a box class")
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn box_module_exports_every_wrapper_class() {
		let src = box_module_source();
		for (class, _) in BOX_CLASSES {
			assert!(
				src.contains(&format!("export class {class}")),
				"box module must export {class}, got:\n{src}"
			);
		}
		assert!(
			src.contains(&format!("export class {BASE}")),
			"box module must export the base class:\n{src}"
		);
	}

	#[test]
	fn preamble_defines_wrappers_without_export() {
		let src = box_preamble();
		assert!(!src.contains("export"), "preamble must not export:\n{src}");
		assert!(src.contains("class NInt extends NBox"), "{src}");
		assert!(src.contains("class NString extends NBox"), "{src}");
	}

	#[test]
	fn each_wrapper_installs_its_global_type_tag_on_the_prototype() {
		let src = box_module_source();
		for (class, tag) in BOX_CLASSES {
			assert!(
				src.contains(&format!(
					"{class}.prototype[Symbol.for(\"nymph.tag\")] = Symbol.for(\"nymph.{tag}\");"
				)),
				"{class} must install its nymph.{tag} tag:\n{src}"
			);
		}
	}
}
