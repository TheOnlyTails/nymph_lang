//! The uniform-value-boxing runtime representation (slice #2, the keystone).
//!
//! Every Nymph primitive value compiles to a *boxed value* — a per-type wrapper
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
//! Per the slice, the wrappers are BARE: no `plus`/`equals`/`display` methods yet
//! — those are lowered from the `impl`s targeting each type in later slices
//! (#10a arithmetic, #6 equals/hash, #8 display). #2 is representation + literal
//! boxing only.

/// The virtual module specifier the box wrapper classes are importable under.
/// Stable across the whole boxing branch (later slices' emitted `import`s name
/// it verbatim), and the key `nymph-compiler` injects [`BOX_MODULE_SOURCE`]
/// into the bundle graph under.
pub const BOX_MODULE_KEY: &str = "std/box";

/// The base class name every per-type wrapper extends; holds the `.v` payload.
const BASE: &str = "NBox";

/// Each primitive box wrapper: `(JS class name, the `nymph.<type>` discriminant
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
	out.push_str("}\n");
	for (class, _) in BOX_CLASSES {
		out.push_str(&format!("{kw}class {class} extends {BASE} {{}}\n"));
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
