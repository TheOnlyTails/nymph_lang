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
//!   (`nymph-compiler::HostRuntimeGraph`). This is slice #7's "boxes must be importable
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

/// TypeScript declarations for compiler-provided virtual runtime modules.
/// Internal stdlib adapters and downstream consumers use this same source.
pub const BOX_MODULE_DECLARATIONS: &str = include_str!("../../../stdlib/src/virtual-modules.d.ts");

/// The base class name every per-type wrapper extends; holds the `.v` payload.
const BASE: &str = "NBox";

const HASH_MAP_RUNTIME: &str = include_str!("./hashmap_runtime.js");

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
fn class_defs(export: bool, option_enum_name: &str) -> String {
	let kw = if export { "export " } else { "" };
	let mut out = String::new();
	out.push_str(&format!("{kw}class {BASE} {{\n"));
	out.push_str("\tconstructor(v) {\n\t\tthis.v = v;\n\t}\n");
	out.push_str("\tequals(other) {\n\t\treturn new NBool(nymphEquals(this, other));\n\t}\n");
	out.push_str("\thash() {\n\t\treturn new NInt(nymphHash(this));\n\t}\n");
	out.push_str("\tdisplay() {\n\t\treturn new NString(nymphDisplay(this));\n\t}\n");
	out.push_str("\tdebug() {\n\t\treturn new NString(nymphDebug(this));\n\t}\n");
	out.push_str("\ttoString() {\n\t\treturn this.debug().v;\n\t}\n");
	out.push_str("}\n");
	out.push_str(&format!(
		"const NYMPH_OPTION_ENUM_NAME = \"{option_enum_name}\";\n"
	));
	out.push_str(HASH_MAP_RUNTIME);
	if export {
		out.push_str("export { NymphRange, nymphKeyEquals as protocolEquals, nymphHash as structuralHash, nymphDisplay as structuralDisplay, nymphDebug as structuralDebug, nymphProtocolDisplay, nymphProtocolDebug };\n");
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
				"{kw}class {class} extends {BASE} {{\n\tindex(key) {{\n\t\treturn this.v[key.v];\n\t}}\n\tpush(item) {{\n\t\tthis.v.push(item);\n\t}}\n\titer() {{\n\t\treturn new NymphListIterator(this.v);\n\t}}\n}}\n"
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
	class_defs(false, "Option")
}

/// The `export`ed `std/box` runtime module source, injected into the bundle
/// graph under [`BOX_MODULE_KEY`] (see the module docs).
#[must_use]
pub fn box_module_source() -> String {
	class_defs(true, "Option")
}

/// The importable box runtime using the exact emitted compiler-Option binding
/// as its native iterator discriminant namespace.
#[must_use]
pub fn box_module_source_with_option_enum(option_enum_name: &str) -> String {
	class_defs(true, option_enum_name)
}

/// The declaration source paired with [`box_module_source`].
#[must_use]
pub const fn box_module_declarations() -> &'static str {
	BOX_MODULE_DECLARATIONS
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
	fn box_declarations_cover_every_runtime_export() {
		let declarations = box_module_declarations();
		for name in [
			"NBox",
			"NInt",
			"NUint",
			"NFloat",
			"NChar",
			"NBool",
			"NString",
			"NList",
			"NTuple",
			"NMap",
			"protocolEquals",
			"structuralHash",
			"structuralDisplay",
			"structuralDebug",
		] {
			assert!(
				declarations.contains(&format!("export class {name}"))
					|| declarations.contains(&format!("export function {name}")),
				"virtual declarations must export {name}:\n{declarations}"
			);
		}
	}

	#[test]
	fn virtual_declarations_name_the_runtime_module() {
		assert!(box_module_declarations().contains("declare module \"std/box\""));
	}

	#[test]
	fn every_wrapper_to_string_forwards_to_structural_debug() {
		let src = box_module_source();
		assert!(src.contains("toString()"));
		assert!(src.contains("return this.debug().v"));
		assert!(!src.contains("Symbol.toPrimitive"));
	}

	#[test]
	fn preamble_defines_wrappers_without_export() {
		let src = box_preamble();
		assert!(!src.contains("export"), "preamble must not export:\n{src}");
		assert!(src.contains("class NInt extends NBox"), "{src}");
		assert!(src.contains("class NString extends NBox"), "{src}");
	}

	#[test]
	fn list_wrapper_exposes_the_mutation_used_by_nymph_default_methods() {
		let src = box_module_source();
		assert!(src.contains("push(item)"), "{src}");
		assert!(src.contains("this.v.push(item)"), "{src}");
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
