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
const LIST_RUNTIME: &str = include_str!("./list_runtime.js");
const ECHO_RUNTIME: &str = include_str!("./echo_runtime.js");
const ACTIVATION_RUNTIME: &str = include_str!("./activation_runtime.js");
const TASK_RUNTIME: &str = include_str!("./task_runtime.js");
#[cfg(test)]
const TASK_RUNTIME_TESTS: &str = include_str!("./task_runtime.test.js");

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
fn class_defs(export: bool, option_enum_name: &str, echo: bool) -> String {
	let kw = if export { "export " } else { "" };
	let mut out = String::new();
	out.push_str(&format!("{kw}class {BASE} {{\n"));
	out.push_str("\tconstructor(v) {\n\t\tthis.v = v;\n");
	if echo {
		out.push_str("\t\tnymphEchoBoxes.add(this);\n");
	}
	out.push_str("\t}\n");
	out.push_str("}\n");
	out.push_str("const NYMPH_TYPE_INTERN = new globalThis.Map();\nconst NYMPH_TYPE_RESULT = Symbol(\"nymph.type.result\");\nconst NYMPH_TYPE_ARGUMENTS = new globalThis.WeakMap();\n");
	out.push_str("function nymphType(base, args) {\n\tif (args.length === 0) return base;\n\targs = Object.freeze([...args]);\n\tlet node = NYMPH_TYPE_INTERN.get(base);\n\tif (node === undefined) { node = new globalThis.Map(); nymphMapSet(NYMPH_TYPE_INTERN, base, node); }\n\tfor (const arg of args) { let next = node.get(arg); if (next === undefined) { next = new globalThis.Map(); nymphMapSet(node, arg, next); } node = next; }\n\tlet result = node.get(NYMPH_TYPE_RESULT);\n\tif (result === undefined) { result = Object.create(base); nymphWeakMapSet(NYMPH_TYPE_ARGUMENTS, result, args); nymphMapSet(node, NYMPH_TYPE_RESULT, result); }\n\treturn result;\n}\nfunction nymphTypeProjection(receiver, path) {\n\tlet type = Object.getPrototypeOf(receiver);\n\tfor (const index of path) { const args = NYMPH_TYPE_ARGUMENTS.get(type); if (args === undefined || index >= args.length) return NBox.prototype; type = args[index]; }\n\treturn type;\n}\n");
	out.push_str("const NYMPH_VARIANT_INTERN = new globalThis.WeakMap();\nfunction nymphVariant(type, variant) {\n\tlet variants = NYMPH_VARIANT_INTERN.get(type);\n\tif (variants === undefined) { variants = new globalThis.WeakMap(); nymphWeakMapSet(NYMPH_VARIANT_INTERN, type, variants); }\n\tlet result = variants.get(variant);\n\tif (result === undefined) { result = Object.freeze(nymphAssign(Object.create(type), variant)); nymphWeakMapSet(variants, variant, result); }\n\treturn result;\n}\n");
	out.push_str(&format!(
		"const NYMPH_OPTION_ENUM_NAME = \"{option_enum_name}\";\n"
	));
	out.push_str(LIST_RUNTIME);
	let hash_map_runtime = HASH_MAP_RUNTIME
		.replace(
			"/* NYMPH_ECHO_REGISTRIES */",
			if echo {
				"const nymphEchoBoxes = new WeakSet();\nconst nymphEchoStructuralShapes = new WeakMap();"
			} else {
				""
			},
		)
		.replace(
			"/* NYMPH_ECHO_REGISTER_STRUCTURAL */",
			if echo {
				"nymphEchoStructuralShapes.set(value, value[NYMPH_STRUCTURAL_SHAPE]);"
			} else {
				""
			},
		);
	out.push_str(&hash_map_runtime);
	if echo {
		out.push_str(ECHO_RUNTIME);
	}
	out.push_str(ACTIVATION_RUNTIME);
	out.push_str(TASK_RUNTIME);
	out.push_str(
		"const NYMPH_I64_MIN = -(1n << 63n);\nconst NYMPH_I64_MAX = (1n << 63n) - 1n;\nconst NYMPH_U64_MAX = (1n << 64n) - 1n;\n\
function nymphIntegerPayload(value, min, max, name) {\n\
\tif (typeof value === \"number\") {\n\
\t\tif (!Number.isSafeInteger(value)) throw new TypeError(`${name} payload must be an exact integer`);\n\
\t\tvalue = BigInt(value);\n\
\t}\n\
\tif (typeof value !== \"bigint\") throw new TypeError(`${name} payload must be a BigInt`);\n\
\tif (value < min || value > max) throw new RangeError(`${name} overflow`);\n\
\treturn value;\n\
}\n\
function nymphCheckedInt(value) { return nymphIntegerPayload(value, NYMPH_I64_MIN, NYMPH_I64_MAX, \"int\"); }\n\
function nymphCheckedUInt(value) { return nymphIntegerPayload(value, 0n, NYMPH_U64_MAX, \"uint\"); }\n\
function nymphTrustedInt(value) { if (typeof value !== \"bigint\") throw new TypeError(\"trusted int FFI must return BigInt\"); return nymphCheckedInt(value); }\n\
function nymphTrustedUInt(value) { if (typeof value !== \"bigint\") throw new TypeError(\"trusted uint FFI must return BigInt\"); return nymphCheckedUInt(value); }\n\
const NYMPH_OPAQUE_IDENTITY = new globalThis.WeakMap();\n\
function nymphBoxOpaque(identity, value) {\n\
\tif ((typeof value !== \"object\" || value === null) && typeof value !== \"function\") throw new TypeError(\"trusted opaque FFI must return a live reference\");\n\
\tconst box = Object.create(null);\n\
\tObject.defineProperty(box, \"v\", { value });\n\
\tNYMPH_OPAQUE_IDENTITY.set(box, identity);\n\
\treturn Object.freeze(box);\n\
}\n\
function nymphUnboxOpaque(identity, value) {\n\
\tif (NYMPH_OPAQUE_IDENTITY.get(value) !== identity) throw new TypeError(\"opaque external identity mismatch\");\n\
\treturn value.v;\n\
}\n\
function nymphCheckedShift(value, count, left) {\n\
\tif (typeof count !== \"bigint\" || count < 0n || count >= 64n) throw new RangeError(\"integer shift count must be in 0..63\");\n\
\treturn left ? value << count : value >> count;\n\
}\n\
function nymphCheckedPower(value, exponent) {\n\
\tif (typeof exponent !== \"bigint\" || exponent < 0n) throw new RangeError(\"integer exponent must be nonnegative\");\n\
\tif (exponent === 0n) return 1n;\n\
\tif (value === 0n || value === 1n) return value;\n\
\tif (value === -1n) return exponent % 2n === 0n ? 1n : -1n;\n\
\tif (exponent >= 64n) throw new RangeError(\"integer power overflow\");\n\
\treturn value ** exponent;\n\
}\n\
function nymphHostIndex(value) {\n\
\tif (typeof value !== \"bigint\" || value < 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) throw new RangeError(\"host index is out of range\");\n\
\treturn Number(value);\n\
}\n\
function nymphCollectionIndex(value, length, checked) {\n\
\tif (typeof value !== \"bigint\") throw new TypeError(\"collection index must be an integer\");\n\
\tconst normalized = value < 0n ? BigInt(length) + value : value;\n\
\tif (checked && (normalized < 0n || normalized >= BigInt(length))) throw new RangeError(\"index is outside the collection\");\n\
\treturn Number(normalized);\n\
}\n\
function nymphSliceBound(value, length, inclusive, checked) {\n\
\tif (value === null) return null;\n\
\tif (typeof value !== \"bigint\") throw new TypeError(\"slice bound must be an integer\");\n\
\tlet normalized = value < 0n ? BigInt(length) + value : value;\n\
\tconst maximum = inclusive ? BigInt(length) - 1n : BigInt(length);\n\
\tif (checked && (normalized < 0n || normalized > maximum)) throw new RangeError(\"slice bound is outside the collection\");\n\
\tif (inclusive) normalized += 1n;\n\
\treturn Number(normalized);\n\
}\n\
function nymphListSlice(value, start, end, inclusive, checked) {\n\
\tconst length = value.v.length;\n\
\tconst from = nymphSliceBound(start, length, false, checked) ?? 0;\n\
\tconst to = nymphSliceBound(end, length, inclusive, checked) ?? length;\n\
\tconst result = new NList(value.v.slice(from, to));\n\
\treturn nymphSetPrototypeOf(result, Object.getPrototypeOf(value));\n\
}\n\
function nymphStringSlice(value, start, end, inclusive, checked) {\n\
\tconst points = Array.from(value.v);\n\
\tconst from = nymphSliceBound(start, points.length, false, checked) ?? 0;\n\
\tconst to = nymphSliceBound(end, points.length, inclusive, checked) ?? points.length;\n\
\treturn new NString(points.slice(from, to).join(\"\"));\n\
}\n\
function nymphIntegerToFloat(value) {\n\
\tif (typeof value === \"number\") return value;\n\
\tif (typeof value !== \"bigint\" || value < NYMPH_I64_MIN || value > NYMPH_U64_MAX) throw new RangeError(\"integer-to-float input is out of range\");\n\
\treturn Number(value);\n\
}\n\
function nymphCheckedDivide(left, right) {\n\
\tif (typeof right === \"bigint\" && right === 0n) throw new RangeError(\"integer division by zero\");\n\
\treturn nymphIntegerToFloat(left) / nymphIntegerToFloat(right);\n\
}\n\
function nymphFloatToInteger(value, unsigned) {\n\
\tif (typeof value === \"bigint\") return unsigned ? nymphCheckedUInt(value) : nymphCheckedInt(value);\n\
\tif (typeof value !== \"number\" || !Number.isFinite(value)) throw new RangeError(\"float-to-integer conversion requires a finite value\");\n\
\tconst integer = BigInt(Math.trunc(value));\n\
\treturn unsigned ? nymphCheckedUInt(integer) : nymphCheckedInt(integer);\n\
}\n\
function nymphCharCode(value) {\n\
\tif (typeof value !== \"bigint\" || value < 0n || value > 0x10ffffn || (value >= 0xd800n && value <= 0xdfffn)) throw new RangeError(\"Invalid code point\");\n\
\treturn Number(value);\n\
}\n",
	);
	if export {
		out.push_str("export { NymphRange, nymphStructuralValue, nymphProtocolDisplay, nymphProtocolDebug, nymphProtocolDisplayStep, nymphPrintStep, nymphPrintlnStep, ");
		if echo {
			out.push_str("nymphEcho, ");
		}
		out.push_str("nymphTransactionBegin, nymphTransactionCommit, nymphTransactionRollback, nymphSetProperty, nymphDeleteProperty, nymphAssign, nymphSetPrototypeOf, nymphArraySplice, nymphArrayPush, nymphArrayPop, nymphArraySetLength, nymphMapSet, nymphWeakMapSet, nymphRuntimeClass, nymphRuntimeEnum, nymphHostIndex, nymphListSlice, nymphStringSlice, nymphFloatToInteger, nymphIntegerToFloat, nymphCheckedDivide, nymphCharCode, nymphCheckedShift, nymphCheckedPower, nymphTrustedInt, nymphTrustedUInt, nymphBoxOpaque, nymphUnboxOpaque, nymphActivate, nymphCaptureFrame, nymphCallable, nymphMarkCallable, nymphMethodStep, nymphPush, nymphTailCall, nymphTailCallMember, nymphReturn, nymphSuspend, nymphDefect, nymphResume, nymphRegisterCleanup, nymphEnterCleanupScope, nymphLeaveCleanupScope, nymphUnwindCleanupScopes, nymphCommitStateTransition, nymphTaskRecipe, nymphTaskDrive, nymphTaskSpawn, nymphHandleObserve, nymphHandleCancel, nymphCheckpoint, nymphCurrentExecutionSignal, nymphTaskSelect, nymphTaskRace, nymphStartRoot, nymphRenderDefect, nymphRunTask };\n");
	}
	for (class, _) in BOX_CLASSES {
		if *class == "NMap" {
			out.push_str(&format!(
				"{kw}class {class} extends {BASE} {{\n\tconstructor(entries) {{\n\t\tsuper(entries instanceof NymphHamt ? entries : NymphHamt.from(entries));\n\t}}\n\tget size() {{ return this.v.size; }}\n\tget(key) {{ return this.v.get(key); }}\n\thas(key) {{ return this.v.has(key); }}\n\twith(key, value) {{ return new NMap(this.v.set(key, value)); }}\n\twithout(key) {{ return new NMap(this.v.delete(key)[0]); }}\n\tkeys() {{ return this.v.keys(); }}\n\tvalues() {{ return this.v.values(); }}\n\tentries() {{ return this.v.entries(); }}\n\t[Symbol.iterator]() {{ return this.v[Symbol.iterator](); }}\n}}\n"
			));
		} else if *class == "NInt" {
			let register = if echo {
				" nymphEchoBoxes.add(value);"
			} else {
				""
			};
			out.push_str(&format!(
				"{kw}class {class} extends {BASE} {{ constructor(v) {{ super(nymphCheckedInt(v)); }} static direct(v) {{ const value = Object.create(this.prototype); value.v = v;{register} return value; }} }}\n"
			));
		} else if *class == "NUint" {
			let register = if echo {
				" nymphEchoBoxes.add(value);"
			} else {
				""
			};
			out.push_str(&format!(
				"{kw}class {class} extends {BASE} {{ constructor(v) {{ super(nymphCheckedUInt(v)); }} static direct(v) {{ const value = Object.create(this.prototype); value.v = v;{register} return value; }} }}\n"
			));
		} else if *class == "NTuple" {
			// Native Map's constructor reads pair entries through numeric properties.
			// Keep that boundary compatible while the tuple's canonical storage stays `.v`.
			out.push_str(&format!(
				"{kw}class {class} extends {BASE} {{\n\tindex(key) {{\n\t\treturn this.v[nymphCollectionIndex(key.v, this.v.length, true)];\n\t}}\n\tindexDirect(key) {{\n\t\treturn this.v[nymphCollectionIndex(key.v, this.v.length, false)];\n\t}}\n\tget 0() {{\n\t\treturn this.v[0];\n\t}}\n\tget 1() {{\n\t\treturn this.v[1];\n\t}}\n}}\n"
			));
		} else if *class == "NList" {
			out.push_str(&format!(
				"{kw}class {class} extends {BASE} {{\n\tconstructor(items) {{\n\t\tsuper(NymphPersistentVector.from(items));\n\t}}\n\tcopy(vector) {{\n\t\treturn nymphSetPrototypeOf(new NList(vector), Object.getPrototypeOf(this));\n\t}}\n\tindex(key) {{\n\t\treturn this.v.get(nymphCollectionIndex(key.v, this.v.length, true));\n\t}}\n\tindexDirect(key) {{\n\t\treturn this.v.get(nymphCollectionIndex(key.v, this.v.length, false));\n\t}}\n\tappended(item) {{\n\t\treturn this.copy(this.v.append(item));\n\t}}\n\treplaced(key, item) {{\n\t\treturn this.copy(this.v.replace(nymphListIndex(key), item));\n\t}}\n\tslice(start, end) {{\n\t\treturn this.copy(this.v.slice(nymphListIndex(start), nymphListIndex(end)));\n\t}}\n}}\n"
			));
		} else if *class == "NString" {
			out.push_str(&format!(
				"{kw}class {class} extends {BASE} {{\n\tindex(key) {{\n\t\tconst points = Array.from(this.v);\n\t\treturn new NChar(points[nymphCollectionIndex(key.v, points.length, true)]);\n\t}}\n\tindexDirect(key) {{\n\t\tconst points = Array.from(this.v);\n\t\treturn new NChar(points[nymphCollectionIndex(key.v, points.length, false)]);\n\t}}\n}}\n"
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
	if export {
		out.push_str("export { nymphType, nymphTypeProjection, nymphVariant };\n");
	}
	out
}

/// The inline, non-`export`ed box class definitions prepended into a single
/// emitted module that constructs a box (see the module docs and [`crate::emit`]).
#[must_use]
pub fn box_preamble() -> String {
	class_defs(false, "Option", true)
}

#[must_use]
pub fn box_preamble_release() -> String {
	class_defs(false, "Option", false)
}

/// The `export`ed `std/box` runtime module source, injected into the bundle
/// graph under [`BOX_MODULE_KEY`] (see the module docs).
#[must_use]
pub fn box_module_source() -> String {
	class_defs(true, "Option", true)
}

/// The importable box runtime using the exact emitted compiler-Option binding
/// as its native iterator discriminant namespace.
#[must_use]
pub fn box_module_source_with_option_enum(option_enum_name: &str) -> String {
	class_defs(true, option_enum_name, true)
}

#[must_use]
pub fn box_module_source_with_option_enum_release(option_enum_name: &str) -> String {
	class_defs(true, option_enum_name, false)
}

/// The declaration source paired with [`box_module_source`].
#[must_use]
pub const fn box_module_declarations() -> &'static str {
	BOX_MODULE_DECLARATIONS
}

/// The JS class name a boxed [`nymph_hir::hir::NumKind`] uses. Panics on
/// [`nymph_hir::hir::NumKind::Raw`], which is never a boxed value.
#[must_use]
pub fn num_box_class(kind: nymph_hir::hir::NumKind) -> &'static str {
	use nymph_hir::hir::NumKind;
	match kind {
		NumKind::Float => "NFloat",
		NumKind::Raw => {
			unreachable!("NumKind::Raw is an unboxed internal number, not a box class")
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::process::Command;

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
	fn canonical_type_objects_snapshot_their_argument_identity_sequence() {
		let src = box_module_source();
		assert!(src.contains("args = Object.freeze([...args]);"), "{src}");
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
			"nymphProtocolDisplay",
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
	fn preamble_defines_wrappers_without_export() {
		let src = box_preamble();
		assert!(!src.contains("export"), "preamble must not export:\n{src}");
		assert!(src.contains("class NInt extends NBox"), "{src}");
		assert!(src.contains("class NString extends NBox"), "{src}");
	}

	#[test]
	fn opaque_boxes_preserve_alias_identity_and_reject_nominal_or_shape_repair() {
		let script = format!(
			"{}\n\
			 const host = {{ closed: false }};\n\
			 const boxed = nymphBoxOpaque(117n, host);\n\
			 const alias = boxed;\n\
			 const mismatch = (() => {{ try {{ nymphUnboxOpaque(118n, boxed); return 'repaired'; }} catch (error) {{ return error.message; }} }})();\n\
			 const shape = (() => {{ try {{ nymphUnboxOpaque(117n, {{ v: host }}); return 'repaired'; }} catch (error) {{ return error.message; }} }})();\n\
			 const scalar = (() => {{ try {{ nymphBoxOpaque(117n, 1); return 'repaired'; }} catch (error) {{ return error.message; }} }})();\n\
			 nymphUnboxOpaque(117n, alias).closed = true;\n\
			 console.log([nymphUnboxOpaque(117n, boxed) === host, host.closed, 'equals' in boxed, 'hash' in boxed, JSON.stringify(boxed), mismatch, shape, scalar].join('|'));",
			box_preamble_release(),
		);
		let output = Command::new("node")
			.arg("--input-type=module")
			.arg("--eval")
			.arg(script)
			.output()
			.expect("Node must be available for opaque FFI runtime tests");
		assert!(
			output.status.success(),
			"{}",
			String::from_utf8_lossy(&output.stderr)
		);
		assert_eq!(
			String::from_utf8_lossy(&output.stdout).trim(),
			"true|true|false|false|{}|opaque external identity mismatch|opaque external identity mismatch|trusted opaque FFI must return a live reference"
		);
	}

	#[test]
	fn list_wrapper_exposes_only_persistent_operations() {
		let src = box_module_source();
		assert!(src.contains("appended(item)"), "{src}");
		assert!(
			src.contains("return this.copy(this.v.append(item))"),
			"{src}"
		);
		assert!(!src.contains("legacy"), "{src}");
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

	#[test]
	fn activation_runtime_drives_states_once_and_unwinds_cancellation_in_reverse() {
		let script = format!(
			"{}\n\
			 const order = [];\n\
			 const descend = nymphCallable((frame) => {{\n\
			   const n = frame.liveLocals[0];\n\
			   nymphRegisterCleanup(() => order.push(`a${{n}}`));\n\
			   nymphRegisterCleanup(() => order.push(`b${{n}}`));\n\
			   return n === 0 ? nymphReturn(n) : nymphTailCall(descend, undefined, [n - 1], n);\n\
			 }});\n\
			 if (nymphActivate(descend, undefined, [100000], 0) !== 0) throw new Error('tail result');\n\
			 if (nymphNextFrameSlot !== 1) throw new Error('tail allocated another logical frame');\n\
			 if (order[0] !== 'b100000' || order[1] !== 'a100000' || order.at(-1) !== 'a0') throw new Error('cleanup order');\n\
			 const child = nymphCallable((frame) => nymphReturn(frame.liveLocals[0] + 1));\n\
			 const parent = nymphCallable((frame) => {{\n\
			   if (frame.resumeState === 0) return nymphPush(child, undefined, [frame.liveLocals[0]], 8, 1, 1);\n\
			   return nymphReturn(frame.liveLocals[1] + 1);\n\
			 }});\n\
			 if (nymphActivate(parent, undefined, [1], 7) !== 3) throw new Error('push resume');\n\
			 if (nymphNextFrameSlot !== 3) throw new Error('non-tail call did not push one frame');\n\
			 const external = (value) => value + 1;\n\
			 const externalParent = nymphCallable((frame) => {{\n\
			   if (frame.resumeState === 0) return nymphPush(external, undefined, [frame.liveLocals[0]], 8, 1, 1);\n\
			   return nymphReturn(frame.liveLocals[1] + 1);\n\
			 }});\n\
			 if (nymphActivate(externalParent, undefined, [1], 8) !== 3) throw new Error('external direct result');\n\
			 if (nymphNextFrameSlot !== 4) throw new Error('external call pushed a frame');\n\
			 let effects = 0;\n\
			 const suspended = nymphCallable((frame) => {{\n\
			   if (frame.resumeState === 0) return nymphSuspend(() => {{ effects += 1; return 41; }}, 1, 0);\n\
			   return nymphReturn(frame.liveLocals[0] + 1);\n\
			 }});\n\
			 const retained = nymphActivate(suspended, undefined, [], 9);\n\
			 if (retained.value !== 41 || effects !== 1) throw new Error('suspend effect count');\n\
			 if (retained.resume() !== 42 || effects !== 1) throw new Error('resume replayed effect');\n\
			 const fail = nymphCallable(() => {{\n\
			   nymphRegisterCleanup(() => {{ throw new Error('first'); }});\n\
			   nymphRegisterCleanup(() => {{ throw new Error('second'); }});\n\
			   return nymphDefect(new Error('primary'));\n\
			 }});\n\
			 try {{ nymphActivate(fail, undefined, [], 10); throw new Error('missing defect'); }}\n\
			 catch (error) {{\n\
			   if (!(error instanceof AggregateError)) throw error;\n\
			   if (error.errors.map((item) => item.message).join(',') !== 'primary,second,first') throw error;\n\
			 }}\n\
			 const cancelOrder = [];\n\
			 const waitingChild = nymphCallable(() => {{\n\
			   nymphRegisterCleanup(() => cancelOrder.push('child-outer'));\n\
			   nymphEnterCleanupScope();\n\
			   nymphRegisterCleanup(() => cancelOrder.push('child-inner'));\n\
			   return nymphSuspend(() => 'waiting', 1, 0);\n\
			 }});\n\
			 const waitingParent = nymphCallable((frame) => {{\n\
			   nymphRegisterCleanup(() => cancelOrder.push('parent-first'));\n\
			   nymphRegisterCleanup(() => cancelOrder.push('parent-second'));\n\
			   return nymphPush(waitingChild, undefined, [], 11, 1, 0);\n\
			 }});\n\
			 const cancellation = nymphActivate(waitingParent, undefined, [], 11);\n\
			 const reason = new Error('cancel');\n\
			 try {{ cancellation.cancel(reason); throw new Error('missing cancellation'); }}\n\
			 catch (error) {{ if (error !== reason) throw error; }}\n\
			 if (cancelOrder.join(',') !== 'child-inner,child-outer,parent-second,parent-first') throw new Error(`cancel order: ${{cancelOrder}}`);\n\
			 const nested = nymphCallable(() => nymphReturn(nymphActivate(child, undefined, [0], 12)));\n\
			 try {{ nymphActivate(nested, undefined, [], 12); throw new Error('missing nested guard'); }}\n\
			 catch (error) {{ if (!String(error).includes('must be pushed by the activation driver')) throw error; }}\n",
			ACTIVATION_RUNTIME
		);
		let output = Command::new("node")
			.arg("--input-type=module")
			.arg("--eval")
			.arg(script)
			.output()
			.expect("Node must be available for codegen runtime tests");
		assert!(
			output.status.success(),
			"{}",
			String::from_utf8_lossy(&output.stderr)
		);
	}

	#[test]
	fn state_transition_closes_old_state_and_cleans_new_state_on_failure() {
		let script = format!(
			"{}\n\
			 const successOrder = [];\n\
			 const success = nymphCallable(() => {{\n\
			   nymphEnterCleanupScope();\n\
			   const oldA = nymphRegisterCleanup(() => successOrder.push('old-a'));\n\
			   const oldB = nymphRegisterCleanup(() => successOrder.push('old-b'));\n\
			   nymphEnterCleanupScope();\n\
			   nymphRegisterCleanup(() => successOrder.push('body'));\n\
			   nymphEnterCleanupScope();\n\
			   const newA = nymphRegisterCleanup(() => successOrder.push('new-a'));\n\
			   const newB = nymphRegisterCleanup(() => successOrder.push('new-b'));\n\
			   nymphCommitStateTransition(2, [oldA, newA, oldB, newB]);\n\
			   return nymphReturn(undefined);\n\
			 }});\n\
			 nymphActivate(success, undefined, [], 0);\n\
			 if (successOrder.join(',') !== 'body,old-b,old-a,new-b,new-a') throw new Error(`success order: ${{successOrder}}`);\n\
			 const failureOrder = [];\n\
			 const failure = nymphCallable(() => {{\n\
			   nymphEnterCleanupScope();\n\
			   const oldA = nymphRegisterCleanup(() => failureOrder.push('old-a'));\n\
			   const oldB = nymphRegisterCleanup(() => failureOrder.push('old-b'));\n\
			   nymphEnterCleanupScope();\n\
			   nymphRegisterCleanup(() => {{ failureOrder.push('body'); throw new Error('body failed'); }});\n\
			   nymphEnterCleanupScope();\n\
			   const newA = nymphRegisterCleanup(() => failureOrder.push('new-a'));\n\
			   const newB = nymphRegisterCleanup(() => failureOrder.push('new-b'));\n\
			   nymphCommitStateTransition(2, [oldA, newA, oldB, newB]);\n\
			   return nymphReturn(undefined);\n\
			 }});\n\
			 try {{ nymphActivate(failure, undefined, [], 0); throw new Error('missing failure'); }}\n\
			 catch (error) {{ if (!String(error).includes('body failed')) throw error; }}\n\
			 if (failureOrder.join(',') !== 'body,old-b,old-a,new-b,new-a') throw new Error(`failure order: ${{failureOrder}}`);\n",
			ACTIVATION_RUNTIME
		);
		let output = Command::new("node")
			.arg("--input-type=module")
			.arg("--eval")
			.arg(script)
			.output()
			.expect("Node must be available for codegen runtime tests");
		assert!(
			output.status.success(),
			"{}",
			String::from_utf8_lossy(&output.stderr)
		);
	}

	#[test]
	fn structured_task_runtime_obeys_execution_cancellation_and_ownership_contracts() {
		let script = [ACTIVATION_RUNTIME, TASK_RUNTIME, TASK_RUNTIME_TESTS].join("\n");
		let output = Command::new("node")
			.arg("--input-type=module")
			.arg("--eval")
			.arg(script)
			.output()
			.expect("Node must be available for task runtime tests");
		assert!(
			output.status.success(),
			"{}",
			String::from_utf8_lossy(&output.stderr)
		);
		assert_eq!(
			String::from_utf8_lossy(&output.stdout).trim(),
			"structured task runtime assertions passed"
		);
	}
}
